use nom::branch::alt;
use nom::bytes::complete::{tag, take_till, take_until};
use nom::combinator::{all_consuming, map, opt, rest, value};
use nom::error::ParseError;
use nom::multi::separated_list1;
use nom::sequence::{pair, preceded, terminated};
use nom::Parser;

use super::oracle_effect::imperative::parse_discard_card_filter;
use super::oracle_modal::split_short_label_prefix;
use super::oracle_nom::bridge::nom_on_lower;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::primitives::{scan_contains, split_once_on};
use super::oracle_nom::quantity as nom_quantity;
use super::oracle_static::parse_dynamic_x_clause;
use super::oracle_target::{parse_target, parse_type_phrase};
use super::oracle_util::parse_count_expr;
use super::oracle_util::parse_mana_symbols;
use super::oracle_util::parse_number;
use super::oracle_util::TextPair;
use crate::types::ability::{
    AbilityCost, AggregateFunction, BeholdCostAction, Comparator, ControllerRef, CostReduction,
    CounterCostSelection, FilterProp, ObjectProperty, PlayerScope, QuantityExpr, QuantityRef,
    SacrificeCost, TapCreaturesRequirement, TargetFilter, TypedFilter, EXILE_COST_X,
    REMOVE_COUNTER_COST_ALL, REMOVE_COUNTER_COST_ANY_NUMBER, REMOVE_COUNTER_COST_X,
};
use crate::types::counter::parse_counter_match;
use crate::types::zones::Zone;

/// Parse the cost portion before `:` in an Oracle activated ability.
/// Input: the raw text before the colon, e.g., "{T}", "{2}{W}, Sacrifice a creature", "Pay 3 life".
/// Returns an AbilityCost (possibly Composite for multi-part costs).
pub fn parse_oracle_cost(text: &str) -> AbilityCost {
    let text = text.trim();
    let lower = text.to_lowercase();

    // CR 118.3: Top-level " or " splits the entire cost into alternatives.
    // E.g., "{3}, {T} or {R}, {T}" → OneOf([Composite([Mana(3), Tap]), Composite([Mana(R), Tap])]).
    // Must check before comma-splitting so the `or` isn't consumed as part of a segment.
    // Guard: both sides must contain `{` (mana/tap symbols) to distinguish from
    // filter phrases like "creature or artifact" inside a Sacrifice cost.
    if let Ok((_, (before, _after))) = split_once_on(&lower, " or ") {
        let consumed = before.len();
        let left_text = &text[..consumed];
        let right_text = &text[consumed + " or ".len()..];
        if left_text.contains('{') && right_text.contains('{') {
            let left = parse_oracle_cost_no_or(left_text);
            let right = parse_oracle_cost_no_or(right_text);
            return AbilityCost::OneOf {
                costs: vec![left, right],
            };
        }
    }

    parse_oracle_cost_no_or(text)
}

/// Inner cost parser that handles comma-splitting but NOT top-level `or`.
/// Prevents infinite recursion when parsing each alternative of a OneOf.
fn parse_oracle_cost_no_or(text: &str) -> AbilityCost {
    let text = text.trim();

    // Split on ", " for composite costs
    let parts = fixup_from_among_remove_counter_parts(split_cost_parts(text));
    if parts.len() > 1 {
        let mut costs: Vec<AbilityCost> =
            parts.iter().map(|p| parse_single_cost(p.trim())).collect();
        // CR 601.2b: "Sacrifice A, B, and C" splits into ["Sacrifice A", "B", "C"].
        // Bare noun-phrase continuations after a verb-cost are additional instances
        // of that same cost. Applies to Sacrifice, Exile, and TapCreatures.
        fixup_bare_noun_continuations(&mut costs);
        return AbilityCost::Composite { costs };
    }

    parse_single_cost(parts.first().map_or(text, String::as_str))
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

/// CR 601.2b / CR 602.2b + CR 122.1: "Remove N counters from among [type],
/// [type], and [type] you control" is one activation-cost component. The
/// top-level splitter cannot know whether a comma belongs to a type list or to
/// the next cost, so merge only contiguous fragments that still parse as a
/// complete `RemoveCounter` from-among cost.
fn fixup_from_among_remove_counter_parts(parts: Vec<&str>) -> Vec<String> {
    let mut fixed = Vec::new();
    let mut i = 0;

    while i < parts.len() {
        let mut part = parts[i].trim().to_string();
        if is_from_among_remove_counter_cost(&part) {
            let mut next = i + 1;
            while next < parts.len() {
                let candidate = format!("{part}, {}", parts[next].trim());
                if !is_from_among_remove_counter_cost(&candidate) {
                    break;
                }
                part = candidate;
                next += 1;
            }
            fixed.push(part);
            i = next;
        } else {
            fixed.push(part);
            i += 1;
        }
    }

    fixed
}

fn is_from_among_remove_counter_cost(text: &str) -> bool {
    matches!(
        parse_single_cost(text),
        AbilityCost::RemoveCounter {
            target: Some(_),
            selection: CounterCostSelection::AmongObjects,
            ..
        }
    )
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
            AbilityCost::Sacrifice(_) => last_verb = Some(PrecedingVerb::Sacrifice),
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
                        costs[i] = AbilityCost::Sacrifice(SacrificeCost::count(filter, 1));
                    }
                    PrecedingVerb::Exile { zone } => {
                        costs[i] = AbilityCost::Exile {
                            count: 1,
                            zone,
                            filter: Some(filter),
                        };
                    }
                    PrecedingVerb::TapCreatures => {
                        costs[i] = AbilityCost::TapCreatures {
                            requirement: TapCreaturesRequirement::count(1),
                            filter,
                        };
                    }
                }
            }
            _ => {
                last_verb = None;
            }
        }
    }
}

fn parse_behold_cost(lower: &str) -> Option<AbilityCost> {
    type E<'a> = super::oracle_nom::error::OracleError<'a>;
    let (input, _) = tag::<_, _, E<'_>>("behold ").parse(lower).ok()?;
    let (input, count) =
        if let Ok((rest, _)) = alt((tag::<_, _, E<'_>>("a "), tag("an "))).parse(input) {
            (rest, 1)
        } else if let Ok((rest, count)) =
            terminated(nom_primitives::parse_number, tag::<_, _, E<'_>>(" ")).parse(input)
        {
            (rest, count)
        } else {
            (input, 1)
        };
    let (_, filter_text) = take_till::<_, _, E<'_>>(|c| c == '.' || c == '(')
        .parse(input)
        .ok()?;
    let (_, (filter_text, exile)) = all_consuming(alt((
        map(
            terminated(
                take_until::<_, _, E<'_>>(" and exile it"),
                tag(" and exile it"),
            ),
            |filter_text: &str| (filter_text.trim(), true),
        ),
        map(rest, |filter_text: &str| (filter_text.trim(), false)),
    )))
    .parse(filter_text.trim())
    .ok()?;
    let (filter, remainder) = parse_type_phrase(filter_text);
    if !remainder.trim().is_empty() || matches!(filter, TargetFilter::Any) {
        return None;
    }
    let action = if exile {
        BeholdCostAction::ExileChosen
    } else {
        BeholdCostAction::ChooseOrReveal
    };
    Some(AbilityCost::Behold {
        count,
        filter,
        action,
    })
}

/// CR 701.4a (behold) + CR 601.2b/f (additional cost) + CR 400.7j: Parse the
/// SPELLED-OUT choose-or-reveal behold cost printed without the "behold" keyword.
///
/// CR 701.4a: "Behold a [quality]" means "Reveal a [quality] card from your hand
/// or choose a [quality] permanent you control on the battlefield." Some cards
/// print this action longhand in the cost line itself rather than as reminder
/// text after a "behold" keyword:
///   - "choose a creature you control or reveal a creature card from your hand"
///     (Monstrous Emergence)
///
/// This is the exact action of `BeholdCostAction::ChooseOrReveal`: choose a
/// matching permanent you control OR reveal a matching card from your hand,
/// without moving it. `eligible_behold_choices` already scopes the controlled
/// leg to "you control" and the revealed leg to your hand, so the emitted
/// `Behold` filter is the bare type shared by both legs. The two legs must name
/// the same type (always true on printed cards); a mismatch falls through to the
/// generic cost parser.
///
/// The "warped creature card you own in exile" leg (Close Encounter) is NOT this
/// shape — exile-zone selection and the "warped" property are unsupported by
/// `eligible_behold_choices`, so that card is handled by honest deferral, not
/// here.
fn parse_choose_or_reveal_behold_cost(lower: &str) -> Option<AbilityCost> {
    type E<'a> = super::oracle_nom::error::OracleError<'a>;
    let (input, _) = tag::<_, _, E<'_>>("choose ").parse(lower).ok()?;
    let (input, _) = alt((tag::<_, _, E<'_>>("a "), tag("an ")))
        .parse(input)
        .ok()?;
    // First leg type phrase, bounded by " you control or reveal ".
    let (_, choose_type_text) = take_until::<_, _, E<'_>>(" you control or reveal ")
        .parse(input)
        .ok()?;
    let (after_choose, _) = terminated(
        take_until::<_, _, E<'_>>(" you control or reveal "),
        tag(" you control or reveal "),
    )
    .parse(input)
    .ok()?;
    // Second leg: "a/an <type> card from your hand".
    let (after_article, _) = alt((tag::<_, _, E<'_>>("a "), tag("an ")))
        .parse(after_choose)
        .ok()?;
    let (_, reveal_type_text) = all_consuming(terminated(
        take_until::<_, _, E<'_>>(" card from your hand"),
        tag(" card from your hand"),
    ))
    .parse(after_article)
    .ok()?;

    let (choose_filter, choose_rem) = parse_type_phrase(choose_type_text.trim());
    let (reveal_filter, reveal_rem) = parse_type_phrase(reveal_type_text.trim());
    if !choose_rem.trim().is_empty()
        || !reveal_rem.trim().is_empty()
        || matches!(choose_filter, TargetFilter::Any)
        || choose_filter != reveal_filter
    {
        return None;
    }

    Some(AbilityCost::Behold {
        count: 1,
        filter: choose_filter,
        action: BeholdCostAction::ChooseOrReveal,
    })
}

fn parse_remove_counter_kind(
    input: &str,
) -> super::oracle_nom::error::OracleResult<'_, crate::types::counter::CounterMatch> {
    type E<'a> = super::oracle_nom::error::OracleError<'a>;
    alt((
        value(
            crate::types::counter::CounterMatch::Any,
            alt((tag("counters"), tag("counter"))),
        ),
        map(
            terminated(
                take_until::<_, _, E<'_>>(" counter"),
                alt((tag(" counters"), tag(" counter"))),
            ),
            parse_counter_match,
        ),
    ))
    .parse(input)
}

fn parse_remove_counter_quantity_and_kind(
    input: &str,
) -> Option<(u32, crate::types::counter::CounterMatch)> {
    type E<'a> = super::oracle_nom::error::OracleError<'a>;
    let input = input.trim();
    if let Ok((_, counter_type)) = all_consuming(preceded(
        tag::<_, _, E<'_>>("all "),
        parse_remove_counter_kind,
    ))
    .parse(input)
    {
        return Some((REMOVE_COUNTER_COST_ALL, counter_type));
    }
    // CR 601.2b / CR 602.2b: "any number of" requires a count choice just
    // like literal X, but stays a separate sentinel for parser/data clarity.
    if let Ok((_, counter_type)) = all_consuming(preceded(
        tag::<_, _, E<'_>>("any number of "),
        parse_remove_counter_kind,
    ))
    .parse(input)
    {
        return Some((REMOVE_COUNTER_COST_ANY_NUMBER, counter_type));
    }
    // CR 601.2b: "X" is a variable cost announced before target selection.
    if let Ok((_, counter_type)) = all_consuming(preceded(
        alt((tag::<_, _, E<'_>>("x "), tag("X "))),
        parse_remove_counter_kind,
    ))
    .parse(input)
    {
        return Some((REMOVE_COUNTER_COST_X, counter_type));
    }
    // CR 107.3 + CR 601.2b: "one or more" counters is a player-chosen variable
    // count (X), announced at activation; "that much" / "counters removed this
    // way" then scale by the chosen value.
    if let Ok((_, counter_type)) = all_consuming(preceded(
        tag::<_, _, E<'_>>("one or more "),
        parse_remove_counter_kind,
    ))
    .parse(input)
    {
        return Some((REMOVE_COUNTER_COST_X, counter_type));
    }
    if let Ok((_, (count, counter_type))) = all_consuming(pair(
        terminated(nom_primitives::parse_number, tag::<_, _, E<'_>>(" ")),
        parse_remove_counter_kind,
    ))
    .parse(input)
    {
        return Some((count, counter_type));
    }
    all_consuming(preceded(
        alt((tag::<_, _, E<'_>>("a "), tag("an "))),
        parse_remove_counter_kind,
    ))
    .parse(input)
    .ok()
    .map(|(_, counter_type)| (1, counter_type))
}

fn parse_remove_counter_target(target_text: &str) -> (Option<TargetFilter>, CounterCostSelection) {
    let (target_text, selection) = pair(opt(tag::<_, _, nom::error::Error<&str>>("among ")), rest)
        .parse(target_text)
        .map(|(_, (among, target_text))| {
            (
                target_text,
                if among.is_some() {
                    CounterCostSelection::AmongObjects
                } else {
                    CounterCostSelection::SingleObject
                },
            )
        })
        .unwrap_or((target_text, CounterCostSelection::SingleObject));
    let (target, remainder) = parse_target(target_text);
    if !remainder.trim().is_empty() || matches!(target, TargetFilter::Any | TargetFilter::SelfRef) {
        return (None, CounterCostSelection::SingleObject);
    }
    (Some(target), selection)
}

pub fn parse_single_cost(text: &str) -> AbilityCost {
    type E<'a> = super::oracle_nom::error::OracleError<'a>;
    let text = text.trim();
    let lower = text.to_lowercase();

    if let Some(cost) = parse_behold_cost(&lower) {
        return cost;
    }

    // CR 701.4a + CR 601.2b/f: spelled-out "choose … or reveal …" behold cost
    // (Monstrous Emergence). Tried after the keyword form; both yield `Behold`.
    if let Some(cost) = parse_choose_or_reveal_behold_cost(&lower) {
        return cost;
    }

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
            return AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1));
        }
        // CR 107.2: "sacrifice any number of [filter]" — player chooses 0..=all
        // eligible permanents (Rottenmouth Viper, Scapeshift-class additional costs).
        if let Some(((), rest_after_any)) = nom_on_lower(rest, &rest_lower, |i| {
            value((), tag("any number of ")).parse(i)
        }) {
            let filter_text = rest_after_any.trim().trim_end_matches('.');
            let target_phrase = format!("target {filter_text}");
            let (filter, remainder) = parse_target(&target_phrase);
            if remainder.trim().is_empty() {
                return AbilityCost::Sacrifice(SacrificeCost::count(filter, u32::MAX));
            }
        }
        // Try to extract a numeric count: "sacrifice two creatures", "sacrifice three lands"
        // CR 107.3a: `X` in an activation or additional cost is chosen as part
        // of activating or casting, so preserve it as a variable cost marker.
        let (use_count, filter_text) = if let Some(((), rest_after_x)) =
            nom_on_lower(rest, &rest_lower, |i| value((), tag("x ")).parse(i))
        {
            (u32::MAX, rest_after_x.trim().to_string())
        } else if let Some((count, rest_after_count)) = parse_number(&rest_lower) {
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
        return AbilityCost::Sacrifice(SacrificeCost::count(
            ensure_another_sacrifice_filter(filter, &filter_text),
            use_count,
        ));
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
            let life_amount_text = take_till::<_, _, E<'_>>(|c| c == '.' || c == '(')
                .parse(rest_lower.as_str())
                .map(|(_, amount)| amount.trim())
                .unwrap_or(rest_lower.as_str());
            if all_consuming(tag::<_, _, E<'_>>("x life"))
                .parse(life_amount_text)
                .is_ok()
            {
                return AbilityCost::PayLife {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                };
            }
            if let Some((n, after_n)) = parse_number(&rest_lower) {
                // CR 119.4 + CR 122.1: "Pay N life for each <clause>" — a
                // per-object multiplier on the life cost (e.g. Tornado's
                // "Pay 3 life for each velocity counter on this enchantment").
                // Model on parse_unless_for_each_payment
                // (oracle_effect/mod.rs:14482). `after_n` is
                // "life for each <clause>" because parse_number trim_start()s
                // the remainder, so "life " / "for each " carry their
                // separators on the TRAILING side.
                if let Ok((_, for_each_clause)) = preceded(
                    tag::<_, _, E<'_>>("life "),
                    preceded(
                        tag::<_, _, E<'_>>("for each "),
                        nom::combinator::rest::<&str, E<'_>>,
                    ),
                )
                .parse(after_n)
                {
                    if let Ok((_, qty)) =
                        nom_quantity::parse_for_each_clause_ref_complete(for_each_clause)
                    {
                        return AbilityCost::PayLife {
                            amount: QuantityExpr::Multiply {
                                factor: n as i32,
                                inner: Box::new(QuantityExpr::Ref { qty }),
                            },
                        };
                    }
                }
                // Flat "Pay N life" — no " for each " tail.
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
                selection: crate::types::ability::CardSelectionMode::Chosen,
                self_scope: crate::types::ability::DiscardSelfScope::SourceCard,
            };
        }
        if nom_on_lower(rest, &rest_lower, |i| value((), tag("a card")).parse(i)).is_some() {
            return AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                self_scope: crate::types::ability::DiscardSelfScope::FromHand,
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
                selection: crate::types::ability::CardSelectionMode::Chosen,
                self_scope: crate::types::ability::DiscardSelfScope::FromHand,
            };
        }
        // CR 701.9a + CR 608.2c: "Discard a/<N> <type> card(s)" — capture the
        // card-type filter so only matching cards can pay the cost (Lotleth
        // Troll: "Discard a creature card:"). Without this the typed
        // restriction is dropped and any card pays the cost. Mirrors the
        // effect-form discard and the trigger-side cost parser, which both
        // delegate to `parse_discard_card_filter`: `parse_count_expr` eats the
        // leading count token ("a "/"two ") and the remainder is the typed noun
        // phrase. Ordered before the plain `parse_number` arm so "two creature
        // cards" is not swallowed as an untyped count.
        if let Some((count, after_count)) = parse_count_expr(&rest_lower) {
            if let Some(filter) = parse_discard_card_filter(after_count.trim_start()) {
                return AbilityCost::Discard {
                    count,
                    filter: Some(filter),
                    selection: crate::types::ability::CardSelectionMode::Chosen,
                    self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                };
            }
        }
        if let Some((n, _)) = parse_number(&rest_lower) {
            return AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: n as i32 },
                filter: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                self_scope: crate::types::ability::DiscardSelfScope::FromHand,
            };
        }
        return AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            selection: crate::types::ability::CardSelectionMode::Chosen,
            self_scope: crate::types::ability::DiscardSelfScope::FromHand,
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
        // CR 107.3a + CR 118.8: "Exile X card(s) from your graveyard" — variable
        // count announced during casting (Harvest Pyre). Ordered before the typed
        // `parse_type_phrase` arm, which cannot represent a bare zone with no filter.
        if let Some(((), rest_after_x)) =
            nom_on_lower(rest, &rest_lower, |i| value((), tag("x ")).parse(i))
        {
            let after_lower = rest_after_x.to_lowercase();
            if nom_on_lower(rest_after_x, &after_lower, |i| {
                value(
                    (),
                    alt((
                        tag("card from your graveyard"),
                        tag("cards from your graveyard"),
                    )),
                )
                .parse(i)
            })
            .is_some()
            {
                return AbilityCost::Exile {
                    count: EXILE_COST_X,
                    zone: Some(Zone::Graveyard),
                    filter: None,
                };
            }
        }
        // CR 118.8: "Exile N card(s) from your graveyard" without a type filter.
        if let Some((count, after_count)) = parse_number(&rest_lower) {
            let after_count_lower = after_count.trim_start().to_lowercase();
            if nom_on_lower(after_count.trim_start(), &after_count_lower, |i| {
                value(
                    (),
                    alt((
                        tag("card from your graveyard"),
                        tag("cards from your graveyard"),
                    )),
                )
                .parse(i)
            })
            .is_some()
            {
                return AbilityCost::Exile {
                    count,
                    zone: Some(Zone::Graveyard),
                    filter: None,
                };
            }
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
            let (counter_phrase, target, selection) = pair(
                terminated(take_until::<_, _, E<'_>>(" from "), tag(" from ")),
                nom::combinator::rest,
            )
            .parse(rest_lower.as_str())
            .ok()
            .map_or(
                (
                    rest_lower.as_str(),
                    None,
                    CounterCostSelection::SingleObject,
                ),
                |(_, split): (&str, (&str, &str))| {
                    let (before_from, target_text) = split;
                    let (target, selection) = parse_remove_counter_target(target_text);
                    (before_from.trim(), target, selection)
                },
            );
            if let Some((count, counter_type)) =
                parse_remove_counter_quantity_and_kind(counter_phrase)
            {
                return AbilityCost::RemoveCounter {
                    count,
                    counter_type,
                    target,
                    selection,
                };
            }
        }
    }

    // "Tap an untapped creature you control" / "Tap two untapped creatures you control"
    // / "Tap another untapped creature you control" / "Tap X untapped [type] you control"
    if let Some(((), tap_rest)) = nom_on_lower(text, &lower, |i| {
        value((), alt((tag("tap "), tag("tapped ")))).parse(i)
    }) {
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
                return AbilityCost::TapCreatures {
                    requirement: TapCreaturesRequirement::count(count),
                    filter,
                };
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

    // "Forage" — exile three cards from your graveyard or sacrifice a Food
    // (CR 701.61a). A modal cost: both ways are offered, so a player who can't
    // exile three cards can still forage by sacrificing a Food (and vice versa).
    if lower == "forage" {
        return AbilityCost::OneOf {
            costs: vec![
                AbilityCost::Exile {
                    count: 3,
                    zone: Some(Zone::Graveyard),
                    filter: None,
                },
                AbilityCost::Sacrifice(SacrificeCost::count(
                    TargetFilter::Typed(TypedFilter::permanent().subtype("Food".to_string())),
                    1,
                )),
            ],
        };
    }

    // "Pay {E}" / "Pay {E}{E}" / "Pay N {E}" — energy costs (CR 107.14)
    if let Some(energy) = try_parse_energy_cost(&lower) {
        return AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed {
                value: energy as i32,
            },
        };
    }

    // "Return a land you control to its owner's hand" — bounce cost
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("return ")).parse(i)) {
        let rest_lower = rest.to_lowercase();
        if let Some(filter_and_zone) = try_parse_return_to_hand_cost(&rest_lower) {
            return filter_and_zone;
        }
    }

    // CR 701.3d: "Unattach this Equipment" / "Unattach ~" — explicit
    // activation costs on Equipment such as Sunforger.
    if nom_on_lower(text, &lower, |i| {
        value((), alt((tag("unattach this equipment"), tag("unattach ~")))).parse(i)
    })
    .is_some()
    {
        return AbilityCost::Unattach;
    }

    // "reveal your hand" — reveal the controller's entire hand.
    // CR 701.20a: Reveal means show to all players. Used as alternative cost
    // (Land Grant class). Modeled as EffectCost wrapping Effect::RevealHand.
    // Verified: CR 701.20 (docs/MagicCompRules.txt:3430).
    if nom_on_lower(text, &lower, |i| {
        value((), tag("reveal your hand")).parse(i)
    })
    .is_some()
    {
        return AbilityCost::EffectCost {
            effect: Box::new(crate::types::ability::Effect::RevealHand {
                target: TargetFilter::SelfRef,
                card_filter: TargetFilter::Any,
                count: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                choice_optional: false,
                reveal: true,
            }),
        };
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

    // CR 117.1 + CR 601.2b + CR 107.4a/107.4e/202.1: "Exile any number of
    // [color] cards from your graveyard with [N] or more/greater [color] mana
    // symbols among their mana costs" — Baron Helmut Zemo's Boast cost. Must run
    // before the EffectCost fallback, which would otherwise wrap the exile as a
    // non-functional `ChangeZone` effect-cost.
    if let Some(cost) = try_parse_exile_with_aggregate_cost(&lower) {
        return cost;
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

/// CR 601.2f + CR 602.2b: Recognize the *head* of a self ACTIVATED-ability
/// cost-reduction sentence — "this ability costs {N} less to activate" —
/// regardless of any trailing "if [condition]" / "for each [condition]" tail.
/// Deliberately scoped to the activated-ability form only (NOT the spell
/// "this spell costs {N} less to cast" form, which uses a different path).
///
/// CR 601.2f folds all cost reductions into the total cost; CR 602.2b makes an
/// activated ability's activation cost the analog of a spell's mana cost. The
/// upstream suffix-conditional stripper uses this to decline peeling the trailing
/// "if [condition]" off such a sentence, so the whole sentence reaches
/// `try_parse_cost_reduction` (whose own "if" arm re-homes the condition). #3223.
///
/// Combinator-based (parser-combinator gate): runs on an already-lowercase slice
/// and mirrors `try_parse_cost_reduction`'s `parse_mana_symbols` path.
pub(crate) fn is_self_cost_reduction_prefix(lower: &str) -> bool {
    // Scoped to the ACTIVATED-ability form only ("this ability costs {N} less to
    // activate"). The spell form ("this spell costs {N} less to cast") is parsed
    // through a different (spell) path that does NOT route through
    // `strip_cost_reduction_node`, so suppressing the suffix split there would
    // only strand its condition as a swallowed clause (e.g. Lashwhip Predator).
    let Ok((rest, _)) = tag::<_, _, nom::error::Error<&str>>("this ability costs ").parse(lower)
    else {
        return false;
    };

    // Extract the {N} mana amount (same parse_mana_symbols path as the reducer).
    let Some((_mana_cost, after_mana)) = parse_mana_symbols(rest) else {
        return false;
    };

    let after_mana = after_mana.trim_start();
    tag::<_, _, nom::error::Error<&str>>("less to activate")
        .parse(after_mana)
        .is_ok()
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
        // CR 107.3c: When the cost reduction is "{X}" and X is *defined by the
        // text* ("..., where X is <count>"), the reduction is a dynamic amount,
        // not a player-chosen one. Route to the where-X branch; any other shard
        // shape (colored/colorless reductions) stays an honest gap — CR 118.7a
        // limits cost reduction to the generic component.
        crate::types::mana::ManaCost::Cost { generic: 0, shards }
            if shards.as_slice() == [crate::types::mana::ManaCostShard::X] =>
        {
            return try_parse_dynamic_x_cost_reduction(after_mana.trim_start());
        }
        _ => return None, // Only generic mana reduction supported
    };

    let after_mana = after_mana.trim_start();

    // CR 602.2b: An activated ability's analog to a spell's mana cost is its activation cost.
    // CR 601.2f: Cost reductions reduce that cost, with the mana component floored at {0}.
    // CR 102.1: The active player is the player whose turn it is, so "during your
    //           turn" is the controller-is-active-player test.
    // Timing-gated flat form ("... less to activate during your turn[s]" / "... less
    // to cast during your turn[s]") is therefore exactly the `IsYourTurn` flat
    // conditional (count = Fixed(1)). Checked before the generic "if [condition]"
    // form because "during your turn" is not introduced by "if". Hylda's Crown of
    // Winter: "This ability costs {1} less to activate during your turn."
    if nom_on_lower(after_mana, after_mana, |i| {
        value(
            (),
            (
                alt((
                    tag("less to activate during your "),
                    tag("less to cast during your "),
                )),
                alt((tag("turns"), tag("turn"))),
            ),
        )
        .parse(i)
    })
    .is_some_and(|((), rest)| rest.trim().trim_end_matches('.').trim().is_empty())
    {
        return Some(CostReduction {
            amount_per,
            count: QuantityExpr::Fixed { value: 1 },
            condition: Some(crate::types::ability::ParsedCondition::IsYourTurn),
        });
    }

    // CR 602.2b + CR 601.2f conditional flat form: "... less to activate if [condition]" /
    // "... less to cast if [condition]". The reduction is a flat {amount_per}
    // (count = Fixed(1)) gated by `condition`. Checked before the "for each"
    // form; if the "if " marker is present but the condition does not parse,
    // return None so the clause stays a loud gap (coverage honesty) rather than
    // silently mis-parsing.
    if let Some(((), cond_text)) = nom_on_lower(after_mana, after_mana, |i| {
        value(
            (),
            alt((tag("less to activate if "), tag("less to cast if "))),
        )
        .parse(i)
    }) {
        let cond_text = cond_text.trim().trim_end_matches('.').trim();
        let condition = super::oracle_condition::parse_restriction_condition(cond_text)?;
        return Some(CostReduction {
            amount_per,
            count: QuantityExpr::Fixed { value: 1 },
            condition: Some(condition),
        });
    }

    // Strip " less to activate for each " or " less to cast for each "
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
    if let Ok((_, qty)) = nom_quantity::parse_for_each_clause_ref_complete(after_less) {
        return Some(CostReduction {
            amount_per,
            count: QuantityExpr::Ref { qty },
            condition: None,
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
        condition: None,
    })
}

/// CR 601.2f + CR 602.2b + CR 107.3c: Parse the dynamic-{X} activated-ability
/// cost-reduction tail "less to activate, where X is <count>" (verb axis also
/// accepts "less to cast"). `input` is the already-lowercase slice immediately
/// after the leading "{X}" amount.
///
/// CR 107.3c: because X is defined by the ability's own text ("where X is ..."),
/// the controller does not choose it — the reduction is a dynamic amount. This
/// maps to `CostReduction { amount_per: 1, count: Ref(<qty>), .. }` so the
/// runtime `apply_cost_reduction` computes `reduce_by = 1 * count` and resolves
/// `count` from game state. CR 118.7a/CR 601.2f then reduce only the generic
/// component, flooring at {0}.
///
/// Covers the entire "{X} less to activate, where X is <any QuantityRef>" class
/// (Survey Mechan, The Dominion Bracelet, and any future card of this shape) by
/// delegating the count phrase to `parse_dynamic_x_clause`. Returns `None` when
/// the where-X clause does not parse so the clause stays an honest gap rather
/// than a misparse.
fn try_parse_dynamic_x_cost_reduction(input: &str) -> Option<CostReduction> {
    // Strip the verb. No trailing space: the where-X clause begins with ", ".
    let ((), after_verb) = nom_on_lower(input, input, |i| {
        value((), alt((tag("less to activate"), tag("less to cast")))).parse(i)
    })?;

    // Delegate ", where x is <phrase>" to the shared dynamic-X combinator.
    let (_, qty) = parse_dynamic_x_clause(after_verb).ok()?;
    Some(CostReduction {
        amount_per: 1,
        count: QuantityExpr::Ref { qty },
        condition: None,
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

/// CR 109.4 + CR 701.21: Sacrifice costs phrased "another [type]" / "other [type]"
/// must carry `FilterProp::Another` so the ability source is excluded (Bound by
/// Moonsilver, Mazirek class). `parse_target("target another …")` usually adds
/// the property, but belt-and-suspenders here in case the type phrase is
/// recovered without the prefix (article stripping, numeric count paths, etc.).
fn ensure_another_sacrifice_filter(filter: TargetFilter, phrase: &str) -> TargetFilter {
    let lower = phrase.trim().to_lowercase();
    let has_another_prefix = nom_on_lower(&lower, &lower, |i| {
        value((), alt((tag("another "), tag("other ")))).parse(i)
    })
    .is_some();
    if !has_another_prefix {
        return filter;
    }
    match filter {
        TargetFilter::Typed(mut typed) => {
            if !typed.properties.contains(&FilterProp::Another) {
                typed.properties.push(FilterProp::Another);
            }
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|f| ensure_another_sacrifice_filter(f, phrase))
                .collect(),
        },
        other => other,
    }
}

/// CR 117.1 + CR 601.2b + CR 107.4a/107.4e/202.1: Parse Baron Helmut Zemo's
/// Boast cost — "Exile any number of [color] cards from your graveyard with [N]
/// or more [color] mana symbols among their mana costs" — into a standalone
/// `AbilityCost::ExileWithAggregate` (the aggregate-threshold sibling of
/// `CollectEvidence`). `lower` is the already-lowercased cost text.
///
/// Composed from nom combinators (no string-dispatch): each grammar axis (the
/// any-number prefix, the filter color + card noun, the graveyard zone, the
/// threshold number, the comparator words, the aggregated color + symbol noun) is
/// a single `tag`/`alt`/`parse_color`/`parse_number` step. Hybrid symbols count
/// for each of their colors at resolution time (CR 107.4e) via the
/// `ObjectProperty::ManaSymbolCount` resolver.
fn try_parse_exile_with_aggregate_cost(lower: &str) -> Option<AbilityCost> {
    type E<'a> = super::oracle_nom::error::OracleError<'a>;
    let (i, _) = tag::<_, _, E<'_>>("exile any number of ")
        .parse(lower)
        .ok()?;
    // Filter: "[color] card(s)".
    let (i, filter_color) = nom_primitives::parse_color(i).ok()?;
    let (i, _) = alt((tag::<_, _, E<'_>>(" cards"), tag(" card")))
        .parse(i)
        .ok()?;
    // Zone: "from your graveyard" — owned by you, in the graveyard.
    let (i, _) = tag::<_, _, E<'_>>(" from your graveyard with ")
        .parse(i)
        .ok()?;
    // Threshold: "[N] or more/greater".
    let (i, n) = nom_primitives::parse_number(i).ok()?;
    let (i, _) = alt((tag::<_, _, E<'_>>(" or more "), tag(" or greater ")))
        .parse(i)
        .ok()?;
    // Aggregated property: "[color] mana symbols among their mana costs".
    let (i, agg_color) = nom_primitives::parse_color(i).ok()?;
    let (i, _) = alt((
        tag::<_, _, E<'_>>(" mana symbols among their mana costs"),
        tag(" mana symbols among their costs"),
    ))
    .parse(i)
    .ok()?;
    // The whole cost phrase must have been consumed — a trailing remainder means
    // this is a different (unsupported) shape that must not silently match.
    if !i.trim().is_empty() {
        return None;
    }

    let filter = TargetFilter::Typed(
        TypedFilter::card()
            .controller(ControllerRef::You)
            .properties(vec![
                FilterProp::HasColor {
                    color: filter_color,
                },
                FilterProp::InZone {
                    zone: Zone::Graveyard,
                },
            ]),
    );
    Some(AbilityCost::ExileWithAggregate {
        filter,
        function: AggregateFunction::Sum,
        property: ObjectProperty::ManaSymbolCount(agg_color),
        comparator: Comparator::GE,
        value: n as i32,
        zone: Zone::Graveyard,
    })
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
    // "~" is the self-reference placeholder. Preserve it as an explicit
    // SelfRef so the runtime does not treat an unconstrained filter as "any
    // permanent you control".
    if nom_on_lower(filter_text, filter_text, |i| {
        value(
            (),
            alt((
                tag("~"),
                tag("this card"),
                tag("this creature"),
                tag("this artifact"),
                tag("this equipment"),
                tag("this land"),
                tag("this permanent"),
                tag("this enchantment"),
            )),
        )
        .parse(i)
    })
    .is_some_and(|((), rest)| rest.trim().is_empty())
    {
        return Some(AbilityCost::ReturnToHand {
            count: 1,
            filter: Some(TargetFilter::SelfRef),
            from_zone: None,
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
    let filter = match filter {
        TargetFilter::Any => {
            // CR 201.5: A cost using the source card's own name, such as
            // "Return Recurring Nightmare to its owner's hand", refers to that
            // source object.
            TargetFilter::SelfRef
        }
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: None,
            properties,
        }) if type_filters.is_empty() && properties.is_empty() => {
            // CR 201.5: A cost using the source card's own name, such as
            // "Return Recurring Nightmare to its owner's hand", refers to that
            // source object.
            TargetFilter::SelfRef
        }
        filter => filter,
    };
    Some(AbilityCost::ReturnToHand {
        count: 1,
        filter: Some(filter),
        from_zone: None,
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

/// CR 702.24a: Parse a sequence of mana costs separated by " or ", e.g.,
/// `"{G} or {W}"` for Elephant Grass-style cumulative upkeep, or `"{1}{R} or
/// {2}{B}"` for hybrid alternatives. Returns `Some(Vec<ManaCost>)` only when at
/// least two alternatives are present — a single mana cost is *not* a
/// disjunction and should fall through to the caller's plain mana-cost branch.
///
/// This is a building block for any disjunctive mana cost (cumulative upkeep,
/// kicker, additional cost, alternative cost) — not just cumulative upkeep.
///
/// Implementation: `separated_list1(tag(" or "), parse_mana_cost_nom)` with a
/// trailing `all_consuming` guard. `parse_mana_cost_nom` is a nom-compatible
/// wrapper over the legacy `parse_mana_symbols` helper (which returns
/// `Option<(ManaCost, &str)>` rather than an `IResult`).
pub(crate) fn parse_or_separated_mana_costs(
    text: &str,
) -> Option<Vec<crate::types::mana::ManaCost>> {
    let (_, costs) = all_consuming(separated_list1(
        tag::<_, _, super::oracle_nom::error::OracleError<'_>>(" or "),
        parse_mana_cost_nom,
    ))
    .parse(text.trim())
    .ok()?;
    if costs.len() < 2 {
        None
    } else {
        Some(costs)
    }
}

/// Nom-compatible wrapper over `parse_mana_symbols`. Consumes a single
/// brace-delimited mana cost (`{G}`, `{2}{U}`, etc.) and returns the parsed
/// `ManaCost` plus the remaining input. Fails as a nom error if the input
/// doesn't start with a mana symbol.
fn parse_mana_cost_nom(
    input: &str,
) -> super::oracle_nom::error::OracleResult<'_, crate::types::mana::ManaCost> {
    match parse_mana_symbols(input) {
        Some((cost, rest)) => Ok((rest, cost)),
        None => Err(nom::Err::Error(
            super::oracle_nom::error::OracleError::from_error_kind(
                input,
                nom::error::ErrorKind::Tag,
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        ControllerRef, ObjectScope, SharedQuality, TypeFilter, TypedFilter,
    };
    use crate::types::counter::CounterMatch;
    use crate::types::mana::{ManaCost, ManaCostShard};

    #[test]
    fn cost_tap() {
        assert_eq!(parse_oracle_cost("{T}"), AbilityCost::Tap);
    }

    // CR 702.24a: `parse_or_separated_mana_costs` building-block tests.
    // Covers disjunctive mana costs used by cumulative upkeep, kicker
    // alternatives, and any other "{X} or {Y}" mana cost class.

    #[test]
    fn parse_or_separated_mana_costs_two_alternatives() {
        let r = parse_or_separated_mana_costs("{G} or {W}").unwrap();
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn parse_or_separated_mana_costs_single_returns_none() {
        assert!(parse_or_separated_mana_costs("{G}").is_none());
    }

    #[test]
    fn parse_or_separated_mana_costs_three_alternatives() {
        let r = parse_or_separated_mana_costs("{G} or {W} or {U}").unwrap();
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn parse_or_separated_mana_costs_multi_symbol_costs() {
        // CR 702.24a: alternatives can be multi-symbol, not just single pips.
        let r = parse_or_separated_mana_costs("{1}{R} or {2}{B}").unwrap();
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn parse_or_separated_mana_costs_trailing_text_rejected() {
        // Trailing non-mana text must cause the helper to return None so the
        // caller can fall through to a more general parser.
        assert!(parse_or_separated_mana_costs("{G} or {W} and pay 1 life").is_none());
    }

    // Tests for issue #2394: Marath, Will of the Wild - variable X counter removal
    #[test]
    fn parse_remove_x_counters_uses_x_sentinel() {
        let (count, counter_type) = parse_remove_counter_quantity_and_kind("X +1/+1 counters")
            .expect("should parse X counter removal");

        assert_eq!(
            count, REMOVE_COUNTER_COST_X,
            "X should be encoded as the X sentinel"
        );
        assert!(matches!(counter_type, CounterMatch::OfType(_)));
    }

    #[test]
    fn parse_remove_numeric_counters_uses_actual_value() {
        let (count, counter_type) = parse_remove_counter_quantity_and_kind("3 +1/+1 counters")
            .expect("should parse numeric counter removal");

        assert_eq!(count, 3, "numeric value should be preserved");
        assert!(matches!(counter_type, CounterMatch::OfType(_)));
    }

    #[test]
    fn parse_any_number_of_counters_uses_any_number_sentinel() {
        let (count, counter_type) =
            parse_remove_counter_quantity_and_kind("any number of +1/+1 counters")
                .expect("should parse 'any number of' counter removal");

        assert_eq!(
            count, REMOVE_COUNTER_COST_ANY_NUMBER,
            "'any number of' should be encoded separately from literal X"
        );
        assert!(matches!(counter_type, CounterMatch::OfType(_)));
    }

    // "Remove one or more [type] counters" is a player-chosen variable count
    // (CR 107.3 / 601.2b), not a literal 1. Before the fix, parse_number ate
    // "one" as 1 and "or more +1/+1" leaked into a Generic counter type.
    #[test]
    fn parse_remove_one_or_more_counters_uses_x_sentinel() {
        let (count, counter_type) =
            parse_remove_counter_quantity_and_kind("one or more +1/+1 counters")
                .expect("should parse 'one or more' counter removal");

        assert_eq!(
            count, REMOVE_COUNTER_COST_X,
            "'one or more' should be encoded as the X sentinel, not literal 1"
        );
        assert_eq!(
            counter_type,
            CounterMatch::OfType(crate::types::counter::CounterType::Plus1Plus1),
            "counter type must be typed +1/+1, not Generic(\"or more +1/+1\")"
        );
    }

    #[test]
    fn parse_remove_one_or_more_generic_counters_uses_x_sentinel() {
        let (count, counter_type) =
            parse_remove_counter_quantity_and_kind("one or more charge counters")
                .expect("should parse 'one or more' generic counter removal");

        assert_eq!(count, REMOVE_COUNTER_COST_X);
        assert_eq!(
            counter_type,
            CounterMatch::OfType(crate::types::counter::CounterType::Generic(
                "charge".to_string()
            )),
        );
    }

    // No-regression: the new tag("one or more ") must NOT over-match a bare
    // singular "one [type] counter", which is a literal count of 1.
    #[test]
    fn parse_remove_one_singular_counter_uses_literal_one() {
        let (count, counter_type) = parse_remove_counter_quantity_and_kind("one +1/+1 counter")
            .expect("should parse singular 'one' counter removal");

        assert_eq!(count, 1, "bare 'one' is a literal count of 1");
        assert_eq!(
            counter_type,
            CounterMatch::OfType(crate::types::counter::CounterType::Plus1Plus1),
        );
    }

    #[test]
    fn cost_untap() {
        assert_eq!(parse_oracle_cost("{Q}"), AbilityCost::Untap);
    }

    #[test]
    fn cost_two_generic_hybrid_mana() {
        assert_eq!(
            parse_oracle_cost("{2/U}{2/B}{2/R}{2/G}"),
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 0,
                    shards: vec![
                        ManaCostShard::TwoBlue,
                        ManaCostShard::TwoBlack,
                        ManaCostShard::TwoRed,
                        ManaCostShard::TwoGreen,
                    ],
                },
            }
        );
    }

    #[test]
    fn cost_tapped_four_untapped_humans() {
        assert_eq!(
            parse_oracle_cost("Tapped four untapped Humans you control"),
            AbilityCost::TapCreatures {
                requirement: TapCreaturesRequirement::count(4),
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Subtype("Human".to_string())],
                    controller: Some(ControllerRef::You),
                    properties: Vec::new(),
                }),
            }
        );
    }

    #[test]
    fn cost_unattach_this_equipment() {
        assert_eq!(
            parse_oracle_cost("Unattach this Equipment"),
            AbilityCost::Unattach
        );
        assert_eq!(parse_oracle_cost("Unattach ~"), AbilityCost::Unattach);
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
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1))
        );
    }

    #[test]
    fn cost_return_self_to_hand() {
        assert_eq!(
            parse_oracle_cost("Return ~ to its owner's hand"),
            AbilityCost::ReturnToHand {
                count: 1,
                filter: Some(TargetFilter::SelfRef),
                from_zone: None,
            }
        );
    }

    #[test]
    fn cost_return_this_land_to_hand_is_self_ref() {
        assert_eq!(
            parse_oracle_cost("Return this land to its owner's hand"),
            AbilityCost::ReturnToHand {
                count: 1,
                filter: Some(TargetFilter::SelfRef),
                from_zone: None,
            }
        );
    }

    #[test]
    fn cost_return_card_name_to_hand_is_self_ref() {
        assert_eq!(
            parse_oracle_cost("Return Recurring Nightmare to its owner's hand"),
            AbilityCost::ReturnToHand {
                count: 1,
                filter: Some(TargetFilter::SelfRef),
                from_zone: None,
            }
        );
    }

    #[test]
    fn cost_recurring_nightmare_activation_cost_parses_self_return() {
        match parse_oracle_cost(
            "{2}{B}, Sacrifice a creature, Return Recurring Nightmare to its owner's hand",
        ) {
            AbilityCost::Composite { costs } => {
                assert!(costs.iter().any(|cost| matches!(
                    cost,
                    AbilityCost::ReturnToHand {
                        filter: Some(TargetFilter::SelfRef),
                        ..
                    }
                )));
            }
            other => panic!("expected composite cost, got {other:?}"),
        }
    }

    #[test]
    fn cost_sacrifice_creature() {
        match parse_oracle_cost("Sacrifice a creature") {
            AbilityCost::Sacrifice(cost) => {
                let target = &cost.target;
                assert!(matches!(
                    target,
                    TargetFilter::Typed(ref tf) if matches!(tf.get_primary_type(), Some(TypeFilter::Creature))
                ));
            }
            other => panic!("Expected Sacrifice, got {:?}", other),
        }
    }

    #[test]
    fn cost_sacrifice_another_permanent() {
        match parse_oracle_cost("Sacrifice another permanent") {
            AbilityCost::Sacrifice(cost) => {
                let TargetFilter::Typed(tf) = &cost.target else {
                    panic!("expected typed sacrifice target, got {:?}", cost.target);
                };
                assert!(
                    tf.type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Permanent)),
                    "expected permanent filter, got {:?}",
                    tf.type_filters
                );
                assert!(
                    tf.properties.contains(&FilterProp::Another),
                    "another permanent must carry FilterProp::Another, got {:?}",
                    tf.properties
                );
            }
            other => panic!("Expected Sacrifice, got {:?}", other),
        }
    }

    #[test]
    fn cost_sacrifice_any_number_nonland_permanents() {
        match parse_oracle_cost("Sacrifice any number of nonland permanents") {
            AbilityCost::Sacrifice(cost) => {
                assert_eq!(
                    cost.requirement,
                    crate::types::ability::SacrificeRequirement::Count { count: u32::MAX }
                );
                assert!(matches!(
                    cost.target,
                    TargetFilter::Typed(ref tf)
                        if tf.type_filters.iter().any(|f| matches!(f, TypeFilter::Non(_)))
                ));
            }
            other => panic!("Expected Sacrifice any number nonland, got {:?}", other),
        }
    }

    #[test]
    fn cost_sacrifice_x_squirrels() {
        match parse_oracle_cost("Sacrifice X Squirrels") {
            AbilityCost::Sacrifice(cost) => {
                assert_eq!(
                    cost.requirement,
                    crate::types::ability::SacrificeRequirement::Count { count: u32::MAX }
                );
                assert!(matches!(
                    cost.target,
                    TargetFilter::Typed(ref tf)
                        if tf
                            .type_filters
                            .iter()
                            .any(|filter| matches!(filter, TypeFilter::Subtype(name) if name == "Squirrel"))
                ));
            }
            other => panic!("Expected Sacrifice, got {:?}", other),
        }
    }

    #[test]
    fn cost_exile_x_cards_from_graveyard() {
        assert_eq!(
            parse_oracle_cost("Exile X cards from your graveyard"),
            AbilityCost::Exile {
                count: EXILE_COST_X,
                zone: Some(Zone::Graveyard),
                filter: None,
            }
        );
    }

    #[test]
    fn cost_exile_two_cards_from_graveyard() {
        assert_eq!(
            parse_oracle_cost("Exile two cards from your graveyard"),
            AbilityCost::Exile {
                count: 2,
                zone: Some(Zone::Graveyard),
                filter: None,
            }
        );
    }

    #[test]
    fn cost_tap_untapped_creature_you_control() {
        assert_eq!(
            parse_oracle_cost("Tap an untapped creature you control"),
            AbilityCost::TapCreatures {
                requirement: TapCreaturesRequirement::count(1),
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
        // Regression: flat "Pay 2 life" is unaffected by the for-each arm.
        assert_eq!(
            parse_oracle_cost("Pay 2 life"),
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 }
            }
        );
    }

    #[test]
    fn cost_pay_life_for_each_counter() {
        // CR 119.4 + CR 122.1: Tornado — "Pay 3 life for each velocity counter
        // on this enchantment". The per-counter multiplier must be preserved.
        let (_, expected_qty) = nom_quantity::parse_for_each_clause_ref_complete(
            "velocity counter on this enchantment",
        )
        .expect("for-each clause");
        assert!(matches!(
            expected_qty,
            QuantityRef::CountersOn {
                scope: crate::types::ability::ObjectScope::Source,
                counter_type: Some(_),
            }
        ));
        assert_eq!(
            parse_oracle_cost("Pay 3 life for each velocity counter on this enchantment"),
            AbilityCost::PayLife {
                amount: QuantityExpr::Multiply {
                    factor: 3,
                    inner: Box::new(QuantityExpr::Ref { qty: expected_qty }),
                },
            }
        );
    }

    #[test]
    fn cost_pay_life_for_each_creature() {
        // Building-block test: the for-each composition covers any
        // `parse_for_each_clause_ref` form, not just counter scopes. factor: 1 is
        // kept intentionally (resolves identically to a bare Ref).
        let (_, expected_qty) =
            nom_quantity::parse_for_each_clause_ref_complete("creature you control")
                .expect("for-each clause");
        assert_eq!(
            parse_oracle_cost("Pay 1 life for each creature you control"),
            AbilityCost::PayLife {
                amount: QuantityExpr::Multiply {
                    factor: 1,
                    inner: Box::new(QuantityExpr::Ref { qty: expected_qty }),
                },
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
                selection: crate::types::ability::CardSelectionMode::Chosen,
                self_scope: crate::types::ability::DiscardSelfScope::FromHand,
            }
        );
    }

    /// CR 701.9a + CR 608.2c: A typed cost-form discard must capture its
    /// card-type filter (Lotleth Troll: "Discard a creature card:"). Before this
    /// the filter was dropped, letting any card pay the cost.
    #[test]
    fn cost_discard_typed_creature_card() {
        assert_eq!(
            parse_oracle_cost("Discard a creature card"),
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: Some(TargetFilter::Typed(TypedFilter::creature())),
                selection: crate::types::ability::CardSelectionMode::Chosen,
                self_scope: crate::types::ability::DiscardSelfScope::FromHand,
            }
        );
    }

    /// The typed-filter arm must not swallow plural untyped discards: "Discard
    /// two cards" stays `filter: None, count: 2`.
    #[test]
    fn cost_discard_two_untyped_cards_keeps_no_filter() {
        assert_eq!(
            parse_oracle_cost("Discard two cards"),
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 2 },
                filter: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                self_scope: crate::types::ability::DiscardSelfScope::FromHand,
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
                selection: crate::types::ability::CardSelectionMode::Chosen,
                self_scope: crate::types::ability::DiscardSelfScope::SourceCard,
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
                selection: crate::types::ability::CardSelectionMode::Chosen,
                self_scope: crate::types::ability::DiscardSelfScope::FromHand,
            }
        );
    }

    #[test]
    fn cost_composite_tap_mana_sacrifice() {
        match parse_oracle_cost("{T}, {2}{B}, Sacrifice a creature") {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 3);
                assert_eq!(costs[0], AbilityCost::Tap);
                assert!(matches!(costs[2], AbilityCost::Sacrifice(_)));
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
    fn cost_exile_colored_card_with_mana_value_x_from_hand() {
        use crate::types::ability::{Comparator, FilterProp, QuantityExpr, QuantityRef};

        match parse_oracle_cost("Exile a green card with mana value X from your hand") {
            AbilityCost::Exile {
                zone,
                filter: Some(TargetFilter::Typed(typed)),
                ..
            } => {
                assert_eq!(zone, Some(Zone::Hand));
                assert!(typed.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::HasColor {
                        color: crate::types::mana::ManaColor::Green
                    }
                )));
                assert!(typed.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Cmc {
                        comparator: Comparator::EQ,
                        value: QuantityExpr::Ref {
                            qty: QuantityRef::Variable { name }
                        }
                    } if name == "X"
                )));
            }
            other => panic!("Expected Exile with green + CmcEQ(X), got {:?}", other),
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

    /// CR 602.2b + CR 601.2f + CR 102.1: the "during your turn[s]" timing-gated
    /// flat form maps to a `Fixed(1)` reduction gated by `IsYourTurn`, for both
    /// the activate and cast verb axes and both turn-plurality forms. This is the
    /// building-block test behind Hylda's Crown of Winter.
    #[test]
    fn cost_reduction_during_your_turn_maps_to_is_your_turn() {
        use crate::types::ability::ParsedCondition;
        for text in [
            "this ability costs {1} less to activate during your turn",
            "this ability costs {2} less to activate during your turns",
            "this spell costs {1} less to cast during your turn",
        ] {
            let r = try_parse_cost_reduction(text).unwrap_or_else(|| panic!("must parse: {text}"));
            assert_eq!(r.count, QuantityExpr::Fixed { value: 1 }, "{text}");
            assert_eq!(
                r.condition,
                Some(ParsedCondition::IsYourTurn),
                "during-your-turn must gate on IsYourTurn: {text}"
            );
        }
        // "{2} less to activate during your turn" keeps amount_per = 2.
        let two =
            try_parse_cost_reduction("this ability costs {2} less to activate during your turns")
                .unwrap();
        assert_eq!(two.amount_per, 2);
    }

    /// CR 508.1a + CR 601.2f: the conditional flat form gated by "you attacked
    /// with a <filter>" extracts a filtered `YouAttackedWithAtLeast { count: 1 }`.
    /// The trailing "this turn" is stripped upstream as a duration before the
    /// reparse, so the bare form is what reaches the reducer (Thaumaton Torpedo).
    #[test]
    fn cost_reduction_if_attacked_with_filter_gate() {
        use crate::types::ability::ParsedCondition;
        let r = try_parse_cost_reduction(
            "this ability costs {3} less to activate if you attacked with a spacecraft",
        )
        .expect("must parse filtered attacked-with gate");
        assert_eq!(r.amount_per, 3);
        assert_eq!(r.count, QuantityExpr::Fixed { value: 1 });
        match r.condition {
            Some(ParsedCondition::YouAttackedWithAtLeast {
                count: 1,
                filter: Some(TargetFilter::Typed(tf)),
            }) => assert!(
                tf.type_filters
                    .iter()
                    .any(|f| matches!(f, TypeFilter::Subtype(s) if s == "Spacecraft")),
                "expected Spacecraft subtype filter, got {:?}",
                tf.type_filters
            ),
            other => panic!("expected filtered attacked-with gate, got {other:?}"),
        }
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
            AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 1 }
            }
        );
    }

    #[test]
    fn cost_pay_energy_triple() {
        assert_eq!(
            parse_oracle_cost("Pay {E}{E}{E}"),
            AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 3 }
            }
        );
    }

    #[test]
    fn cost_return_land_to_hand() {
        match parse_oracle_cost("Return a land you control to its owner's hand") {
            AbilityCost::ReturnToHand {
                count,
                filter,
                from_zone,
            } => {
                assert_eq!(count, 1);
                assert!(filter.is_some());
                assert!(from_zone.is_none());
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
                from_zone: None,
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
    fn cost_remove_counter_from_permanent_you_control() {
        match parse_oracle_cost("Remove a counter from a permanent you control") {
            AbilityCost::RemoveCounter {
                count,
                counter_type,
                target: Some(TargetFilter::Typed(filter)),
                selection,
            } => {
                assert_eq!(count, 1);
                assert_eq!(counter_type, CounterMatch::Any);
                assert_eq!(selection, CounterCostSelection::SingleObject);
                assert_eq!(filter.controller, Some(ControllerRef::You));
                assert!(
                    filter
                        .type_filters
                        .iter()
                        .any(|filter| matches!(filter, TypeFilter::Permanent)),
                    "expected permanent filter, got {:?}",
                    filter.type_filters
                );
            }
            other => panic!("Expected targeted RemoveCounter cost, got {:?}", other),
        }
    }

    #[test]
    fn cost_remove_x_counters_from_among_creatures_you_control() {
        match parse_oracle_cost("Remove X counters from among creatures you control") {
            AbilityCost::RemoveCounter {
                count,
                counter_type,
                target: Some(TargetFilter::Typed(filter)),
                selection,
            } => {
                assert_eq!(count, REMOVE_COUNTER_COST_X);
                assert_eq!(counter_type, CounterMatch::Any);
                assert_eq!(selection, CounterCostSelection::AmongObjects);
                assert_eq!(filter.controller, Some(ControllerRef::You));
                assert!(
                    filter
                        .type_filters
                        .iter()
                        .any(|filter| matches!(filter, TypeFilter::Creature)),
                    "expected creature filter, got {:?}",
                    filter.type_filters
                );
            }
            other => panic!("Expected from-among RemoveCounter cost, got {:?}", other),
        }
    }

    /// Regression: Tekuthal's activation cost is "{1}{U/P}{U/P}, Remove three counters from
    /// among other artifacts, creatures, and planeswalkers you control". The comma-separated
    /// type list is part of a single RemoveCounter cost, not three separate cost parts.
    /// Reverts to three Unimplemented parts (coverage gap) if split_cost_parts incorrectly
    /// breaks on the internal commas.
    #[test]
    fn cost_tekuthal_remove_three_counters_from_among_or_types() {
        match parse_oracle_cost(
            "{1}{U/P}{U/P}, Remove three counters from among other artifacts, creatures, and planeswalkers you control",
        ) {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 2, "expected mana + remove-counter, got {:?}", costs);
                assert!(matches!(costs[0], AbilityCost::Mana { .. }), "part 0 should be Mana");
                match &costs[1] {
                    AbilityCost::RemoveCounter { count, counter_type, target: Some(target), selection } => {
                        assert_eq!(*count, 3);
                        assert_eq!(*counter_type, CounterMatch::Any);
                        assert_eq!(*selection, CounterCostSelection::AmongObjects);
                        match target {
                            TargetFilter::Or { filters } => {
                                assert_eq!(filters.len(), 3, "expected 3 OR legs (artifact|creature|planeswalker), got {filters:?}");
                                let types: Vec<_> = filters.iter().filter_map(|f| {
                                    if let TargetFilter::Typed(t) = f { Some(t) } else { None }
                                }).collect();
                                assert_eq!(types.len(), 3, "all legs should be Typed filters");
                                for typed in &types {
                                    assert_eq!(typed.controller, Some(ControllerRef::You), "each leg needs 'you control'");
                                    assert!(typed.properties.contains(&FilterProp::Another), "each leg needs 'other'");
                                }
                                let all_types: Vec<TypeFilter> = types.iter().flat_map(|t| t.type_filters.iter().cloned()).collect();
                                assert!(all_types.iter().any(|t| matches!(t, TypeFilter::Artifact)));
                                assert!(all_types.iter().any(|t| matches!(t, TypeFilter::Creature)));
                                assert!(all_types.iter().any(|t| matches!(t, TypeFilter::Planeswalker)));
                            }
                            other => panic!("expected Or filter for 3-type cost, got {other:?}"),
                        }
                    }
                    other => panic!("expected RemoveCounter with target, got {other:?}"),
                }
            }
            other => panic!("expected Composite cost, got {:?}", other),
        }
    }

    #[test]
    fn cost_from_among_type_list_does_not_swallow_later_cost() {
        match parse_oracle_cost(
            "{1}, Remove three counters from among other artifacts, creatures, and planeswalkers you control, Sacrifice a creature",
        ) {
            AbilityCost::Composite { costs } => {
                assert_eq!(
                    costs.len(),
                    3,
                    "expected mana + remove-counter + sacrifice, got {costs:?}"
                );
                assert!(matches!(costs[0], AbilityCost::Mana { .. }));
                assert!(matches!(
                    costs[1],
                    AbilityCost::RemoveCounter {
                        target: Some(TargetFilter::Or { .. }),
                        selection: CounterCostSelection::AmongObjects,
                        ..
                    }
                ));
                match &costs[2] {
                    AbilityCost::Sacrifice(sacrifice) => {
                        assert_eq!(sacrifice.requirement.fixed_count(), Some(1));
                        match &sacrifice.target {
                            TargetFilter::Typed(filter) => assert!(
                                filter
                                    .type_filters
                                    .iter()
                                    .any(|filter| matches!(filter, TypeFilter::Creature)),
                                "expected creature sacrifice, got {filter:?}"
                            ),
                            other => panic!("expected typed creature sacrifice, got {other:?}"),
                        }
                    }
                    other => panic!("expected trailing Sacrifice cost, got {other:?}"),
                }
            }
            other => panic!("expected Composite cost, got {other:?}"),
        }
    }

    #[test]
    fn cost_remove_counter_from_self_stays_source_cost() {
        assert_eq!(
            parse_oracle_cost("Remove a +1/+1 counter from ~"),
            AbilityCost::RemoveCounter {
                count: 1,
                counter_type: CounterMatch::OfType(crate::types::counter::CounterType::Plus1Plus1),
                target: None,
                selection: CounterCostSelection::SingleObject,
            }
        );
    }

    #[test]
    fn cost_remove_any_number_of_storage_counters_from_self() {
        assert_eq!(
            parse_oracle_cost("Remove any number of storage counters from ~"),
            AbilityCost::RemoveCounter {
                count: REMOVE_COUNTER_COST_ANY_NUMBER,
                counter_type: CounterMatch::OfType(crate::types::counter::CounterType::Generic(
                    "storage".to_string()
                ),),
                target: None,
                selection: CounterCostSelection::SingleObject,
            }
        );
    }

    #[test]
    fn cost_remove_any_number_of_charge_counters_from_self() {
        assert_eq!(
            parse_oracle_cost("Remove any number of charge counters from ~"),
            AbilityCost::RemoveCounter {
                count: REMOVE_COUNTER_COST_ANY_NUMBER,
                counter_type: CounterMatch::OfType(crate::types::counter::CounterType::Generic(
                    "charge".to_string()
                ),),
                target: None,
                selection: CounterCostSelection::SingleObject,
            }
        );
    }

    #[test]
    fn cost_remove_any_number_of_counters_from_self() {
        assert_eq!(
            parse_oracle_cost("Remove any number of counters from ~"),
            AbilityCost::RemoveCounter {
                count: REMOVE_COUNTER_COST_ANY_NUMBER,
                counter_type: CounterMatch::Any,
                target: None,
                selection: CounterCostSelection::SingleObject,
            }
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

    /// CR 118.3: Mirrodin Shard cycle — "{3}, {T} or {R}, {T}" produces
    /// OneOf([Composite([Mana(3), Tap]), Composite([Mana(R), Tap])]).
    /// The " or " splits the entire cost into two alternatives.
    #[test]
    fn cost_tap_or_mana_granite_shard() {
        match parse_oracle_cost("{3}, {T} or {R}, {T}") {
            AbilityCost::OneOf { costs } => {
                assert_eq!(costs.len(), 2, "expected 2 alternatives, got {:?}", costs);
                // Left alternative: {3}, {T}
                match &costs[0] {
                    AbilityCost::Composite { costs: left } => {
                        assert_eq!(left.len(), 2);
                        assert!(matches!(&left[0], AbilityCost::Mana { .. }));
                        assert_eq!(left[1], AbilityCost::Tap);
                    }
                    other => panic!("Expected Composite for left alt, got {:?}", other),
                }
                // Right alternative: {R}, {T}
                match &costs[1] {
                    AbilityCost::Composite { costs: right } => {
                        assert_eq!(right.len(), 2);
                        assert!(matches!(&right[0], AbilityCost::Mana { .. }));
                        assert_eq!(right[1], AbilityCost::Tap);
                    }
                    other => panic!("Expected Composite for right alt, got {:?}", other),
                }
            }
            other => panic!("Expected OneOf, got {:?}", other),
        }
    }

    /// Crystal Shard uses blue: "{3}, {T} or {U}, {T}".
    #[test]
    fn cost_tap_or_mana_crystal_shard() {
        match parse_oracle_cost("{3}, {T} or {U}, {T}") {
            AbilityCost::OneOf { costs } => {
                assert_eq!(costs.len(), 2);
                // Left: {3}, {T}
                assert!(matches!(&costs[0], AbilityCost::Composite { .. }));
                // Right: {U}, {T}
                assert!(matches!(&costs[1], AbilityCost::Composite { .. }));
            }
            other => panic!("Expected OneOf, got {:?}", other),
        }
    }

    /// Standalone "{T} or {G}" — two single-cost alternatives.
    #[test]
    fn cost_tap_or_mana_standalone() {
        match parse_oracle_cost("{T} or {G}") {
            AbilityCost::OneOf { costs } => {
                assert_eq!(costs.len(), 2);
                assert_eq!(costs[0], AbilityCost::Tap);
                assert!(matches!(&costs[1], AbilityCost::Mana { .. }));
            }
            other => panic!("Expected OneOf, got {:?}", other),
        }
    }

    /// CR 602.2b + CR 601.2f: the conditional flat form "costs {N} less to activate if
    /// [condition]" parses to a `CostReduction` with `count = Fixed(1)` and a
    /// `condition` gate (Esquire of the King, Razorlash Transmogrant, …) — the
    /// previously-dropped `Effect:this` clause.
    #[test]
    fn cost_reduction_conditional_flat_form_carries_condition() {
        let def = try_parse_cost_reduction(
            "this ability costs {2} less to activate if you control a legendary creature",
        )
        .expect("conditional cost reduction should parse");
        assert_eq!(def.amount_per, 2);
        assert_eq!(def.count, QuantityExpr::Fixed { value: 1 });
        assert!(
            def.condition.is_some(),
            "the 'if [condition]' gate must be captured, got {:?}",
            def.condition
        );

        // "if you're <something the condition parser doesn't model>" must NOT
        // silently mis-parse: an unrecognized condition yields no reduction
        // (stays a loud gap) rather than an unconditional one.
        assert!(
            try_parse_cost_reduction(
                "this ability costs {2} less to activate if the moon is gibbous"
            )
            .is_none(),
            "unparseable condition must not produce an (unconditional) reduction"
        );
    }

    #[test]
    fn cost_reduction_conditional_opponent_nonbasic_lands() {
        let reduction = try_parse_cost_reduction(
            "this ability costs {4} less to activate if an opponent controls four or more nonbasic lands",
        )
        .expect("opponent nonbasic land gate should parse");
        assert_eq!(reduction.amount_per, 4);
        assert_eq!(reduction.count, QuantityExpr::Fixed { value: 1 });
        assert!(reduction.condition.is_some());
    }

    /// Regression: the "for each" scaling form is unchanged and carries no
    /// condition.
    #[test]
    fn cost_reduction_for_each_form_unconditional() {
        let def = try_parse_cost_reduction(
            "this ability costs {1} less to activate for each artifact you control",
        )
        .expect("for-each cost reduction should still parse");
        assert_eq!(def.amount_per, 1);
        assert_eq!(def.condition, None, "for-each form is unconditional");
        assert!(
            !matches!(def.count, QuantityExpr::Fixed { .. }),
            "for-each count is a dynamic ref, not Fixed"
        );
    }

    /// CR 305.6 + CR 601.2f: domain-scaled cost reduction — "costs {N} less to
    /// activate/cast for each basic land type among lands you control" — must
    /// resolve to the `BasicLandTypeCount` (domain) quantity. Covers Jodah's
    /// Codex / Wandering Treefolk / Radha's Firebrand (activate) and Scion of
    /// Draco (cast). Regression for the previously-dropped `for each` domain arm.
    #[test]
    fn cost_reduction_for_each_basic_land_type_is_domain() {
        use crate::types::ability::{ControllerRef, QuantityRef};

        // Activated-ability form (Jodah's Codex, Wandering Treefolk).
        let activate = try_parse_cost_reduction(
            "this ability costs {1} less to activate for each basic land type among lands you control",
        )
        .expect("domain cost reduction (activate) should parse");
        assert_eq!(activate.amount_per, 1);
        assert_eq!(activate.condition, None);
        assert_eq!(
            activate.count,
            QuantityExpr::Ref {
                qty: QuantityRef::BasicLandTypeCount {
                    controller: ControllerRef::You,
                },
            },
        );

        // Spell form (Scion of Draco).
        let cast = try_parse_cost_reduction(
            "this spell costs {2} less to cast for each basic land type among lands you control",
        )
        .expect("domain cost reduction (cast) should parse");
        assert_eq!(cast.amount_per, 2);
        assert_eq!(
            cast.count,
            QuantityExpr::Ref {
                qty: QuantityRef::BasicLandTypeCount {
                    controller: ControllerRef::You,
                },
            },
        );
    }

    /// CR 105.1 + CR 601.2f + CR 115.1: Dragonfire Blade — equip cost scales
    /// with the number of colors on the creature chosen as the equip target.
    #[test]
    fn cost_reduction_for_each_color_of_creature_it_targets() {
        use crate::types::ability::{QuantityExpr, QuantityRef};

        let reduction = try_parse_cost_reduction(
            "this ability costs {1} less to activate for each color of the creature it targets",
        )
        .expect("Dragonfire Blade equip discount should parse");
        assert_eq!(reduction.amount_per, 1);
        assert_eq!(reduction.condition, None);
        assert_eq!(
            reduction.count,
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectColorCount {
                    scope: crate::types::ability::ObjectScope::Target,
                },
            },
        );
    }

    /// #3223: the self cost-reduction *head* recognizer matches both the bare
    /// sentence and a sentence carrying a trailing "if [condition]" tail; it
    /// rejects unrelated effect sentences. Drives the upstream
    /// `strip_suffix_conditional` decline that keeps the whole sentence intact.
    #[test]
    fn is_self_cost_reduction_prefix_matches_head_and_full_sentence() {
        assert!(is_self_cost_reduction_prefix(
            "this ability costs {2} less to activate"
        ));
        assert!(is_self_cost_reduction_prefix(
            "this ability costs {2} less to activate if you control a legendary creature"
        ));

        // Scoped to the activated-ability form only. The spell form ("this spell
        // costs {N} less to cast") is parsed through a different path and must
        // NOT be suppressed here (would strand its condition — Lashwhip Predator).
        assert!(!is_self_cost_reduction_prefix(
            "this spell costs {1} less to cast"
        ));
        assert!(!is_self_cost_reduction_prefix(
            "this spell costs {2} less to cast if your opponents control three or more creatures"
        ));

        assert!(!is_self_cost_reduction_prefix("this ability gains haste"));
        assert!(!is_self_cost_reduction_prefix(
            "creatures you control get +1/+1"
        ));
        assert!(!is_self_cost_reduction_prefix("draw a card"));
    }

    /// CR 107.3c: The dynamic-{X} head still routes through the self
    /// cost-reduction prefix recognizer so the upstream suffix splitter keeps
    /// the whole "..., where X is ..." sentence intact (it must reach
    /// `try_parse_cost_reduction`). Verifies the assumption that no change to
    /// `is_self_cost_reduction_prefix` is needed.
    #[test]
    fn is_self_cost_reduction_prefix_matches_dynamic_x_head() {
        assert!(is_self_cost_reduction_prefix(
            "this ability costs {x} less to activate, where x is the number of differently named lands you control"
        ));
        assert!(is_self_cost_reduction_prefix(
            "this ability costs {x} less to activate, where x is this creature's power"
        ));
    }

    /// CR 107.3c: Survey Mechan — "{X} less to activate, where X is the number
    /// of differently named lands you control" maps to a dynamic count
    /// (`amount_per: 1`, `count = Ref(ObjectCountDistinct[Name])`), not a
    /// player-chosen X. Discriminating: a revert (no {X} arm) returns `None`,
    /// flipping the `expect`.
    #[test]
    fn cost_reduction_dynamic_x_differently_named_lands() {
        let reduction = try_parse_cost_reduction(
            "this ability costs {x} less to activate, where x is the number of differently named lands you control",
        )
        .expect("dynamic-X cost reduction should parse");
        assert_eq!(reduction.amount_per, 1);
        assert_eq!(reduction.condition, None);
        match &reduction.count {
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCountDistinct { qualities, filter },
            } => {
                assert_eq!(qualities.as_slice(), [SharedQuality::Name]);
                assert!(
                    matches!(
                        filter,
                        TargetFilter::Typed(TypedFilter {
                            controller: Some(ControllerRef::You),
                            ..
                        })
                    ),
                    "expected lands you control, got {filter:?}"
                );
            }
            other => panic!("Expected ObjectCountDistinct[Name], got {other:?}"),
        }
    }

    /// CR 107.3c: The Dominion Bracelet (granted ability) — "{X} less to
    /// activate, where X is this creature's power" maps to `Power { scope:
    /// Source }`. Confirms the arm covers the whole `parse_quantity_ref`
    /// vocabulary, not just object counts.
    #[test]
    fn cost_reduction_dynamic_x_this_creatures_power() {
        let reduction = try_parse_cost_reduction(
            "this ability costs {x} less to activate, where x is this creature's power",
        )
        .expect("dynamic-X power cost reduction should parse");
        assert_eq!(reduction.amount_per, 1);
        assert_eq!(reduction.condition, None);
        assert!(
            matches!(
                reduction.count,
                QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Source
                    }
                }
            ),
            "expected Power(Source), got {:?}",
            reduction.count
        );
    }

    /// Honesty: an unrecognized where-X phrase stays an honest gap (`None`),
    /// never a misparse. Discriminating against an "always Some" arm.
    #[test]
    fn cost_reduction_dynamic_x_unrecognized_returns_none() {
        assert!(try_parse_cost_reduction(
            "this ability costs {x} less to activate, where x is the florble"
        )
        .is_none());
    }

    /// The dynamic-X arm also accepts the "less to cast" verb (spell form),
    /// covering both activation and cast cost-reduction families.
    #[test]
    fn cost_reduction_dynamic_x_spell_verb() {
        let reduction = try_parse_cost_reduction(
            "this spell costs {x} less to cast, where x is the number of differently named lands you control",
        )
        .expect("dynamic-X spell cost reduction should parse");
        assert_eq!(reduction.amount_per, 1);
        assert!(matches!(
            reduction.count,
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCountDistinct { .. }
            }
        ));
    }
}
