use super::oracle_target::{parse_target, parse_type_phrase};
use super::oracle_util::parse_mana_symbols;
use super::oracle_util::parse_number;
use super::oracle_util::TextPair;
use crate::types::ability::{
    AbilityCost, CostReduction, FilterProp, QuantityExpr, QuantityRef, TargetFilter, TypedFilter,
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
        let costs: Vec<AbilityCost> = parts.iter().map(|p| parse_single_cost(p.trim())).collect();
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

    // "Sacrifice ~" / "Sacrifice a/an {filter}"
    if lower.starts_with("sacrifice ") {
        let rest = &text[10..].trim();
        let rest_lower = rest.to_lowercase();
        if rest_lower.starts_with('~')
            || rest_lower.starts_with("cardname")
            || rest_lower.starts_with("this ")
        {
            return AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef,
            };
        }
        // "Sacrifice a {filter}"
        let rest_no_article = if rest_lower.starts_with("a ") {
            &rest[2..]
        } else if rest_lower.starts_with("an ") {
            &rest[3..]
        } else {
            rest
        };
        let (filter, _) = parse_target(&format!("target {}", rest_no_article));
        return AbilityCost::Sacrifice { target: filter };
    }

    // "Pay N life" / "N life"
    if (lower.starts_with("pay ") || lower.ends_with(" life")) && lower.contains("life") {
        let rest = lower.strip_prefix("pay ").unwrap_or(&lower);
        if let Some(n) = rest
            .split_whitespace()
            .next()
            .and_then(|w| w.parse::<u32>().ok())
        {
            return AbilityCost::PayLife { amount: n };
        }
    }

    // "Discard a card" / "Discard N cards"
    if let Some(rest) = lower.strip_prefix("discard ") {
        // CR 207.2c: "Discard this card" — Channel self-ref cost (ability word, not keyword).
        if rest == "this card" {
            return AbilityCost::Discard {
                count: 1,
                filter: None,
                random: false,
                self_ref: true,
            };
        }
        if rest.starts_with("a card") {
            return AbilityCost::Discard {
                count: 1,
                filter: None,
                random: false,
                self_ref: false,
            };
        }
        if let Ok(n) = rest.split_whitespace().next().unwrap_or("").parse::<u32>() {
            return AbilityCost::Discard {
                count: n,
                filter: None,
                random: false,
                self_ref: false,
            };
        }
        return AbilityCost::Discard {
            count: 1,
            filter: None,
            random: false,
            self_ref: false,
        };
    }

    if let Some(rest) = lower.strip_prefix("exile ") {
        // CR 112.3: Self-exile costs — "Exile this card from your graveyard/hand"
        // or "Exile this artifact/creature/enchantment/land"
        if let Some(zone) = try_parse_self_exile_cost(rest) {
            return AbilityCost::Exile {
                count: 1,
                zone,
                filter: Some(TargetFilter::SelfRef),
            };
        }
        // "Exile the top card of your library" / "Exile the top N cards of your library"
        if let Some(count) = try_parse_exile_top_library(rest) {
            return AbilityCost::Exile {
                count,
                zone: Some(Zone::Library),
                filter: None,
            };
        }
        let count = parse_number(rest).map(|(n, _)| n).unwrap_or(1);
        let filter_start = parse_number(&text[6..])
            .map(|(_, remaining)| remaining)
            .unwrap_or(&text[6..]);
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
    if let Some(rest) = lower.strip_prefix("blight ") {
        let count = super::oracle_util::parse_number(rest)
            .map(|(n, _)| n)
            .unwrap_or(1);
        return AbilityCost::Blight { count };
    }

    // "Remove N {type} counter(s) from ~"
    if let Some(rest) = lower.strip_prefix("remove ") {
        if rest.contains("counter") {
            if let Some((count, remainder)) = super::oracle_util::parse_number(rest) {
                let counter_type = remainder
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
        }
    }

    // "Tap an untapped creature you control" / "Tap two untapped creatures you control"
    // / "Tap another untapped creature you control"
    if let Some(rest) = lower.strip_prefix("tap ") {
        let (count, filter_text) = if let Some(rest) = rest.strip_prefix("another untapped ") {
            (1, rest)
        } else if let Some(rest) = rest.strip_prefix("an untapped ") {
            (1, rest)
        } else if let Some(rest) = rest.strip_prefix("an ") {
            (1, rest)
        } else if let Some((n, rest)) = super::oracle_util::parse_number(rest) {
            let rest = rest
                .trim_start()
                .strip_prefix("untapped ")
                .unwrap_or(rest.trim_start());
            (n, rest)
        } else {
            (0, "")
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
    if let Some(rest) = lower.strip_prefix("collect evidence ") {
        if let Some((n, _)) = parse_number(rest.trim()) {
            return AbilityCost::Exile {
                count: n,
                zone: Some(Zone::Graveyard),
                filter: None,
            };
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
    if let Some(rest) = lower.strip_prefix("return ") {
        if let Some(filter_and_zone) = try_parse_return_to_hand_cost(rest, &text[7..]) {
            return filter_and_zone;
        }
    }

    // "Reveal this card from your hand" — reveal self cost
    if lower == "reveal this card from your hand"
        || lower.starts_with("reveal this card from your hand")
    {
        return AbilityCost::Reveal { count: 1 };
    }

    // "Exert this creature" / "Exert ~" — exert cost (CR 701.43)
    if lower.starts_with("exert this ")
        || lower.starts_with("exert ~")
        || lower == "exert this creature"
    {
        return AbilityCost::Exert;
    }

    // "Mill a card" / "Mill N cards" — mill cost
    if let Some(rest) = lower.strip_prefix("mill ") {
        if rest == "a card" {
            return AbilityCost::Mill { count: 1 };
        }
        if let Some((n, _)) = parse_number(rest) {
            return AbilityCost::Mill { count: n };
        }
    }

    // "Pay {N}{W}" — mana cost with "pay" prefix
    if let Some(mana_text) = lower.strip_prefix("pay ") {
        let mana_text = mana_text.trim();
        if mana_text.starts_with('{') {
            if let Some((cost, rest)) = parse_mana_symbols(mana_text) {
                if rest.trim().is_empty() {
                    return AbilityCost::Mana { cost };
                }
            }
        }
    }

    // Waterbend {N}: tap-to-pay cost for Avatar waterbending abilities.
    if let Some(rest) = lower.strip_prefix("waterbend ") {
        if let Some((mana_cost, _)) = parse_mana_symbols(rest.trim()) {
            return AbilityCost::Waterbend { cost: mana_cost };
        }
    }

    // Vehicle tier costs: "12+ | {3}{W}" — skip the tier prefix and parse the actual cost
    if lower.contains(" | ") {
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

    // Mana cost: {N}{W}{U} etc.
    if text.starts_with('{') {
        if let Some((cost, rest)) = parse_mana_symbols(text) {
            if rest.trim().is_empty() {
                return AbilityCost::Mana { cost };
            }
        }
    }

    AbilityCost::Unimplemented {
        description: text.to_string(),
    }
}

/// CR 601.2f: Parse "this ability/spell costs {N} less to activate/cast for each [condition]".
/// Returns `None` for unrecognized patterns.
pub(crate) fn try_parse_cost_reduction(text: &str) -> Option<CostReduction> {
    let rest = text
        .strip_prefix("this ability costs ")
        .or_else(|| text.strip_prefix("this spell costs "))?;

    // Extract the {N} mana amount
    let (mana_cost, after_mana) = parse_mana_symbols(rest)?;
    let amount_per = match mana_cost {
        crate::types::mana::ManaCost::Cost { generic, shards } if shards.is_empty() => generic,
        _ => return None, // Only generic mana reduction supported
    };

    // Strip " less to activate for each " or " less to cast for each "
    let after_less = after_mana
        .trim_start()
        .strip_prefix("less to activate for each ")
        .or_else(|| {
            after_mana
                .trim_start()
                .strip_prefix("less to cast for each ")
        })?;

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
    if let Some(rest) = trimmed.strip_prefix("a ") {
        return rest;
    }
    if let Some(rest) = trimmed.strip_prefix("an ") {
        return rest;
    }

    trimmed
}

/// CR 112.3: Parse self-exile cost patterns like "this card from your graveyard",
/// "this artifact", "this creature from your hand". Returns the zone (if specified).
/// Also handles `~` (normalized card name) variants.
fn try_parse_self_exile_cost(rest: &str) -> Option<Option<Zone>> {
    let rest = rest.trim();
    let is_self = rest.starts_with("this ") || rest.starts_with("~ ");
    // "~ from your graveyard" / "this card from your graveyard"
    if is_self && rest.ends_with("from your graveyard") {
        return Some(Some(Zone::Graveyard));
    }
    // Bare "~" means exile self (normalized card name)
    if rest == "~" {
        return Some(None);
    }
    // "this card from your hand" / "this creature from your hand"
    if is_self && (rest.ends_with("from your hand") || rest.ends_with("from your hand.")) {
        return Some(Some(Zone::Hand));
    }
    // "this artifact" / "this creature" / "this enchantment" / "this land" / "this permanent"
    // / "this card" / "this vehicle" (self-exile from battlefield)
    if let Some(type_word) = rest.strip_prefix("this ") {
        let type_word = type_word.trim_end_matches('.');
        if matches!(
            type_word,
            "artifact" | "creature" | "enchantment" | "land" | "permanent" | "card" | "vehicle"
        ) {
            return Some(None); // battlefield (implicit)
        }
    }
    None
}

/// Parse "the top card of your library" / "the top N cards of your library".
fn try_parse_exile_top_library(rest: &str) -> Option<u32> {
    let rest = rest.strip_prefix("the top ")?.trim();
    if rest.starts_with("card of your library") {
        return Some(1);
    }
    if let Some((n, remainder)) = parse_number(rest) {
        if remainder.trim().starts_with("cards of your library") {
            return Some(n);
        }
    }
    None
}

/// CR 107.9: Parse energy costs like "{E}", "{E}{E}", "pay N {e}", "pay eight {e}".
fn try_parse_energy_cost(lower: &str) -> Option<u32> {
    let text = lower.strip_prefix("pay ").unwrap_or(lower).trim();
    // Count {e} symbols
    if text.contains("{e}") {
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
fn try_parse_return_to_hand_cost(rest_lower: &str, _rest_original: &str) -> Option<AbilityCost> {
    // Must end with "to its owner's hand" or "to your hand"
    if !rest_lower.contains("to its owner's hand") && !rest_lower.contains("to your hand") {
        return None;
    }
    // Strip the destination
    let filter_text = rest_lower
        .split(" to its owner's hand")
        .next()
        .or_else(|| rest_lower.split(" to your hand").next())?;
    // Strip article
    let filter_text = filter_text
        .strip_prefix("a ")
        .or_else(|| filter_text.strip_prefix("an "))
        .unwrap_or(filter_text);
    let target_text = format!("target {filter_text}");
    let (filter, _) = parse_target(&target_text);
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
    let ability_words = [
        "Cohort",
        "Boast",
        "Metalcraft",
        "Exhaust",
        "Max speed",
        "Renew",
        "Delirium",
        "Blow Up",
        "GOOOOAAAALLL!",
        "Smear Campaign",
        "Web Support",
        "Kerplunk",
        "Gestalt Mode",
        "Brimstone",
        "Mesmerize",
        "Wings of Light",
        "Jecht Beam",
    ];
    for word in &ability_words {
        if let Some(rest) = text.strip_prefix(word) {
            let rest = strip_em_dash(rest)?;
            let cost = parse_single_cost(rest.trim());
            if !matches!(cost, AbilityCost::Unimplemented { .. }) {
                return Some(cost);
            }
        }
    }
    // Ticket costs: "{TK}{TK} — {T}", "{TK}{TK}{TK} — {3}"
    if lower.starts_with("{tk}") {
        if let Some(dash_pos) = text
            .find(" — ")
            .or_else(|| text.find(" — "))
            .or_else(|| text.find(" - "))
        {
            let rest = &text[dash_pos..];
            let rest = strip_em_dash(rest)?;
            let cost = parse_single_cost(rest.trim());
            if !matches!(cost, AbilityCost::Unimplemented { .. }) {
                return Some(cost);
            }
        }
    }
    None
}

/// Strip em-dash/en-dash separator: " — ", " — ", " - "
fn strip_em_dash(text: &str) -> Option<&str> {
    text.strip_prefix(" — ")
        .or_else(|| text.strip_prefix(" — "))
        .or_else(|| text.strip_prefix(" - "))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{TypeFilter, TypedFilter};
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
                target: TargetFilter::SelfRef
            }
        );
    }

    #[test]
    fn cost_sacrifice_creature() {
        match parse_oracle_cost("Sacrifice a creature") {
            AbilityCost::Sacrifice { target } => {
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
            AbilityCost::PayLife { amount: 3 }
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
                count: 1,
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
                count: 1,
                filter: None,
                random: false,
                self_ref: true,
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
                assert_eq!(costs[0], AbilityCost::PayLife { amount: 1 });
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
    fn cost_reveal_self_from_hand() {
        assert_eq!(
            parse_oracle_cost("Reveal this card from your hand"),
            AbilityCost::Reveal { count: 1 }
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
