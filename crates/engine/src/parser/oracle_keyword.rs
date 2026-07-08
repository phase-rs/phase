use std::borrow::Cow;

use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::{alpha1, space0, space1};
use nom::combinator::{all_consuming, eof, not, opt, peek, value};
use nom::sequence::preceded;
use nom::Parser;

use super::oracle_cost::parse_oracle_cost;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::primitives::{scan_at_word_boundaries, scan_contains, split_once_on};
use super::oracle_quantity::parse_cda_quantity;
use super::oracle_target::parse_type_phrase;
use super::oracle_util::{strip_reminder_text, strip_where_x_is_clause};
use crate::types::ability::{
    AbilityCost, AdditionalCost, ControllerRef, CostObjectCount, Effect, EffectScope, FilterProp,
    QuantityExpr, SacrificeRequirement, TapStateChange, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::keywords::{
    normalize_bands_with_other_quality, BloodthirstValue, BuybackCost, CyclingCost, EmbalmCost,
    EternalizeCost, FlashbackCost, Keyword, WardCost,
};
use crate::types::mana::{ManaCost, ManaCostShard};
use crate::types::zones::Zone;

/// CR 702.16 + CR 702.11f: Expand compound "X from A and from B" keyword lines.
/// Handles both "protection from X and from Y" and "hexproof from X and from Y"
/// by splitting into individual keyword entries. Also expands the
/// "from each color" / "from all colors" shorthand (CR 105.2) into one entry
/// per WUBRG color so the runtime gets typed `Color(ManaColor)` variants
/// instead of an opaque string. Bare "each color"/"all colors" only — phrases
/// like "each color that's not in your commander's color identity" (Commander's
/// Plate) or "each color with the most votes" (Council Guardian) carry
/// additional qualifiers and pass through unchanged for a future dynamic
/// handler.
///
/// CR 702.16i: also expands a bare comma-list continuation — after a
/// "protection from A" prefix, subsequent list members that are neither
/// "from …"-prefixed continuations nor genuine trailing keywords (e.g. Tinfoil
/// Helm's "protection from aliens, birds, …, and hybrid mana", or a color list
/// "protection from white, blue, red") each become their own protection entry.
/// A genuine trailing keyword (Akroma's "vigilance") resets the prefix and
/// passes through unexpanded.
fn protection_prefix(i: &str) -> nom::IResult<&str, &'static str, OracleError<'_>> {
    // (prefix_with_space, emit_prefix_no_space) — strip the prefix+space,
    // emit the prefix without space. Shared by the prefix branch and the
    // fast-path guard so both recognize the same set.
    alt((
        value("protection from", tag("protection from ")),
        value("hexproof from", tag("hexproof from ")),
    ))
    .parse(i)
}

pub(crate) fn expand_protection_parts<'a>(parts: &[&'a str]) -> Vec<Cow<'a, str>> {
    // Fast path: skip allocation when no expansion is needed
    if !parts.iter().any(|p| {
        let l = p.to_ascii_lowercase();
        scan_contains(&l, "and from ")
            || contains_each_or_all_colors_phrase(&l)
            || tag::<_, _, OracleError<'_>>("from ")
                .parse(l.as_str())
                .is_ok()
            || tag::<_, _, OracleError<'_>>("and from ")
                .parse(l.as_str())
                .is_ok()
    }) && !(parts.len() > 1
        && parts
            .iter()
            .any(|p| protection_prefix(&p.to_ascii_lowercase()).is_ok()))
    {
        return parts.iter().map(|&p| Cow::Borrowed(p)).collect();
    }

    let mut expanded: Vec<Cow<'a, str>> = Vec::new();
    // Track which keyword prefix we're expanding (None, "protection", or "hexproof")
    let mut active_prefix: Option<&'static str> = None;

    for &part in parts {
        let lower = part.to_ascii_lowercase();

        // Check for "protection from X and from Y" or "hexproof from X and from Y"
        let prefix_match: Option<&str> = protection_prefix(lower.as_str()).ok().map(|(_, v)| v);

        if let Some(prefix) = prefix_match {
            // Strip "protection from " or "hexproof from " (prefix + space)
            let after = &lower[prefix.len() + 1..]; // +1 for the trailing space
                                                    // CR 702.11f / CR 702.16: split on " and from "
            let mut remainder = after;
            while let Ok((_, (before, rest))) = split_once_on(remainder, " and from ") {
                push_quality_entry(&mut expanded, prefix, before);
                remainder = rest;
            }
            push_quality_entry(&mut expanded, prefix, remainder);
            active_prefix = Some(prefix);
        } else if let Some(pfx) = active_prefix {
            if let Ok((rest, _)) =
                alt((tag::<_, _, OracleError<'_>>("and from "), tag("from "))).parse(lower.as_str())
            {
                // CR 702.16g: ", and from Zombies" or ", from Werewolves" —
                // an explicit "from"-prefixed continuation of the prior prefix.
                push_quality_entry(&mut expanded, pfx, rest);
            } else if is_bare_protection_quality(&lower) {
                // CR 702.16i: bare comma-list member — a simple subtype/color/
                // quality noun phrase that continues the shorthand list
                // ("protection from aliens, birds, …" or "protection from white,
                // blue, red"). Each such member behaves as its own separate
                // protection ability.
                push_quality_entry(&mut expanded, pfx, &lower);
            } else {
                // Anything that is NOT a bare quality — a genuine trailing
                // keyword (Akroma's "vigilance"), a verb-clause restriction rider
                // ("and can't be blocked by …"), or a leaked relative clause from
                // a stripped trailing sentence ("equipment you control that are
                // already attached to it") — ends the protection list and passes
                // through unexpanded for the downstream clause splitter.
                active_prefix = None;
                expanded.push(Cow::Borrowed(part));
            }
        } else {
            expanded.push(Cow::Borrowed(part));
        }
    }
    expanded
}

/// CR 702.16i + CR 702.16a: Positive gate for a bare protection-list
/// continuation member. Per CR 702.16i a "protection from each [set]" member is
/// a *characteristic, quality, or player* — in practice a color ("white"), a
/// single subtype/type word ("birds", "aliens"), or a short bounded noun phrase
/// ("hybrid mana"). It is NEVER:
///   - a genuine non-protection keyword (Akroma's trailing "vigilance") —
///     these must reset the list;
///   - a verb-clause restriction rider ("can't be blocked by …") from a
///     compound grant line — these must reset so the downstream clause
///     splitter handles them;
///   - a leaked relative clause from a stripped trailing sentence
///     ("equipment you control that are already attached to it").
///
/// So a member qualifies as a bare quality iff it is a short (1–2 word) noun
/// phrase, contains no clause-connective/relative markers, and is either a
/// recognized creature/type subtype OR does not map to a concrete non-protection
/// keyword. A recognized subtype ALWAYS qualifies (subtype precedence), so a
/// token that happens to be both a subtype and a keyword name still continues
/// the list rather than resetting it. WUBRG colors and Tinfoil subtypes
/// (aliens/birds/eldrazi/lizards/mutants/robots/yetis) satisfy this; "hybrid
/// mana" (2 words) also qualifies; the failing riders/leaks above do not.
fn is_bare_protection_quality(word: &str) -> bool {
    let trimmed = word.trim().trim_end_matches(['.', ',', ';']).trim();
    if trimmed.is_empty() {
        return false;
    }
    // At most a two-word noun phrase. Longer phrases are qualified clauses /
    // leaked prose, not bare qualities (CR 702.16i members are atomic).
    let word_count = trimmed.split_whitespace().count();
    if word_count > 2 {
        return false;
    }
    // Clause-connective / relative / possessive markers never appear in a bare
    // quality — their presence signals a leaked clause, not a quality.
    if trimmed.split_whitespace().any(|w| {
        matches!(
            w,
            "that" | "which" | "who" | "you" | "your" | "with" | "of" | "this" | "already" | "and"
        )
    }) {
        return false;
    }
    // CR 702.16i + CR 205.3 (205.3m creature / 205.3g artifact / ... subtypes):
    // a recognized subtype is always a protection
    // *quality*, even when the token happens to collide with a keyword name.
    // Some tokens are simultaneously a keyword and a subtype (a shifting overlap
    // as MTGJSON's card-type vocabulary and the keyword set both grow — e.g. a
    // creature type that shares a spelling with a keyword ability). Protection
    // "from <subtype>" is the intended reading, so the subtype classification
    // must take precedence over the keyword-reset check below; otherwise a bare
    // list like "protection from white, <subtype>, …" would wrongly reset and
    // drop the subtype member. `is_subtype_word` is the canonical single-token
    // subtype recognizer (the same vocabulary `game::keywords`'s
    // `source_subtype_matches_protection_quality` resolves against); it only
    // accepts atomic tokens, so gate on the single-word case and let the
    // multi-word "hybrid mana" quality fall through to the keyword guard.
    if word_count == 1 && crate::parser::oracle_util::is_subtype_word(trimmed) {
        return true;
    }
    // A concrete non-protection keyword (Akroma's "vigilance", "first strike",
    // "flying") resets the list. `map_keyword` maps unrecognized words to `None`,
    // so colors and subtypes (which are not keywords) pass this guard.
    !matches!(
        super::oracle_static::map_keyword(trimmed),
        Some(kw) if !matches!(kw, Keyword::Unknown(_)) && !is_protection_or_hexproof(&kw)
    )
}

/// CR 702.16 / CR 702.11: Recognize a protection- or hexproof-family keyword.
/// A protection/hexproof result must NOT reset the active protection list.
fn is_protection_or_hexproof(kw: &Keyword) -> bool {
    matches!(
        kw,
        Keyword::Protection(_) | Keyword::Hexproof | Keyword::HexproofFrom(_)
    )
}

/// Push one "<prefix> <quality>" entry — or 5 WUBRG entries when the quality
/// is the bare "each color" / "all colors" shorthand. CR 702.16 + CR 105.2:
/// "protection from each color" means protection from W, U, B, R, AND G
/// simultaneously (Akroma's Will reminder text on Spectra Ward confirms
/// this enumeration). Equivalent reasoning applies to "hexproof from each
/// color" under CR 702.11d. The normalized lookup tolerates trailing
/// punctuation (period/comma/semicolon) in case an upstream caller hasn't
/// stripped it; the emitted non-shorthand entry preserves the original
/// quality slice to avoid changing behavior for cards with qualifier text.
fn push_quality_entry<'a>(out: &mut Vec<Cow<'a, str>>, prefix: &str, quality: &str) {
    let q = quality.trim();
    let normalized = q.trim_end_matches(['.', ',', ';']).to_ascii_lowercase();
    if normalized == "each color" || normalized == "all colors" {
        for color in ["white", "blue", "black", "red", "green"] {
            out.push(Cow::Owned(format!("{prefix} {color}")));
        }
    } else {
        out.push(Cow::Owned(format!("{prefix} {q}")));
    }
}

/// CR 105.2: Word-boundary-aware check for "from each color" / "from all
/// colors" — distinguishes the bare WUBRG shorthand from longer color-stem
/// words like "from each colored permanent". The trailing `peek(not(alpha1))`
/// guard requires the match to end at a non-alphabetic boundary, so
/// `scan_contains` overmatches like "from each colored ..." are rejected
/// at the fast-path stage. Correctness is preserved either way (the slow
/// path's `push_quality_entry` exact-match guard refuses to expand qualified
/// phrases), but this keeps the fast-path optimization sound under future
/// Oracle text.
fn contains_each_or_all_colors_phrase(text: &str) -> bool {
    scan_at_word_boundaries(text, |i| {
        let (rest, _) = alt((
            tag::<_, _, OracleError<'_>>("from each color"),
            tag::<_, _, OracleError<'_>>("from all colors"),
        ))
        .parse(i)?;
        peek(not(alpha1::<_, OracleError<'_>>)).parse(rest)?;
        Ok((rest, ()))
    })
    .is_some()
}

/// CR 702.33a-c: Parse a kicker or multikicker keyword line into the casting
/// cost declaration used by the engine. This lives with keyword parsing because
/// Oracle prints kicker as a keyword line, while runtime casting consumes it as
/// `AdditionalCost`.
pub(crate) fn parse_kicker_additional_cost_line(raw: &str, lower: &str) -> Option<AdditionalCost> {
    let (lower_after_prefix, repeatable) = alt((
        value(
            true,
            alt((
                tag::<_, _, OracleError<'_>>("multikicker "),
                tag("multikicker—"),
            )),
        ),
        value(false, alt((tag("kicker "), tag("kicker—")))),
    ))
    .parse(lower)
    .ok()?;

    let raw_after_prefix = &raw[raw.len() - lower_after_prefix.len()..];

    if repeatable {
        return Some(AdditionalCost::Kicker {
            costs: vec![parse_kicker_cost_payload(raw_after_prefix)?],
            repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
        });
    }

    let costs = if let Ok((_, (lower_first, lower_second))) =
        split_once_on(lower_after_prefix, " and/or ")
    {
        let separator_len = " and/or ".len();
        let raw_first = &raw_after_prefix[..lower_first.len()];
        let raw_second = &raw_after_prefix[lower_first.len() + separator_len..];
        debug_assert_eq!(lower_second.len(), raw_second.len());
        vec![
            parse_kicker_cost_payload(raw_first)?,
            parse_kicker_cost_payload(raw_second)?,
        ]
    } else {
        vec![parse_kicker_cost_payload(raw_after_prefix)?]
    };

    Some(AdditionalCost::Kicker {
        costs,
        repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
    })
}

fn parse_kicker_cost_payload(input: &str) -> Option<AbilityCost> {
    let stripped = strip_reminder_text(input);
    let cost_text = stripped.trim().trim_end_matches('.').trim();
    if cost_text.is_empty() {
        return None;
    }
    Some(parse_oracle_cost(cost_text))
}

/// Try to extract keywords from a keyword-only line (comma-separated).
/// Returns `Some(keywords)` if the entire line consists of recognizable keywords
/// AND at least one part matches an MTGJSON keyword name (preventing false positives
/// from standalone ability lines like "Equip {1}").
///
/// Returns only keywords not already covered by MTGJSON names — these are typically
/// parameterized keywords where MTGJSON lists the name (e.g. "Protection") but
/// Oracle text has the full form (e.g. "Protection from multicolored").
pub(crate) fn extract_keyword_line(
    line: &str,
    mtgjson_keyword_names: &[String],
) -> Option<Vec<Keyword>> {
    let line_without_reminder = strip_reminder_text(line);
    let line = strip_keyword_activation_cost_prefix(line_without_reminder.trim());

    if mtgjson_keyword_names.is_empty() {
        return parse_mtgjson_missing_standalone_keyword_line(line);
    }

    if mtgjson_keyword_names.iter().any(|n| n == "mobilize") {
        if let Some(kw) = parse_mobilize_keyword_line(line) {
            return Some(vec![kw]);
        }
    }

    if mtgjson_keyword_names.iter().any(|n| n == "firebending") {
        if let Some(kw) = parse_firebending_keyword_line(line) {
            return Some(vec![kw]);
        }
    }

    if mtgjson_keyword_names.iter().any(|n| n == "bloodthirst") {
        if let Some(kw) = parse_bloodthirst_keyword_line(line) {
            if kw == Keyword::Bloodthirst(BloodthirstValue::Fixed(1)) {
                return Some(Vec::new());
            }
            return Some(vec![kw]);
        }
    }

    // CR 303.4a: "Enchant A, B, [and/or] C" — multi-type enchant restriction.
    // The comma-separated list is a single keyword (one TargetFilter::Or), not
    // multiple comma-separated keywords. Detect and handle before the generic
    // comma-split path which would treat "land" and "or planeswalker" as
    // unrecognized keyword parts and reject the line. Gated on MTGJSON reporting
    // "Enchant" so non-enchant "X, Y, or Z" lines are unaffected.
    if mtgjson_keyword_names.iter().any(|n| n == "enchant") {
        if let Some(kw) = try_parse_multi_type_enchant(line) {
            return Some(vec![kw]);
        }
    }

    let raw_parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if raw_parts.is_empty() {
        return None;
    }

    // CR 702.16: Expand "protection from X and from Y" into individual parts
    let parts = expand_protection_parts(&raw_parts);

    let mut any_mtgjson_match = false;
    let mut new_keywords = Vec::new();

    for part in &parts {
        let lower = part.to_lowercase();

        // Check if this part matches or extends an MTGJSON keyword name.
        // Exact match: "flying" == "flying"
        // Prefix match: "protection from multicolored" starts with "protection"
        let mtgjson_match = mtgjson_keyword_names.iter().any(|name| {
            lower == *name
                || lower.strip_prefix(name.as_str()).is_some_and(|rest| {
                    alt((tag::<_, _, OracleError<'_>>(" "), tag("\u{2014}")))
                        .parse(rest)
                        .is_ok()
                })
        });

        if mtgjson_match {
            any_mtgjson_match = true;

            // Exact name match means MTGJSON already carries one parsed copy.
            if mtgjson_keyword_names.contains(&lower) {
                // CR 702.85c / CR 702.40b: keywords whose instances each trigger
                // separately are printed as repeated bare words ("Cascade, cascade"),
                // but MTGJSON's keywords array dedupes them. The Oracle line is the only
                // place printed multiplicity survives — emit one Keyword per occurrence
                // so the runtime's per-instance trigger loop fires correctly. Synthesis
                // reconciles the deduped MTGJSON copy against these.
                if let Some(kw) = parse_keyword_from_oracle(&lower) {
                    if kw.instances_function_separately() {
                        new_keywords.push(kw);
                    }
                }
                continue;
            }

            // Prefix match: Oracle text has more detail (e.g. "protection from red").
            // Extract the full parameterized keyword.
            if let Some(kw) = parse_keyword_from_oracle(&lower) {
                new_keywords.push(kw);
                continue;
            }
        }

        // Not an MTGJSON match — try parsing as any keyword (for keyword-only line validation)
        if let Some(kw) = parse_keyword_from_oracle(&lower) {
            if !matches!(kw, Keyword::Unknown(_)) {
                // Keywords not in MTGJSON (e.g., firebending) must be extracted here.
                // They also validate the line as a keyword line.
                any_mtgjson_match = true;
                new_keywords.push(kw);
                continue;
            }
        }

        // Unrecognized part — not a keyword line
        return None;
    }

    if any_mtgjson_match {
        Some(new_keywords)
    } else {
        None
    }
}

fn strip_keyword_activation_cost_prefix(line: &str) -> &str {
    if let Some(keyword_text) = strip_mana_activation_cost_prefix(line) {
        return keyword_text;
    }
    strip_ticket_activation_cost_prefix(line).unwrap_or(line)
}

fn strip_mana_activation_cost_prefix(line: &str) -> Option<&str> {
    let Ok((rest, _cost)) = nom_primitives::parse_mana_cost.parse(line) else {
        return None;
    };
    strip_activation_cost_dash(rest)
}

fn strip_ticket_activation_cost_prefix(line: &str) -> Option<&str> {
    let lower = line.to_ascii_lowercase();
    let mut rest = lower.as_str();
    let mut consumed = 0;
    let mut matched = false;

    while let Ok((next, _)) = tag::<_, _, OracleError<'_>>("{tk}").parse(rest) {
        matched = true;
        consumed = lower.len() - next.len();
        rest = next;
    }

    matched
        .then(|| &line[consumed..])
        .and_then(strip_activation_cost_dash)
}

fn strip_activation_cost_dash(rest: &str) -> Option<&str> {
    preceded(
        space0,
        alt((
            tag::<_, _, OracleError<'_>>("\u{2014}"),
            tag("\u{2013}"),
            tag("-"),
        )),
    )
    .parse(rest)
    .ok()
    .map(|(keyword_text, _)| keyword_text.trim_start())
}

fn parse_mtgjson_missing_standalone_keyword_line(line: &str) -> Option<Vec<Keyword>> {
    let lower = line.to_lowercase();
    let keyword = parse_keyword_from_oracle(&lower)?;
    match keyword {
        Keyword::ForMirrodin => Some(vec![keyword]),
        // CR 702.89a: Umbra armor (printed as "umbra armor"/"totem armor") is a
        // standalone keyword line MTGJSON does not surface in its `keywords` array,
        // so it must be recovered from the Oracle line here.
        Keyword::TotemArmor => Some(vec![keyword]),
        // CR 702.22: "Bands with other [quality]" carries the quality in Oracle
        // text; MTGJSON's keyword list has no typed payload to preserve it.
        Keyword::BandsWithOther(_) => Some(vec![keyword]),
        _ => None,
    }
}

// CR 702.181a: "Mobilize N" creates N tapped and attacking Warrior tokens.
fn parse_mobilize_keyword_line(line: &str) -> Option<Keyword> {
    let lower = line.trim().trim_end_matches('.').to_ascii_lowercase();
    let (rest, _) = tag::<_, _, OracleError<'_>>("mobilize ")
        .parse(lower.as_str())
        .ok()?;
    let rest = rest.trim();

    if let Ok((remaining, value)) = nom_primitives::parse_number.parse(rest) {
        if remaining.trim().is_empty() {
            return Some(Keyword::Mobilize(QuantityExpr::Fixed {
                value: value as i32,
            }));
        }
    }

    let (rest, _) = tag::<_, _, OracleError<'_>>("x").parse(rest).ok()?;
    let quantity_text = strip_where_x_is_clause(rest)?;
    parse_cda_quantity(quantity_text).map(Keyword::Mobilize)
}

fn parse_firebending_keyword_line(line: &str) -> Option<Keyword> {
    let lower = line.trim().trim_end_matches('.').to_ascii_lowercase();
    let (rest, _) = tag::<_, _, OracleError<'_>>("firebending ")
        .parse(lower.as_str())
        .ok()?;
    let rest = rest.trim();

    if let Ok((remaining, value)) = nom_primitives::parse_number.parse(rest) {
        if remaining.trim().is_empty() {
            return Some(Keyword::Firebending(QuantityExpr::Fixed {
                value: value as i32,
            }));
        }
    }

    let (rest, _) = tag::<_, _, OracleError<'_>>("x").parse(rest).ok()?;
    let quantity_text = strip_where_x_is_clause(rest)?;
    parse_cda_quantity(quantity_text).map(Keyword::Firebending)
}

// Enchant combinators moved to `parser/oracle_nom/enchant.rs` so the MTGJSON
// `FromStr` path (`types/keywords.rs::parse_enchant_target`) and this Oracle-
// line parser compose against the same atoms.
use super::oracle_nom::enchant::{parse_enchant_controller_suffix, parse_enchant_type_list};

/// CR 303.4a + CR 702.5: Parse the Aura's "Enchant [types]" line into a single
/// `Keyword::Enchant(TargetFilter)`. Multi-type lists ("Enchant creature, land,
/// or planeswalker") produce a `TargetFilter::Or` of typed filters so the Aura
/// can legally target any permanent matching any listed type. Single-type
/// lines are left to the legacy `parse_enchant_target` path — this helper only
/// claims the multi-type union the generic path cannot represent. An optional
/// trailing controller clause ("you control" / "an opponent controls") applies
/// uniformly to every leg.
fn try_parse_multi_type_enchant(line: &str) -> Option<Keyword> {
    let lower = line.trim().trim_end_matches('.').to_ascii_lowercase();

    // `enchant ` + list + optional controller + terminator.
    let (rest, _) = tag::<_, _, OracleError<'_>>("enchant ")
        .parse(lower.as_str())
        .ok()?;
    let (rest, legs) = parse_enchant_type_list(rest).ok()?;
    let (rest, controller) = opt(parse_enchant_controller_suffix).parse(rest).ok()?;
    if !rest.is_empty() {
        return None;
    }

    // Multi-type union only — single-type lines fall through to the legacy
    // FromStr path so Pacifism / Rancor / Enchanted-Evening class cards
    // continue to emit plain `Keyword::Enchant(Typed)` instead of `Or{[Typed]}`.
    if legs.len() < 2 {
        return None;
    }

    let filters: Vec<TargetFilter> = legs
        .into_iter()
        .map(|leg| {
            let mut f = TypedFilter::new(leg.type_filter);
            if !leg.properties.is_empty() {
                f = f.properties(leg.properties);
            }
            if let Some(ref c) = controller {
                f = f.controller(c.clone());
            }
            TargetFilter::Typed(f)
        })
        .collect();

    Some(Keyword::Enchant(TargetFilter::Or { filters }))
}

/// CR 702.21a: Parse a non-mana ward cost from the em-dash remainder.
/// Handles "pay N life", "discard a card", "sacrifice a permanent/creature/etc."
/// Also handles compound costs like "{2}, Pay 2 life" → Compound([Mana, PayLife]).
fn parse_ward_cost(cost_text: &str) -> Option<Keyword> {
    let lower = cost_text.trim().trim_end_matches('.').to_lowercase();

    // CR 702.21a: Detect compound costs — comma-separated sub-costs.
    // Only split on ", " that is NOT inside mana braces {}.
    // Example: "{2}, Pay 2 life" → ["{2}", "Pay 2 life"]
    if lower.contains(", ") {
        let parts = split_outside_braces(&lower);
        if parts.len() > 1 {
            let sub_costs: Vec<WardCost> = parts
                .iter()
                .filter_map(|part| parse_ward_cost_single(part.trim()))
                .collect();
            if sub_costs.len() == parts.len() {
                return Some(Keyword::Ward(WardCost::Compound(sub_costs)));
            }
        }
    }

    // Single cost
    let cost = parse_ward_cost_single(&lower)?;
    Some(Keyword::Ward(cost))
}

/// Parse a single ward cost component (not compound).
fn parse_ward_cost_single(lower: &str) -> Option<WardCost> {
    // "pay N life"
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("pay ").parse(lower) {
        if let Some(life_str) = rest.strip_suffix(" life") {
            if let Ok(n) = life_str.trim().parse::<i32>() {
                return Some(WardCost::PayLife(n));
            }
        }
    }

    // "discard a card" / "discard two cards" etc.
    if tag::<_, _, OracleError<'_>>("discard").parse(lower).is_ok() {
        return Some(WardCost::DiscardCard);
    }

    // "sacrifice [N] permanent(s)/creature(s)/etc." — extract count and filter
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("sacrifice ").parse(lower) {
        let (count, after_count) = nom_primitives::parse_number
            .parse(rest)
            .map(|(rem, n)| (n, rem.trim_start()))
            .unwrap_or((
                1,
                rest.strip_prefix("a ")
                    .or(rest.strip_prefix("an "))
                    .unwrap_or(rest),
            ));
        let (filter, _) = parse_type_phrase(after_count);
        return Some(WardCost::Sacrifice { count, filter });
    }

    // CR 702.21a + CR 701.67: "waterbend {N}" — ward cost paid via waterbend mechanic.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("waterbend").parse(lower) {
        let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(rest.trim());
        return Some(WardCost::Waterbend(cost));
    }

    // Fall back to mana cost parsing
    let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(lower.trim());
    Some(WardCost::Mana(cost))
}

/// Split a string on ", " but only when the comma is outside mana braces {}.
fn split_outside_braces(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0u32;
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(text[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(text[start..].trim());
    parts
}

/// CR 702.34a: Parse a flashback cost following the em-dash separator.
/// Handles every shape the Oracle prints after `Flashback—`:
///   - Pure mana                     (degenerate: `Flashback—{2}{R}` is rare; standard "Flashback {cost}" goes through FromStr)
///   - Single non-mana cost          ("tap N untapped white creatures you control", "sacrifice a creature")
///   - Compound (mana + non-mana)    ("{1}{U}, Pay 3 life", "{R}{R}, Discard X cards")
///   - Compound (multiple non-mana)  (none in current data, but composes naturally)
///
/// Delegates to `parse_oracle_cost`, which already splits comma-separated parts into
/// `AbilityCost::Composite`. Dispatches into `FlashbackCost::Mana` only when the result
/// is a single `Mana` sub-cost; otherwise wraps the whole `AbilityCost` in `NonMana`,
/// letting the runtime split (see `split_flashback_cost` in casting.rs) extract the
/// mana sub-cost from a Composite for normal mana payment while routing the residual
/// non-mana sub-costs through `pay_additional_cost`.
fn parse_flashback_cost(cost_text: &str) -> Option<FlashbackCost> {
    let trimmed = cost_text.trim().trim_end_matches('.').trim_end_matches(')');
    // Strip reminder text in parentheses: take everything before the first " (".
    let clean = opt(take_until::<_, _, OracleError<'_>>(" ("))
        .parse(trimmed)
        .map(|(_, before)| before.unwrap_or(trimmed))
        .unwrap_or(trimmed)
        .trim();
    if clean.is_empty() {
        return None;
    }
    let cost = super::oracle_cost::parse_oracle_cost(clean);
    match cost {
        AbilityCost::Mana { cost: mana_cost } => Some(FlashbackCost::Mana(mana_cost)),
        // Filter out parse failures: parse_oracle_cost returns AbilityCost::Unimplemented
        // for unrecognized text. Don't manufacture a meaningless flashback ability.
        AbilityCost::Unimplemented { .. } => None,
        other => Some(FlashbackCost::NonMana(other)),
    }
}

/// CR 702.29a: Parse a cycling cost that appears after the em-dash
/// (e.g., "cycling—pay 2 life" → `CyclingCost::NonMana(PayLife { life: 2 })`).
///
/// Mirrors `parse_flashback_cost` exactly: delegates to `parse_oracle_cost`
/// so compound comma-separated costs compose into `AbilityCost::Composite`,
/// which the synthesis in `database::synthesis::synthesize_cycling` splices
/// alongside the mandatory "discard this card" sub-cost.
/// CR 702.27a: Parse a buyback cost following the em-dash separator
/// (e.g., "buyback—sacrifice a land" on Constant Mists). Mirrors
/// `parse_flashback_cost`: delegates to `parse_oracle_cost` so comma-separated
/// parts compose into `AbilityCost::Composite`, and wraps the result in
/// `BuybackCost::Mana` when it's a pure mana cost or `BuybackCost::NonMana`
/// otherwise.
fn parse_buyback_cost(cost_text: &str) -> Option<BuybackCost> {
    let trimmed = cost_text.trim().trim_end_matches('.').trim_end_matches(')');
    let clean = opt(take_until::<_, _, OracleError<'_>>(" ("))
        .parse(trimmed)
        .map(|(_, before)| before.unwrap_or(trimmed))
        .unwrap_or(trimmed)
        .trim();
    if clean.is_empty() {
        return None;
    }
    let cost = super::oracle_cost::parse_oracle_cost(clean);
    match cost {
        AbilityCost::Mana { cost: mana_cost } => Some(BuybackCost::Mana(mana_cost)),
        AbilityCost::Unimplemented { .. } => None,
        other => Some(BuybackCost::NonMana(other)),
    }
}

/// CR 702.74a: Parse an evoke cost following the em-dash separator
/// (e.g., "evoke—exile a white card from your hand" on Solitude). Mirrors
/// `parse_flashback_cost` / `parse_buyback_cost`: delegates to
/// `parse_oracle_cost` so comma-separated parts compose into
/// `AbilityCost::Composite`, and wraps the result in `EvokeCost::Mana` when
/// it's a pure mana cost or `EvokeCost::NonMana` otherwise.
fn parse_evoke_cost(cost_text: &str) -> Option<crate::types::keywords::EvokeCost> {
    use crate::types::keywords::EvokeCost;
    let trimmed = cost_text.trim().trim_end_matches('.').trim_end_matches(')');
    let clean = opt(take_until::<_, _, OracleError<'_>>(" ("))
        .parse(trimmed)
        .map(|(_, before)| before.unwrap_or(trimmed))
        .unwrap_or(trimmed)
        .trim();
    if clean.is_empty() {
        return None;
    }
    let cost = super::oracle_cost::parse_oracle_cost(clean);
    match cost {
        AbilityCost::Mana { cost: mana_cost } => Some(EvokeCost::Mana(mana_cost)),
        AbilityCost::Unimplemented { .. } => None,
        other => Some(EvokeCost::NonMana(other)),
    }
}

/// CR 702.103a + CR 118.9: Parse a bestow cost following the em-dash separator.
/// Classic Theros bestow ("Bestow {3}{G}{G}") is a pure mana cost delivered via
/// MTGJSON's keywords array (the `FromStr` path). The em-dash form carries a
/// compound cost — "Bestow—{R}, Collect evidence 6." on Detective's Phoenix —
/// where the mana sub-cost is paid normally and the residual non-mana sub-cost
/// (Collect evidence) is paid via `pay_additional_cost`. Mirrors
/// `parse_flashback_cost` / `parse_evoke_cost`: delegates to `parse_oracle_cost`
/// so comma-separated parts compose into `AbilityCost::Composite`, and wraps the
/// result in `BestowCost::Mana` when it's a pure mana cost or `BestowCost::NonMana`
/// otherwise (the runtime split via `split_bestow_cost_components` extracts the
/// mana sub-cost from a Composite for normal payment).
fn parse_bestow_cost(cost_text: &str) -> Option<crate::types::keywords::BestowCost> {
    use crate::types::keywords::BestowCost;
    let trimmed = cost_text.trim().trim_end_matches('.').trim_end_matches(')');
    let clean = opt(take_until::<_, _, OracleError<'_>>(" ("))
        .parse(trimmed)
        .map(|(_, before)| before.unwrap_or(trimmed))
        .unwrap_or(trimmed)
        .trim();
    if clean.is_empty() {
        return None;
    }
    let cost = super::oracle_cost::parse_oracle_cost(clean);
    match cost {
        AbilityCost::Mana { cost: mana_cost } => Some(BestowCost::Mana(mana_cost)),
        AbilityCost::Unimplemented { .. } => None,
        other => Some(BestowCost::NonMana(other)),
    }
}

/// CR 702.30a: Parse an echo cost following the em-dash separator
/// (e.g., "echo—discard a card" on Rakdos Headliner / Deepcavern Imp).
/// Mirrors `parse_evoke_cost`: delegates to `parse_oracle_cost` so
/// comma-separated parts compose into `AbilityCost::Composite`, and wraps the
/// result in `EchoCost::Mana` when it's a pure mana cost or `EchoCost::NonMana`
/// otherwise.
fn parse_echo_cost(cost_text: &str) -> Option<crate::types::keywords::EchoCost> {
    use crate::types::keywords::EchoCost;
    let trimmed = cost_text.trim().trim_end_matches('.').trim_end_matches(')');
    let clean = opt(take_until::<_, _, OracleError<'_>>(" ("))
        .parse(trimmed)
        .map(|(_, before)| before.unwrap_or(trimmed))
        .unwrap_or(trimmed)
        .trim();
    if clean.is_empty() {
        return None;
    }
    let cost = super::oracle_cost::parse_oracle_cost(clean);
    match cost {
        AbilityCost::Mana { cost: mana_cost } => Some(EchoCost::Mana(mana_cost)),
        AbilityCost::Unimplemented { .. } => None,
        other => Some(EchoCost::NonMana(other)),
    }
}

fn parse_cycling_cost(cost_text: &str) -> Option<CyclingCost> {
    let trimmed = cost_text.trim().trim_end_matches('.').trim_end_matches(')');
    // Strip reminder text in parentheses: take everything before the first " (".
    let clean = opt(take_until::<_, _, OracleError<'_>>(" ("))
        .parse(trimmed)
        .map(|(_, before)| before.unwrap_or(trimmed))
        .unwrap_or(trimmed)
        .trim();
    if clean.is_empty() {
        return None;
    }
    let cost = super::oracle_cost::parse_oracle_cost(clean);
    match cost {
        AbilityCost::Mana { cost: mana_cost } => Some(CyclingCost::Mana(mana_cost)),
        AbilityCost::Unimplemented { .. } => None,
        other => Some(CyclingCost::NonMana(other)),
    }
}

/// CR 702.128a + CR 602.1a: Parse an Embalm em-dash cost ("embalm—{2}{W}{W},
/// discard a card" → `EmbalmCost::NonMana(Composite[..])`). Mirrors
/// `parse_cycling_cost`: reminder-strip, delegate to `parse_oracle_cost`, wrap a
/// single `Mana` cost in `Mana`, anything composite/non-mana in `NonMana`, and
/// reject `Unimplemented`.
fn parse_embalm_cost(cost_text: &str) -> Option<EmbalmCost> {
    let trimmed = cost_text.trim().trim_end_matches('.').trim_end_matches(')');
    let clean = opt(take_until::<_, _, OracleError<'_>>(" ("))
        .parse(trimmed)
        .map(|(_, before)| before.unwrap_or(trimmed))
        .unwrap_or(trimmed)
        .trim();
    if clean.is_empty() {
        return None;
    }
    match super::oracle_cost::parse_oracle_cost(clean) {
        AbilityCost::Mana { cost: mana_cost } => Some(EmbalmCost::Mana(mana_cost)),
        AbilityCost::Unimplemented { .. } => None,
        other => Some(EmbalmCost::NonMana(other)),
    }
}

/// CR 702.129a + CR 602.1a: Parse an Eternalize em-dash cost
/// ("eternalize—{3}{U}{U}, discard a card" → `EternalizeCost::NonMana(..)`,
/// Champion of Wits family). Mirrors `parse_embalm_cost`/`parse_cycling_cost`.
fn parse_eternalize_cost(cost_text: &str) -> Option<EternalizeCost> {
    let trimmed = cost_text.trim().trim_end_matches('.').trim_end_matches(')');
    let clean = opt(take_until::<_, _, OracleError<'_>>(" ("))
        .parse(trimmed)
        .map(|(_, before)| before.unwrap_or(trimmed))
        .unwrap_or(trimmed)
        .trim();
    if clean.is_empty() {
        return None;
    }
    match super::oracle_cost::parse_oracle_cost(clean) {
        AbilityCost::Mana { cost: mana_cost } => Some(EternalizeCost::Mana(mana_cost)),
        AbilityCost::Unimplemented { .. } => None,
        other => Some(EternalizeCost::NonMana(other)),
    }
}

fn parse_bloodthirst_keyword_line(line: &str) -> Option<Keyword> {
    let lower = line.to_ascii_lowercase();
    let stripped = strip_reminder_text(&lower);
    let text = stripped.trim().trim_end_matches('.');
    let (rest, _) = tag::<_, _, OracleError<'_>>("bloodthirst ")
        .parse(text)
        .ok()?;
    let value_text = rest.trim();
    if value_text == "x" {
        return Some(Keyword::Bloodthirst(BloodthirstValue::X));
    }
    let (rem, n) = nom_primitives::parse_number.parse(value_text).ok()?;
    if rem.is_empty() {
        Some(Keyword::Bloodthirst(BloodthirstValue::Fixed(n)))
    } else {
        None
    }
}

/// CR 702.48a: Offering — "<Subtype> offering (reminder text)". The leading word
/// is the creature/permanent type a player may sacrifice to cast this spell for
/// its alternative cost (e.g. "Goblin offering", "Artifact offering"). MTGJSON
/// sends only the bare "Offering" keyword name with no quality, so without this
/// the line carrying the quality is never turned into `Keyword::Offering(quality)`
/// and the cast path (which keys on that quality) is unreachable.
fn parse_offering_keyword_line(line: &str) -> Option<Keyword> {
    let stripped = strip_reminder_text(line);
    let text = stripped.trim().trim_end_matches('.').trim();
    // Input is lowercased; the whole line must be "<single word> offering".
    let (_, (quality, _)) = all_consuming((alpha1, tag::<_, _, OracleError<'_>>(" offering")))
        .parse(text)
        .ok()?;
    // Canonicalize to subtype casing ("goblin" -> "Goblin") so the runtime cost
    // path (`effective_offering_quality`) matches the printed subtype.
    let mut chars = quality.chars();
    let capitalized = chars.next()?.to_ascii_uppercase().to_string() + chars.as_str();
    Some(Keyword::Offering(capitalized))
}

/// CR 702.167b: Build the typed materials filter for a Craft ability. A bare
/// type/subtype in the materials clause matches *either* a permanent on the
/// battlefield you control *or* a card in your graveyard you own (an exception
/// to CR 109.2). The result is a `TargetFilter::Or` of those two zone-scoped
/// legs so the dual-zone eligibility helper and the runtime filter evaluator
/// agree on what may be exiled. This is the single authority for the materials
/// filter shape — `FromStr`, `keyword_from_tagged`, and the Oracle-line parser
/// all route through it.
pub fn craft_materials_filter(types: &[TypeFilter]) -> TargetFilter {
    craft_materials_from_typed_filter(TypedFilter {
        type_filters: types.to_vec(),
        ..TypedFilter::default()
    })
}

fn craft_materials_from_typed_filter(filter: TypedFilter) -> TargetFilter {
    let with_types = |base: TypedFilter| -> TypedFilter {
        filter
            .type_filters
            .iter()
            .cloned()
            .fold(base, |acc, tf| acc.with_type(tf))
    };
    TargetFilter::Or {
        filters: vec![
            // Battlefield leg: a permanent you control matching the printed materials class.
            TargetFilter::Typed(
                with_types(TypedFilter::permanent())
                    .controller(ControllerRef::You)
                    .properties({
                        let mut props = filter.properties.clone();
                        props.push(FilterProp::InZone {
                            zone: Zone::Battlefield,
                        });
                        props
                    }),
            ),
            // Graveyard leg: a card you own matching the printed materials class.
            TargetFilter::Typed(with_types(TypedFilter::card()).properties({
                let mut props = filter.properties.clone();
                props.push(FilterProp::InZone {
                    zone: Zone::Graveyard,
                });
                props.push(FilterProp::Owned {
                    controller: ControllerRef::You,
                });
                props
            })),
        ],
    }
}

fn craft_materials_from_filter(filter: TargetFilter) -> Option<TargetFilter> {
    match filter {
        TargetFilter::Typed(typed) => Some(craft_materials_from_typed_filter(typed)),
        TargetFilter::Or { filters } => Some(TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(craft_materials_from_filter)
                .collect::<Option<Vec<_>>>()?,
        }),
        _ => None,
    }
}

fn craft_materials_any() -> TargetFilter {
    craft_materials_from_typed_filter(TypedFilter::default())
}

/// CR 702.167b: Default materials class (creature) used when only the bare
/// "Craft" keyword is available (no Oracle line to specify the materials).
pub fn craft_materials_default() -> TargetFilter {
    craft_materials_filter(&[TypeFilter::Creature])
}

/// CR 702.167a/b: Parse a "craft with [materials] [cost]" Oracle line into a
/// `Keyword::Craft`. Craft owns the count prefix and the CR 702.167b dual-zone
/// lowering; the material class itself delegates to the shared type-phrase
/// parser so colors, type disjunctions, and subtypes stay on the project-wide
/// target-filter grammar instead of a Craft-only tag list.
fn parse_craft_keyword_line(text: &str) -> Option<Keyword> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("craft with ")
        .parse(text)
        .ok()?;
    let (rest, (materials, count)) = parse_craft_materials(rest)?;
    let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(rest.trim());
    Some(Keyword::Craft {
        cost,
        materials,
        count,
    })
}

/// CR 702.167b: Parse the materials clause of a craft line into
/// `(filter, count, remainder)`. The remainder is the trailing mana-cost text.
fn parse_craft_materials(input: &str) -> Option<(&str, (TargetFilter, CostObjectCount))> {
    let (cost_text, materials_text) = take_until::<_, _, OracleError<'_>>("{").parse(input).ok()?;
    let materials_text = materials_text.trim();
    if parse_craft_unmodeled_material_clause(materials_text).is_ok() {
        return None;
    }
    let (materials_text, count) = parse_craft_material_count(materials_text)?;
    let materials_text = materials_text.trim();
    let materials = if materials_text.is_empty() {
        craft_materials_any()
    } else if parse_craft_relative_material_clause(materials_text).is_ok()
        || take_until::<_, _, OracleError<'_>>(",")
            .parse(materials_text)
            .is_ok()
    {
        return None;
    } else {
        let (filter, rest) = parse_type_phrase(materials_text);
        if !rest.trim().is_empty() {
            return None;
        }
        craft_materials_from_filter(filter)?
    };
    Some((cost_text, (materials, count)))
}

fn parse_craft_relative_material_clause(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    let (rest, _) = tag("that").parse(input)?;
    let (rest, _) = alt((value((), space1), value((), eof))).parse(rest)?;
    Ok((rest, ()))
}

fn parse_craft_unmodeled_material_clause(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    alt((
        parse_craft_relative_material_clause,
        value(
            (),
            (
                nom_primitives::parse_number,
                space1,
                parse_craft_relative_material_clause,
            ),
        ),
    ))
    .parse(input)
}

fn parse_craft_material_count(input: &str) -> Option<(&str, CostObjectCount)> {
    if input == "one or more" {
        return Some(("", CostObjectCount::at_least(1)));
    }
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("one or more ").parse(input) {
        return Some((rest, CostObjectCount::at_least(1)));
    }
    if let Ok((rest, count)) = nom_primitives::parse_number.parse(input) {
        if rest == " or more" {
            return Some(("", CostObjectCount::at_least(count)));
        }
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" or more ").parse(rest) {
            return Some((rest, CostObjectCount::at_least(count)));
        }
        if let Ok((rest, _)) = space1::<_, OracleError<'_>>.parse(rest) {
            return Some((rest, CostObjectCount::exactly(count)));
        }
    }
    Some((input, CostObjectCount::exactly(1)))
}

/// CR 702.18a / 702.11a: the CR keyword that a descriptive "can't be the target
/// [of ...]" prohibition corresponds to. These phrasings ARE Shroud / Hexproof
/// (CR 702.18a: "Shroud" means "can't be the target of spells or abilities";
/// CR 702.11a: Hexproof restricts only opponents' spells/abilities), so callers
/// map them onto the existing keyword targeting checks rather than a bespoke rule
/// static, getting the correct controller scope for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CantBeTargetedScope {
    /// CR 702.18a: blanket — can't be targeted by ANY player (Shroud).
    AnyPlayer,
    /// CR 702.11a: only spells/abilities an opponent controls (Hexproof).
    OpponentsOnly,
}

/// Classify a predicate carrying a "can't be the target[ed]" prohibition.
///
/// Returns `None` when no such prohibition is present, or when its scope is one
/// this parser does not yet model precisely (e.g. a specific spell type), so a
/// caller never collapses an unrecognized scope into a blanket restriction.
pub(crate) fn classify_cant_be_targeted(predicate_lower: &str) -> Option<CantBeTargetedScope> {
    let is_prohibition = scan_contains(predicate_lower, "can't be the target")
        || scan_contains(predicate_lower, "cannot be the target")
        || scan_contains(predicate_lower, "can't be targeted")
        || scan_contains(predicate_lower, "cannot be targeted");
    if !is_prohibition {
        return None;
    }
    // CR 702.11a: an opponent-controlled qualifier makes this Hexproof, not Shroud.
    if scan_contains(predicate_lower, "your opponents control")
        || scan_contains(predicate_lower, "an opponent controls")
    {
        return Some(CantBeTargetedScope::OpponentsOnly);
    }
    // CR 702.18a: the bare form ("~ can't be targeted") or the unqualified
    // "spells or abilities" scope is blanket Shroud. Any other qualifier is left
    // unclassified so it is not mistreated as a blanket restriction.
    let bare = !scan_contains(predicate_lower, " of ");
    let unqualified_scope = scan_contains(predicate_lower, "spells or abilities")
        || scan_contains(predicate_lower, "spell or ability")
        || scan_contains(predicate_lower, "spells and abilities");
    (bare || unqualified_scope).then_some(CantBeTargetedScope::AnyPlayer)
}

///
/// Oracle text uses space-separated format: "protection from red", "ward {2}",
/// "flashback {2}{U}". Converts to the colon format that `FromStr` expects,
/// handling the "from" preposition used by protection keywords.
pub(crate) fn parse_keyword_from_oracle(text: &str) -> Option<Keyword> {
    use crate::types::keywords::PartnerType;

    // CR 702.124: Partner variant keywords — must come BEFORE generic "partner" match.
    // MTGJSON sends Character Select, Friends Forever, and generic Partner all as keyword "Partner".
    // Oracle text em-dash suffix disambiguates them.
    if let Ok((_, result)) = alt((
        value(
            Some(Keyword::Partner(PartnerType::CharacterSelect)),
            tag::<_, _, OracleError<'_>>("partner\u{2014}character select"),
        ),
        value(
            Some(Keyword::Partner(PartnerType::FriendsForever)),
            tag("partner\u{2014}friends forever"),
        ),
        value(
            Some(Keyword::Partner(PartnerType::ChooseABackground)),
            tag("choose a background"),
        ),
        value(
            Some(Keyword::Partner(PartnerType::DoctorsCompanion)),
            alt((tag("doctor\u{2019}s companion"), tag("doctor's companion"))),
        ),
        // CR 702.124c: "Partner with [Name]" — handled at the build_oracle_face level
        // via MTGJSON keyword detection. Skip here to avoid producing a duplicate with
        // incorrect casing from the lowered oracle text.
        value(None, tag("partner with ")),
    ))
    .parse(text)
    {
        return result;
    }

    // CR 702.24: Cumulative upkeep granted via a quoted ability ("[enchanted
    // creature] has \"Cumulative upkeep {1}\"") routes through this shared keyword
    // parser; the top-level keyword-line path calls the dedicated cost-aware
    // parser directly, so delegate to it here too (Mana Chains, Dreams of the
    // Dead, Decomposition).
    if let Some(kw) = super::oracle_special::parse_cumulative_upkeep_keyword(text) {
        return Some(kw);
    }

    if let Some(kw) = parse_bloodthirst_keyword_line(text) {
        return Some(kw);
    }

    if let Some(kw) = parse_firebending_keyword_line(text) {
        return Some(kw);
    }

    // CR 702.48a: "<Subtype> offering" — the Oracle line carries the quality the
    // bare "Offering" keyword name lacks.
    if let Some(kw) = parse_offering_keyword_line(text) {
        return Some(kw);
    }

    // CR 702.112a: Renown N — parameterized keyword from Oracle text.
    // MTGJSON's keyword list carries only "Renown"; the Oracle line supplies N.
    if let Ok((_, (_, _, n))) = all_consuming((
        tag::<_, _, OracleError<'_>>("renown"),
        space1,
        nom_primitives::parse_number,
    ))
    .parse(text)
    {
        return Some(Keyword::Renown(n));
    }

    // CR 702.68a: Frenzy N — parameterized keyword from Oracle/reminder/grant text.
    // MTGJSON's keyword list carries only "Frenzy"; the Oracle line supplies N.
    if let Ok((_, (_, _, n))) = all_consuming((
        tag::<_, _, OracleError<'_>>("frenzy"),
        space1,
        nom_primitives::parse_number,
    ))
    .parse(text)
    {
        return Some(Keyword::Frenzy(n));
    }

    // CR 702.167a/b: Craft with [materials] [cost] — the Oracle line carries the
    // materials class and activation cost that the bare "Craft" keyword lacks.
    if let Some(kw) = parse_craft_keyword_line(text) {
        return Some(kw);
    }
    if tag::<_, _, OracleError<'_>>("craft with ")
        .parse(text)
        .is_ok()
    {
        return None;
    }

    // First try direct parse (handles simple keywords like "flying")
    let direct: Keyword = text.parse().unwrap();
    if !matches!(direct, Keyword::Unknown(_)) {
        return Some(direct);
    }

    // CR 702.29e: "basic landcycling {cost}" — multi-word typecycling variant.
    // Must be checked before the single-word typecycling guard below.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("basic landcycling").parse(text) {
        let cost_str = rest.trim();
        if !cost_str.is_empty() {
            let colon_form = format!("typecycling:Basic Land:{cost_str}");
            let parsed: Keyword = colon_form.parse().unwrap();
            if !matches!(parsed, Keyword::Unknown(_)) {
                return Some(parsed);
            }
        }
    }

    // CR 702.29a: Cycling with em-dash cost (non-mana or compound cost).
    // "cycling—pay 2 life" (Street Wraith), "cycling—{2}{R}" (if any), or compound.
    // `parse_cycling_cost` delegates to `parse_oracle_cost` so comma-separated parts
    // compose into `AbilityCost::Composite`; synthesis then appends the mandatory
    // "discard this card" sub-cost. Placed before typecycling so the empty-subtype
    // guard never has to consider em-dash forms.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("cycling\u{2014}").parse(text) {
        if let Some(cyc_cost) = parse_cycling_cost(rest) {
            return Some(Keyword::Cycling(cyc_cost));
        }
    }

    // CR 702.29: Typecycling — "{subtype}cycling {cost}" e.g. "plainscycling {2}"
    // Guard: subtype prefix must be a single word (no spaces) to avoid false positives.
    if let Ok((_, (subtype, after_cycling))) = split_once_on(text, "cycling") {
        if !subtype.is_empty() && !subtype.contains(' ') {
            let cost_str = after_cycling.trim();
            if !cost_str.is_empty() {
                let colon_form = format!("typecycling:{subtype}:{cost_str}");
                let parsed: Keyword = colon_form.parse().unwrap();
                if !matches!(parsed, Keyword::Unknown(_)) {
                    return Some(parsed);
                }
            }
        }
    }

    // CR 702.21a: Ward with non-mana costs uses em-dash separator (U+2014).
    // "ward—pay N life", "ward—discard a card", "ward—sacrifice a permanent"
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("ward\u{2014}").parse(text) {
        return parse_ward_cost(rest);
    }

    // CR 702.34a: Flashback with em-dash cost — covers single non-mana costs
    // ("flashback—tap N untapped white creatures you control"), single mana costs
    // ("flashback—{2}{R}"), and compound costs ("flashback—{1}{U}, Pay 3 life").
    // `parse_flashback_cost` delegates to `parse_oracle_cost`, which composes
    // comma-separated parts into `AbilityCost::Composite` so the runtime split
    // (`split_flashback_cost` in casting.rs) can route mana sub-costs through the
    // mana-payment flow and residual sub-costs through `pay_additional_cost`.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("flashback\u{2014}").parse(text) {
        if let Some(fb_cost) = parse_flashback_cost(rest) {
            return Some(Keyword::Flashback(fb_cost));
        }
    }

    // CR 702.103a + CR 118.9: Bestow with em-dash cost — covers compound costs
    // such as Detective's Phoenix "Bestow—{R}, Collect evidence 6." Pure-mana
    // bestow ("Bestow {3}{G}{G}") arrives via MTGJSON's keywords array (FromStr
    // path). `parse_bestow_cost` delegates to `parse_oracle_cost`, which composes
    // comma-separated parts into `AbilityCost::Composite` so the runtime split
    // (`split_bestow_cost_components` in casting.rs) can route the mana sub-cost
    // through the mana-payment flow and the residual (Collect evidence) through
    // `pay_additional_cost`.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("bestow\u{2014}").parse(text) {
        if let Some(bestow_cost) = parse_bestow_cost(rest) {
            return Some(Keyword::Bestow(bestow_cost));
        }
    }

    // CR 702.27a: Buyback with em-dash cost — non-mana costs like
    // "buyback—sacrifice a land" (Constant Mists). Pure-mana buyback
    // ("Buyback {3}") is handled by the direct `FromStr` path above.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("buyback\u{2014}").parse(text) {
        if let Some(bb_cost) = parse_buyback_cost(rest) {
            return Some(Keyword::Buyback(bb_cost));
        }
    }

    // CR 702.74a + CR 601.2f-h: Evoke with em-dash cost — covers non-mana
    // alternative costs ("evoke—exile a white card from your hand" on the MH2
    // Incarnations: Solitude, Endurance, Grief, Subtlety, Fury) and the
    // forward-compatible compound shape (mana + non-mana). Pure-mana evoke
    // ("Evoke {3}{U}", original Lorwyn cycle) arrives via MTGJSON's keywords
    // array and is handled by the `FromStr` path above.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("evoke\u{2014}").parse(text) {
        if let Some(ev_cost) = parse_evoke_cost(rest) {
            return Some(Keyword::Evoke(ev_cost));
        }
    }

    // CR 702.30a: Echo with em-dash cost — non-mana echo ("echo—discard a card"
    // on Rakdos Headliner / Deepcavern Imp). Pure-mana echo ("Echo {R}") arrives
    // via the space-mana fallback below. Placed before that fallback because the
    // generic space-split mangles "echo—discard a card".
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("echo\u{2014}").parse(text) {
        if let Some(echo_cost) = parse_echo_cost(rest) {
            return Some(Keyword::Echo(echo_cost));
        }
    }

    // CR 702.128a + CR 602.1a: Embalm with em-dash cost — composite mana +
    // non-mana ("embalm—{2}{W}{W}, discard a card"). Pure-mana embalm
    // ("Embalm {3}{W}") arrives via MTGJSON's keywords array (FromStr path).
    // `parse_embalm_cost` delegates to `parse_oracle_cost` so comma-separated
    // parts compose into `AbilityCost::Composite`; synthesis then appends the
    // mandatory self-exile sub-cost.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("embalm\u{2014}").parse(text) {
        if let Some(embalm_cost) = parse_embalm_cost(rest) {
            return Some(Keyword::Embalm(embalm_cost));
        }
    }

    // CR 702.129a + CR 602.1a: Eternalize with em-dash cost — composite mana +
    // non-mana ("eternalize—{3}{U}{U}, discard a card", Champion of Wits family).
    // Pure-mana eternalize arrives via the FromStr path above.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("eternalize\u{2014}").parse(text) {
        if let Some(eternalize_cost) = parse_eternalize_cost(rest) {
            return Some(Keyword::Eternalize(eternalize_cost));
        }
    }

    // CR 702.120a: Escalate with em-dash cost — covers non-mana costs such as
    // Collective Effort's "Escalate—Tap an untapped creature you control."
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("escalate\u{2014}").parse(text) {
        let cost = normalize_escalate_cost(parse_oracle_cost(rest));
        if !matches!(cost, AbilityCost::Unimplemented { .. }) {
            return Some(Keyword::Escalate(cost));
        }
    }

    // CR 702.138a: Escape with em-dash cost — composite mana + exile-from-graveyard
    // ("escape—{2}{U}{R}, exile four other cards from your graveyard"). Mirrors the
    // evoke/embalm/eternalize/escalate em-dash siblings above: detection is a
    // structural split on the em-dash inside `parse_escape_keyword`, which delegates
    // the comma-separated cost list wholesale to `parse_oracle_cost` (nom
    // combinators), composing the clauses into `AbilityCost::Composite`. Escape
    // appears on instants/sorceries (Run for Your Life, Cling to Dust) as well as
    // permanents (Uro, Kroxa); registering it here lets BOTH `is_keyword_cost_line`
    // guards in `dispatch_line_nom` (the `is_spell` guard and the general
    // keyword-cost guard) extract it uniformly with its alt-cost siblings, instead
    // of relying on a position-sensitive dedicated intercept. The `tag` prefix gate
    // is required because `parse_escape_keyword` splits on *any* em-dash; without it
    // an unrelated em-dash line could misfire.
    if tag::<_, _, OracleError<'_>>("escape\u{2014}")
        .parse(text)
        .is_ok()
    {
        if let Some(kw) = super::oracle_special::parse_escape_keyword(text) {
            return Some(kw);
        }
    }

    // CR 702.74a: "hideaway N" — parameterized keyword.
    // Delegates to nom combinator for number parsing.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("hideaway ").parse(text) {
        if let Ok((rem, n)) = nom_primitives::parse_number.parse(rest.trim()) {
            if rem.is_empty() {
                return Some(Keyword::Hideaway(n));
            }
        }
    }

    // Digital-only Specialize: "specialize {cost}" alternative activation cost.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("specialize ").parse(text) {
        let cost_str = rest.trim();
        if !cost_str.is_empty() {
            let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_str);
            return Some(Keyword::Specialize(cost));
        }
    }

    // CR 702.87a: "level up {cost}" — two-word keyword name.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("level up ").parse(text) {
        let cost_str = rest.trim();
        if !cost_str.is_empty() {
            let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_str);
            return Some(Keyword::LevelUp(cost));
        }
    }

    // CR 702.162a: "more than meets the eye {cost}" — alternative cost to cast the
    // card converted (back face up). The Oracle line supplies the alternative cost.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("more than meets the eye ").parse(text) {
        let cost_str = rest.trim();
        if !cost_str.is_empty() {
            let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_str);
            return Some(Keyword::MoreThanMeetsTheEye(cost));
        }
    }

    // CR 701.57a: "discover N"
    // Delegates to nom combinator for number parsing.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("discover ").parse(text) {
        if let Ok((rem, n)) = nom_primitives::parse_number.parse(rest.trim()) {
            if rem.is_empty() {
                return Some(Keyword::Discover(n));
            }
        }
    }

    // Gift keyword: "gift a card", "gift a treasure", "gift a food", "gift a tapped fish"
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("gift a ").parse(text) {
        use crate::types::keywords::GiftKind;
        let kind = match rest.trim() {
            "card" => GiftKind::Card,
            "treasure" => GiftKind::Treasure,
            "food" => GiftKind::Food,
            "tapped fish" => GiftKind::TappedFish,
            _ => return None,
        };
        return Some(Keyword::Gift(kind));
    }

    // CR 702.49d: Commander ninjutsu — multi-word keyword name (like "level up").
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("commander ninjutsu ").parse(text) {
        let cost_str = rest.trim();
        if !cost_str.is_empty() {
            let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_str);
            return Some(Keyword::CommanderNinjutsu(cost));
        }
    }

    // CR 702.62a: Suspend N—{cost} — "suspend N—{cost}" with em-dash or ascii dash.
    // Format: "suspend 4—{u}" or "suspend 1—{r}".
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("suspend ").parse(text) {
        // Parse the count (digits before the em-dash)
        if let Ok((after_count, count)) = nom_primitives::parse_number.parse(rest.trim()) {
            // Strip em-dash (U+2014) or ASCII dash separators
            let cost_str = after_count
                .strip_prefix('\u{2014}')
                .or_else(|| after_count.strip_prefix("—"))
                .or_else(|| after_count.strip_prefix("--"))
                .unwrap_or(after_count)
                .trim();
            if !cost_str.is_empty() {
                let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_str);
                return Some(Keyword::Suspend { count, cost });
            }
        }
    }

    // CR 702.113a: Awaken N—{cost} — same N—{cost} format as Suspend.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("awaken ").parse(text) {
        if let Ok((after_count, count)) = nom_primitives::parse_number.parse(rest.trim()) {
            let cost_str = after_count
                .strip_prefix('\u{2014}') // allow-noncombinator: em-dash punctuation separator
                .or_else(|| after_count.strip_prefix("—")) // allow-noncombinator: em-dash variant
                .or_else(|| after_count.strip_prefix("--")) // allow-noncombinator: ascii dash fallback
                .unwrap_or(after_count)
                .trim();
            if !cost_str.is_empty() {
                let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_str);
                return Some(Keyword::Awaken { count, cost });
            }
        }
    }

    // CR 702.77a: Reinforce N—{cost} — "[Cost], Discard this card: Put N +1/+1 counters
    // on target creature." Same N—{cost} format as Suspend/Awaken.
    // Uses parse_number_or_x to handle "Reinforce X—{cost}" (e.g. Swell of Courage).
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("reinforce ").parse(text) {
        if let Ok((after_count, count)) = nom_primitives::parse_number_or_x.parse(rest.trim()) {
            let cost_str = after_count
                .strip_prefix('\u{2014}') // allow-noncombinator: em-dash punctuation separator
                .or_else(|| after_count.strip_prefix("\u{2014}")) // allow-noncombinator: em-dash variant
                .or_else(|| after_count.strip_prefix("--")) // allow-noncombinator: ascii dash fallback
                .unwrap_or(after_count)
                .trim();
            if !cost_str.is_empty() {
                let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_str);
                return Some(Keyword::Reinforce { count, cost });
            }
        }
    }

    // CR 702.160a + CR 718.3b: Prototype {cost} — {P}/{T}. The Oracle line carries
    // the secondary (prototype) power/toughness that the bare MTGJSON keyword lacks;
    // the generic name/param split below would drop the "— P/T" segment. CR 718.3b:
    // the prototyped spell/permanent uses ONLY this alternative P/T — never the
    // top-level (full-cast) P/T — so it must come from this Oracle segment.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("prototype ").parse(text) {
        if let Ok((_, (cost_str, pt_str))) =
            split_once_on(rest, "\u{2014}").or_else(|_| split_once_on(rest, "--"))
        {
            let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_str.trim());
            if let Ok((after_power, power)) = nom_primitives::parse_number.parse(pt_str.trim()) {
                if let Ok((tough_str, _)) = tag::<_, _, OracleError<'_>>("/").parse(after_power) {
                    if let Ok((_, toughness)) = nom_primitives::parse_number.parse(tough_str) {
                        return Some(Keyword::Prototype {
                            cost,
                            power: Some(power as i32),
                            toughness: Some(toughness as i32),
                        });
                    }
                }
            }
        }
    }

    // CR 702.60a: Ripple N — when you cast this spell, you may reveal the top N cards
    // of your library and cast any with the same name without paying their mana cost.
    // Cards: Surging Aether, Surging Dementia, Surging Might, Surging Sentinels;
    // Thrumming Stone grants Ripple 4.
    if let Ok((_, n)) = all_consuming(preceded(
        tag::<_, _, OracleError<'_>>("ripple "),
        nom_primitives::parse_number,
    ))
    .parse(text)
    {
        return Some(Keyword::Ripple(n));
    }

    // CR 702.89a/b: "umbra armor" — and the obsolete "totem armor" the Oracle text
    // of older cards was updated from — is a single two-word keyword. The generic
    // name/parameter split below would read "umbra"/"totem" as the name and drop
    // "armor", so recognize the whole phrase here (mirrors the `ripple N` check).
    if all_consuming(alt((
        tag::<_, _, OracleError<'_>>("umbra armor"),
        tag("totem armor"),
    )))
    .parse(text)
    .is_ok()
    {
        return Some(Keyword::TotemArmor);
    }

    if let Ok((quality, _)) = tag::<_, _, OracleError<'_>>("bands with other ").parse(text) {
        let normalized = normalize_bands_with_other_quality(quality);
        if !normalized.is_empty() {
            return Some(Keyword::BandsWithOther(normalized));
        }
    }

    // For parameterized keywords, find the first space to split name from parameter.
    // Oracle format: "protection from multicolored" → name="protection", rest="from multicolored"
    // Oracle format: "ward {2}" → name="ward", rest="{2}"
    let (_, (name, rest)) = split_once_on(text, " ").ok()?;
    let rest = rest.trim();

    // CR 702.32a: Fading N.
    // CR 702.63a: Vanishing N.
    // CR 702.112a: Renown N.
    // CR 702.68a: Frenzy N.
    // Bare-integer count keywords take ONLY a leading integer. The generic
    // remainder path below would slurp a trailing clause (e.g. "vanishing 3 if
    // that creature doesn't have vanishing") into the FromStr param, where
    // `p.parse::<u32>()` fails and silently falls back to the default
    // (Vanishing(0)). Take only the leading numeric token and discard the
    // trailing text so "<kw> N <anything>" yields Keyword(N). This mirrors the
    // Renown/Frenzy/Ripple `parse_number` arms above, except it keeps (rather
    // than rejects) the count when trailing text follows.
    // CR 702.63b: Vanishing without a number has no count, so `parse_number`
    // fails and we fall through to the generic path, preserving today's
    // Vanishing(0) routing.
    let param: Cow<'_, str> = if is_numeric_count_keyword(name) {
        match nom_primitives::parse_number.parse(rest) {
            Ok((remainder, _)) => Cow::Borrowed(&rest[..rest.len() - remainder.len()]),
            Err(_) => Cow::Borrowed(rest),
        }
    } else {
        // Strip "from" preposition (used by protection keywords).
        tag::<_, _, OracleError<'_>>("from ")
            .parse(rest)
            .map_or(Cow::Borrowed(rest), |(rem, _)| Cow::Borrowed(rem))
    };

    let colon_form = format!("{name}:{param}");
    let parsed: Keyword = colon_form.parse().unwrap();
    if matches!(parsed, Keyword::Unknown(_)) {
        return None;
    }
    Some(parsed)
}

/// Bare-integer-count keywords whose `FromStr` arm does `p.parse().unwrap_or(N)`
/// (or wraps the integer in `QuantityExpr::Fixed`) over the parameter string —
/// see the arms in `types/keywords.rs`. For these the generic normalizer must
/// take ONLY the leading integer and drop any trailing clause, or the count is
/// silently lost to the fallback.
///
/// CR 702.32a: Fading N.
/// CR 702.63a: Vanishing N.
/// CR 702.112a: Renown N.
/// CR 702.68a: Frenzy N.
/// CR 702.122a: Crew N.
fn is_numeric_count_keyword(name: &str) -> bool {
    // One `tag` per keyword name, grouped into ≤21-element `alt` blocks for
    // nom's tuple limit. `all_consuming` requires an exact whole-name match so
    // non-numeric keywords like "protection"/"landwalk" cannot leak into the
    // numeric branch.
    all_consuming(alt((
        alt((
            tag::<_, _, OracleError<'_>>("rampage"),
            tag("bushido"),
            tag("frenzy"),
            tag("absorb"),
            tag("fading"),
            tag("vanishing"),
            tag("dredge"),
            tag("modular"),
            tag("renown"),
            tag("fabricate"),
            tag("annihilator"),
            tag("tribute"),
            tag("afterlife"),
        )),
        alt((
            tag("casualty"),
            tag("mobilize"),
            tag("poisonous"),
            tag("amplify"),
            tag("graft"),
            tag("devour"),
            tag("toxic"),
            tag("saddle"),
            // Teamwork N — leading integer is the total-power threshold (mirrors
            // Crew/Saddle). The "(As an additional cost ...)" reminder text is
            // stripped before keyword parsing.
            tag("teamwork"),
            tag("soulshift"),
            tag("backup"),
            tag("firebending"),
            tag("hideaway"),
            tag("afflict"),
            // CR 702.122a: "Crew N" — leading integer is the total power
            // threshold; trailing clauses (e.g. once-per-turn riders) must be
            // dropped by the generic normalizer like every other count keyword.
            tag("crew"),
        )),
    )))
    .parse(name)
    .is_ok()
}

fn normalize_escalate_cost(cost: AbilityCost) -> AbilityCost {
    match cost {
        AbilityCost::EffectCost { effect } => match *effect {
            // CR 701.26a: a single-target tap effect-cost becomes a typed
            // tap-creatures cost. Untap / mass scopes keep the effect-cost form.
            Effect::SetTapState {
                target,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            } => AbilityCost::TapCreatures {
                requirement: crate::types::ability::TapCreaturesRequirement::count(1),
                filter: target,
            },
            effect => AbilityCost::EffectCost {
                effect: Box::new(effect),
            },
        },
        other => other,
    }
}

/// Get a lowercase display name for a keyword variant.
pub fn keyword_display_name(keyword: &Keyword) -> String {
    match keyword {
        Keyword::Flying => "flying".to_string(),
        Keyword::FirstStrike => "first strike".to_string(),
        Keyword::DoubleStrike => "double strike".to_string(),
        Keyword::Trample => "trample".to_string(),
        Keyword::TrampleOverPlaneswalkers => "trample over planeswalkers".to_string(),
        Keyword::Deathtouch => "deathtouch".to_string(),
        Keyword::Lifelink => "lifelink".to_string(),
        Keyword::Vigilance => "vigilance".to_string(),
        Keyword::Haste => "haste".to_string(),
        Keyword::Reach => "reach".to_string(),
        Keyword::Defender => "defender".to_string(),
        Keyword::Menace => "menace".to_string(),
        Keyword::Indestructible => "indestructible".to_string(),
        Keyword::Hexproof => "hexproof".to_string(),
        Keyword::HexproofFrom(_) => "hexproof from".to_string(),
        Keyword::Shroud => "shroud".to_string(),
        Keyword::Flash => "flash".to_string(),
        Keyword::Fear => "fear".to_string(),
        Keyword::Intimidate => "intimidate".to_string(),
        Keyword::Skulk => "skulk".to_string(),
        Keyword::Shadow => "shadow".to_string(),
        Keyword::Horsemanship => "horsemanship".to_string(),
        Keyword::Wither => "wither".to_string(),
        Keyword::Infect => "infect".to_string(),
        Keyword::Afflict(n) => format!("afflict {n}"),
        Keyword::StartingIntensity(n) => format!("starting intensity {n}"),
        Keyword::Prowess => "prowess".to_string(),
        Keyword::Undying => "undying".to_string(),
        Keyword::Persist => "persist".to_string(),
        Keyword::Cascade => "cascade".to_string(),
        Keyword::Convoke => "convoke".to_string(),
        Keyword::Waterbend => "waterbend".to_string(),
        Keyword::Delve => "delve".to_string(),
        Keyword::Devoid => "devoid".to_string(),
        Keyword::Exalted => "exalted".to_string(),
        Keyword::Flanking => "flanking".to_string(),
        Keyword::Changeling => "changeling".to_string(),
        Keyword::Phasing => "phasing".to_string(),
        Keyword::Battlecry => "battlecry".to_string(),
        Keyword::Decayed => "decayed".to_string(),
        Keyword::Unleash => "unleash".to_string(),
        Keyword::Riot => "riot".to_string(),
        Keyword::LivingWeapon => "living weapon".to_string(),
        Keyword::JobSelect => "job select".to_string(),
        Keyword::TotemArmor => "totem armor".to_string(),
        Keyword::Evolve => "evolve".to_string(),
        Keyword::Extort => "extort".to_string(),
        Keyword::Exploit => "exploit".to_string(),
        Keyword::Explore => "explore".to_string(),
        Keyword::Ascend => "ascend".to_string(),
        Keyword::StartYourEngines => "start your engines!".to_string(),
        Keyword::Soulbond => "soulbond".to_string(),
        Keyword::Banding => "banding".to_string(),
        Keyword::BandsWithOther(quality) => format!("bands with other {}", quality.to_lowercase()),
        // CR 702.24a: Cumulative upkeep's display includes its base cost so
        // tooltips and AI hint text show the actual payment ("cumulative upkeep
        // — {1}", "cumulative upkeep — Pay 2 life", etc.) instead of a bare
        // keyword name. No generic typed-cost formatter exists today; the
        // local helper handles only the four shapes the cumulative-upkeep
        // parser emits (Mana, PayLife, Sacrifice, OneOf).
        Keyword::CumulativeUpkeep(ref cost) => {
            format!(
                "cumulative upkeep — {}",
                format_cumulative_upkeep_cost(cost)
            )
        }
        Keyword::Epic => "epic".to_string(),
        Keyword::Fuse => "fuse".to_string(),
        Keyword::Gravestorm => "gravestorm".to_string(),
        Keyword::Haunt => "haunt".to_string(),
        Keyword::Improvise => "improvise".to_string(),
        Keyword::Ingest => "ingest".to_string(),
        Keyword::Melee => "melee".to_string(),
        Keyword::Mentor => "mentor".to_string(),
        Keyword::Myriad => "myriad".to_string(),
        Keyword::Provoke => "provoke".to_string(),
        Keyword::Rebound => "rebound".to_string(),
        Keyword::Retrace => "retrace".to_string(),
        Keyword::Ripple(_) => "ripple".to_string(),
        Keyword::SplitSecond => "split second".to_string(),
        Keyword::Storm => "storm".to_string(),
        Keyword::Suspend { .. } => "suspend".to_string(),
        Keyword::Totem => "totem".to_string(),
        Keyword::Warp(_) => "warp".to_string(),
        Keyword::Sneak(_) => "sneak".to_string(),
        Keyword::WebSlinging(_) => "web-slinging".to_string(),
        Keyword::Mobilize(_) => "mobilize".to_string(),
        Keyword::Gift(_) => "gift".to_string(),
        Keyword::Discover(n) => format!("discover {n}"),
        Keyword::Spree => "spree".to_string(),
        Keyword::Ravenous => "ravenous".to_string(),
        Keyword::Daybound => "daybound".to_string(),
        Keyword::Nightbound => "nightbound".to_string(),
        Keyword::Enlist => "enlist".to_string(),
        Keyword::ReadAhead => "read ahead".to_string(),
        Keyword::Compleated => "compleated".to_string(),
        Keyword::Conspire => "conspire".to_string(),
        Keyword::Demonstrate => "demonstrate".to_string(),
        Keyword::Dethrone => "dethrone".to_string(),
        Keyword::DoubleTeam => "double team".to_string(),
        Keyword::LivingMetal => "living metal".to_string(),
        Keyword::Firebending(_) => "firebending".to_string(),
        // Parameterized keywords — return just the base name
        Keyword::Dredge(_) => "dredge".to_string(),
        Keyword::Modular(_) => "modular".to_string(),
        Keyword::Renown(_) => "renown".to_string(),
        Keyword::Fabricate(_) => "fabricate".to_string(),
        Keyword::Annihilator(_) => "annihilator".to_string(),
        Keyword::Bushido(_) => "bushido".to_string(),
        Keyword::Frenzy(_) => "frenzy".to_string(),
        Keyword::Tribute(_) => "tribute".to_string(),
        Keyword::Afterlife(_) => "afterlife".to_string(),
        Keyword::Fading(_) => "fading".to_string(),
        Keyword::Vanishing(_) => "vanishing".to_string(),
        Keyword::Rampage(_) => "rampage".to_string(),
        Keyword::Absorb(_) => "absorb".to_string(),
        Keyword::Crew { .. } => "crew".to_string(),
        Keyword::Poisonous(_) => "poisonous".to_string(),
        Keyword::Bloodthirst(_) => "bloodthirst".to_string(),
        Keyword::Amplify(_) => "amplify".to_string(),
        Keyword::Graft(_) => "graft".to_string(),
        Keyword::Devour(_) => "devour".to_string(),
        Keyword::Toxic(_) => "toxic".to_string(),
        Keyword::Saddle(_) => "saddle".to_string(),
        Keyword::Teamwork(_) => "teamwork".to_string(),
        Keyword::Soulshift(_) => "soulshift".to_string(),
        Keyword::Backup(_) => "backup".to_string(),
        Keyword::Squad(_) => "squad".to_string(),
        Keyword::Typecycling { ref subtype, .. } => {
            format!("{}cycling", subtype.to_lowercase())
        }
        Keyword::Protection(_) => "protection".to_string(),
        Keyword::Kicker(_) => "kicker".to_string(),
        Keyword::Cycling(_) => "cycling".to_string(),
        Keyword::Flashback(_) => "flashback".to_string(),
        Keyword::Ward(_) => "ward".to_string(),
        Keyword::Equip(_) => "equip".to_string(),
        Keyword::Landwalk(_) => "landwalk".to_string(),
        Keyword::Partner(ref pt) => {
            use crate::types::keywords::PartnerType;
            match pt {
                PartnerType::Generic => "partner".to_string(),
                PartnerType::With(name) => format!("partner with {name}"),
                PartnerType::FriendsForever => "friends forever".to_string(),
                PartnerType::CharacterSelect => "character select".to_string(),
                PartnerType::DoctorsCompanion => "doctor's companion".to_string(),
                PartnerType::ChooseABackground => "choose a background".to_string(),
            }
        }
        Keyword::Companion(_) => "companion".to_string(),
        Keyword::Ninjutsu(_) => "ninjutsu".to_string(),
        Keyword::CommanderNinjutsu(_) => "commander ninjutsu".to_string(),
        Keyword::Enchant(_) => "enchant".to_string(),
        Keyword::EtbCounter { .. } => "etb counter".to_string(),
        Keyword::Reconfigure(_) => "reconfigure".to_string(),
        Keyword::Bestow(_) => "bestow".to_string(),
        Keyword::Embalm(_) => "embalm".to_string(),
        Keyword::Eternalize(_) => "eternalize".to_string(),
        Keyword::Unearth(_) => "unearth".to_string(),
        Keyword::Prowl(_) => "prowl".to_string(),
        Keyword::Morph(_) => "morph".to_string(),
        Keyword::Megamorph(_) => "megamorph".to_string(),
        Keyword::Madness(_) => "madness".to_string(),
        Keyword::Miracle(_) => "miracle".to_string(),
        Keyword::Dash(_) => "dash".to_string(),
        Keyword::Emerge(_) => "emerge".to_string(),
        Keyword::Escape(_) => "escape".to_string(),
        Keyword::Harmonize(_) => "harmonize".to_string(),
        Keyword::Mayhem(_) => "mayhem".to_string(),
        Keyword::Evoke(_) => "evoke".to_string(),
        Keyword::Foretell(_) => "foretell".to_string(),
        Keyword::Mutate(_) => "mutate".to_string(),
        Keyword::Disturb(_) => "disturb".to_string(),
        Keyword::Disguise(_) => "disguise".to_string(),
        Keyword::Blitz(_) => "blitz".to_string(),
        Keyword::Overload(_) => "overload".to_string(),
        Keyword::Spectacle(_) => "spectacle".to_string(),
        Keyword::Surge(_) => "surge".to_string(),
        Keyword::Encore(_) => "encore".to_string(),
        Keyword::Buyback(_) => "buyback".to_string(),
        Keyword::Echo(_) => "echo".to_string(),
        Keyword::Outlast(_) => "outlast".to_string(),
        Keyword::Scavenge(_) => "scavenge".to_string(),
        Keyword::Fortify(_) => "fortify".to_string(),
        Keyword::Prototype { .. } => "prototype".to_string(),
        Keyword::Plot(_) => "plot".to_string(),
        Keyword::Craft { .. } => "craft".to_string(),
        Keyword::Offspring(_) => "offspring".to_string(),
        Keyword::Impending { counters, .. } => format!("impending {counters}"),
        Keyword::LevelUp(_) => "level up".to_string(),
        Keyword::Hideaway(_) => "hideaway".to_string(),
        Keyword::Casualty(n) => format!("casualty {n}"),
        Keyword::Entwine(_) => "entwine".to_string(),
        Keyword::Affinity(_) => "affinity".to_string(),
        Keyword::Splice { .. } => "splice".to_string(),
        Keyword::Bargain => "bargain".to_string(),
        Keyword::Sunburst => "sunburst".to_string(),
        Keyword::Champion(_) => "champion".to_string(),
        Keyword::Training => "training".to_string(),
        Keyword::Assist => "assist".to_string(),
        Keyword::Augment => "augment".to_string(),
        Keyword::Aftermath => "aftermath".to_string(),
        Keyword::JumpStart => "jump-start".to_string(),
        Keyword::Cipher => "cipher".to_string(),
        Keyword::Transmute(_) => "transmute".to_string(),
        Keyword::Transfigure(_) => "transfigure".to_string(),
        Keyword::Cleave(_) => "cleave".to_string(),
        Keyword::Undaunted => "undaunted".to_string(),
        Keyword::Station => "station".to_string(),
        Keyword::Paradigm => "paradigm".to_string(),
        Keyword::Replicate(_) => "replicate".to_string(),
        Keyword::Awaken { .. } => "awaken".to_string(),
        Keyword::Escalate(_) => "escalate".to_string(),
        Keyword::Recover(_) => "recover".to_string(),
        Keyword::ForMirrodin => "for mirrodin!".to_string(),
        Keyword::MoreThanMeetsTheEye(_) => "more than meets the eye".to_string(),
        Keyword::Freerunning(_) => "freerunning".to_string(),
        Keyword::Increment => "increment".to_string(),
        Keyword::Specialize(_) => "specialize".to_string(),
        Keyword::Offering(quality) => format!("{} offering", quality.to_lowercase()),
        Keyword::Reinforce { count, .. } => {
            if *count == 0 {
                "reinforce x".to_string()
            } else {
                format!("reinforce {count}")
            }
        }
        Keyword::Unknown(s) => s.to_lowercase(),
    }
}
/// CR 702.24a: Render a cumulative-upkeep base cost as the display fragment
/// used after `cumulative upkeep — `. Only the four cost shapes the
/// cumulative-upkeep parser actually emits are handled (`Mana`, `PayLife`,
/// `Sacrifice`, `OneOf`); any other variant falls through to a debug
/// representation so a future cost shape never silently swallows the cost
/// text in tooltips.
fn format_cumulative_upkeep_cost(cost: &AbilityCost) -> String {
    match cost {
        AbilityCost::Mana { cost } => format_mana_cost_symbols(cost),
        AbilityCost::PayLife { amount } => match amount {
            QuantityExpr::Fixed { value } => format!("Pay {value} life"),
            other => format!("Pay {other:?} life"),
        },
        AbilityCost::Sacrifice(cost) => {
            let subject = format_sacrifice_subject(&cost.target);
            match &cost.requirement {
                SacrificeRequirement::Count { count } => {
                    if *count == 1 {
                        format!("Sacrifice a {subject}")
                    } else {
                        format!("Sacrifice {count} {subject}s")
                    }
                }
                SacrificeRequirement::Aggregate { value, .. } => {
                    format!("Sacrifice {subject} with total power {value} or greater")
                }
            }
        }
        AbilityCost::OneOf { costs } => costs
            .iter()
            .map(format_cumulative_upkeep_cost)
            .collect::<Vec<_>>()
            .join(" or "),
        other => format!("{other:?}"),
    }
}

/// Render a `ManaCost` as MTG-style brace symbols (e.g. `{2}{U}{U}`).
/// `NoCost` collapses to `{0}`; `SelfManaCost` / `SelfManaValue` render the Oracle
/// phrase players see on cards like Snapcaster Mage's flashback or Sliver
/// Gravemother's encore grant.
fn format_mana_cost_symbols(cost: &ManaCost) -> String {
    match cost {
        ManaCost::NoCost => "{0}".to_string(),
        ManaCost::SelfManaCost => "its mana cost".to_string(),
        ManaCost::SelfManaValue => "its mana value".to_string(),
        ManaCost::SelfManaCostReduced { reduction } => {
            format!("its mana cost reduced by {{{reduction}}}")
        }
        ManaCost::Cost { shards, generic } => {
            let mut out = String::new();
            if *generic > 0 {
                out.push_str(&format!("{{{generic}}}"));
            }
            for shard in shards {
                out.push('{');
                out.push_str(mana_shard_symbol(*shard));
                out.push('}');
            }
            if out.is_empty() {
                "{0}".to_string()
            } else {
                out
            }
        }
    }
}

/// Render a single mana shard as its MTG abbreviation (inverse of
/// `ManaCostShard::FromStr`). Used by cumulative-upkeep display formatting;
/// kept local to this module because no other caller needs it today.
fn mana_shard_symbol(shard: ManaCostShard) -> &'static str {
    match shard {
        ManaCostShard::White => "W",
        ManaCostShard::Blue => "U",
        ManaCostShard::Black => "B",
        ManaCostShard::Red => "R",
        ManaCostShard::Green => "G",
        ManaCostShard::Colorless => "C",
        ManaCostShard::Snow => "S",
        ManaCostShard::X => "X",
        ManaCostShard::TwoOrMoreColorSource => "Z",
        ManaCostShard::WhiteBlue => "W/U",
        ManaCostShard::WhiteBlack => "W/B",
        ManaCostShard::BlueBlack => "U/B",
        ManaCostShard::BlueRed => "U/R",
        ManaCostShard::BlackRed => "B/R",
        ManaCostShard::BlackGreen => "B/G",
        ManaCostShard::RedWhite => "R/W",
        ManaCostShard::RedGreen => "R/G",
        ManaCostShard::GreenWhite => "G/W",
        ManaCostShard::GreenBlue => "G/U",
        ManaCostShard::TwoWhite => "2/W",
        ManaCostShard::TwoBlue => "2/U",
        ManaCostShard::TwoBlack => "2/B",
        ManaCostShard::TwoRed => "2/R",
        ManaCostShard::TwoGreen => "2/G",
        ManaCostShard::PhyrexianWhite => "W/P",
        ManaCostShard::PhyrexianBlue => "U/P",
        ManaCostShard::PhyrexianBlack => "B/P",
        ManaCostShard::PhyrexianRed => "R/P",
        ManaCostShard::PhyrexianGreen => "G/P",
        ManaCostShard::PhyrexianWhiteBlue => "W/U/P",
        ManaCostShard::PhyrexianWhiteBlack => "W/B/P",
        ManaCostShard::PhyrexianBlueBlack => "U/B/P",
        ManaCostShard::PhyrexianBlueRed => "U/R/P",
        ManaCostShard::PhyrexianBlackRed => "B/R/P",
        ManaCostShard::PhyrexianBlackGreen => "B/G/P",
        ManaCostShard::PhyrexianRedWhite => "R/W/P",
        ManaCostShard::PhyrexianRedGreen => "R/G/P",
        ManaCostShard::PhyrexianGreenWhite => "G/W/P",
        ManaCostShard::PhyrexianGreenBlue => "G/U/P",
        ManaCostShard::ColorlessWhite => "C/W",
        ManaCostShard::ColorlessBlue => "C/U",
        ManaCostShard::ColorlessBlack => "C/B",
        ManaCostShard::ColorlessRed => "C/R",
        ManaCostShard::ColorlessGreen => "C/G",
    }
}

/// Best-effort lowercase noun for the sacrificed permanent (e.g. "land",
/// "creature"). Falls back to "permanent" when the filter is more complex than
/// a single primary type; cumulative-upkeep sacrifice costs in practice are
/// always single-type ("Sacrifice a land", "Sacrifice a creature").
fn format_sacrifice_subject(target: &TargetFilter) -> String {
    if let TargetFilter::Typed(tf) = target {
        if let Some(primary) = tf.get_primary_type() {
            return type_filter_subject_name(primary);
        }
    }
    "permanent".to_string()
}

fn type_filter_subject_name(tf: &TypeFilter) -> String {
    match tf {
        TypeFilter::Creature => "creature".to_string(),
        TypeFilter::Land => "land".to_string(),
        TypeFilter::Artifact => "artifact".to_string(),
        TypeFilter::Enchantment => "enchantment".to_string(),
        TypeFilter::Instant => "instant".to_string(),
        TypeFilter::Sorcery => "sorcery".to_string(),
        TypeFilter::Planeswalker => "planeswalker".to_string(),
        TypeFilter::Battle => "battle".to_string(),
        TypeFilter::Kindred => "kindred".to_string(),
        TypeFilter::Permanent => "permanent".to_string(),
        TypeFilter::Card => "card".to_string(),
        TypeFilter::Any => "permanent".to_string(),
        TypeFilter::Subtype(s) => s.to_ascii_lowercase(),
        TypeFilter::Non(inner) => format!("non-{}", type_filter_subject_name(inner)),
        TypeFilter::AnyOf(_) => "permanent".to_string(),
    }
}

/// Check if a line is a keyword with a cost (e.g., "Cycling {2}", "Flashback {3}{R}", "Crew 3").
/// These are handled by MTGJSON keywords and should be skipped by the Oracle parser.
pub(crate) fn is_keyword_cost_line(lower: &str) -> bool {
    let keyword_costs = [
        "cycling",
        "basic landcycling",
        "flashback",
        "crew",
        "ward",
        "equip", // already handled earlier but as safety
        "bestow",
        "embalm",
        "eternalize",
        "unearth",
        "commander ninjutsu",
        "ninjutsu",
        "prowl",
        "morph",
        "megamorph",
        "madness",
        "dash",
        "emerge",
        "escape",
        "evoke",
        "foretell",
        "mutate",
        "disturb",
        "disguise",
        "blitz",
        "overload",
        "spectacle",
        "freerunning",
        "surge",
        "encore",
        "buyback",
        "echo",
        "outlast",
        "scavenge",
        "fortify",
        "prototype",
        "plot",
        "craft",
        "offspring",
        "impending",
        "reconfigure",
        "suspend",
        "level up",
        "transfigure",
        "transmute",
        "forecast",
        "recover",
        "escalate",
        "awaken",
        "reinforce",
        "retrace",
        "adapt",
        "monstrosity",
        "affinity",
        "convoke",
        "waterbend",
        "delve",
        "improvise",
        "miracle",
        "splice",
        "entwine",
        "toxic",
        "saddle",
        "teamwork",
        "soulshift",
        "backup",
        "squad",
        "warp",
        "sneak",
        "web-slinging",
        "mobilize",
        "hideaway",
        "gift",
        "discover",
        "harmonize",
        "collect evidence",
        "mayhem",
        "more than meets the eye",
        "living weapon",
        "champion",
        "amplify",
        "bloodthirst",
        "tribute",
        "persist",
        "undying",
        "fabricate",
        "modular",
        "partner",
        "spree",
        "casualty",
        "bargain",
        "demonstrate",
        "strive",
        "exploit",
        "devoid",
    ];
    keyword_costs.iter().any(|kw| {
        tag::<_, _, OracleError<'_>>(*kw)
            .parse(lower)
            .is_ok_and(|(rest, _)| {
                rest.is_empty()
                    || rest.as_bytes().first() == Some(&b' ')
                    || rest.as_bytes().first() == Some(&b'\t')
                    || tag::<_, _, OracleError<'_>>("\u{2014}")
                        .parse(rest)
                        .is_ok()
            })
    })
        // CR 702.29: Typecycling — first word ends in "cycling" but isn't "cycling" itself
        || lower
            .split_whitespace()
            .next()
            .is_some_and(|w| w.ends_with("cycling") && w != "cycling")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityCost, SacrificeCost};
    use crate::types::mana::ManaCost;

    #[test]
    fn parse_keyword_from_oracle_cascade() {
        // CR 702.85a: Cascade is a no-parameter keyword.
        let kw = parse_keyword_from_oracle("cascade").unwrap();
        assert_eq!(kw, Keyword::Cascade);
    }

    /// CR 702.24: a GRANTED cumulative upkeep (the quoted-ability grant path
    /// routes through `parse_keyword_from_oracle`) must parse with its cost, like
    /// the top-level keyword-line path — Mana Chains / Dreams of the Dead (mana),
    /// Decomposition (pay-life em-dash form).
    #[test]
    fn parse_keyword_from_oracle_granted_cumulative_upkeep() {
        assert!(matches!(
            parse_keyword_from_oracle("cumulative upkeep {1}"),
            Some(Keyword::CumulativeUpkeep(AbilityCost::Mana { .. }))
        ));
        assert!(matches!(
            parse_keyword_from_oracle("cumulative upkeep {2}"),
            Some(Keyword::CumulativeUpkeep(AbilityCost::Mana { .. }))
        ));
        assert!(matches!(
            parse_keyword_from_oracle("cumulative upkeep\u{2014}pay 1 life"),
            Some(Keyword::CumulativeUpkeep(AbilityCost::PayLife { .. }))
        ));
    }

    /// CR 702.85c: a spell printing cascade as repeated bare words has one instance
    /// per word; each triggers separately. MTGJSON dedupes the keywords array to a
    /// single "Cascade", so the Oracle line is the sole source of printed
    /// multiplicity — extract_keyword_line must recover every occurrence.
    #[test]
    fn extract_keyword_line_recovers_repeated_cascade_instances() {
        let mtgjson_kws = vec!["cascade".to_string()];
        let two = extract_keyword_line("Cascade, cascade", &mtgjson_kws)
            .expect("repeated cascade line is a keyword line");
        assert_eq!(
            two.iter().filter(|k| matches!(k, Keyword::Cascade)).count(),
            2
        );
        let four = extract_keyword_line("Cascade, cascade, cascade, cascade", &mtgjson_kws)
            .expect("repeated cascade line is a keyword line");
        assert_eq!(
            four.iter()
                .filter(|k| matches!(k, Keyword::Cascade))
                .count(),
            4
        );
    }

    /// CR 702.85c regression guard: a single printed cascade (Bloodbraid Elf) must
    /// still net exactly one instance — recovery must not over-count.
    #[test]
    fn extract_keyword_line_single_cascade_yields_one_instance() {
        let mtgjson_kws = vec!["cascade".to_string()];
        let one = extract_keyword_line("Cascade", &mtgjson_kws)
            .expect("single cascade line is a keyword line");
        assert_eq!(
            one.iter().filter(|k| matches!(k, Keyword::Cascade)).count(),
            1
        );
    }

    /// CR 702.116b: a creature printing myriad as repeated bare words has one
    /// instance per word; each triggers separately. MTGJSON dedupes the keywords
    /// array to a single "Myriad", so the Oracle line is the sole source of printed
    /// multiplicity — extract_keyword_line must recover every occurrence. Scurry of
    /// Squirrels ("Myriad, myriad") is the real card this fixes.
    #[test]
    fn extract_keyword_line_recovers_repeated_myriad_instances() {
        let mtgjson_kws = vec!["myriad".to_string()];
        let two = extract_keyword_line("Myriad, myriad", &mtgjson_kws)
            .expect("repeated myriad line is a keyword line");
        assert_eq!(
            two.iter().filter(|k| matches!(k, Keyword::Myriad)).count(),
            2
        );
    }

    /// CR 702.116b regression guard: a single printed myriad must net exactly one
    /// instance — recovery must not over-count.
    #[test]
    fn extract_keyword_line_single_myriad_yields_one_instance() {
        let mtgjson_kws = vec!["myriad".to_string()];
        let one = extract_keyword_line("Myriad", &mtgjson_kws)
            .expect("single myriad line is a keyword line");
        assert_eq!(
            one.iter().filter(|k| matches!(k, Keyword::Myriad)).count(),
            1
        );
    }

    /// BUILDING-BLOCK / forward-looking test (CR 702.83a: Exalted is a triggered
    /// ability; CR 113.2c: multiple instances of an ability function independently).
    /// Exercises the `instances_function_separately()` recovery path for Exalted at
    /// the building-block level. This is NOT a claim that a specific printed card is
    /// fixed: no clean real "Exalted, exalted" keyword-only line exists yet — the one
    /// candidate (Urza's Dark Cannonball) prints "{cost} — Exalted, exalted", which
    /// the `{cost} —` keyword-line parser does not yet strip (see the deferred-gap
    /// pin below). When a clean printed instance lands, this test already covers it.
    #[test]
    fn extract_keyword_line_recovers_repeated_exalted_instances() {
        let mtgjson_kws = vec!["exalted".to_string()];
        let two = extract_keyword_line("Exalted, exalted", &mtgjson_kws)
            .expect("repeated exalted line is a keyword line");
        assert_eq!(
            two.iter().filter(|k| matches!(k, Keyword::Exalted)).count(),
            2
        );
    }

    /// CR 113.2c / CR 702.83a: Urza's Dark Cannonball prints a keyword line behind
    /// an activation-cost prefix. The prefix is not part of the keyword text, so
    /// `extract_keyword_line` must still recover both Exalted instances.
    #[test]
    fn extract_keyword_line_cost_prefixed_exalted_recovers_instances() {
        let mtgjson_kws = vec!["exalted".to_string()];
        let result = extract_keyword_line("{TK}{TK} — Exalted, exalted", &mtgjson_kws)
            .expect("cost-prefixed repeated exalted line is a keyword line");
        assert_eq!(
            result
                .iter()
                .filter(|k| matches!(k, Keyword::Exalted))
                .count(),
            2
        );
    }

    /// CR 702.39b: repeated Provoke instances each trigger separately. MTGJSON
    /// dedupes the keywords array to one "Provoke", so keyword-line extraction
    /// must recover every printed occurrence before synthesis installs triggers.
    #[test]
    fn extract_keyword_line_recovers_repeated_provoke_instances() {
        let mtgjson_kws = vec!["provoke".to_string()];
        let result = extract_keyword_line("Provoke, provoke", &mtgjson_kws)
            .expect("repeated provoke line is a keyword line");
        assert_eq!(
            result
                .iter()
                .filter(|keyword| matches!(keyword, Keyword::Provoke))
                .count(),
            2
        );
    }

    /// CR 702.60a: Ripple N triggers when the spell is cast. N is captured into
    /// the parameterized `Keyword::Ripple(u32)`; trailing text is rejected.
    #[test]
    fn parse_keyword_from_oracle_ripple() {
        assert_eq!(
            parse_keyword_from_oracle("ripple 4"),
            Some(Keyword::Ripple(4)),
            "ripple 4 (Thrumming Stone grant)"
        );
        assert_eq!(
            parse_keyword_from_oracle("ripple 2"),
            Some(Keyword::Ripple(2)),
            "ripple 2 — N is captured"
        );
        assert_eq!(parse_keyword_from_oracle("ripple 4 extra"), None);
    }

    /// CR 702.63a: Vanishing N.
    /// CR 702.32a: Fading N.
    /// CR 702.112a: Renown N.
    ///
    /// The numeric-count normalizer must keep the leading integer even when a
    /// trailing clause follows (e.g. Flesh Duplicate's "vanishing 3 if ..."),
    /// instead of feeding the whole remainder to FromStr and falling back. This
    /// tests the building-block class, not a single card.
    #[test]
    fn parse_keyword_from_oracle_numeric_count_with_trailing_text() {
        // Regression: Flesh Duplicate's conditional except-clause grant.
        assert_eq!(
            parse_keyword_from_oracle("vanishing 3 if that creature doesn't have vanishing"),
            Some(Keyword::Vanishing(3)),
            "trailing 'if ...' clause must not erase the count"
        );
        // No-trailing-text form is unchanged.
        assert_eq!(
            parse_keyword_from_oracle("vanishing 3"),
            Some(Keyword::Vanishing(3))
        );
        // CR 702.63b: a single-word bare keyword has no space, so the normalizer's
        // `split_once_on(text, " ")` fails and the line is not recognized here
        // (bare vanishing reaches the engine via the MTGJSON colon-form path, not
        // this Oracle-grant normalizer). This is unchanged pre-existing behavior;
        // the fix must not start spuriously accepting the space-less form.
        assert_eq!(parse_keyword_from_oracle("vanishing"), None);
        // Fading shares the normalizer with no dedicated arm — proves the class.
        assert_eq!(
            parse_keyword_from_oracle("fading 2 if it's an artifact"),
            Some(Keyword::Fading(2))
        );
        // Renown's dedicated `all_consuming` arm rejects trailing text, so the
        // trailing-text form falls through to the fixed normalizer.
        assert_eq!(
            parse_keyword_from_oracle("renown 2 if it's your turn"),
            Some(Keyword::Renown(2))
        );
        // CR 702.122a: Crew N is also a bare-integer count keyword. A conditional
        // grant must keep the leading total-power threshold and drop the trailing
        // clause, exactly like the rest of the class.
        assert_eq!(
            parse_keyword_from_oracle("crew 2 if it's an artifact"),
            Some(Keyword::Crew {
                power: 2,
                once_per_turn: None,
            })
        );
        // Non-numeric keyword must NOT be hijacked by the numeric branch: the
        // "from " preposition strip still produces a protection target.
        assert!(matches!(
            parse_keyword_from_oracle("protection from red"),
            Some(Keyword::Protection(_))
        ));
    }

    #[test]
    fn parse_keyword_from_oracle_bands_with_other_quality() {
        assert_eq!(
            parse_keyword_from_oracle("bands with other wolves"),
            Some(Keyword::BandsWithOther("Wolf".to_string()))
        );
        assert_eq!(
            parse_keyword_from_oracle("bands with other legends"),
            Some(Keyword::BandsWithOther("Legend".to_string()))
        );
    }

    #[test]
    fn extract_keyword_line_bands_with_other_quality() {
        let result = extract_keyword_line("Bands with other Wolves", &[])
            .expect("bands with other should parse from Oracle keyword line");
        assert_eq!(result, vec![Keyword::BandsWithOther("Wolf".to_string())]);
    }

    /// CR 702.48a: Offering — the Oracle line "<Subtype> offering (...)" carries
    /// the quality that the bare MTGJSON "Offering" keyword name lacks. Previously
    /// no arm matched, so Keyword::Offering was never produced and the cast path
    /// was unreachable. Quality is canonicalized to subtype casing.
    #[test]
    fn parse_keyword_from_oracle_offering() {
        assert_eq!(
            parse_keyword_from_oracle(
                "goblin offering (you may cast this spell any time you could cast an instant \
                 by sacrificing a goblin and paying the difference in mana costs.)"
            ),
            Some(Keyword::Offering("Goblin".to_string())),
            "Patron of the Akki — Goblin offering"
        );
        assert_eq!(
            parse_keyword_from_oracle("artifact offering (...)"),
            Some(Keyword::Offering("Artifact".to_string())),
            "Blast-Furnace Hellkite — Artifact offering"
        );
        // Not a keyword line: a prose sentence merely ending in "offering".
        assert_eq!(parse_keyword_from_oracle("make a generous offering"), None);
    }

    #[test]
    fn extract_keyword_line_ripple_preserves_oracle_depth() {
        let mtgjson_kws = vec!["ripple".to_string()];

        let result = extract_keyword_line("Ripple 4", &mtgjson_kws)
            .expect("Ripple N line should be recognized as a keyword line");

        assert_eq!(
            result,
            vec![Keyword::Ripple(4)],
            "Oracle text carries the ripple depth that MTGJSON's bare keyword omits"
        );
    }

    /// CR 702.85a: Full Oracle text for Bloodbraid Elf and Shardless Agent
    /// must parse to include `Keyword::Cascade`. Locks in cascade keyword
    /// extraction for the canonical reference cards so a future parser
    /// regression cannot silently drop it.
    #[test]
    fn parse_oracle_text_extracts_cascade_for_canonical_cards() {
        use crate::parser::oracle::parse_oracle_text;

        let bloodbraid = parse_oracle_text(
            "Haste\nCascade",
            "Bloodbraid Elf",
            &["Haste".to_string(), "Cascade".to_string()],
            &["Creature".to_string()],
            &["Elf".to_string(), "Berserker".to_string()],
        );
        assert!(
            bloodbraid.extracted_keywords.contains(&Keyword::Cascade),
            "Bloodbraid Elf must have Keyword::Cascade extracted, got {:?}",
            bloodbraid.extracted_keywords
        );

        let shardless = parse_oracle_text(
            "Cascade",
            "Shardless Agent",
            &["Cascade".to_string()],
            &["Artifact".to_string(), "Creature".to_string()],
            &["Human".to_string(), "Wizard".to_string()],
        );
        assert!(
            shardless.extracted_keywords.contains(&Keyword::Cascade),
            "Shardless Agent must have Keyword::Cascade extracted, got {:?}",
            shardless.extracted_keywords
        );
    }

    #[test]
    fn parse_keyword_from_oracle_toxic() {
        // CR 702.164: Toxic N — parameterized keyword from Oracle text
        let kw = parse_keyword_from_oracle("toxic 2").unwrap();
        assert_eq!(kw, Keyword::Toxic(2));
    }

    #[test]
    fn parse_keyword_from_oracle_renown() {
        // CR 702.112a: Renown N — parameterized keyword from Oracle text.
        let kw = parse_keyword_from_oracle("renown 2").unwrap();
        assert_eq!(kw, Keyword::Renown(2));
    }

    #[test]
    fn parse_keyword_from_oracle_frenzy() {
        // CR 702.68a: Frenzy N — parameterized keyword from Oracle/grant text.
        let kw = parse_keyword_from_oracle("frenzy 2").unwrap();
        assert_eq!(kw, Keyword::Frenzy(2));
        // CR 702.68a: the Frenzy Sliver grant line "frenzy 1" must resolve to
        // Frenzy(1), not fall to Unknown/Unimplemented.
        let kw1 = parse_keyword_from_oracle("frenzy 1").unwrap();
        assert_eq!(kw1, Keyword::Frenzy(1));
    }

    #[test]
    fn parse_keyword_from_oracle_saddle() {
        // CR 702.171a: Saddle N
        let kw = parse_keyword_from_oracle("saddle 3").unwrap();
        assert_eq!(kw, Keyword::Saddle(3));
    }

    #[test]
    fn parse_keyword_from_oracle_soulshift() {
        // CR 702.46: Soulshift N
        let kw = parse_keyword_from_oracle("soulshift 7").unwrap();
        assert_eq!(kw, Keyword::Soulshift(7));
    }

    #[test]
    fn parse_keyword_from_oracle_backup() {
        // CR 702.165: Backup N
        let kw = parse_keyword_from_oracle("backup 1").unwrap();
        assert_eq!(kw, Keyword::Backup(1));
    }

    #[test]
    fn parse_keyword_from_oracle_squad() {
        // CR 702.157: Squad {cost}
        let kw = parse_keyword_from_oracle("squad {2}").unwrap();
        assert!(matches!(kw, Keyword::Squad(ManaCost::Cost { .. })));
    }

    #[test]
    fn parse_keyword_from_oracle_more_than_meets_the_eye() {
        use crate::types::mana::ManaCostShard;
        // CR 702.162a: "more than meets the eye {cost}" — the alternative cost
        // is supplied by the Oracle line. Colored cost case (Flamewar: {B}{R}).
        let kw = parse_keyword_from_oracle("more than meets the eye {b}{r}").unwrap();
        match kw {
            Keyword::MoreThanMeetsTheEye(ManaCost::Cost { shards, generic }) => {
                assert_eq!(generic, 0);
                assert!(shards.contains(&ManaCostShard::Black));
                assert!(shards.contains(&ManaCostShard::Red));
                assert_eq!(shards.len(), 2);
            }
            other => panic!("expected MoreThanMeetsTheEye({{B}}{{R}}), got {other:?}"),
        }

        // Class-general: a generic + single color cost parses the same way,
        // proving the arm is not specialized to one card's cost.
        let kw2 = parse_keyword_from_oracle("more than meets the eye {2}{u}").unwrap();
        match kw2 {
            Keyword::MoreThanMeetsTheEye(ManaCost::Cost { shards, generic }) => {
                assert_eq!(generic, 2);
                assert!(shards.contains(&ManaCostShard::Blue));
                assert_eq!(shards.len(), 1);
            }
            other => panic!("expected MoreThanMeetsTheEye({{2}}{{U}}), got {other:?}"),
        }
    }

    #[test]
    fn parse_keyword_from_oracle_typecycling() {
        // CR 702.29: Typecycling — "plainscycling {2}" is typecycling, not regular cycling
        let kw = parse_keyword_from_oracle("plainscycling {2}").unwrap();
        assert!(matches!(kw, Keyword::Typecycling { .. }));
        if let Keyword::Typecycling { subtype, .. } = &kw {
            assert_eq!(subtype, "Plains");
        }

        // "forestcycling {1}{G}" — different subtype
        let kw2 = parse_keyword_from_oracle("forestcycling {1}{G}").unwrap();
        if let Keyword::Typecycling { subtype, .. } = &kw2 {
            assert_eq!(subtype, "Forest");
        }
    }

    #[test]
    fn parse_keyword_from_oracle_regular_cycling_not_typecycling() {
        // "cycling {2}" must remain regular Cycling, not Typecycling
        let kw = parse_keyword_from_oracle("cycling {2}").unwrap();
        assert!(matches!(kw, Keyword::Cycling(CyclingCost::Mana(_))));
    }

    #[test]
    fn parse_keyword_from_oracle_cycling_em_dash_pay_life() {
        // CR 702.29a: Street Wraith — "cycling—pay 2 life" must yield
        // Keyword::Cycling(CyclingCost::NonMana(PayLife { life: 2 })).
        let kw = parse_keyword_from_oracle("cycling\u{2014}pay 2 life").unwrap();
        let Keyword::Cycling(CyclingCost::NonMana(ac)) = kw else {
            panic!("expected Cycling NonMana variant, got {kw:?}");
        };
        assert!(
            matches!(ac, AbilityCost::PayLife { .. }),
            "expected PayLife, got {ac:?}"
        );
    }

    #[test]
    fn parse_keyword_from_oracle_cycling_mana_backward_compat() {
        // Regression: plain mana cycling still dispatches through the direct
        // `FromStr` path and yields CyclingCost::Mana (unchanged behaviour).
        let kw = parse_keyword_from_oracle("cycling {2}").unwrap();
        let Keyword::Cycling(CyclingCost::Mana(_)) = kw else {
            panic!("expected Cycling Mana variant, got {kw:?}");
        };
    }

    #[test]
    fn parse_keyword_from_oracle_protection_from_color() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        // CR 702.16: "protection from red" parses to Protection(Color(Red))
        let kw = parse_keyword_from_oracle("protection from red").unwrap();
        assert_eq!(
            kw,
            Keyword::Protection(ProtectionTarget::Color(ManaColor::Red))
        );

        let kw = parse_keyword_from_oracle("protection from blue").unwrap();
        assert_eq!(
            kw,
            Keyword::Protection(ProtectionTarget::Color(ManaColor::Blue))
        );
    }

    #[test]
    fn parse_keyword_from_oracle_protection_from_chosen_color() {
        use crate::types::keywords::ProtectionTarget;

        // CR 702.16: "protection from the chosen color" parses to Protection(ChosenColor)
        let kw = parse_keyword_from_oracle("protection from the chosen color").unwrap();
        assert_eq!(kw, Keyword::Protection(ProtectionTarget::ChosenColor));
    }

    #[test]
    fn parse_keyword_from_oracle_protection_from_each_of_your_opponents() {
        use crate::types::ability::ControllerRef;
        use crate::types::keywords::ProtectionTarget;

        // Issue #767 / CR 702.16k: Figure of Fable's Avatar form. Previously
        // fell through to ProtectionTarget::CardType("each of your opponents"),
        // which never matched any source at runtime.
        let kw = parse_keyword_from_oracle("protection from each of your opponents").unwrap();
        assert_eq!(
            kw,
            Keyword::Protection(ProtectionTarget::FromPlayer(ControllerRef::Opponent))
        );
    }

    #[test]
    fn parse_keyword_from_oracle_gift_a_card() {
        use crate::types::keywords::GiftKind;
        let kw = parse_keyword_from_oracle("gift a card").unwrap();
        assert_eq!(kw, Keyword::Gift(GiftKind::Card));
    }

    #[test]
    fn parse_keyword_from_oracle_gift_a_treasure() {
        use crate::types::keywords::GiftKind;
        let kw = parse_keyword_from_oracle("gift a treasure").unwrap();
        assert_eq!(kw, Keyword::Gift(GiftKind::Treasure));
    }

    #[test]
    fn parse_keyword_from_oracle_gift_a_food() {
        use crate::types::keywords::GiftKind;
        let kw = parse_keyword_from_oracle("gift a food").unwrap();
        assert_eq!(kw, Keyword::Gift(GiftKind::Food));
    }

    #[test]
    fn parse_keyword_from_oracle_gift_a_tapped_fish() {
        use crate::types::keywords::GiftKind;
        let kw = parse_keyword_from_oracle("gift a tapped fish").unwrap();
        assert_eq!(kw, Keyword::Gift(GiftKind::TappedFish));
    }

    #[test]
    fn gift_is_keyword_cost_line() {
        assert!(is_keyword_cost_line("gift a card"));
        assert!(is_keyword_cost_line("gift a treasure"));
        assert!(is_keyword_cost_line("gift a tapped fish"));
    }

    #[test]
    fn is_keyword_cost_line_new_keywords() {
        assert!(is_keyword_cost_line("toxic 2"));
        assert!(is_keyword_cost_line("saddle 3"));
        assert!(is_keyword_cost_line("soulshift 7"));
        assert!(is_keyword_cost_line("backup 1"));
        assert!(is_keyword_cost_line("squad {2}"));
    }

    #[test]
    fn is_keyword_cost_line_typecycling() {
        // Typecycling lines should be recognized as keyword cost lines
        assert!(is_keyword_cost_line("plainscycling {2}"));
        assert!(is_keyword_cost_line("forestcycling {1}{G}"));
        assert!(is_keyword_cost_line("islandcycling {2}"));
        // Regular cycling still matches (existing behavior)
        assert!(is_keyword_cost_line("cycling {2}"));
    }

    // --- expand_protection_parts tests ---

    /// CR 702.16i: a bare comma-separated color list continues the "protection
    /// from" prefix. Building-block test with synthetic split input (the real
    /// Swords use "protection from red and from blue" — the already-working
    /// "and from" path — so this exercises the new bare-list branch directly).
    #[test]
    fn expand_protection_bare_color_list_continues_prefix() {
        let expanded = expand_protection_parts(&["protection from white", "blue", "red"]);
        let entries: Vec<&str> = expanded.iter().map(|c| c.as_ref()).collect();
        assert!(
            entries.contains(&"protection from white"),
            "got {entries:?}"
        );
        assert!(entries.contains(&"protection from blue"), "got {entries:?}");
        assert!(entries.contains(&"protection from red"), "got {entries:?}");
        assert_eq!(
            entries.len(),
            3,
            "expected exactly three entries, got {entries:?}"
        );
    }

    /// CR 702.16i + CR 702.16a: Tinfoil Helm — a bare comma-list of subtypes and
    /// "hybrid mana" all continue the "protection from" prefix into eight
    /// separate protection entries. The subtype/quality tokens are non-keywords
    /// (map_keyword → None) so they do NOT reset the list.
    #[test]
    fn expand_protection_tinfoil_bare_subtype_list() {
        // Caller (`split_keyword_list`) has already split on ", and "/", "/" and ",
        // so the Oxford-comma "and " is consumed and each element is a clean token.
        let expanded = expand_protection_parts(&[
            "protection from aliens",
            "birds",
            "eldrazi",
            "lizards",
            "mutants",
            "robots",
            "yetis",
            "hybrid mana",
        ]);
        let protection_entries = expanded
            .iter()
            // allow-noncombinator: test assertion on already-expanded output, not parsing dispatch.
            .filter(|c| c.as_ref().starts_with("protection from "))
            .count();
        assert_eq!(
            protection_entries, 8,
            "expected eight protection entries, got {:?}",
            expanded
        );
    }

    /// CR 702.16i: a genuine trailing keyword (Akroma's "vigilance") ends the
    /// protection list and passes through unexpanded — it must NOT become
    /// "protection vigilance". Synthetic input, robust regardless of the caller
    /// split path.
    #[test]
    fn expand_protection_trailing_keyword_resets() {
        let expanded =
            expand_protection_parts(&["protection from black", "protection from red", "vigilance"]);
        let entries: Vec<&str> = expanded.iter().map(|c| c.as_ref()).collect();
        assert_eq!(
            entries,
            vec!["protection from black", "protection from red", "vigilance"],
            "vigilance must reset the list, not become a protection quality"
        );
    }

    /// CR 702.16i: a multi-word trailing keyword ("first strike") also resets.
    #[test]
    fn expand_protection_trailing_multiword_keyword_resets() {
        let expanded = expand_protection_parts(&["protection from black", "first strike"]);
        let entries: Vec<&str> = expanded.iter().map(|c| c.as_ref()).collect();
        assert_eq!(entries, vec!["protection from black", "first strike"]);
    }

    /// CR 702.16i: a genuine single-word keyword that is NOT a creature subtype
    /// ("haste") still resets the list — subtype precedence must not weaken the
    /// keyword-reset path for non-subtype keywords.
    #[test]
    fn expand_protection_trailing_haste_keyword_resets() {
        let expanded = expand_protection_parts(&["protection from white", "haste"]);
        let entries: Vec<&str> = expanded.iter().map(|c| c.as_ref()).collect();
        assert_eq!(
            entries,
            vec!["protection from white", "haste"],
            "haste is a keyword, not a subtype, so it must reset the list"
        );
        assert!(!crate::parser::oracle_util::is_subtype_word("haste"));
    }

    /// CR 702.16i + CR 205.3 (205.3m creature subtypes): subtype precedence — a recognized creature subtype
    /// is always classified as a bare protection *quality* and continues the
    /// list, even for a token that also collides with a keyword name. This is the
    /// invariant the subtype-precedence branch guarantees: for every recognized
    /// single-word subtype, `is_bare_protection_quality` returns true regardless
    /// of what `map_keyword` would return for that same token. Reverting the
    /// subtype-precedence branch would break this for any subtype that is also a
    /// keyword name (a latent overlap between the growing MTGJSON subtype
    /// vocabulary and the keyword set).
    #[test]
    fn bare_protection_quality_subtype_precedence_over_keyword_collision() {
        use crate::parser::oracle_util::is_subtype_word;

        // Real singular subtypes continue the list (never reset), exercising the
        // subtype-precedence branch directly. `is_subtype_word` matches the
        // singular head, so use singular tokens here.
        for subtype in ["alien", "sliver", "cleric", "dragon"] {
            assert!(
                is_subtype_word(subtype),
                "{subtype} must be a recognized subtype token"
            );
            assert!(
                is_bare_protection_quality(subtype),
                "{subtype} is a recognized subtype and must be a protection quality"
            );
        }

        // Invariant: whenever a single-word token is a recognized subtype, it is
        // a bare quality — independent of its keyword status. This is what makes
        // "protection from <color>, <subtype-that-is-also-a-keyword>, …" continue
        // rather than reset. (No such collision exists in the current vocabulary,
        // so this asserts the guarantee holds for the whole subtype class rather
        // than a single hand-picked token.)
        for token in ["merfolk", "wall", "assassin", "advisor"] {
            if is_subtype_word(token) {
                assert!(
                    is_bare_protection_quality(token),
                    "recognized subtype {token} must always be a protection quality"
                );
            }
        }
    }

    /// CR 702.16i: subtype-and-quality members expand into individual protection
    /// entries alongside colors and never reset, mirroring the Tinfoil Helm class
    /// with a leading color prefix. Regression companion to the subtype-precedence
    /// unit test above at the `expand_protection_parts` seam.
    #[test]
    fn expand_protection_color_then_subtype_list_all_continue() {
        let expanded =
            expand_protection_parts(&["protection from white", "sliver", "cleric", "hybrid mana"]);
        let protection_entries = expanded
            .iter()
            // allow-noncombinator: test assertion on already-expanded output, not parsing dispatch.
            .filter(|c| c.as_ref().starts_with("protection from "))
            .count();
        assert_eq!(
            protection_entries, 4,
            "color + two subtypes + hybrid mana must all expand as protection, got {expanded:?}"
        );
    }

    /// CR 702.16i: a compound rider — "hexproof from that color … and can't be
    /// blocked by creatures of that color" — must NOT swallow the "can't be
    /// blocked …" restriction as a bogus hexproof quality. The verb-clause guard
    /// resets the active prefix so the restriction passes through for the
    /// downstream clause splitter (regression guard for the block-restriction
    /// grant path).
    #[test]
    fn expand_protection_verb_clause_rider_resets() {
        let expanded = expand_protection_parts(&[
            "hexproof from that color until end of turn",
            "can't be blocked by creatures of that color this turn",
        ]);
        let entries: Vec<&str> = expanded.iter().map(|c| c.as_ref()).collect();
        assert!(
            entries.contains(&"can't be blocked by creatures of that color this turn"),
            "restriction rider must pass through unexpanded, got {entries:?}"
        );
        let swallowed = entries.iter().any(|e| {
            // allow-noncombinator: test assertion on already-expanded output, not parsing dispatch.
            e.contains("can't be blocked") && e.starts_with("hexproof from ")
        });
        assert!(
            !swallowed,
            "restriction must not be swallowed as a hexproof quality, got {entries:?}"
        );
    }

    #[test]
    fn expand_protection_baneslayer_pattern() {
        // CR 702.16: "protection from Demons and from Dragons" → two Protection keywords
        let keywords = extract_keyword_line(
            "Flying, first strike, lifelink, protection from Demons and from Dragons",
            &[
                "flying".to_string(),
                "first strike".to_string(),
                "lifelink".to_string(),
                "protection".to_string(),
            ],
        )
        .unwrap();
        let protection_count = keywords
            .iter()
            .filter(|k| matches!(k, Keyword::Protection(_)))
            .count();
        assert_eq!(
            protection_count, 2,
            "expected two separate Protection keywords"
        );
    }

    #[test]
    fn expand_protection_two_colors() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        // CR 702.16: "protection from black and from red" → two color protections
        let keywords = extract_keyword_line(
            "Flying, protection from black and from red",
            &["flying".to_string(), "protection".to_string()],
        )
        .unwrap();
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Black
            )))
        );
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Red
            )))
        );
    }

    #[test]
    fn expand_protection_three_comma_continuation() {
        // CR 702.16: comma + Oxford comma continuation
        let keywords = extract_keyword_line(
            "First strike, protection from Vampires, from Werewolves, and from Zombies",
            &["first strike".to_string(), "protection".to_string()],
        )
        .unwrap();
        let protection_count = keywords
            .iter()
            .filter(|k| matches!(k, Keyword::Protection(_)))
            .count();
        assert_eq!(
            protection_count, 3,
            "expected three separate Protection keywords"
        );
    }

    #[test]
    fn expand_protection_preserves_qualifier_text() {
        use crate::types::keywords::ProtectionTarget;

        // Emrakul pattern: qualifier text preserved after split
        let keywords = extract_keyword_line(
            "protection from spells and from permanents that were cast this turn",
            &["protection".to_string()],
        )
        .unwrap();
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::CardType(
                "spells".to_string()
            )))
        );
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::CardType(
                "permanents that were cast this turn".to_string()
            )))
        );
    }

    #[test]
    fn expand_protection_from_everything_no_split() {
        use crate::types::keywords::ProtectionTarget;

        // CR 702.16j: "protection from everything" → typed `Everything` variant
        // (no " and from " present, no expansion).
        let keywords =
            extract_keyword_line("protection from everything", &["protection".to_string()])
                .unwrap();
        assert_eq!(keywords.len(), 1);
        assert_eq!(
            keywords[0],
            Keyword::Protection(ProtectionTarget::Everything)
        );
    }

    #[test]
    fn expand_protection_single_no_expansion() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        // Single protection — expansion is a no-op
        let keywords = extract_keyword_line(
            "Flying, protection from red",
            &["flying".to_string(), "protection".to_string()],
        )
        .unwrap();
        let prots: Vec<_> = keywords
            .iter()
            .filter(|k| matches!(k, Keyword::Protection(_)))
            .collect();
        assert_eq!(prots.len(), 1);
        assert_eq!(
            prots[0],
            &Keyword::Protection(ProtectionTarget::Color(ManaColor::Red))
        );
    }

    #[test]
    fn expand_protection_non_protection_line_unchanged() {
        // Non-protection keyword line — all matched by MTGJSON, no extracted keywords
        let keywords = extract_keyword_line(
            "Flying, first strike, lifelink",
            &[
                "flying".to_string(),
                "first strike".to_string(),
                "lifelink".to_string(),
            ],
        )
        .unwrap();
        assert!(
            keywords.is_empty(),
            "all keywords matched by MTGJSON, none extracted"
        );
    }

    #[test]
    fn expand_protection_three_way_inline_and_from() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        // Three-way inline split: "protection from red and from blue and from green"
        let keywords = extract_keyword_line(
            "Flying, protection from red and from blue and from green",
            &["flying".to_string(), "protection".to_string()],
        )
        .unwrap();
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Red
            )))
        );
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Blue
            )))
        );
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Green
            )))
        );
    }

    #[test]
    fn expand_protection_from_each_color_to_five_wubrg() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        // CR 702.16 + CR 105.2: "protection from each color" is shorthand for
        // protection from white, blue, black, red, and green simultaneously
        // (Akroma's Will, Iridescent Angel, Spectra Ward, etc.).
        let keywords = extract_keyword_line(
            "Flying, protection from each color",
            &["flying".to_string(), "protection".to_string()],
        )
        .unwrap();
        let prots: Vec<_> = keywords
            .iter()
            .filter_map(|k| match k {
                Keyword::Protection(pt) => Some(pt.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            prots.len(),
            5,
            "expected 5 color protections, got {prots:?}"
        );
        for color in [
            ManaColor::White,
            ManaColor::Blue,
            ManaColor::Black,
            ManaColor::Red,
            ManaColor::Green,
        ] {
            assert!(
                prots.contains(&ProtectionTarget::Color(color)),
                "missing Protection(Color({color:?})) in {prots:?}"
            );
        }
    }

    #[test]
    fn expand_protection_from_all_colors_to_five_wubrg() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        // CR 702.16 + CR 105.2: "protection from all colors" is the same
        // shorthand as "from each color" (Pristine Angel pattern, simplified
        // form ignoring the artifact clause).
        let keywords =
            extract_keyword_line("protection from all colors", &["protection".to_string()])
                .unwrap();
        let prots: Vec<_> = keywords
            .iter()
            .filter_map(|k| match k {
                Keyword::Protection(pt) => Some(pt.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            prots.len(),
            5,
            "expected 5 color protections, got {prots:?}"
        );
        assert!(prots.contains(&ProtectionTarget::Color(ManaColor::White)));
        assert!(prots.contains(&ProtectionTarget::Color(ManaColor::Blue)));
        assert!(prots.contains(&ProtectionTarget::Color(ManaColor::Black)));
        assert!(prots.contains(&ProtectionTarget::Color(ManaColor::Red)));
        assert!(prots.contains(&ProtectionTarget::Color(ManaColor::Green)));
    }

    #[test]
    fn expand_hexproof_from_each_color_to_five_wubrg() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        // CR 702.11d + CR 105.2: "hexproof from each color" — Breaker of
        // Creation. Mirrors the protection-from-each-color expansion.
        let keywords =
            extract_keyword_line("hexproof from each color", &["hexproof".to_string()]).unwrap();
        let hf: Vec<_> = keywords
            .iter()
            .filter_map(|k| match k {
                Keyword::HexproofFrom(f) => Some(f.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(hf.len(), 5, "expected 5 color hexproofs, got {hf:?}");
        for color in [
            ManaColor::White,
            ManaColor::Blue,
            ManaColor::Black,
            ManaColor::Red,
            ManaColor::Green,
        ] {
            assert!(
                hf.contains(&HexproofFilter::Color(color)),
                "missing HexproofFrom(Color({color:?})) in {hf:?}"
            );
        }
    }

    #[test]
    fn contains_each_or_all_colors_phrase_word_boundary() {
        // Word-boundary guard: bare phrases match, color-stem extensions don't.
        assert!(super::contains_each_or_all_colors_phrase(
            "protection from each color"
        ));
        assert!(super::contains_each_or_all_colors_phrase(
            "protection from all colors"
        ));
        assert!(super::contains_each_or_all_colors_phrase(
            "protection from each color."
        ));
        assert!(super::contains_each_or_all_colors_phrase(
            "has protection from each color and from artifacts"
        ));
        // Negative: "colored" is not "color"; "colorless" not "colors".
        assert!(!super::contains_each_or_all_colors_phrase(
            "draw a card from each colored permanent"
        ));
        assert!(!super::contains_each_or_all_colors_phrase(
            "search from all colorless lands"
        ));
        // Not a from-phrase at all.
        assert!(!super::contains_each_or_all_colors_phrase(
            "for each color among permanents you control"
        ));
    }

    #[test]
    fn expand_protection_from_each_color_with_trailing_period() {
        use crate::types::keywords::ProtectionTarget;

        // Defensive: if an upstream caller forgets to strip the trailing
        // period, the helper still recognizes the shorthand and emits the
        // 5 typed Color protections rather than falling through to the
        // no-op CardType branch.
        let mut expanded: Vec<Cow<'_, str>> = Vec::new();
        super::push_quality_entry(&mut expanded, "protection from", "each color.");
        assert_eq!(expanded.len(), 5);

        // End-to-end through extract_keyword_line is the more conservative
        // check — current callers do strip the period, so we don't assert
        // on that path. The helper-level guard is what we're locking in.
        let keywords =
            extract_keyword_line("protection from each color", &["protection".to_string()])
                .unwrap();
        let prots = keywords
            .iter()
            .filter(|k| matches!(k, Keyword::Protection(ProtectionTarget::Color(_))))
            .count();
        assert_eq!(prots, 5);
    }

    #[test]
    fn protection_from_each_color_with_qualifier_not_expanded() {
        use crate::types::keywords::ProtectionTarget;

        // Guard: Commander's Plate ("protection from each color that's not in
        // your commander's color identity") and Council Guardian ("protection
        // from each color with the most votes") are dynamic, conditional
        // qualifiers — the "each color" prefix here is NOT the bare 5-WUBRG
        // shorthand. Expansion must leave them untouched so a future dynamic
        // handler can interpret them.
        let keywords = extract_keyword_line(
            "protection from each color that's not in your commander's color identity",
            &["protection".to_string()],
        )
        .unwrap();
        let prots: Vec<_> = keywords
            .iter()
            .filter(|k| matches!(k, Keyword::Protection(_)))
            .collect();
        assert_eq!(
            prots.len(),
            1,
            "qualified 'each color' phrase must not expand, got {prots:?}"
        );
        // Should remain as CardType (or future dynamic variant) — not 5 Color entries.
        assert!(
            !matches!(prots[0], Keyword::Protection(ProtectionTarget::Color(_))),
            "qualified 'each color' was wrongly expanded to a Color variant: {prots:?}"
        );
    }

    #[test]
    fn extract_keyword_line_transmute() {
        // CR 702.53a: Transmute {cost} — single-keyword line with parameterized cost
        let mtgjson_kws = vec!["transmute".to_string()];

        // Verify parse_keyword_from_oracle works directly
        let direct = parse_keyword_from_oracle("transmute {1}{b}{b}");
        assert!(
            direct.is_some(),
            "parse_keyword_from_oracle should handle 'transmute {{1}}{{b}}{{b}}'"
        );
        assert!(matches!(direct.unwrap(), Keyword::Transmute(_)));

        let result = extract_keyword_line("Transmute {1}{B}{B}", &mtgjson_kws);
        assert!(result.is_some(), "Should recognize as keyword line");
        let keywords = result.unwrap();
        assert_eq!(keywords.len(), 1);
        assert!(matches!(keywords[0], Keyword::Transmute(_)));
    }

    #[test]
    fn parse_keyword_from_oracle_umbra_and_totem_armor() {
        // CR 702.89a/b: both the current "umbra armor" and the obsolete
        // "totem armor" spelling map to Keyword::TotemArmor.
        assert_eq!(
            parse_keyword_from_oracle("umbra armor"),
            Some(Keyword::TotemArmor)
        );
        assert_eq!(
            parse_keyword_from_oracle("totem armor"),
            Some(Keyword::TotemArmor)
        );
    }

    #[test]
    fn extract_keyword_line_umbra_armor_reachable_without_mtgjson_keyword() {
        // CR 702.89a: the Umbra cycle's "Umbra armor (…)" line carries reminder
        // text and is NOT surfaced in MTGJSON's `keywords` array, so it must be
        // recovered from the Oracle line. Regression guard that the runtime
        // umbra-armor replacement is actually reachable (the keyword is produced).
        for line in [
            "Umbra armor (If enchanted permanent would be destroyed, instead remove all damage marked on it and destroy this Aura.)",
            "Totem armor (If enchanted creature would be destroyed, instead remove all damage marked on it and destroy this Aura.)",
        ] {
            let result = extract_keyword_line(line, &[]);
            assert_eq!(
                result,
                Some(vec![Keyword::TotemArmor]),
                "umbra/totem armor line must yield Keyword::TotemArmor, got {result:?} for {line:?}"
            );
        }
    }

    #[test]
    fn extract_keyword_line_splice() {
        // CR 702.47a: Splice onto [type] {cost}
        let mtgjson_kws = vec!["splice".to_string()];
        let result = extract_keyword_line("Splice onto Arcane {1}{W}", &mtgjson_kws);
        assert!(result.is_some(), "Should recognize as keyword line");
        let keywords = result.unwrap();
        assert_eq!(keywords.len(), 1);
        // CR 702.47a: the splice subtype AND its cost must both be captured.
        match &keywords[0] {
            Keyword::Splice { subtype, cost } => {
                assert_eq!(subtype, "Arcane");
                assert_eq!(
                    *cost,
                    crate::database::mtgjson::parse_mtgjson_mana_cost("{1}{W}")
                );
            }
            other => panic!("expected Keyword::Splice, got {other:?}"),
        }
    }

    #[test]
    fn extract_keyword_line_mobilize_where_x_quantity() {
        use crate::types::ability::{CountScope, QuantityRef, TypeFilter, ZoneRef};

        let mtgjson_kws = vec!["mobilize".to_string()];
        let result = extract_keyword_line(
            "Mobilize X, where X is the number of creature cards in your graveyard",
            &mtgjson_kws,
        )
        .expect("mobilize where-X line should be recognized");

        assert_eq!(result.len(), 1);
        match &result[0] {
            Keyword::Mobilize(QuantityExpr::Ref {
                qty:
                    QuantityRef::ZoneCardCount {
                        zone,
                        card_types,
                        scope,
                        filter: None,
                    },
            }) => {
                assert_eq!(*zone, ZoneRef::Graveyard);
                assert_eq!(card_types, &vec![TypeFilter::Creature]);
                assert_eq!(*scope, CountScope::Controller);
            }
            other => panic!("expected dynamic Mobilize ZoneCardCount, got {other:?}"),
        }
    }

    #[test]
    fn extract_keyword_line_mobilize_fixed_quantity() {
        let mtgjson_kws = vec!["mobilize".to_string()];
        let result = extract_keyword_line("Mobilize 2", &mtgjson_kws)
            .expect("fixed mobilize line should be recognized");

        assert_eq!(
            result,
            vec![Keyword::Mobilize(QuantityExpr::Fixed { value: 2 })]
        );
    }

    #[test]
    fn extract_keyword_line_firebending_source_power() {
        use crate::types::ability::{ObjectScope, QuantityRef};

        let mtgjson_kws = vec!["firebending".to_string()];
        let result = extract_keyword_line("Firebending X, where X is ~'s power.", &mtgjson_kws)
            .expect("firebending source-power line should be recognized");

        assert_eq!(result.len(), 1);
        assert!(matches!(
            &result[0],
            Keyword::Firebending(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Source
                }
            })
        ));
    }

    #[test]
    fn extract_keyword_line_firebending_comma_separated_fixed_amounts() {
        let mtgjson_kws = vec![
            "flying".to_string(),
            "firebending".to_string(),
            "menace".to_string(),
            "trample".to_string(),
            "haste".to_string(),
        ];

        let flying = extract_keyword_line("Flying, firebending 2", &mtgjson_kws)
            .expect("comma-separated flying/firebending should parse");
        assert_eq!(
            flying,
            vec![Keyword::Firebending(QuantityExpr::Fixed { value: 2 })]
        );

        let menace = extract_keyword_line("Menace, firebending 3", &mtgjson_kws)
            .expect("comma-separated menace/firebending should parse");
        assert_eq!(
            menace,
            vec![Keyword::Firebending(QuantityExpr::Fixed { value: 3 })]
        );

        let trample_haste = extract_keyword_line("Trample, firebending 4, haste", &mtgjson_kws)
            .expect("comma-separated trample/firebending/haste should parse");
        assert_eq!(
            trample_haste,
            vec![Keyword::Firebending(QuantityExpr::Fixed { value: 4 })]
        );
    }

    #[test]
    fn extract_keyword_line_firebending_creatures_you_control() {
        use crate::types::ability::{ControllerRef, QuantityRef, TargetFilter, TypeFilter};

        let mtgjson_kws = vec!["firebending".to_string()];
        let result = extract_keyword_line(
            "Firebending X, where X is the number of creatures you control.",
            &mtgjson_kws,
        )
        .expect("firebending creature-count line should be recognized");

        assert_eq!(result.len(), 1);
        match &result[0] {
            Keyword::Firebending(QuantityExpr::Ref {
                qty:
                    QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(filter),
                    },
            }) => {
                assert_eq!(filter.type_filters, vec![TypeFilter::Creature]);
                assert_eq!(filter.controller, Some(ControllerRef::You));
            }
            other => panic!("expected dynamic Firebending ObjectCount, got {other:?}"),
        }
    }

    #[test]
    fn extract_keyword_line_firebending_experience_counters() {
        use crate::types::ability::{CountScope, QuantityRef};
        use crate::types::player::PlayerCounterKind;

        let mtgjson_kws = vec!["firebending".to_string()];
        let result = extract_keyword_line(
            "Firebending X, where X is the number of experience counters you have.",
            &mtgjson_kws,
        )
        .expect("firebending experience-count line should be recognized");

        assert_eq!(result.len(), 1);
        assert!(matches!(
            &result[0],
            Keyword::Firebending(QuantityExpr::Ref {
                qty: QuantityRef::PlayerCounter {
                    kind: PlayerCounterKind::Experience,
                    scope: CountScope::Controller,
                }
            })
        ));
    }

    fn craft_keyword(text: &str) -> (TargetFilter, CostObjectCount) {
        match parse_keyword_from_oracle(text).expect("craft keyword should parse") {
            Keyword::Craft {
                materials, count, ..
            } => (materials, count),
            other => panic!("expected Craft keyword, got {other:?}"),
        }
    }

    fn craft_filter_has_type(filter: &TargetFilter, wanted: &TypeFilter) -> bool {
        match filter {
            TargetFilter::Typed(typed) => typed.type_filters.iter().any(|tf| tf == wanted),
            TargetFilter::Or { filters } => filters
                .iter()
                .any(|filter| craft_filter_has_type(filter, wanted)),
            _ => false,
        }
    }

    #[test]
    fn parse_craft_materials_composes_count_and_type_phrase() {
        let (materials, count) = craft_keyword("craft with two creatures {5}{b}");
        assert_eq!(count, CostObjectCount::exactly(2));
        assert!(craft_filter_has_type(&materials, &TypeFilter::Creature));

        let (materials, count) = craft_keyword("craft with six artifacts {4}");
        assert_eq!(count, CostObjectCount::exactly(6));
        assert!(craft_filter_has_type(&materials, &TypeFilter::Artifact));
    }

    #[test]
    fn parse_craft_materials_supports_at_least_and_subtypes() {
        let (materials, count) = craft_keyword("craft with one or more dinosaurs {4}{r}");
        assert_eq!(count, CostObjectCount::at_least(1));
        assert!(craft_filter_has_type(
            &materials,
            &TypeFilter::Subtype("Dinosaur".to_string())
        ));

        let (materials, count) = craft_keyword("craft with cave {5}{g}");
        assert_eq!(count, CostObjectCount::exactly(1));
        assert!(craft_filter_has_type(
            &materials,
            &TypeFilter::Subtype("Cave".to_string())
        ));
    }

    #[test]
    fn parse_craft_materials_supports_unqualified_one_or_more() {
        let (materials, count) = craft_keyword("craft with one or more {5}");
        assert_eq!(count, CostObjectCount::at_least(1));
        match materials {
            TargetFilter::Or { filters } => assert_eq!(filters.len(), 2),
            other => panic!("expected any-material dual-zone filter, got {other:?}"),
        }
    }

    #[test]
    fn parse_craft_materials_refuses_unmodeled_selection_constraints() {
        for text in [
            "craft with two that share a card type {6}",
            "craft with four or more nonlands with activated abilities {8}{u}",
            "craft with a dinosaur, a merfolk, a pirate, and a vampire {4}",
        ] {
            let parsed = parse_keyword_from_oracle(text);
            assert!(
                parsed.is_none(),
                "{text} must not parse as an approximate Craft cost, got {parsed:?}"
            );
        }
    }

    #[test]
    fn parse_keyword_from_oracle_firebending_fixed_amount() {
        assert_eq!(
            parse_keyword_from_oracle("firebending 5"),
            Some(Keyword::Firebending(QuantityExpr::Fixed { value: 5 }))
        );
    }

    #[test]
    fn extract_keyword_line_bloodthirst_x_overrides_mtgjson_fallback() {
        let result = extract_keyword_line(
            "Bloodthirst X (This creature enters with X +1/+1 counters on it, where X is the damage dealt to your opponents this turn.)",
            &["bloodthirst".to_string()],
        )
        .expect("bloodthirst X line should be recognized");

        assert_eq!(result, vec![Keyword::Bloodthirst(BloodthirstValue::X)]);
    }

    #[test]
    fn parse_keyword_from_oracle_bloodthirst_fixed_and_x() {
        assert_eq!(
            parse_keyword_from_oracle("bloodthirst 2").unwrap(),
            Keyword::Bloodthirst(BloodthirstValue::Fixed(2))
        );
        assert_eq!(
            parse_keyword_from_oracle("bloodthirst x").unwrap(),
            Keyword::Bloodthirst(BloodthirstValue::X)
        );
    }

    #[test]
    fn parse_keyword_from_oracle_landwalk_variants() {
        // CR 702.14: Landwalk variants from Oracle text
        let kw = parse_keyword_from_oracle("swampwalk").unwrap();
        assert_eq!(kw, Keyword::Landwalk("Swamp".to_string()));

        let kw = parse_keyword_from_oracle("islandwalk").unwrap();
        assert_eq!(kw, Keyword::Landwalk("Island".to_string()));

        let kw = parse_keyword_from_oracle("forestwalk").unwrap();
        assert_eq!(kw, Keyword::Landwalk("Forest".to_string()));
    }

    #[test]
    fn parse_keyword_from_oracle_unit_keywords() {
        // Unit keywords that should be recognized
        let kw = parse_keyword_from_oracle("bargain").unwrap();
        assert_eq!(kw, Keyword::Bargain);

        let kw = parse_keyword_from_oracle("training").unwrap();
        assert_eq!(kw, Keyword::Training);

        let kw = parse_keyword_from_oracle("jump-start").unwrap();
        assert_eq!(kw, Keyword::JumpStart);

        let kw = parse_keyword_from_oracle("undaunted").unwrap();
        assert_eq!(kw, Keyword::Undaunted);

        let kw = parse_keyword_from_oracle("for mirrodin!").unwrap();
        assert_eq!(kw, Keyword::ForMirrodin);
    }

    #[test]
    fn extract_keyword_line_for_mirrodin_without_mtgjson_keyword() {
        let keywords = extract_keyword_line(
            "For Mirrodin! (When this Equipment enters, create a 2/2 red Rebel creature token, then attach this to it.)",
            &[],
        )
        .expect("For Mirrodin! should be extracted even when MTGJSON omits it");

        assert_eq!(keywords, vec![Keyword::ForMirrodin]);
        assert!(
            extract_keyword_line("Flying", &[]).is_none(),
            "the MTGJSON-missing path should stay scoped to known omissions"
        );
    }

    #[test]
    fn is_keyword_cost_line_rejects_trigger_text() {
        // "when you cycle a card" is trigger text, not a keyword cost line
        assert!(!is_keyword_cost_line("when you cycle a card"));
        assert!(!is_keyword_cost_line(
            "whenever you cycle or discard a card"
        ));
    }

    #[test]
    fn is_keyword_cost_line_em_dash() {
        // CR 702.138: Escape uses em-dash separator — must be recognized
        assert!(is_keyword_cost_line(
            "escape\u{2014}{w}, exile two other cards from your graveyard."
        ));
    }

    #[test]
    fn parse_keyword_from_oracle_escape_em_dash() {
        // CR 702.138a: Escape joins the em-dash alt-cost keyword family.
        // parse_keyword_from_oracle receives already-lowercased oracle text.
        use crate::types::keywords::EscapeCost;
        let kw = parse_keyword_from_oracle(
            "escape\u{2014}{2}{u}{r}, exile four other cards from your graveyard",
        )
        .expect("escape em-dash keyword must parse");
        match kw {
            Keyword::Escape(EscapeCost::NonMana(AbilityCost::Composite { costs })) => {
                assert!(
                    matches!(
                        costs.as_slice(),
                        [
                            AbilityCost::Mana { .. },
                            AbilityCost::Exile {
                                count: 4,
                                zone: Some(Zone::Graveyard),
                                ..
                            }
                        ]
                    ),
                    "unexpected escape composite cost: {costs:?}"
                );
            }
            other => panic!("expected Keyword::Escape(NonMana(Composite)), got {other:?}"),
        }
    }

    #[test]
    fn parse_keyword_from_oracle_suspend() {
        use crate::types::mana::ManaCost;

        // CR 702.62a: Suspend N—{cost}
        let kw = parse_keyword_from_oracle("suspend 4\u{2014}{u}").unwrap();
        match kw {
            Keyword::Suspend { count, cost } => {
                assert_eq!(count, 4);
                assert!(matches!(cost, ManaCost::Cost { generic: 0, shards } if shards.len() == 1));
            }
            other => panic!("Expected Suspend, got {other:?}"),
        }

        // Suspend 1—{R} (Rift Bolt)
        let kw = parse_keyword_from_oracle("suspend 1\u{2014}{r}").unwrap();
        match kw {
            Keyword::Suspend { count, .. } => assert_eq!(count, 1),
            other => panic!("Expected Suspend, got {other:?}"),
        }
    }

    #[test]
    fn is_keyword_cost_line_suspend() {
        // CR 702.62a: Suspend lines must be recognized as keyword cost lines
        assert!(is_keyword_cost_line("suspend 4\u{2014}{u}"));
        assert!(is_keyword_cost_line("suspend 1\u{2014}{r}"));
    }

    #[test]
    fn parse_prototype_keyword_line_extracts_pt() {
        use crate::types::mana::ManaCost;

        // CR 702.160a + CR 718.3b: "Prototype {cost} — {P}/{T}" carries the
        // alternative power/toughness. The prototype P/T (2/1) must come from the
        // Oracle "— P/T" segment, NOT the card's top-level P/T (Arcane Proxy: 4/3).
        let kw = parse_keyword_from_oracle("prototype {1}{u}{u} \u{2014} 2/1").unwrap();
        match kw {
            Keyword::Prototype {
                cost,
                power,
                toughness,
            } => {
                assert_eq!(power, Some(2));
                assert_eq!(toughness, Some(1));
                assert!(
                    matches!(cost, ManaCost::Cost { generic: 1, ref shards } if shards.len() == 2),
                    "expected {{1}}{{U}}{{U}}, got {cost:?}"
                );
            }
            other => panic!("Expected Prototype with P/T, got {other:?}"),
        }
    }

    #[test]
    fn parse_prototype_keyword_line_without_pt_falls_through() {
        // Graceful degradation: a cost-only "prototype {2}" line (no "— P/T")
        // must NOT panic — it falls through to the cost-only keyword path.
        let kw = parse_keyword_from_oracle("prototype {2}");
        if let Some(Keyword::Prototype {
            power, toughness, ..
        }) = kw
        {
            assert_eq!(power, None);
            assert_eq!(toughness, None);
        }
    }

    #[test]
    fn parse_partner_variant_oracle_text() {
        use crate::types::keywords::PartnerType;

        // CR 702.124: Partner variant keywords from Oracle text
        let kw = parse_keyword_from_oracle(
            "partner\u{2014}character select (you can have two commanders if both have this ability.)",
        ).unwrap();
        assert_eq!(kw, Keyword::Partner(PartnerType::CharacterSelect));

        let kw = parse_keyword_from_oracle(
            "partner\u{2014}friends forever (you can have two commanders if both have this ability.)",
        ).unwrap();
        assert_eq!(kw, Keyword::Partner(PartnerType::FriendsForever));

        let kw = parse_keyword_from_oracle(
            "choose a background (you can have a background as a second commander.)",
        )
        .unwrap();
        assert_eq!(kw, Keyword::Partner(PartnerType::ChooseABackground));

        let kw = parse_keyword_from_oracle(
            "doctor\u{2019}s companion (you can have two commanders if the other is the doctor.)",
        )
        .unwrap();
        assert_eq!(kw, Keyword::Partner(PartnerType::DoctorsCompanion));

        // Also test with straight apostrophe
        let kw = parse_keyword_from_oracle("doctor's companion").unwrap();
        assert_eq!(kw, Keyword::Partner(PartnerType::DoctorsCompanion));
    }

    // --- CR 702.11f: hexproof from X and from Y expansion ---

    #[test]
    fn expand_hexproof_from_compound() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        // CR 702.11f: "hexproof from white and from black" → two HexproofFrom keywords
        let expanded = expand_protection_parts(&["hexproof from white and from black"]);
        assert!(expanded.len() == 2);
        assert_eq!(expanded[0], "hexproof from white");
        assert_eq!(expanded[1], "hexproof from black");

        // Through extract_keyword_line
        let keywords = extract_keyword_line(
            "hexproof from white and from black",
            &["hexproof".to_string()],
        )
        .unwrap();
        assert!(keywords.len() == 2);
        assert_eq!(
            keywords[0],
            Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::White))
        );
        assert_eq!(
            keywords[1],
            Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Black))
        );
    }

    #[test]
    fn hexproof_from_single_no_expansion() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        // Single hexproof-from — no expansion needed
        let keywords =
            extract_keyword_line("hexproof from red", &["hexproof".to_string()]).unwrap();
        let hf: Vec<_> = keywords
            .iter()
            .filter(|k| matches!(k, Keyword::HexproofFrom(_)))
            .collect();
        assert_eq!(hf.len(), 1);
        assert_eq!(
            hf[0],
            &Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Red))
        );
    }

    #[test]
    fn hexproof_from_oracle_parses() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        // parse_keyword_from_oracle handles "hexproof from red"
        let kw = parse_keyword_from_oracle("hexproof from red").unwrap();
        assert_eq!(
            kw,
            Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Red))
        );

        let kw = parse_keyword_from_oracle("hexproof from artifacts").unwrap();
        assert_eq!(
            kw,
            Keyword::HexproofFrom(HexproofFilter::CardType("artifacts".to_string()))
        );
    }

    /// CR 702.xxx: Paradigm (Strixhaven) — bare-keyword recognition.
    /// Assign when WotC publishes SOS CR update.
    #[test]
    fn parse_keyword_from_oracle_paradigm() {
        let kw = parse_keyword_from_oracle("paradigm").unwrap();
        assert_eq!(kw, Keyword::Paradigm);
    }

    /// CR 702.34a: Compound flashback cost ("Flashback—{1}{U}, Pay 3 life") —
    /// Deep Analysis class. Parses to FlashbackCost::NonMana wrapping a
    /// Composite of Mana + PayLife sub-costs. The runtime split
    /// (`split_flashback_cost_components` in casting.rs) routes the mana piece
    /// through the normal mana-payment flow and the life piece through
    /// `pay_additional_cost`.
    #[test]
    fn parse_keyword_from_oracle_flashback_compound_mana_and_life() {
        use crate::types::ability::QuantityExpr;
        use crate::types::mana::ManaCostShard;

        // Lowercased Oracle text passed through `parse_keyword_from_oracle` after
        // reminder text is stripped by the upstream pipeline.
        let kw = parse_keyword_from_oracle("flashback\u{2014}{1}{u}, pay 3 life").unwrap();
        let Keyword::Flashback(FlashbackCost::NonMana(AbilityCost::Composite { costs })) = kw
        else {
            panic!("expected NonMana(Composite), got {:?}", kw);
        };
        assert_eq!(costs.len(), 2);
        let AbilityCost::Mana { cost: mana } = &costs[0] else {
            panic!("expected Mana sub-cost, got {:?}", costs[0]);
        };
        assert_eq!(
            mana,
            &ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Blue],
            }
        );
        assert_eq!(
            costs[1],
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 3 }
            }
        );
    }

    /// CR 702.129a + CR 602.1a: Champion of Wits family —
    /// "eternalize—{3}{U}{U}, discard a card" must parse to
    /// `Eternalize(EternalizeCost::NonMana(Composite[Mana{3UU}, Discard]))`,
    /// i.e. the discard suffix is NOT dropped.
    #[test]
    fn parse_keyword_from_oracle_eternalize_em_dash_discard() {
        use crate::types::mana::ManaCostShard;

        let kw = parse_keyword_from_oracle("eternalize\u{2014}{3}{u}{u}, discard a card").unwrap();
        let Keyword::Eternalize(EternalizeCost::NonMana(AbilityCost::Composite { costs })) = kw
        else {
            panic!("expected Eternalize NonMana(Composite), got {kw:?}");
        };
        assert_eq!(
            costs.len(),
            2,
            "mana + discard, no exile-self yet (synthesis)"
        );
        let AbilityCost::Mana { cost: mana } = &costs[0] else {
            panic!("expected Mana sub-cost, got {:?}", costs[0]);
        };
        assert_eq!(
            mana,
            &ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            }
        );
        assert!(
            matches!(&costs[1], AbilityCost::Discard { .. }),
            "discard suffix must survive, got {:?}",
            costs[1]
        );
    }

    /// CR 702.128a: Embalm em-dash composite cost parses the discard suffix.
    #[test]
    fn parse_keyword_from_oracle_embalm_em_dash_discard() {
        let kw = parse_keyword_from_oracle("embalm\u{2014}{2}{w}{w}, discard a card").unwrap();
        let Keyword::Embalm(EmbalmCost::NonMana(AbilityCost::Composite { costs })) = kw else {
            panic!("expected Embalm NonMana(Composite), got {kw:?}");
        };
        assert_eq!(costs.len(), 2);
        assert!(matches!(&costs[0], AbilityCost::Mana { .. }));
        assert!(matches!(&costs[1], AbilityCost::Discard { .. }));
    }

    /// Regression: pure-mana embalm/eternalize still dispatch through the direct
    /// `FromStr` path to the `Mana` variant (backward compat at the keyword level).
    #[test]
    fn parse_keyword_from_oracle_eternalize_mana_backward_compat() {
        let kw = parse_keyword_from_oracle("eternalize {3}{b}{b}").unwrap();
        assert!(matches!(kw, Keyword::Eternalize(EternalizeCost::Mana(_))));
    }

    /// CR 702.34a regression: Battle Screech's tap-creatures flashback shape
    /// must continue to parse to `FlashbackCost::NonMana(TapCreatures)`.
    #[test]
    fn parse_keyword_from_oracle_flashback_tap_creatures_unchanged() {
        let kw = parse_keyword_from_oracle(
            "flashback\u{2014}tap three untapped white creatures you control",
        )
        .unwrap();
        let Keyword::Flashback(FlashbackCost::NonMana(AbilityCost::TapCreatures {
            requirement,
            ..
        })) = kw
        else {
            panic!("expected NonMana(TapCreatures), got {:?}", kw);
        };
        assert_eq!(requirement.fixed_count(), Some(3));
    }

    /// CR 702.34a regression: simple `Flashback {cost}` (Cackling Counterpart,
    /// Roar of the Wurm) goes through the FromStr direct-parse branch and
    /// produces `FlashbackCost::Mana`.
    #[test]
    fn parse_keyword_from_oracle_flashback_simple_mana_unchanged() {
        let kw = parse_keyword_from_oracle("flashback {3}{g}").unwrap();
        let Keyword::Flashback(FlashbackCost::Mana(_)) = kw else {
            panic!("expected FlashbackCost::Mana, got {:?}", kw);
        };
    }

    /// CR 702.74a + CR 118.9: MH2 Incarnation evoke ("Evoke—Exile a [color]
    /// card from your hand.") parses into `EvokeCost::NonMana(Exile{..})`.
    /// Discriminator for #580: pre-fix `parse_keyword_from_oracle` returns
    /// `None` for this line (no `evoke—` arm); post-fix returns the typed
    /// non-mana cost so the runtime can surface the alt-cast prompt.
    #[test]
    fn parse_keyword_from_oracle_evoke_exile_white_card_from_hand() {
        use crate::types::ability::FilterProp;
        use crate::types::keywords::EvokeCost;
        use crate::types::mana::ManaColor;
        use crate::types::zones::Zone;

        let kw =
            parse_keyword_from_oracle("evoke\u{2014}exile a white card from your hand.").unwrap();
        let Keyword::Evoke(EvokeCost::NonMana(AbilityCost::Exile {
            count,
            zone,
            filter,
        })) = kw
        else {
            panic!("expected Evoke(NonMana(Exile)), got {:?}", kw);
        };
        assert_eq!(count, 1u32);
        assert_eq!(zone, Some(Zone::Hand));
        // The filter must carry a White color property — verifies Solitude's
        // "white card" Oracle subject mapped through to the typed filter.
        let filter = filter.expect("expected a card-color filter");
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {:?}", filter);
        };
        assert!(
            typed.properties.iter().any(|p| {
                matches!(
                    p,
                    FilterProp::HasColor {
                        color: ManaColor::White
                    }
                )
            }),
            "expected a HasColor(White) property on the exile filter, got {:?}",
            typed.properties,
        );
    }

    /// CR 702.74a regression: pure-mana Evoke ({2}{U} Mulldrifter-class) must
    /// continue to flow through the `FromStr` ingestion path and produce
    /// `EvokeCost::Mana`. Guarantees the EvokeCost lift is compatible with
    /// the legacy Lorwyn evoke serialization.
    #[test]
    fn from_str_evoke_pure_mana_unchanged() {
        use crate::types::keywords::EvokeCost;
        use std::str::FromStr;
        let kw = Keyword::from_str("Evoke:2U").unwrap();
        let Keyword::Evoke(EvokeCost::Mana(_)) = kw else {
            panic!("expected Evoke(Mana), got {:?}", kw);
        };
    }

    /// CR 702.120a: Escalate accepts any additional-cost shape, not just mana.
    #[test]
    fn parse_keyword_from_oracle_escalate_tap_creature_cost() {
        let kw = parse_keyword_from_oracle("escalate\u{2014}tap an untapped creature you control")
            .unwrap();
        let Keyword::Escalate(AbilityCost::TapCreatures { requirement, .. }) = kw else {
            panic!("expected Escalate(TapCreatures), got {:?}", kw);
        };
        assert_eq!(requirement.fixed_count(), Some(1));
    }

    /// CR 303.4a + CR 702.5: "Enchant creature, land, or planeswalker"
    /// (Imprisoned in the Moon) must extract a single `Keyword::Enchant` with a
    /// `TargetFilter::Or` union — not drop the keyword when later legs fail
    /// to match a keyword name.
    #[test]
    fn extract_enchant_multi_type_union() {
        let kws = extract_keyword_line(
            "Enchant creature, land, or planeswalker",
            &["enchant".to_string()],
        )
        .expect("multi-type enchant line should extract a keyword");
        assert_eq!(kws.len(), 1, "expected one enchant keyword");
        let Keyword::Enchant(TargetFilter::Or { filters }) = &kws[0] else {
            panic!("expected Keyword::Enchant(Or), got {:?}", kws[0]);
        };
        assert_eq!(filters.len(), 3);
        let got_types: Vec<_> = filters
            .iter()
            .map(|f| match f {
                TargetFilter::Typed(tf) => tf.type_filters.clone(),
                other => panic!("expected Typed leg, got {other:?}"),
            })
            .collect();
        assert_eq!(
            got_types,
            vec![
                vec![TypeFilter::Creature],
                vec![TypeFilter::Land],
                vec![TypeFilter::Planeswalker],
            ]
        );
    }

    /// Single-type "Enchant creature" must continue to flow through the legacy
    /// MTGJSON-parameterized path (FromStr on `Keyword::Enchant:creature`).
    /// The new multi-type helper only claims lists — single-type lines are
    /// skipped so Pacifism / Rancor / Enchanted-Evening class cards aren't
    /// affected.
    #[test]
    fn extract_enchant_single_type_not_claimed_by_multi_helper() {
        // Single-type enchant with no commas — helper must bail.
        assert!(super::try_parse_multi_type_enchant("Enchant creature").is_none());
        assert!(super::try_parse_multi_type_enchant("Enchant creature you control").is_none());
    }

    /// Controller suffix ("you control") must apply uniformly to every leg of
    /// a multi-type enchant list.
    #[test]
    fn extract_enchant_multi_type_controller_suffix() {
        let kw =
            super::try_parse_multi_type_enchant("Enchant creature or planeswalker you control")
                .expect("multi-type with controller suffix should parse");
        let Keyword::Enchant(TargetFilter::Or { filters }) = kw else {
            panic!("expected Or");
        };
        for leg in &filters {
            let TargetFilter::Typed(tf) = leg else {
                panic!("expected Typed");
            };
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
    }

    /// CR 205.4a + CR 702.5a: Supertype adjectives belong to the Enchant list
    /// leg they prefix; they must not be dropped or applied to sibling legs.
    #[test]
    fn extract_enchant_multi_type_preserves_per_leg_supertype() {
        use crate::types::card_type::Supertype;

        let kw = super::try_parse_multi_type_enchant("Enchant legendary creature or planeswalker")
            .expect("multi-type with qualified leg should parse");
        let Keyword::Enchant(TargetFilter::Or { filters }) = kw else {
            panic!("expected Or");
        };
        assert_eq!(filters.len(), 2);

        let TargetFilter::Typed(first) = &filters[0] else {
            panic!("expected first Typed leg");
        };
        assert_eq!(first.type_filters, vec![TypeFilter::Creature]);
        assert!(first.properties.contains(&FilterProp::HasSupertype {
            value: Supertype::Legendary
        }));

        let TargetFilter::Typed(second) = &filters[1] else {
            panic!("expected second Typed leg");
        };
        assert_eq!(second.type_filters, vec![TypeFilter::Planeswalker]);
        assert!(
            !second
                .properties
                .iter()
                .any(|prop| matches!(prop, FilterProp::HasSupertype { .. })),
            "supertype leaked to sibling leg: {:?}",
            second.properties
        );
    }

    /// CR 702.5a: "Enchant creature or Food" (Sugar Coat, BLB) — Food is an
    /// artifact subtype, not a core card type, so it requires explicit support
    /// in `parse_enchant_type_leg`. The result must be a two-leg `Or` filter
    /// covering both creatures and Food artifacts.
    #[test]
    fn extract_enchant_creature_or_food_subtype() {
        let kw = super::try_parse_multi_type_enchant("Enchant creature or Food")
            .expect("\"Enchant creature or Food\" should parse");
        let Keyword::Enchant(TargetFilter::Or { ref filters }) = kw else {
            panic!("expected Keyword::Enchant(Or), got {kw:?}");
        };
        assert_eq!(filters.len(), 2, "expected two legs");
        let types: Vec<_> = filters
            .iter()
            .map(|f| match f {
                TargetFilter::Typed(tf) => tf.type_filters.clone(),
                other => panic!("expected Typed leg, got {other:?}"),
            })
            .collect();
        assert_eq!(
            types,
            vec![
                vec![TypeFilter::Creature],
                vec![TypeFilter::Subtype("Food".to_string())],
            ]
        );
    }

    /// CR 702.5a: Artifact subtypes must parse as enchant target legs through
    /// the canonical subtype classifier, not a hand-maintained token subset.
    #[test]
    fn extract_enchant_artifact_subtypes() {
        for subtype in crate::types::card_type::ARTIFACT_SUBTYPES {
            let line = format!("Enchant creature or {subtype}");
            let kw = super::try_parse_multi_type_enchant(&line)
                .unwrap_or_else(|| panic!("\"{}\" should parse", line));
            let Keyword::Enchant(TargetFilter::Or { filters }) = kw else {
                panic!("expected Or for {subtype}");
            };
            assert_eq!(filters.len(), 2);
            let TargetFilter::Typed(tf) = &filters[1] else {
                panic!("expected Typed artifact subtype leg for {subtype}");
            };
            assert_eq!(
                tf.type_filters,
                vec![TypeFilter::Subtype((*subtype).to_string())]
            );
        }

        assert!(
            super::try_parse_multi_type_enchant("Enchant creature or Goblin").is_none(),
            "creature subtypes must not be accepted as artifact enchant target legs"
        );
    }

    // ── Cumulative upkeep display (CR 702.24a) ──

    #[test]
    fn cumulative_upkeep_keyword_display_mana() {
        // CR 702.24a: Mana-only cumulative upkeep renders its cost symbols
        // ("cumulative upkeep — {1}") so tooltips show the payment, not just
        // the bare keyword name.
        let kw = Keyword::CumulativeUpkeep(AbilityCost::Mana {
            cost: ManaCost::generic(1),
        });
        let s = keyword_display_name(&kw);
        // allow-noncombinator: substring assertion on display-formatter output, not parsing dispatch.
        assert!(s.contains("cumulative upkeep"), "{s}");
        // allow-noncombinator: substring assertion on display-formatter output, not parsing dispatch.
        assert!(s.contains("{1}"), "{s}");
    }

    #[test]
    fn cumulative_upkeep_keyword_display_pay_life() {
        // CR 702.24a + CR 119.4: Pay-life cumulative upkeep renders as
        // "cumulative upkeep — Pay N life".
        let kw = Keyword::CumulativeUpkeep(AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 2 },
        });
        let s = keyword_display_name(&kw);
        // allow-noncombinator: substring assertion on display-formatter output, not parsing dispatch.
        assert!(s.contains("cumulative upkeep"), "{s}");
        // allow-noncombinator: substring assertion on display-formatter output, not parsing dispatch.
        assert!(s.contains("Pay 2 life"), "{s}");
    }

    #[test]
    fn cumulative_upkeep_keyword_display_sacrifice() {
        // CR 702.24a: Sacrifice cumulative upkeep renders the subject from
        // the typed filter ("Sacrifice a land" for Polar Kraken).
        use crate::types::ability::{TypeFilter, TypedFilter};
        let kw = Keyword::CumulativeUpkeep(AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
            1,
        )));
        let s = keyword_display_name(&kw);
        // allow-noncombinator: substring assertion on display-formatter output, not parsing dispatch.
        assert!(s.contains("cumulative upkeep"), "{s}");
        // allow-noncombinator: substring assertion on display-formatter output, not parsing dispatch.
        assert!(s.contains("Sacrifice a land"), "{s}");
    }

    #[test]
    fn cumulative_upkeep_keyword_display_one_of() {
        // CR 702.24a: Disjunctive cumulative upkeep ("{G} or {W}", Elephant
        // Grass) joins each branch with " or ".
        let kw = Keyword::CumulativeUpkeep(AbilityCost::OneOf {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::Green],
                        generic: 0,
                    },
                },
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::White],
                        generic: 0,
                    },
                },
            ],
        });
        let s = keyword_display_name(&kw);
        // allow-noncombinator: substring assertion on display-formatter output, not parsing dispatch.
        assert!(s.contains("cumulative upkeep"), "{s}");
        // allow-noncombinator: substring assertion on display-formatter output, not parsing dispatch.
        assert!(s.contains(" or "), "{s}");
        // allow-noncombinator: substring assertion on display-formatter output, not parsing dispatch.
        assert!(s.contains("{G}"), "{s}");
        // allow-noncombinator: substring assertion on display-formatter output, not parsing dispatch.
        assert!(s.contains("{W}"), "{s}");
    }

    /// CR 702.173a: Freerunning recognized by is_keyword_cost_line.
    #[test]
    fn is_keyword_cost_line_freerunning() {
        assert!(is_keyword_cost_line("freerunning {3}{b}{b}"));
        assert!(is_keyword_cost_line("freerunning {1}{b}"));
    }

    /// CR 702.173a: Freerunning parsed from oracle text via parse_keyword_from_oracle.
    #[test]
    fn parse_keyword_from_oracle_freerunning() {
        use crate::types::keywords::Keyword;
        let kw = parse_keyword_from_oracle("freerunning {3}{b}{b}").unwrap();
        match kw {
            Keyword::Freerunning(_cost) => {
                // Successfully parsed — cost structure validated by ManaCost parser
            }
            other => panic!("expected Keyword::Freerunning, got {other:?}"),
        }
    }
}
