//! CR 721 — Spacecraft pipe-delimited threshold lines ("N+ | body").
//!
//! Each line of the form `N+ | <body>` attaches a
//! `StaticCondition::HasCounters { counter_type: "charge", minimum: N, ... }`
//! gate to whatever ability the body describes:
//!
//! - Keyword-only body (e.g. `Flying`, `Flying, trample`) → `AddKeyword`
//!   continuous modifications on `SelfRef`.
//! - Static ability body → routed through `parse_static_line{,_multi}`.
//! - Trigger body (`when`, `whenever`, `at the beginning`) → routed through
//!   `parse_trigger_lines`.
//! - Activated ability body (with structural colon) → routed through
//!   `parse_oracle_cost` + `parse_effect_chain` like LEVEL blocks, and gated
//!   with a new `ActivationRestriction::CounterThreshold`.
//!
//! Mirrors `parse_level_blocks` in `oracle_level.rs` — the two patterns share
//! a counter-gated dispatch strategy but differ in layout (inline one-liner
//! vs multi-line block) and counter type (`charge` vs `level`). Keeping them
//! separate modules preserves the clarity of each pattern; extracting a
//! shared helper would obscure more than it saves.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::Parser;
use nom_language::error::VerboseError;

use super::oracle::{find_activated_colon, has_unimplemented, strip_activated_constraints};
use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::parse_effect_chain;
use super::oracle_keyword::parse_keyword_from_oracle;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_special::normalize_self_refs_for_static;
use super::oracle_static::{parse_static_line, parse_static_line_multi};
use super::oracle_trigger::parse_trigger_lines_at_index;

use crate::types::ability::{
    AbilityDefinition, AbilityKind, ActivationRestriction, ContinuousModification, StaticCondition,
    StaticDefinition, TargetFilter, TriggerCondition, TriggerDefinition,
};
use crate::types::counter::{CounterMatch, CounterType};

/// Counter type gating Spacecraft threshold lines (CR 702.184a / CR 721).
pub(crate) const STATION_COUNTER: &str = "charge";

/// CR 721.2a / CR 721.2b: Return the highest `N+` station-symbol threshold
/// printed on this Spacecraft, reading body lines that `parse_spacecraft_threshold_lines`
/// would also recognize. Used by the synthesis layer to derive the
/// creature-shift threshold from the striation with the printed P/T box,
/// rather than from reminder text (CR 721.3).
pub fn max_spacecraft_threshold(lines: &[&str]) -> Option<u32> {
    lines
        .iter()
        .filter_map(|raw| parse_threshold_header(raw.trim()).map(|(n, _)| n))
        .max()
}

/// Parse all `N+ | body` threshold lines in `lines`.
///
/// Returns parsed statics / triggers / activated abilities, plus the set of
/// consumed line indices (so the main oracle dispatcher can skip them).
///
/// `base_trigger_index` is the index that the *first* trigger emitted by this
/// parser will occupy in the card's full printed-trigger list (i.e. the
/// caller's `result.triggers.len()` at invocation time). It is forwarded to
/// `parse_trigger_lines_at_index` so any "and it has this ability" except
/// clause inside a Spacecraft threshold trigger body resolves to the correct
/// printed-trigger slot (CR 707.9a).
pub(crate) fn parse_spacecraft_threshold_lines(
    lines: &[&str],
    card_name: &str,
    base_trigger_index: usize,
) -> (
    Vec<StaticDefinition>,
    Vec<TriggerDefinition>,
    Vec<AbilityDefinition>,
    Vec<usize>,
) {
    let mut statics = Vec::new();
    let mut triggers: Vec<TriggerDefinition> = Vec::new();
    let mut abilities = Vec::new();
    let mut consumed = Vec::new();

    for (idx, raw) in lines.iter().enumerate() {
        let Some((threshold, body)) = parse_threshold_header(raw.trim()) else {
            continue;
        };
        consumed.push(idx);
        let body = body.trim();
        if body.is_empty() {
            continue;
        }

        let static_cond = StaticCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Generic(STATION_COUNTER.to_string())),
            minimum: threshold,
            maximum: None,
        };
        let trigger_cond = TriggerCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Generic(STATION_COUNTER.to_string())),
            minimum: threshold,
            maximum: None,
        };

        // Dispatch the body: keywords first (most common), then trigger /
        // static / activated branches modeled on `parse_level_blocks`.
        if let Some(keyword_mods) = parse_keyword_only_body(body) {
            let description = raw.trim().to_string();
            statics.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .condition(static_cond.clone())
                    .modifications(keyword_mods)
                    .description(description),
            );
            continue;
        }

        if has_trigger_prefix(body) {
            // CR 707.9a: Index for this trigger in the card's full printed
            // trigger list = caller-provided base + triggers already emitted
            // by this parser. Threading the index makes "has this ability"
            // retain the correct printed trigger slot for Spacecraft-gated
            // triggers.
            let mut parsed = parse_trigger_lines_at_index(
                body,
                card_name,
                Some(base_trigger_index + triggers.len()),
            );
            for trig in &mut parsed {
                trig.condition = Some(trigger_cond.clone());
            }
            triggers.extend(parsed);
            continue;
        }

        // Activated ability — structural colon with cost-like prefix.
        if let Some(colon_pos) = find_activated_colon(body) {
            let cost_text = body[..colon_pos].trim();
            let effect_text = body[colon_pos + 1..].trim();
            let (effect_text, constraints) = strip_activated_constraints(effect_text);
            let normalized_cost_text = normalize_self_refs_for_static(cost_text, card_name);
            let cost = parse_oracle_cost(&normalized_cost_text);

            let mut def = parse_effect_chain(&effect_text, AbilityKind::Activated);
            if has_unimplemented(&def) {
                let normalized_effect = normalize_self_refs_for_static(&effect_text, card_name);
                if normalized_effect != effect_text {
                    let alt = parse_effect_chain(&normalized_effect, AbilityKind::Activated);
                    if !has_unimplemented(&alt) {
                        def = alt;
                    }
                }
            }
            def.cost = Some(cost);
            def.description = Some(raw.trim().to_string());
            if constraints.sorcery_speed() {
                def.sorcery_speed = true;
            }
            let mut restrictions = constraints.restrictions;
            restrictions.push(ActivationRestriction::CounterThreshold {
                counters: CounterMatch::OfType(CounterType::Generic(STATION_COUNTER.to_string())),
                minimum: threshold,
                maximum: None,
            });
            def.activation_restrictions = restrictions;
            abilities.push(def);
            continue;
        }

        // Static ability body — multi-line or single — gated with charge threshold.
        let static_text = normalize_self_refs_for_static(body, card_name);
        let multi = parse_static_line_multi(&static_text);
        if !multi.is_empty() {
            for mut sd in multi {
                sd.condition = Some(static_cond.clone());
                statics.push(sd);
            }
            continue;
        }
        if let Some(mut sd) = parse_static_line(&static_text) {
            sd.condition = Some(static_cond.clone());
            statics.push(sd);
            continue;
        }

        // Unrecognized body — leave the line unconsumed so the main dispatcher
        // can diagnose it (it will end up as an `Unimplemented` fallback at
        // worst, which is the correct diagnostic behavior).
        consumed.pop();
    }

    (statics, triggers, abilities, consumed)
}

/// Parse the `N+ |` (or `N+|`) prefix, returning `(threshold, body)`.
///
/// Uses `parse_number` + `alt` over the pipe delimiter variants so the
/// detection is a composed nom pipeline, not a string-matching heuristic.
pub(crate) fn parse_threshold_header(line: &str) -> Option<(u32, &str)> {
    let (rest, n) = nom_primitives::parse_number(line).ok()?;
    let (rest, _) = alt((
        value((), tag::<_, _, VerboseError<&str>>("+ | ")),
        value((), tag("+| ")),
        value((), tag("+ |")),
        value((), tag("+|")),
    ))
    .parse(rest)
    .ok()?;
    Some((n, rest))
}

/// Parse a body that consists entirely of comma-or-`and`-separated keywords.
/// Returns `Some(mods)` where `mods` are `AddKeyword` modifications, or
/// `None` if any part fails to parse as a non-Unknown keyword.
fn parse_keyword_only_body(body: &str) -> Option<Vec<ContinuousModification>> {
    use crate::types::keywords::Keyword;

    // Split on commas, then on " and " — same strategy as `oracle_level.rs`
    // keyword extraction but tolerating the inline " and " conjunction.
    let parts: Vec<&str> = body
        .split([',', ';'])
        .flat_map(|p| {
            p.trim()
                .split(" and ")
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    let mut mods = Vec::with_capacity(parts.len());
    for part in parts {
        let lower = part.trim_end_matches('.').to_lowercase();
        let kw = parse_keyword_from_oracle(&lower)?;
        if matches!(kw, Keyword::Unknown(_)) {
            return None;
        }
        mods.push(ContinuousModification::AddKeyword { keyword: kw });
    }
    Some(mods)
}

/// Detect a trigger-prefix line (`when`, `whenever`, `at the beginning`) via
/// nom `tag` combinators — never substring matching.
fn has_trigger_prefix(body: &str) -> bool {
    let lower = body.to_lowercase();
    let result: Result<(&str, ()), nom::Err<VerboseError<&str>>> = alt((
        value((), tag::<_, _, VerboseError<&str>>("whenever ")),
        value((), tag("when ")),
        value((), tag("at the beginning")),
    ))
    .parse(&lower);
    result.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::counter::CounterType;

    #[test]
    fn threshold_header_nom_extracts_number_and_body() {
        assert_eq!(parse_threshold_header("3+ | Flying"), Some((3, "Flying")));
        assert_eq!(
            parse_threshold_header("12+ | Whenever you draw a card, draw a card."),
            Some((12, "Whenever you draw a card, draw a card."))
        );
        // No-space variant
        assert_eq!(parse_threshold_header("8+|Flying"), Some((8, "Flying")));
        // Rejects non-threshold lines
        assert!(parse_threshold_header("Flying").is_none());
        assert!(parse_threshold_header("{T}: Draw a card").is_none());
    }

    #[test]
    fn keyword_threshold_line_parses_to_addkeyword_static() {
        let lines = ["12+ | Flying"];
        let (statics, triggers, abilities, consumed) =
            parse_spacecraft_threshold_lines(&lines, "Test", 0);
        assert_eq!(consumed, vec![0]);
        assert!(triggers.is_empty());
        assert!(abilities.is_empty());
        assert_eq!(statics.len(), 1);
        assert_eq!(statics[0].modifications.len(), 1);
        assert!(matches!(
            statics[0].condition,
            Some(StaticCondition::HasCounters { minimum: 12, .. })
        ));
        assert_eq!(
            statics[0].affected,
            Some(TargetFilter::SelfRef),
            "keyword thresholds apply to SelfRef"
        );
    }

    #[test]
    fn multi_keyword_threshold_line_parses_all() {
        let lines = ["8+ | Flying, trample"];
        let (statics, _, _, consumed) = parse_spacecraft_threshold_lines(&lines, "Test", 0);
        assert_eq!(consumed, vec![0]);
        assert_eq!(statics.len(), 1);
        assert_eq!(statics[0].modifications.len(), 2);
    }

    #[test]
    fn trigger_threshold_line_parses_with_condition() {
        let lines = ["3+ | Whenever you cast an artifact spell, draw a card."];
        let (_, triggers, _, consumed) =
            parse_spacecraft_threshold_lines(&lines, "Uthros Research Craft", 0);
        assert_eq!(consumed, vec![0]);
        assert_eq!(triggers.len(), 1);
        assert!(matches!(
            triggers[0].condition,
            Some(TriggerCondition::HasCounters { minimum: 3, .. })
        ));
    }

    #[test]
    fn activated_threshold_line_gets_counter_threshold_restriction() {
        let lines = ["1+ | {T}: Draw a card."];
        let (_, _, abilities, consumed) = parse_spacecraft_threshold_lines(&lines, "Test", 0);
        assert_eq!(consumed, vec![0]);
        assert_eq!(abilities.len(), 1);
        let restr = &abilities[0].activation_restrictions;
        assert!(restr.iter().any(|r| matches!(
            r,
            ActivationRestriction::CounterThreshold {
                counters: CounterMatch::OfType(CounterType::Generic(ref name)),
                minimum: 1,
                maximum: None,
            } if name == "charge"
        )));
    }

    #[test]
    fn counter_type_is_charge() {
        // Guard against accidental regression into level/generic.
        assert_eq!(STATION_COUNTER, "charge");
        // And the runtime counter type used in gating is a Generic("charge").
        let _ = CounterType::Generic(STATION_COUNTER.to_string());
    }
}
