use crate::types::ability::{Effect, TargetFilter};

use super::super::oracle_target::parse_target;
use super::super::oracle_util::parse_number;

pub(super) fn try_parse_put_counter<'a>(lower: &str, text: &'a str) -> Option<(Effect, &'a str)> {
    // "put N {type} counter(s) on {target}"
    let after_put = lower[4..].trim();
    let (count, rest) = parse_number(after_put)?;
    // Next word is counter type (e.g. "+1/+1", "loyalty", "charge")
    let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let raw_type = &rest[..type_end];
    let counter_type = normalize_counter_type(raw_type);

    // Skip "counter" or "counters" keyword, then parse target after "on"
    let after_type = rest[type_end..].trim_start();
    let counter_word_len = if after_type.starts_with("counters") {
        "counters".len()
    } else if after_type.starts_with("counter") {
        "counter".len()
    } else {
        0
    };
    let after_counter_word = if counter_word_len > 0 {
        after_type[counter_word_len..].trim_start()
    } else {
        after_type
    };

    let (target, remainder) = if let Some(on_rest) = after_counter_word.strip_prefix("on ") {
        if on_rest.starts_with("this ")
            || on_rest.starts_with("~")
            || on_rest == "it"
            || on_rest.starts_with("it ")
            || on_rest.starts_with("itself")
        {
            (TargetFilter::SelfRef, "")
        } else {
            // Compute the byte offset into `text` for the "on " target portion.
            // Since Oracle text is ASCII, byte offsets between lower and text are identical.
            let on_offset = lower.len() - on_rest.len();
            let (target, rem) = parse_target(&text[on_offset..]);
            (target, rem)
        }
    } else {
        (TargetFilter::SelfRef, "")
    };

    Some((
        Effect::PutCounter {
            counter_type,
            count: count as i32,
            target,
        },
        remainder,
    ))
}

pub(super) fn try_parse_remove_counter(lower: &str) -> Option<Effect> {
    // "remove N {type} counter(s) from {target}"
    let after_remove = lower[7..].trim();
    let (count, rest) = parse_number(after_remove)?;
    let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let raw_type = &rest[..type_end];
    let counter_type = normalize_counter_type(raw_type);

    let after_type = rest[type_end..].trim_start();
    let after_counter_word = after_type
        .strip_prefix("counters")
        .or_else(|| after_type.strip_prefix("counter"))
        .map(|s| s.trim_start())?;

    let target_text = after_counter_word.strip_prefix("from ")?.trim();
    let target = if target_text.starts_with("this ")
        || target_text.starts_with("~")
        || target_text == "it"
        || target_text.starts_with("it ")
        || target_text.starts_with("itself")
    {
        TargetFilter::SelfRef
    } else {
        parse_target(target_text).0
    };

    Some(Effect::RemoveCounter {
        counter_type,
        count: count as i32,
        target,
    })
}

/// Normalize oracle-text counter type strings to canonical engine names.
pub(super) fn normalize_counter_type(raw: &str) -> String {
    match raw {
        "+1/+1" => "P1P1".to_string(),
        "-1/-1" => "M1M1".to_string(),
        other => other.to_string(),
    }
}

/// CR 121.5: Parse "put its counters on [target]" → MoveCounters effect.
/// "its" / "this creature's" are possessive pronouns referring to the ability source.
pub(super) fn try_parse_move_counters<'a>(lower: &str, text: &'a str) -> Option<(Effect, &'a str)> {
    let after_put = lower.strip_prefix("put ")?.trim();
    // Detect "its counters" / "this creature's counters"
    let after_possessive = after_put
        .strip_prefix("its counter")
        .or_else(|| after_put.strip_prefix("this creature's counter"))?;
    // Skip past optional "s" (counter vs counters) then expect " on "
    let after_counters = after_possessive
        .strip_prefix('s')
        .unwrap_or(after_possessive);
    let after_on = after_counters.strip_prefix(" on ")?;

    // Compute byte offset into original `text` for parse_target.
    let offset_in_text = text.len() - after_on.len();
    let (target, remainder) = parse_target(&text[offset_in_text..]);

    Some((
        Effect::MoveCounters {
            source: TargetFilter::SelfRef,
            counter_type: None,
            target,
        },
        remainder,
    ))
}
