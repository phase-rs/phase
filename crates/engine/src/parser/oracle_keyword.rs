use std::borrow::Cow;

use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::space0;
use nom::combinator::{opt, value};
use nom::Parser;
use nom_language::error::VerboseError;

use super::oracle_cost::parse_oracle_cost;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::primitives::{scan_contains, split_once_on};
use super::oracle_quantity::parse_cda_quantity;
use super::oracle_target::parse_type_phrase;
use super::oracle_util::strip_reminder_text;
use crate::types::ability::{
    AbilityCost, AdditionalCost, ControllerRef, QuantityExpr, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::keywords::{BuybackCost, CyclingCost, FlashbackCost, Keyword, WardCost};

/// CR 702.16 + CR 702.11f: Expand compound "X from A and from B" keyword lines.
/// Handles both "protection from X and from Y" and "hexproof from X and from Y"
/// by splitting into individual keyword entries.
pub(crate) fn expand_protection_parts<'a>(parts: &[&'a str]) -> Vec<Cow<'a, str>> {
    // Fast path: skip allocation when no expansion is needed
    if !parts.iter().any(|p| {
        let l = p.to_ascii_lowercase();
        scan_contains(&l, "and from ")
            || tag::<_, _, VerboseError<&str>>("from ")
                .parse(l.as_str())
                .is_ok()
            || tag::<_, _, VerboseError<&str>>("and from ")
                .parse(l.as_str())
                .is_ok()
    }) {
        return parts.iter().map(|&p| Cow::Borrowed(p)).collect();
    }

    let mut expanded: Vec<Cow<'a, str>> = Vec::new();
    // Track which keyword prefix we're expanding (None, "protection", or "hexproof")
    let mut active_prefix: Option<&'static str> = None;

    for &part in parts {
        let lower = part.to_ascii_lowercase();

        // Check for "protection from X and from Y" or "hexproof from X and from Y"
        // (prefix_with_space, emit_prefix_no_space) — strip the prefix+space, emit prefix without space
        let prefix_match: Option<&str> = alt((
            value(
                "protection from",
                tag::<_, _, VerboseError<&str>>("protection from "),
            ),
            value("hexproof from", tag("hexproof from ")),
        ))
        .parse(lower.as_str())
        .ok()
        .map(|(_, v)| v);

        if let Some(prefix) = prefix_match {
            // Strip "protection from " or "hexproof from " (prefix + space)
            let after = &lower[prefix.len() + 1..]; // +1 for the trailing space
                                                    // CR 702.11f / CR 702.16: split on " and from "
            let mut remainder = after;
            while let Ok((_, (before, rest))) = split_once_on(remainder, " and from ") {
                expanded.push(Cow::Owned(format!("{prefix} {}", before.trim())));
                remainder = rest;
            }
            expanded.push(Cow::Owned(format!("{prefix} {}", remainder.trim())));
            active_prefix = Some(prefix);
        } else if let Some(pfx) = active_prefix {
            if let Ok((rest, _)) = alt((tag::<_, _, VerboseError<&str>>("and from "), tag("from ")))
                .parse(lower.as_str())
            {
                // ", and from Zombies" or ", from Werewolves" — continuation
                expanded.push(Cow::Owned(format!("{pfx} {}", rest.trim())));
            } else {
                active_prefix = None;
                expanded.push(Cow::Borrowed(part));
            }
        } else {
            expanded.push(Cow::Borrowed(part));
        }
    }
    expanded
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
                tag::<_, _, VerboseError<&str>>("multikicker "),
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
            repeatable: true,
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
        repeatable: false,
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
    if mtgjson_keyword_names.is_empty() {
        return None;
    }

    if mtgjson_keyword_names.iter().any(|n| n == "mobilize") {
        if let Some(kw) = parse_mobilize_keyword_line(line) {
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
                    alt((tag::<_, _, VerboseError<&str>>(" "), tag("\u{2014}")))
                        .parse(rest)
                        .is_ok()
                })
        });

        if mtgjson_match {
            any_mtgjson_match = true;

            // Exact name match means MTGJSON already has the parsed keyword — skip
            if mtgjson_keyword_names.contains(&lower) {
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

// CR 702.181a: "Mobilize N" creates N tapped and attacking Warrior tokens.
fn parse_mobilize_keyword_line(line: &str) -> Option<Keyword> {
    let lower = line.trim().trim_end_matches('.').to_ascii_lowercase();
    let (rest, _) = tag::<_, _, VerboseError<&str>>("mobilize ")
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

    let (rest, _) = tag::<_, _, VerboseError<&str>>("x").parse(rest).ok()?;
    let (rest, _) = space0::<_, VerboseError<&str>>.parse(rest).ok()?;
    let (quantity_text, _) = alt((
        tag::<_, _, VerboseError<&str>>(", where x is "),
        tag("where x is "),
    ))
    .parse(rest)
    .ok()?;
    parse_cda_quantity(quantity_text).map(Keyword::Mobilize)
}

/// Nom leaf combinator: match one of the six enchantable core types and yield
/// the corresponding `TypeFilter`. Driven by `value()` + `alt()` so additional
/// types slot in as one-line extensions.
fn parse_enchant_type_leg(input: &str) -> nom::IResult<&str, TypeFilter, VerboseError<&str>> {
    alt((
        value(TypeFilter::Creature, tag("creature")),
        value(TypeFilter::Land, tag("land")),
        value(TypeFilter::Artifact, tag("artifact")),
        value(TypeFilter::Enchantment, tag("enchantment")),
        value(TypeFilter::Planeswalker, tag("planeswalker")),
        value(TypeFilter::Permanent, tag("permanent")),
    ))
    .parse(input)
}

/// Nom combinator: separator between enchant list legs. Covers serial-comma
/// (", or "/", and "), bare comma (", "), and bare conjunction (" or "/" and ")
/// forms so "A, B, or C", "A, B, C", and "A or B" all compose uniformly.
fn parse_enchant_list_sep(input: &str) -> nom::IResult<&str, (), VerboseError<&str>> {
    value(
        (),
        alt((
            tag(", or "),
            tag(", and "),
            tag(", "),
            tag(" or "),
            tag(" and "),
        )),
    )
    .parse(input)
}

/// Nom combinator: parse a leg list with serial-comma or bare-conjunction
/// separators. Returns the list in source order.
fn parse_enchant_type_list(input: &str) -> nom::IResult<&str, Vec<TypeFilter>, VerboseError<&str>> {
    use nom::multi::many0;
    use nom::sequence::preceded;

    let (input, first) = parse_enchant_type_leg(input)?;
    let (input, rest) =
        many0(preceded(parse_enchant_list_sep, parse_enchant_type_leg)).parse(input)?;
    let mut legs = Vec::with_capacity(rest.len() + 1);
    legs.push(first);
    legs.extend(rest);
    Ok((input, legs))
}

/// Nom combinator: optional trailing controller clause. Ordered longest-first
/// so "an opponent controls" isn't shadowed by "opponent controls".
fn parse_enchant_controller_suffix(
    input: &str,
) -> nom::IResult<&str, ControllerRef, VerboseError<&str>> {
    alt((
        value(ControllerRef::You, tag(" you control")),
        value(ControllerRef::Opponent, tag(" an opponent controls")),
        value(ControllerRef::Opponent, tag(" opponent controls")),
    ))
    .parse(input)
}

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
    let (rest, _) = tag::<_, _, VerboseError<&str>>("enchant ")
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
        .map(|tf| {
            let mut f = TypedFilter::new(tf);
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
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("pay ").parse(lower) {
        if let Some(life_str) = rest.strip_suffix(" life") {
            if let Ok(n) = life_str.trim().parse::<i32>() {
                return Some(WardCost::PayLife(n));
            }
        }
    }

    // "discard a card" / "discard two cards" etc.
    if tag::<_, _, VerboseError<&str>>("discard")
        .parse(lower)
        .is_ok()
    {
        return Some(WardCost::DiscardCard);
    }

    // "sacrifice [N] permanent(s)/creature(s)/etc." — extract count and filter
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("sacrifice ").parse(lower) {
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
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("waterbend").parse(lower) {
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
    let clean = opt(take_until::<_, _, VerboseError<&str>>(" ("))
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
    let clean = opt(take_until::<_, _, VerboseError<&str>>(" ("))
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

fn parse_cycling_cost(cost_text: &str) -> Option<CyclingCost> {
    let trimmed = cost_text.trim().trim_end_matches('.').trim_end_matches(')');
    // Strip reminder text in parentheses: take everything before the first " (".
    let clean = opt(take_until::<_, _, VerboseError<&str>>(" ("))
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
            tag::<_, _, VerboseError<&str>>("partner\u{2014}character select"),
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

    // First try direct parse (handles simple keywords like "flying")
    let direct: Keyword = text.parse().unwrap();
    if !matches!(direct, Keyword::Unknown(_)) {
        return Some(direct);
    }

    // CR 702.29e: "basic landcycling {cost}" — multi-word typecycling variant.
    // Must be checked before the single-word typecycling guard below.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("basic landcycling").parse(text) {
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
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("cycling\u{2014}").parse(text) {
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
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("ward\u{2014}").parse(text) {
        return parse_ward_cost(rest);
    }

    // CR 702.34a: Flashback with em-dash cost — covers single non-mana costs
    // ("flashback—tap N untapped white creatures you control"), single mana costs
    // ("flashback—{2}{R}"), and compound costs ("flashback—{1}{U}, Pay 3 life").
    // `parse_flashback_cost` delegates to `parse_oracle_cost`, which composes
    // comma-separated parts into `AbilityCost::Composite` so the runtime split
    // (`split_flashback_cost` in casting.rs) can route mana sub-costs through the
    // mana-payment flow and residual sub-costs through `pay_additional_cost`.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("flashback\u{2014}").parse(text) {
        if let Some(fb_cost) = parse_flashback_cost(rest) {
            return Some(Keyword::Flashback(fb_cost));
        }
    }

    // CR 702.27a: Buyback with em-dash cost — non-mana costs like
    // "buyback—sacrifice a land" (Constant Mists). Pure-mana buyback
    // ("Buyback {3}") is handled by the direct `FromStr` path above.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("buyback\u{2014}").parse(text) {
        if let Some(bb_cost) = parse_buyback_cost(rest) {
            return Some(Keyword::Buyback(bb_cost));
        }
    }

    // CR 702.74a: "hideaway N" — parameterized keyword.
    // Delegates to nom combinator for number parsing.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("hideaway ").parse(text) {
        if let Ok((rem, n)) = nom_primitives::parse_number.parse(rest.trim()) {
            if rem.is_empty() {
                return Some(Keyword::Hideaway(n));
            }
        }
    }

    // CR 702.87a: "level up {cost}" — two-word keyword name.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("level up ").parse(text) {
        let cost_str = rest.trim();
        if !cost_str.is_empty() {
            let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_str);
            return Some(Keyword::LevelUp(cost));
        }
    }

    // CR 701.57a: "discover N"
    // Delegates to nom combinator for number parsing.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("discover ").parse(text) {
        if let Ok((rem, n)) = nom_primitives::parse_number.parse(rest.trim()) {
            if rem.is_empty() {
                return Some(Keyword::Discover(n));
            }
        }
    }

    // Gift keyword: "gift a card", "gift a treasure", "gift a food", "gift a tapped fish"
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("gift a ").parse(text) {
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
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("commander ninjutsu ").parse(text) {
        let cost_str = rest.trim();
        if !cost_str.is_empty() {
            let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_str);
            return Some(Keyword::CommanderNinjutsu(cost));
        }
    }

    // CR 702.62a: Suspend N—{cost} — "suspend N—{cost}" with em-dash or ascii dash.
    // Format: "suspend 4—{u}" or "suspend 1—{r}".
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("suspend ").parse(text) {
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
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("awaken ").parse(text) {
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

    // For parameterized keywords, find the first space to split name from parameter.
    // Oracle format: "protection from multicolored" → name="protection", rest="from multicolored"
    // Oracle format: "ward {2}" → name="ward", rest="{2}"
    let (_, (name, rest)) = split_once_on(text, " ").ok()?;
    let rest = rest.trim();

    // Strip "from" preposition (used by protection keywords)
    let param = tag::<_, _, VerboseError<&str>>("from ")
        .parse(rest)
        .map_or(rest, |(rem, _)| rem);

    let colon_form = format!("{name}:{param}");
    let parsed: Keyword = colon_form.parse().unwrap();
    if matches!(parsed, Keyword::Unknown(_)) {
        return None;
    }
    Some(parsed)
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
        Keyword::CumulativeUpkeep(ref cost) => {
            if cost.is_empty() {
                "cumulative upkeep".to_string()
            } else {
                format!("cumulative upkeep\u{2014}{cost}")
            }
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
        Keyword::Ripple => "ripple".to_string(),
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
        Keyword::Tribute(_) => "tribute".to_string(),
        Keyword::Afterlife(_) => "afterlife".to_string(),
        Keyword::Fading(_) => "fading".to_string(),
        Keyword::Vanishing(_) => "vanishing".to_string(),
        Keyword::Rampage(_) => "rampage".to_string(),
        Keyword::Absorb(_) => "absorb".to_string(),
        Keyword::Crew(_) => "crew".to_string(),
        Keyword::Poisonous(_) => "poisonous".to_string(),
        Keyword::Bloodthirst(_) => "bloodthirst".to_string(),
        Keyword::Amplify(_) => "amplify".to_string(),
        Keyword::Graft(_) => "graft".to_string(),
        Keyword::Devour(_) => "devour".to_string(),
        Keyword::Toxic(_) => "toxic".to_string(),
        Keyword::Saddle(_) => "saddle".to_string(),
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
        Keyword::Escape { .. } => "escape".to_string(),
        Keyword::Harmonize(_) => "harmonize".to_string(),
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
        Keyword::Prototype(_) => "prototype".to_string(),
        Keyword::Plot(_) => "plot".to_string(),
        Keyword::Craft(_) => "craft".to_string(),
        Keyword::Offspring(_) => "offspring".to_string(),
        Keyword::Impending(_) => "impending".to_string(),
        Keyword::LevelUp(_) => "level up".to_string(),
        Keyword::Hideaway(_) => "hideaway".to_string(),
        Keyword::Casualty(n) => format!("casualty {n}"),
        Keyword::Entwine(_) => "entwine".to_string(),
        Keyword::Affinity(_) => "affinity".to_string(),
        Keyword::Splice(_) => "splice".to_string(),
        Keyword::Bargain => "bargain".to_string(),
        Keyword::Sunburst => "sunburst".to_string(),
        Keyword::Champion(_) => "champion".to_string(),
        Keyword::Training => "training".to_string(),
        Keyword::Assist => "assist".to_string(),
        Keyword::Aftermath => "aftermath".to_string(),
        Keyword::JumpStart => "jump-start".to_string(),
        Keyword::Cipher => "cipher".to_string(),
        Keyword::Transmute(_) => "transmute".to_string(),
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
        Keyword::Unknown(s) => s.to_lowercase(),
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
        tag::<_, _, VerboseError<&str>>(*kw)
            .parse(lower)
            .is_ok_and(|(rest, _)| {
                rest.is_empty()
                    || rest.as_bytes().first() == Some(&b' ')
                    || rest.as_bytes().first() == Some(&b'\t')
                    || tag::<_, _, VerboseError<&str>>("\u{2014}")
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
    use crate::types::mana::ManaCost;

    #[test]
    fn parse_keyword_from_oracle_cascade() {
        // CR 702.85a: Cascade is a no-parameter keyword.
        let kw = parse_keyword_from_oracle("cascade").unwrap();
        assert_eq!(kw, Keyword::Cascade);
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
    fn extract_keyword_line_transmute() {
        // CR 702.52a: Transmute {cost} — single-keyword line with parameterized cost
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
    fn extract_keyword_line_splice() {
        // CR 702.47a: Splice onto [type] {cost}
        let mtgjson_kws = vec!["splice".to_string()];
        let result = extract_keyword_line("Splice onto Arcane {1}{W}", &mtgjson_kws);
        assert!(result.is_some(), "Should recognize as keyword line");
        let keywords = result.unwrap();
        assert_eq!(keywords.len(), 1);
        assert!(matches!(keywords[0], Keyword::Splice(_)));
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

    /// CR 702.34a regression: Battle Screech's tap-creatures flashback shape
    /// must continue to parse to `FlashbackCost::NonMana(TapCreatures)`.
    #[test]
    fn parse_keyword_from_oracle_flashback_tap_creatures_unchanged() {
        let kw = parse_keyword_from_oracle(
            "flashback\u{2014}tap three untapped white creatures you control",
        )
        .unwrap();
        let Keyword::Flashback(FlashbackCost::NonMana(AbilityCost::TapCreatures { count, .. })) =
            kw
        else {
            panic!("expected NonMana(TapCreatures), got {:?}", kw);
        };
        assert_eq!(count, 3);
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
}
