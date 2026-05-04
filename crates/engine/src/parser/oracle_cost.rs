use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::Parser;

use super::oracle_modal::split_short_label_prefix;
use super::oracle_nom::bridge::nom_on_lower;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::primitives::{scan_contains, split_once_on};
use super::oracle_quantity::parse_for_each_clause;
use super::oracle_target::{parse_target, parse_type_phrase};
use super::oracle_util::parse_mana_symbols;
use super::oracle_util::parse_number;
use super::oracle_util::TextPair;
use crate::types::ability::{
    AbilityCost, CostReduction, FilterProp, PlayerScope, QuantityExpr, QuantityRef, TargetFilter,
    TypedFilter,
};
use crate::types::zones::Zone;

/// Parse the cost portion before `:` in an Oracle activated ability.
/// Input: the raw text before the colon, e.g., "{T}", "{2}{W}, Sacrifice a creature", "Pay 3 life".
/// Returns an AbilityCost (possibly Composite for multi-part costs).
pub fn parse_oracle_cost(text: &str) -> AbilityCost {
    let text = text.trim();

    // Split on ", " for composite costs
    let parts: Vec<&str> = split_cost_parts(text);
    if parts.len() > 1 {
        let mut costs: Vec<AbilityCost> =
            parts.iter().map(|p| parse_single_cost(p.trim())).collect();
        // CR 601.2b: "Sacrifice A, B, and C" splits into ["Sacrifice A", "B", "C"].
        // Bare noun-phrase continuations after a verb-cost are additional instances
        // of that same cost. Applies to Sacrifice, Exile, and TapCreatures.
        fixup_bare_noun_continuations(&mut costs);
        return AbilityCost::Composite { costs };
    }

    parse_single_cost(text)
}

fn split_cost_parts(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut brace_depth = 0u32;
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < text.len() {
        let ch = text[i..].chars().next().expect("valid UTF-8");
        match ch {
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.saturating_sub(1),
            ',' if brace_depth == 0 => {
                let part = text[start..i].trim();
                if !part.is_empty() {
                    parts.push(part);
                }
                start = i + 1;
            }
            ' ' if brace_depth == 0 && bytes[i..].starts_with(b" and ") => {
                let part = text[start..i].trim();
                if !part.is_empty() {
                    parts.push(part);
                }
                start = i + " and ".len();
                i += " and ".len() - 1;
            }
            _ => {}
        }
        i += ch.len_utf8();
    }
    let last = text[start..].trim();
    if !last.is_empty() {
        parts.push(last);
    }
    parts
}

/// CR 601.2b: After comma/and-splitting, bare noun-phrase segments that follow
/// a verb-cost (Sacrifice, Exile, TapCreatures) are continuations of that verb,
/// not independent costs. E.g., "Sacrifice a green creature, a white creature,
/// and a blue creature" splits into three parts but only the first has the verb.
fn fixup_bare_noun_continuations(costs: &mut [AbilityCost]) {
    #[derive(Clone, Copy)]
    enum PrecedingVerb {
        Sacrifice,
        Exile { zone: Option<Zone> },
        TapCreatures,
    }

    let mut last_verb: Option<PrecedingVerb> = None;
    #[allow(clippy::needless_range_loop)]
    for i in 0..costs.len() {
        match &costs[i] {
            AbilityCost::Sacrifice { .. } => last_verb = Some(PrecedingVerb::Sacrifice),
            AbilityCost::Exile { zone, .. } => {
                last_verb = Some(PrecedingVerb::Exile { zone: *zone })
            }
            AbilityCost::TapCreatures { .. } => last_verb = Some(PrecedingVerb::TapCreatures),
            AbilityCost::Unimplemented { description } if last_verb.is_some() => {
                let lower = description.to_lowercase();
                let stripped = strip_article(description, &lower);
                if stripped.is_empty() {
                    continue;
                }
                let (filter, _) = parse_target(&format!("target {}", stripped));
                if matches!(filter, TargetFilter::Any) {
                    continue;
                }
                match last_verb.unwrap() {
                    PrecedingVerb::Sacrifice => {
                        costs[i] = AbilityCost::Sacrifice {
                            target: filter,
                            count: 1,
                        };
                    }
                    PrecedingVerb::Exile { zone } => {
                        costs[i] = AbilityCost::Exile {
                            count: 1,
                            zone,
                            filter: Some(filter),
                        };
                    }
                    PrecedingVerb::TapCreatures => {
                        costs[i] = AbilityCost::TapCreatures { count: 1, filter };
                    }
                }
            }
            _ => {
                last_verb = None;
            }
        }
    }
}

pub fn parse_single_cost(text: &str) -> AbilityCost {
    let text = text.trim();
    let lower = text.to_lowercase();

    // {T} — tap
    if lower == "{t}" {
        return AbilityCost::Tap;
    }

    // {Q} — untap
    if lower == "{q}" {
        return AbilityCost::Untap;
    }

    // Loyalty: [+N], [-N], [0]
    if text.starts_with('[') {
        if let Some(end) = text.find(']') {
            let inner = &text[1..end];
            // Handle minus sign variants: −, –, -
            let normalized = inner.replace(['−', '–'], "-");
            if let Ok(n) = normalized.parse::<i32>() {
                return AbilityCost::Loyalty { amount: n };
            }
            // +N
            if let Some(stripped) = normalized.strip_prefix('+') {
                if let Ok(n) = stripped.parse::<i32>() {
                    return AbilityCost::Loyalty { amount: n };
                }
            }
        }
    }

    // "Sacrifice ~" / "Sacrifice a/an/N {filter}"
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("sacrifice ")).parse(i))
    {
        let rest = rest.trim();
        let rest_lower = rest.to_lowercase();
        let is_self = nom_on_lower(rest, &rest_lower, |i| {
            value((), alt((tag("~"), tag("cardname"), tag("this ")))).parse(i)
        });
        if is_self.is_some() {
            return AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef,
                count: 1,
            };
        }
        // Try to extract a numeric count: "sacrifice two creatures", "sacrifice three lands"
        let (use_count, filter_text) =
            if let Some((count, rest_after_count)) = parse_number(&rest_lower) {
                if count > 1 {
                    // Parsed a count > 1 — use it and strip the count from the filter text
                    (count, rest_after_count.trim().to_string())
                } else {
                    // Count was 1 — treat as single sacrifice with article stripping
                    let stripped = strip_article(rest, &rest_lower);
                    (1, stripped.to_string())
                }
            } else {
                // No number found — strip article
                let stripped = strip_article(rest, &rest_lower);
                (1, stripped.to_string())
            };
        let (filter, _) = parse_target(&format!("target {}", filter_text));
        return AbilityCost::Sacrifice {
            target: filter,
            count: use_count,
        };
    }

    // "Pay N life" / "Pay life equal to <dynamic quantity>" / "N life"
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("pay ")).parse(i)) {
        let rest_lower = rest.to_lowercase();
        // CR 119.4 + CR 903.4 + CR 903.4f: "Pay life equal to the number of
        // colors in your commander(s)' color identity" — War Room. Parse via
        // dedicated combinator so the class covers both "commander's" and
        // "commanders'" apostrophe variants.
        if let Some(qty) = parse_life_equal_to_quantity(&rest_lower) {
            return AbilityCost::PayLife {
                amount: QuantityExpr::Ref { qty },
            };
        }
        if scan_contains(&rest_lower, "life") {
            if let Some((n, _)) = parse_number(&rest_lower) {
                return AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: n as i32 },
                };
            }
        }
        // Pay speed: "pay x speed" / "pay N speed"
        if let Some(speed_text) = rest_lower.strip_suffix(" speed") {
            if speed_text.trim() == "x" {
                return AbilityCost::PaySpeed {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                };
            }
            if let Some((amount, remainder)) = parse_number(speed_text) {
                if remainder.trim().is_empty() {
                    return AbilityCost::PaySpeed {
                        amount: QuantityExpr::Fixed {
                            value: amount as i32,
                        },
                    };
                }
            }
        }
    } else if lower.ends_with(" life") {
        if let Some((n, _)) = parse_number(&lower) {
            return AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: n as i32 },
            };
        }
    }

    // "Discard a card" / "Discard N cards"
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("discard ")).parse(i)) {
        let rest_lower = rest.to_lowercase();
        // CR 207.2c: "Discard this card" — Channel self-ref cost (ability word, not keyword).
        if rest_lower == "this card" {
            return AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                random: false,
                self_ref: true,
            };
        }
        if nom_on_lower(rest, &rest_lower, |i| value((), tag("a card")).parse(i)).is_some() {
            return AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                random: false,
                self_ref: false,
            };
        }
        if rest_lower == "your hand" {
            return AbilityCost::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                filter: None,
                random: false,
                self_ref: false,
            };
        }
        if let Some((n, _)) = parse_number(&rest_lower) {
            return AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: n as i32 },
                filter: None,
                random: false,
                self_ref: false,
            };
        }
        return AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            random: false,
            self_ref: false,
        };
    }

    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("exile ")).parse(i)) {
        let rest_lower = rest.to_lowercase();
        // CR 112.3: Self-exile costs — "Exile this card from your graveyard/hand"
        // or "Exile this artifact/creature/enchantment/land"
        if let Some(zone) = try_parse_self_exile_cost(&rest_lower) {
            return AbilityCost::Exile {
                count: 1,
                zone,
                filter: Some(TargetFilter::SelfRef),
            };
        }
        // "Exile the top card of your library" / "Exile the top N cards of your library"
        if let Some(count) = try_parse_exile_top_library(&rest_lower) {
            return AbilityCost::Exile {
                count,
                zone: Some(Zone::Library),
                filter: None,
            };
        }
        let count = parse_number(&rest_lower).map(|(n, _)| n).unwrap_or(1);
        let filter_start = parse_number(rest)
            .map(|(_, remaining)| remaining)
            .unwrap_or(rest);
        let filter_text = strip_count_article_prefix(filter_start);
        let (filter, remainder) = parse_type_phrase(filter_text);
        if remainder.trim().is_empty() {
            let zone = extract_filter_zone(&filter);
            return AbilityCost::Exile {
                count,
                zone,
                filter: Some(filter),
            };
        }
    }

    // "Blight N"
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("blight ")).parse(i)) {
        let rest_lower = rest.to_lowercase();
        let count = parse_number(&rest_lower).map(|(n, _)| n).unwrap_or(1);
        return AbilityCost::Blight { count };
    }

    // "Remove N {type} counter(s) from ~" or "Remove all {type} counters from ~"
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("remove ")).parse(i)) {
        let rest_lower = rest.to_lowercase();
        if scan_contains(&rest_lower, "counter") {
            // CR 122.1: "remove all" uses u32::MAX as sentinel, resolved at runtime.
            if let Some(((), all_rest)) =
                nom_on_lower(rest, &rest_lower, |i| value((), tag("all ")).parse(i))
            {
                let counter_type = all_rest
                    .to_lowercase()
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                return AbilityCost::RemoveCounter {
                    count: u32::MAX,
                    counter_type,
                    target: None,
                };
            }
            if let Some((count, after_count)) = parse_number(&rest_lower) {
                let counter_type = after_count
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string();
                return AbilityCost::RemoveCounter {
                    count,
                    counter_type,
                    target: None,
                };
            }
            // Fallback: "remove a/an {type} counter from ~"
            let words: Vec<&str> = text.split_whitespace().collect();
            if words.len() >= 4 {
                let counter_type = words[2].to_string();
                return AbilityCost::RemoveCounter {
                    count: 1,
                    counter_type,
                    target: None,
                };
            }
        }
    }

    // "Tap an untapped creature you control" / "Tap two untapped creatures you control"
    // / "Tap another untapped creature you control" / "Tap X untapped [type] you control"
    if let Some(((), tap_rest)) = nom_on_lower(text, &lower, |i| value((), tag("tap ")).parse(i)) {
        let tap_lower = tap_rest.to_lowercase();
        let (count, filter_text) = if let Some(((), r)) = nom_on_lower(tap_rest, &tap_lower, |i| {
            value(
                (),
                alt((tag("another untapped "), tag("an untapped "), tag("an "))),
            )
            .parse(i)
        }) {
            (1u32, r.to_lowercase())
        } else if let Some(((), r)) = nom_on_lower(tap_rest, &tap_lower, |i| {
            // "X untapped [type]" — variable count, use u32::MAX as sentinel.
            value((), alt((tag("x untapped "), tag("x other untapped ")))).parse(i)
        }) {
            (u32::MAX, r.to_lowercase())
        } else if let Some((n, r)) = super::oracle_util::parse_number(&tap_lower) {
            let r = nom_on_lower(
                &tap_rest[tap_lower.len() - r.len()..],
                r.trim_start(),
                |i| value((), tag("untapped ")).parse(i),
            )
            .map(|((), rest)| rest.to_lowercase())
            .unwrap_or_else(|| r.trim_start().to_string());
            (n, r)
        } else {
            (0, String::new())
        };

        if count > 0 {
            let target_text = format!("target {filter_text}");
            let (filter, remainder) = parse_target(&target_text);
            if remainder.trim().is_empty() {
                return AbilityCost::TapCreatures { count, filter };
            }
        }
    }

    // "Collect evidence N" — exile cards with total mana value N or more from graveyard (CR 701.59a)
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| {
        value((), tag("collect evidence ")).parse(i)
    }) {
        let rest_lower = rest.to_lowercase();
        if let Some((n, _)) = parse_number(rest_lower.trim()) {
            return AbilityCost::CollectEvidence { amount: n };
        }
    }

    // "Forage" — exile three cards from graveyard or sacrifice a Food (CR 701.61)
    if lower == "forage" {
        return AbilityCost::Exile {
            count: 3,
            zone: Some(Zone::Graveyard),
            filter: None,
        };
    }

    // "Pay {E}" / "Pay {E}{E}" / "Pay N {E}" — energy costs (CR 107.14)
    if let Some(energy) = try_parse_energy_cost(&lower) {
        return AbilityCost::PayEnergy { amount: energy };
    }

    // "Return a land you control to its owner's hand" — bounce cost
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("return ")).parse(i)) {
        let rest_lower = rest.to_lowercase();
        if let Some(filter_and_zone) = try_parse_return_to_hand_cost(&rest_lower) {
            return filter_and_zone;
        }
    }

    // "Reveal this card from your hand" — reveal self cost
    if nom_on_lower(text, &lower, |i| {
        value((), tag("reveal this card from your hand")).parse(i)
    })
    .is_some()
    {
        return AbilityCost::Reveal {
            count: 1,
            filter: None,
        };
    }

    // "Reveal a [Type] card from your hand" — reveal from hand with type filter.
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("reveal ")).parse(i)) {
        let rest_lower = rest.to_lowercase();
        if let Ok((_, (before, _))) = split_once_on(&rest_lower, "from your hand") {
            let filter_raw = before.trim();
            let filter_raw = strip_article(filter_raw, filter_raw);
            let filter_raw = filter_raw
                .strip_suffix(" card")
                .or_else(|| filter_raw.strip_suffix(" cards"))
                .unwrap_or(filter_raw)
                .trim();
            let (filter, _) = parse_target(&format!("target {filter_raw}"));
            return AbilityCost::Reveal {
                count: 1,
                filter: Some(filter),
            };
        }
    }

    // "Exert this creature" / "Exert ~" — exert cost (CR 701.43)
    if nom_on_lower(text, &lower, |i| {
        value((), alt((tag("exert this "), tag("exert ~")))).parse(i)
    })
    .is_some()
    {
        return AbilityCost::Exert;
    }

    // "Mill a card" / "Mill N cards" — mill cost
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("mill ")).parse(i)) {
        let rest_lower = rest.to_lowercase();
        if rest_lower == "a card" {
            return AbilityCost::Mill { count: 1 };
        }
        if let Some((n, _)) = parse_number(&rest_lower) {
            return AbilityCost::Mill { count: n };
        }
    }

    // "Pay {N}{W}" — mana cost with "pay" prefix
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("pay ")).parse(i)) {
        let rest_lower = rest.to_lowercase();
        let mana_text = rest_lower.trim();
        if mana_text.starts_with('{') {
            if let Some((cost, mana_rest)) = parse_mana_symbols(mana_text) {
                if mana_rest.trim().is_empty() {
                    return AbilityCost::Mana { cost };
                }
            }
        }
    }

    // Waterbend {N}: tap-to-pay cost for Avatar waterbending abilities.
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("waterbend ")).parse(i))
    {
        let rest_lower = rest.to_lowercase();
        if let Some((mana_cost, _)) = parse_mana_symbols(rest_lower.trim()) {
            return AbilityCost::Waterbend { cost: mana_cost };
        }
    }

    // Vehicle tier costs: "12+ | {3}{W}" — skip the tier prefix and parse the actual cost
    if scan_contains(&lower, "| ") {
        let tp = TextPair::new(text, &lower);
        if let Some((before, after)) = tp.split_around(" | ") {
            let prefix = before.lower.trim();
            if let Some(num_part) = prefix.strip_suffix('+') {
                if !num_part.is_empty() && num_part.chars().all(|c| c.is_ascii_digit()) {
                    let actual_cost = after.original.trim();
                    if !actual_cost.is_empty() {
                        let cost = parse_single_cost(actual_cost);
                        if !matches!(cost, AbilityCost::Unimplemented { .. }) {
                            return cost;
                        }
                    }
                }
            }
        }
    }

    // Ability-word prefixed costs: "Cohort — {T}", "Boast — {1}", "Metalcraft — {T}",
    // "Exhaust — {N}", "Max speed — {N}"
    if let Some(cost) = try_strip_ability_word_cost(text) {
        return cost;
    }

    // Mana cost: {N}{W}{U} etc. — delegate to nom combinator for uppercase input.
    if lower.starts_with('{') {
        let upper = text.to_ascii_uppercase();
        if let Ok((rest, cost)) = nom_primitives::parse_mana_cost.parse(&upper) {
            if rest.trim().is_empty() {
                return AbilityCost::Mana { cost };
            }
        }
    }

    // CR 118.3: Fallback — try parsing the cost text as an effect. Many
    // activation costs are structurally identical to effects ("Put a -1/-1
    // counter on ~", "Return a land you control to its owner's hand") and
    // the effect parser already handles them.
    let def = super::oracle_effect::parse_effect_chain(
        text,
        crate::types::ability::AbilityKind::Activated,
    );
    if !matches!(
        def.effect.as_ref(),
        crate::types::ability::Effect::Unimplemented { .. }
    ) {
        return AbilityCost::EffectCost { effect: def.effect };
    }

    AbilityCost::Unimplemented {
        description: text.to_string(),
    }
}

/// CR 601.2f: Parse "this ability/spell costs {N} less to activate/cast for each [condition]".
/// Returns `None` for unrecognized patterns.
pub(crate) fn try_parse_cost_reduction(text: &str) -> Option<CostReduction> {
    let lower = text.to_lowercase();
    let ((), rest) = nom_on_lower(text, &lower, |i| {
        value(
            (),
            alt((tag("this ability costs "), tag("this spell costs "))),
        )
        .parse(i)
    })?;

    // Extract the {N} mana amount
    let rest_lower = rest.to_lowercase();
    let (mana_cost, after_mana) = parse_mana_symbols(&rest_lower)?;
    let amount_per = match mana_cost {
        crate::types::mana::ManaCost::Cost { generic, shards } if shards.is_empty() => generic,
        _ => return None, // Only generic mana reduction supported
    };

    // Strip " less to activate for each " or " less to cast for each "
    let after_mana = after_mana.trim_start();
    let ((), after_less) = nom_on_lower(after_mana, after_mana, |i| {
        value(
            (),
            alt((
                tag("less to activate for each "),
                tag("less to cast for each "),
            )),
        )
        .parse(i)
    })?;

    // Try parse_for_each_clause first (handles counters, player counts, etc.),
    // then fall back to parse_type_phrase for standard object count patterns.
    if let Some(qty) = parse_for_each_clause(after_less) {
        return Some(CostReduction {
            amount_per,
            count: QuantityExpr::Ref { qty },
        });
    }

    // Parse the condition as a type phrase
    let (filter, remainder) = parse_type_phrase(after_less);
    if !remainder.trim().is_empty() {
        return None;
    }

    Some(CostReduction {
        amount_per,
        count: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        },
    })
}

fn strip_count_article_prefix(text: &str) -> &str {
    let trimmed = text.trim();
    nom_on_lower(
        trimmed,
        &trimmed.to_lowercase(),
        nom_primitives::parse_article,
    )
    .map(|((), rest)| rest)
    .unwrap_or(trimmed)
}

/// Strip leading "a " / "an " article from mixed-case text, using lowercase for matching.
fn strip_article<'a>(text: &'a str, lower: &str) -> &'a str {
    nom_on_lower(text, lower, nom_primitives::parse_article)
        .map(|((), rest)| rest)
        .unwrap_or(text)
}

/// CR 112.3: Parse self-exile cost patterns like "this card from your graveyard",
/// "this artifact", "this creature from your hand". Returns the zone (if specified).
/// Also handles `~` (normalized card name) variants.
fn try_parse_self_exile_cost(rest: &str) -> Option<Option<Zone>> {
    let rest = rest.trim().trim_end_matches('.');
    let is_self = nom_on_lower(rest, rest, |i| {
        value((), alt((tag("this "), tag("~ ")))).parse(i)
    })
    .is_some();
    // Bare "~" means exile self (normalized card name)
    if rest == "~" {
        return Some(None);
    }
    // "<self> from your <zone>" / "<self> in your <zone>" — delegate the trailing zone
    // phrase to the shared scanner so hand/graveyard/library/exile are all supported
    // via one combinator with word-boundary safety (rejects "from your graveyardkeeper").
    if is_self {
        if let Some((zone, _ctrl, _props)) = super::oracle_target::scan_zone_phrase(rest) {
            return Some(Some(zone));
        }
    }
    // "this artifact" / "this creature" / "this enchantment" / "this land" / "this permanent"
    // / "this card" / "this vehicle" (self-exile from battlefield)
    if let Some(((), after_this)) = nom_on_lower(rest, rest, |i| value((), tag("this ")).parse(i)) {
        if matches!(
            after_this,
            "artifact" | "creature" | "enchantment" | "land" | "permanent" | "card" | "vehicle"
        ) {
            return Some(None); // battlefield (implicit)
        }
    }
    None
}

/// Parse "the top card of your library" / "the top N cards of your library".
fn try_parse_exile_top_library(rest: &str) -> Option<u32> {
    let ((), after_top) = nom_on_lower(rest, rest, |i| value((), tag("the top ")).parse(i))?;
    let after_top = after_top.trim();
    if nom_on_lower(after_top, after_top, |i| {
        value((), tag("card of your library")).parse(i)
    })
    .is_some()
    {
        return Some(1);
    }
    if let Some((n, remainder)) = parse_number(after_top) {
        if nom_on_lower(remainder.trim(), remainder.trim(), |i| {
            value((), tag("cards of your library")).parse(i)
        })
        .is_some()
        {
            return Some(n);
        }
    }
    None
}

/// CR 107.9: Parse energy costs like "{E}", "{E}{E}", "pay N {e}", "pay eight {e}".
fn try_parse_energy_cost(lower: &str) -> Option<u32> {
    let text = nom_on_lower(lower, lower, |i| value((), tag("pay ")).parse(i))
        .map(|((), rest)| rest)
        .unwrap_or(lower)
        .trim();
    // Count {e} symbols
    if scan_contains(text, "{e}") {
        let count = text.matches("{e}").count() as u32;
        // Verify the text is ONLY {E} symbols (no other text)
        let cleaned = text.replace("{e}", "").replace(' ', "");
        if cleaned.is_empty() {
            return Some(count);
        }
    }
    // "pay N {e}" / "pay eight {e}" / "pay six {e}"
    if text.ends_with("{e}") {
        let prefix = text.trim_end_matches("{e}").trim();
        if let Some((n, _)) = parse_number(prefix) {
            return Some(n);
        }
    }
    None
}

/// Parse "return a land you control to its owner's hand" style bounce costs.
fn try_parse_return_to_hand_cost(rest_lower: &str) -> Option<AbilityCost> {
    // Must end with "to its owner's hand" or "to your hand"
    if !scan_contains(rest_lower, "to its owner's hand")
        && !scan_contains(rest_lower, "to your hand")
    {
        return None;
    }
    // Strip the destination
    let filter_text = split_once_on(rest_lower, " to its owner's hand")
        .map(|(_, (before, _))| before)
        .or_else(|_| split_once_on(rest_lower, " to your hand").map(|(_, (before, _))| before))
        .ok()?;
    // Strip article using nom
    let filter_text = nom_on_lower(filter_text, filter_text, nom_primitives::parse_article)
        .map(|((), rest)| rest)
        .unwrap_or(filter_text);
    // "~" is the self-reference placeholder — filter: None means "this permanent"
    if filter_text == "~" {
        return Some(AbilityCost::ReturnToHand {
            count: 1,
            filter: None,
        });
    }
    let target_text = format!("target {filter_text}");
    let (filter, rem) = parse_target(&target_text);
    let filter = if rem.trim().is_empty() {
        filter
    } else {
        let (filter, _) = parse_type_phrase(filter_text);
        filter
    };
    Some(AbilityCost::ReturnToHand {
        count: 1,
        filter: Some(filter),
    })
}

/// Strip ability-word cost prefixes like "Cohort — {T}", "Boast — {1}",
/// "Exhaust — {3}", "Renew — {1}{G}", "{TK}{TK} — {T}".
/// These are ability words or ticket costs that precede a standard cost.
fn try_strip_ability_word_cost(text: &str) -> Option<AbilityCost> {
    let lower = text.to_lowercase();
    // Use split_short_label_prefix to generically strip ability word prefixes
    // (e.g. "Cohort — {T}", "Boast — {1}", "Exhaust — {3}") without
    // maintaining a hardcoded ability word list.
    if let Some((_label, rest)) = split_short_label_prefix(text, 4) {
        let cost = parse_single_cost(rest.trim());
        if !matches!(cost, AbilityCost::Unimplemented { .. }) {
            return Some(cost);
        }
    }
    // Ticket costs: "{TK}{TK} — {T}", "{TK}{TK}{TK} — {3}"
    if nom_on_lower(text, &lower, |i| value((), tag("{tk}")).parse(i)).is_some() {
        // Try splitting on em-dash, en-dash, or hyphen separator
        let dash_split = split_once_on(text, " \u{2014} ")
            .or_else(|_| split_once_on(text, " \u{2013} "))
            .or_else(|_| split_once_on(text, " - "));
        if let Ok((_, (_, rest))) = dash_split {
            let cost = parse_single_cost(rest.trim());
            if !matches!(cost, AbilityCost::Unimplemented { .. }) {
                return Some(cost);
            }
        }
    }
    None
}

fn extract_filter_zone(filter: &TargetFilter) -> Option<Zone> {
    match filter {
        TargetFilter::Typed(TypedFilter { properties, .. }) => properties.iter().find_map(|prop| {
            if let FilterProp::InZone { zone } = prop {
                Some(*zone)
            } else {
                None
            }
        }),
        _ => None,
    }
}

/// CR 119.4 + CR 903.4: Parse "life equal to <dynamic quantity>" after the
/// leading "pay " token has been consumed. Returns the resolved
/// `QuantityRef`, or `None` if the tail doesn't match a supported dynamic
/// life-amount phrase.
///
/// Composed with nom combinators as prefix + possessive + suffix so the class
/// covers both singular "commander's" and plural "commanders'" apostrophe
/// placements. Additional dynamic life-cost quantities slot in by extending
/// the outer `alt()`.
fn parse_life_equal_to_quantity(rest_lower: &str) -> Option<QuantityRef> {
    let (_, qty) = parse_life_equal_to_quantity_nom(rest_lower).ok()?;
    Some(qty)
}

fn parse_life_equal_to_quantity_nom(
    i: &str,
) -> super::oracle_nom::error::OracleResult<'_, QuantityRef> {
    let (i, _) = value((), tag("life equal to the number of colors in ")).parse(i)?;
    let (i, _) = value(
        (),
        alt((
            tag("your commander's "),
            tag("your commanders' "),
            tag("your commanders "),
        )),
    )
    .parse(i)?;
    let (i, _) = tag("color identity").parse(i)?;
    Ok((i, QuantityRef::ColorsInCommandersColorIdentity))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ControllerRef, TypeFilter, TypedFilter};
    use crate::types::mana::{ManaCost, ManaCostShard};

    #[test]
    fn cost_tap() {
        assert_eq!(parse_oracle_cost("{T}"), AbilityCost::Tap);
    }

    #[test]
    fn cost_untap() {
        assert_eq!(parse_oracle_cost("{Q}"), AbilityCost::Untap);
    }

    #[test]
    fn cost_mana() {
        assert_eq!(
            parse_oracle_cost("{2}{W}"),
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 2,
                    shards: vec![ManaCostShard::White]
                }
            }
        );
    }

    #[test]
    fn cost_tap_and_mana_composite() {
        match parse_oracle_cost("{T}, {1}") {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 2);
                assert_eq!(costs[0], AbilityCost::Tap);
            }
            other => panic!("Expected Composite, got {:?}", other),
        }
    }

    #[test]
    fn cost_zero_mana() {
        assert_eq!(
            parse_oracle_cost("{0}"),
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 0,
                    shards: vec![],
                }
            }
        );
    }

    #[test]
    fn cost_sacrifice_self() {
        assert_eq!(
            parse_oracle_cost("Sacrifice ~"),
            AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef,
                count: 1,
            }
        );
    }

    #[test]
    fn cost_return_self_to_hand() {
        assert_eq!(
            parse_oracle_cost("Return ~ to its owner's hand"),
            AbilityCost::ReturnToHand {
                count: 1,
                filter: None,
            }
        );
    }

    #[test]
    fn cost_sacrifice_creature() {
        match parse_oracle_cost("Sacrifice a creature") {
            AbilityCost::Sacrifice { target, .. } => {
                assert!(matches!(
                    target,
                    TargetFilter::Typed(ref tf) if matches!(tf.get_primary_type(), Some(TypeFilter::Creature))
                ));
            }
            other => panic!("Expected Sacrifice, got {:?}", other),
        }
    }

    #[test]
    fn cost_tap_untapped_creature_you_control() {
        assert_eq!(
            parse_oracle_cost("Tap an untapped creature you control"),
            AbilityCost::TapCreatures {
                count: 1,
                filter: TargetFilter::Typed(
                    TypedFilter::creature().controller(crate::types::ability::ControllerRef::You)
                ),
            }
        );
    }

    #[test]
    fn cost_pay_life() {
        assert_eq!(
            parse_oracle_cost("Pay 3 life"),
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 3 }
            }
        );
    }

    #[test]
    fn cost_pay_life_equal_to_commanders_color_identity() {
        // CR 903.4: War Room — "Pay life equal to the number of colors in your
        // commanders' color identity".
        assert_eq!(
            parse_oracle_cost(
                "Pay life equal to the number of colors in your commanders' color identity"
            ),
            AbilityCost::PayLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::ColorsInCommandersColorIdentity
                }
            }
        );
        // Singular possessive variant.
        assert_eq!(
            parse_oracle_cost(
                "Pay life equal to the number of colors in your commander's color identity"
            ),
            AbilityCost::PayLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::ColorsInCommandersColorIdentity
                }
            }
        );
    }

    #[test]
    fn cost_loyalty_positive() {
        assert_eq!(
            parse_oracle_cost("[+2]"),
            AbilityCost::Loyalty { amount: 2 }
        );
    }

    #[test]
    fn cost_loyalty_negative() {
        assert_eq!(
            parse_oracle_cost("[−3]"),
            AbilityCost::Loyalty { amount: -3 }
        );
    }

    #[test]
    fn cost_loyalty_zero() {
        assert_eq!(parse_oracle_cost("[0]"), AbilityCost::Loyalty { amount: 0 });
    }

    #[test]
    fn cost_discard() {
        assert_eq!(
            parse_oracle_cost("Discard a card"),
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                random: false,
                self_ref: false,
            }
        );
    }

    #[test]
    fn cost_discard_this_card() {
        assert_eq!(
            parse_oracle_cost("Discard this card"),
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                random: false,
                self_ref: true,
            }
        );
    }

    #[test]
    fn cost_discard_your_hand() {
        assert_eq!(
            parse_oracle_cost("Discard your hand"),
            AbilityCost::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller
                    },
                },
                filter: None,
                random: false,
                self_ref: false,
            }
        );
    }

    #[test]
    fn cost_composite_tap_mana_sacrifice() {
        match parse_oracle_cost("{T}, {2}{B}, Sacrifice a creature") {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 3);
                assert_eq!(costs[0], AbilityCost::Tap);
                assert!(matches!(costs[2], AbilityCost::Sacrifice { .. }));
            }
            other => panic!("Expected Composite, got {:?}", other),
        }
    }

    #[test]
    fn cost_composite_pay_life_and_exile_card() {
        match parse_oracle_cost("Pay 1 life and exile a blue card from your hand") {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 2);
                assert_eq!(
                    costs[0],
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 }
                    }
                );
                assert!(matches!(costs[1], AbilityCost::Exile { .. }));
            }
            other => panic!("Expected Composite, got {:?}", other),
        }
    }

    #[test]
    fn cost_exile_colored_card_from_hand() {
        match parse_oracle_cost("Exile a blue card from your hand") {
            AbilityCost::Exile {
                count,
                zone,
                filter,
            } => {
                assert_eq!(count, 1);
                assert_eq!(zone, Some(crate::types::zones::Zone::Hand));
                assert!(matches!(
                    filter,
                    Some(TargetFilter::Typed(TypedFilter {
                        controller: Some(crate::types::ability::ControllerRef::You),
                        ..
                    }))
                ));
            }
            other => panic!("Expected Exile, got {:?}", other),
        }
    }

    #[test]
    fn cost_blight() {
        assert_eq!(
            parse_oracle_cost("Blight 2"),
            AbilityCost::Blight { count: 2 }
        );
    }

    #[test]
    fn cost_blight_one() {
        assert_eq!(
            parse_oracle_cost("Blight 1"),
            AbilityCost::Blight { count: 1 }
        );
    }

    #[test]
    fn cost_reduction_legendary_creature_you_control() {
        let result = try_parse_cost_reduction(
            "this ability costs {1} less to activate for each legendary creature you control",
        );
        let reduction = result.expect("should parse cost reduction");
        assert_eq!(reduction.amount_per, 1);
        match &reduction.count {
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            } => {
                assert!(matches!(
                    filter,
                    TargetFilter::Typed(TypedFilter {
                        controller: Some(crate::types::ability::ControllerRef::You),
                        ..
                    })
                ));
            }
            other => panic!("Expected ObjectCount, got {:?}", other),
        }
    }

    #[test]
    fn cost_reduction_other_equipment_you_control() {
        let result = try_parse_cost_reduction(
            "this ability costs {1} less to activate for each other equipment you control",
        );
        let reduction = result.expect("should parse cost reduction");
        assert_eq!(reduction.amount_per, 1);
        match &reduction.count {
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            } => match filter {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                    assert!(
                        tf.type_filters.iter().any(
                            |filter| matches!(filter, TypeFilter::Subtype(name) if name == "Equipment")
                        ),
                        "expected Equipment subtype, got {:?}",
                        tf.type_filters
                    );
                    assert!(
                        tf.properties
                            .iter()
                            .any(|property| matches!(property, FilterProp::Another)),
                        "expected Another property, got {:?}",
                        tf.properties
                    );
                }
                other => panic!("Expected typed ObjectCount filter, got {:?}", other),
            },
            other => panic!("Expected ObjectCount, got {:?}", other),
        }
    }

    #[test]
    fn cost_reduction_spell_variant() {
        let result = try_parse_cost_reduction(
            "this spell costs {1} less to cast for each creature you control",
        );
        assert!(result.is_some(), "should parse spell cost reduction");
    }

    #[test]
    fn cost_reduction_unrecognized_returns_none() {
        assert!(try_parse_cost_reduction("something else entirely").is_none());
    }

    #[test]
    fn cost_exile_self_from_graveyard() {
        assert_eq!(
            parse_oracle_cost("Exile this card from your graveyard"),
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Graveyard),
                filter: Some(TargetFilter::SelfRef),
            }
        );
    }

    #[test]
    fn cost_exile_self_artifact() {
        assert_eq!(
            parse_oracle_cost("Exile this artifact"),
            AbilityCost::Exile {
                count: 1,
                zone: None,
                filter: Some(TargetFilter::SelfRef),
            }
        );
    }

    #[test]
    fn cost_exile_self_creature() {
        assert_eq!(
            parse_oracle_cost("Exile this creature"),
            AbilityCost::Exile {
                count: 1,
                zone: None,
                filter: Some(TargetFilter::SelfRef),
            }
        );
    }

    #[test]
    fn cost_exile_self_from_hand() {
        assert_eq!(
            parse_oracle_cost("Exile this card from your hand"),
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Hand),
                filter: Some(TargetFilter::SelfRef),
            }
        );
    }

    #[test]
    fn cost_exile_top_of_library() {
        assert_eq!(
            parse_oracle_cost("Exile the top card of your library"),
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Library),
                filter: None,
            }
        );
    }

    #[test]
    fn cost_collect_evidence() {
        assert_eq!(
            parse_oracle_cost("Collect evidence 8"),
            AbilityCost::CollectEvidence { amount: 8 }
        );
    }

    #[test]
    fn cost_pay_energy_single() {
        assert_eq!(
            parse_oracle_cost("Pay {E}"),
            AbilityCost::PayEnergy { amount: 1 }
        );
    }

    #[test]
    fn cost_pay_energy_triple() {
        assert_eq!(
            parse_oracle_cost("Pay {E}{E}{E}"),
            AbilityCost::PayEnergy { amount: 3 }
        );
    }

    #[test]
    fn cost_return_land_to_hand() {
        match parse_oracle_cost("Return a land you control to its owner's hand") {
            AbilityCost::ReturnToHand { count, filter } => {
                assert_eq!(count, 1);
                assert!(filter.is_some());
            }
            other => panic!("Expected ReturnToHand, got {:?}", other),
        }
    }

    #[test]
    fn cost_return_forest_to_hand() {
        match parse_oracle_cost("Return a Forest you control to its owner's hand") {
            AbilityCost::ReturnToHand {
                count,
                filter: Some(TargetFilter::Typed(filter)),
            } => {
                assert_eq!(count, 1);
                assert_eq!(filter.get_subtype(), Some("Forest"));
                // "you control" must be captured — parse_type_phrase delegates to
                // parse_controller_suffix which handles this suffix.
                assert_eq!(filter.controller, Some(ControllerRef::You));
            }
            other => panic!("Expected ReturnToHand Forest filter, got {:?}", other),
        }
    }

    #[test]
    fn cost_reveal_self_from_hand() {
        assert_eq!(
            parse_oracle_cost("Reveal this card from your hand"),
            AbilityCost::Reveal {
                count: 1,
                filter: None
            }
        );
    }

    #[test]
    fn cost_exert_creature() {
        assert_eq!(parse_oracle_cost("Exert this creature"), AbilityCost::Exert);
    }

    #[test]
    fn cost_mill_a_card() {
        assert_eq!(
            parse_oracle_cost("Mill a card"),
            AbilityCost::Mill { count: 1 }
        );
    }

    #[test]
    fn cost_cohort_tap_prefix() {
        assert_eq!(parse_oracle_cost("Cohort — {T}"), AbilityCost::Tap,);
    }

    #[test]
    fn cost_boast_mana_prefix() {
        match parse_oracle_cost("Boast — {1}{W}") {
            AbilityCost::Mana { cost } => {
                assert_eq!(
                    cost,
                    ManaCost::Cost {
                        generic: 1,
                        shards: vec![ManaCostShard::White]
                    }
                );
            }
            other => panic!("Expected Mana, got {:?}", other),
        }
    }

    #[test]
    fn cost_composite_tap_blight() {
        match parse_oracle_cost("{1}{R}, {T}, Blight 1") {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 3);
                assert!(matches!(costs[0], AbilityCost::Mana { .. }));
                assert_eq!(costs[1], AbilityCost::Tap);
                assert_eq!(costs[2], AbilityCost::Blight { count: 1 });
            }
            other => panic!("Expected Composite, got {:?}", other),
        }
    }
}
