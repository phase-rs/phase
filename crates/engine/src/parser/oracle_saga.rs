use std::collections::HashSet;

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::Parser;
use nom_language::error::VerboseError;

use crate::types::ability::{
    AbilityDefinition, AbilityKind, CounterTriggerFilter, Duration, Effect, QuantityExpr,
    ReplacementDefinition, TargetFilter, TriggerDefinition,
};
use crate::types::replacements::ReplacementEvent;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::oracle_effect::parse_effect_chain;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_util::strip_reminder_text;

/// Parse a roman numeral to u32. Handles I(1) through XX(20).
///
/// Delegates to the shared `nom_primitives::parse_roman_numeral` combinator,
/// but requires the entire input to be a roman numeral (no trailing non-roman text).
pub(crate) fn parse_roman_numeral(s: &str) -> Option<u32> {
    let (rest, val) = nom_primitives::parse_roman_numeral(s).ok()?;
    // The original function required the entire string to be a roman numeral.
    // The nom combinator consumes all roman chars, so verify nothing else remains.
    if !rest.is_empty() {
        return None;
    }
    Some(val)
}

/// Parse a saga chapter line. Returns (chapter_numbers, effect_text).
/// Handles "I — effect", "I, II — effect", "III, IV, V — effect" (arbitrary-length lists).
///
/// Also strips the optional flavor-name (chapter title) interjection used on FIN
/// Summon sagas, FIN warden sagas, Weatherseed Treaty, etc.:
/// `"I — Crescent Fang — Search your library…"` → effect = `"Search your library…"`.
pub(crate) fn parse_chapter_line(line: &str) -> Option<(Vec<u32>, String)> {
    // Split the line around the first chapter-separator (em dash preferred, hyphen fallback).
    let (prefix, effect) = split_on_chapter_separator(line)?;

    let nums: Vec<u32> = prefix
        .split(',')
        .filter_map(|part| parse_roman_numeral(part.trim()))
        .collect();

    if nums.is_empty() {
        return None;
    }

    Some((nums, strip_chapter_title(effect.trim()).to_string()))
}

/// Split a chapter line on its first chapter-separator (em dash `" — "` or hyphen
/// fallback `" - "`), returning `(prefix_before_separator, body_after_separator)`.
///
/// Uses `take_until` + `alt(tag,tag)` so the separator alternatives live in a single
/// composable combinator with structured `VerboseError` diagnostics, rather than
/// chained `split_once` calls.
fn split_on_chapter_separator(line: &str) -> Option<(&str, &str)> {
    for sep in [" — ", " - "] {
        let parse = nom::bytes::complete::take_until::<_, _, VerboseError<&str>>(sep).and(tag::<
            _,
            _,
            VerboseError<&str>,
        >(
            sep
        ));
        let mut parser = parse;
        if let Ok((body, (prefix, _))) = parser.parse(line) {
            return Some((prefix, body));
        }
    }
    None
}

/// Strip an optional chapter-title flavor-name prefix from a saga chapter effect.
///
/// Chapter titles (e.g. `"Crescent Fang"`, `"Jecht Beam"`, `"Domain"`) are purely
/// flavorful and have no game meaning. They appear as `"<Title> — <effect>"`
/// inside the chapter body, separated from the actual rules text by another em-dash.
///
/// Recognized by structure, not a name list: the prefix must be short, capitalized,
/// and free of sentence punctuation. Any effect that naturally contains an em-dash
/// would be highly unusual in Oracle text.
fn strip_chapter_title(effect: &str) -> &str {
    let Some((title, body)) = split_on_chapter_separator(effect) else {
        return effect;
    };
    let title = title.trim();
    let looks_like_title = !title.is_empty()
        && title.len() < 40
        && title.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && !title.contains(['.', ',', ';', ':', '!', '?']);
    if looks_like_title {
        body.trim()
    } else {
        effect
    }
}

/// Returns `true` if `line` is a bullet-list continuation of the previous chapter's
/// body (e.g. a `"• Option A"` entry under a `"Choose one —"` chapter).
///
/// Trailing keyword lines (`"Flying"`, `"Menace"`, `"Trample, haste"`) on FIN Summon
/// sagas and Weatherseed-era Wardens are *not* continuations — they belong to the
/// creature's keyword set and must flow through the general dispatcher's keyword
/// extractor (priority 1b in `oracle.rs`).
fn is_chapter_body_continuation(line: &str) -> bool {
    let result: nom::IResult<&str, &str, VerboseError<&str>> =
        alt((tag("•"), tag("·"))).parse(line);
    result.is_ok()
}

/// CR 714: Parse all chapter lines from a Saga's Oracle text.
/// Returns (chapter_triggers, etb_replacement, consumed_line_indices).
pub(crate) fn parse_saga_chapters(
    lines: &[&str],
    _card_name: &str,
) -> (
    Vec<TriggerDefinition>,
    ReplacementDefinition,
    HashSet<usize>,
) {
    let mut chapters: Vec<(Vec<u32>, String)> = Vec::new();
    let mut consumed = HashSet::new();

    for (idx, &line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let stripped = strip_reminder_text(trimmed);
        if stripped.is_empty() {
            continue;
        }

        if let Some((nums, effect)) = parse_chapter_line(&stripped) {
            chapters.push((nums, effect));
            consumed.insert(idx);
        } else if is_chapter_body_continuation(&stripped) && !chapters.is_empty() {
            // Multi-line chapter body: bullet-list continuation of previous chapter
            // (e.g. "I, II — Choose one —\n• Option A.\n• Option B.").
            chapters.last_mut().unwrap().1.push(' ');
            chapters.last_mut().unwrap().1.push_str(&stripped);
            consumed.insert(idx);
        }
        // Any other non-chapter line (trailing keyword like "Flying" on FIN Summon
        // sagas, or the reminder paragraph) is left for the general dispatcher.
    }

    let mut triggers = Vec::new();
    for (nums, effect_text) in &chapters {
        for &n in nums {
            let mut execute = parse_effect_chain(effect_text, AbilityKind::Spell);
            // CR 611.2b + CR 714.2b: A chapter ability that grants an ability with no
            // explicit duration in its Oracle text creates a continuous effect that
            // persists indefinitely. The general-purpose `try_parse_gain_quoted_ability`
            // path defaults to `UntilEndOfTurn` (correct for pump-spell sub-effects like
            // "target creature gains flying"), but for a Saga chapter that grants the
            // Saga itself an activated ability (e.g. Urza's Saga: "I — This Saga gains
            // '{T}: Add {C}.'", Roar of the Fifth People: "II — This Saga gains
            // 'Creatures you control have ...'"), the granted ability must persist
            // while the Saga is on the battlefield. Override the default end-of-turn
            // duration to `UntilHostLeavesPlay` when the chapter text has no explicit
            // duration suffix.
            promote_grant_duration_for_chapter(&mut execute, effect_text);
            let trigger = TriggerDefinition::new(TriggerMode::CounterAdded)
                .valid_card(TargetFilter::SelfRef)
                .counter_filter(CounterTriggerFilter {
                    counter_type: crate::types::counter::CounterType::Lore,
                    threshold: Some(n),
                })
                .execute(execute)
                .trigger_zones(vec![Zone::Battlefield])
                .description(format!("Chapter {n}"));
            triggers.push(trigger);
        }
    }

    // CR 714.3a: As a Saga enters the battlefield, its controller puts a lore counter on it.
    let etb_replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: "lore".to_string(),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Battlefield)
        .description("Saga ETB lore counter".to_string());

    (triggers, etb_replacement, consumed)
}

/// Check if a line is a saga chapter (e.g. "I —", "II —", "III —").
pub(crate) fn is_saga_chapter(lower: &str) -> bool {
    parse_chapter_line(lower).is_some()
}

/// CR 611.2b + CR 714.2b: When a Saga chapter grants the Saga an ability with no
/// explicit duration ("This Saga gains 'X.'"), promote the default `UntilEndOfTurn`
/// to `UntilHostLeavesPlay` so the granted ability persists while the Saga is on
/// the battlefield (and is automatically cleaned up at zone exit by
/// `prune_host_left_effects`).
///
/// Skip the promotion when the chapter text contains an explicit duration suffix
/// (e.g. Roar of the Fifth People IV: "Dinosaurs you control gain double strike
/// and trample until end of turn." — `strip_trailing_duration` already extracted
/// the explicit `UntilEndOfTurn` and we must preserve it).
fn promote_grant_duration_for_chapter(execute: &mut AbilityDefinition, chapter_text: &str) {
    if chapter_has_explicit_duration_suffix(chapter_text) {
        return;
    }
    promote_generic_effect_duration(&mut execute.effect);
}

/// Detect whether the chapter text carries an explicit duration suffix that
/// `strip_trailing_duration` would have honored. Lower-cases the text and tests
/// for the same suffix set the imperative-path stripper recognizes — keeping
/// the two in lockstep prevents this promoter from clobbering a parser-honored
/// explicit duration.
fn chapter_has_explicit_duration_suffix(chapter_text: &str) -> bool {
    let lower = chapter_text.to_lowercase();
    // Match against the suffix set in `oracle_effect::strip_trailing_duration`.
    // Trailing punctuation ('.', ',') is stripped before comparison so the
    // exact-suffix match holds for either "this turn" or "this turn." forms.
    let trimmed = lower
        .trim_end()
        .trim_end_matches(['.', ',', '!', '?'])
        .trim_end();
    const DURATION_SUFFIXES: &[&str] = &[
        " this turn",
        " until end of turn",
        " until the end of your next turn",
        " until the end of their next turn",
        " until their next turn",
        " until your next turn",
        " until ~ leaves the battlefield",
        " until this creature leaves the battlefield",
    ];
    // structural: not dispatch — content classification guard for the
    // chapter-grant duration promoter. Mirrors the suffix set in
    // `oracle_effect::strip_trailing_duration` (which itself uses `ends_with`).
    if DURATION_SUFFIXES.iter().any(|s| trimmed.ends_with(s)) {
        return true;
    }
    // CR 611.2b: "for as long as ..." conditions are also explicit durations.
    // structural: not dispatch — same guard role as the suffix check above.
    nom_primitives::scan_contains(trimmed, "for as long as")
}

/// Promote a top-level `GenericEffect` whose duration is the default
/// `UntilEndOfTurn` (or `None`) to `UntilHostLeavesPlay`.
///
/// Scope is intentionally tight — only the chapter's top-level effect is
/// considered. Saga chapters in the current dataset are flat single-effect
/// grants ("This Saga gains 'X.'") so deeper traversal is unnecessary; if a
/// future printing chains a one-shot effect with a sub-ability grant, extend
/// this walker to descend through `AbilityDefinition.sub_ability` as well.
fn promote_generic_effect_duration(effect: &mut Effect) {
    if let Effect::GenericEffect { duration, .. } = effect {
        match duration {
            None | Some(Duration::UntilEndOfTurn) => {
                *duration = Some(Duration::UntilHostLeavesPlay);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roman_numeral_range() {
        assert_eq!(parse_roman_numeral("I"), Some(1));
        assert_eq!(parse_roman_numeral("ii"), Some(2));
        assert_eq!(parse_roman_numeral("III"), Some(3));
        assert_eq!(parse_roman_numeral("IV"), Some(4));
        assert_eq!(parse_roman_numeral("v"), Some(5));
        assert_eq!(parse_roman_numeral("VI"), Some(6));
        assert_eq!(parse_roman_numeral("VII"), Some(7));
        assert_eq!(parse_roman_numeral("VIII"), Some(8));
        assert_eq!(parse_roman_numeral("IX"), Some(9));
        assert_eq!(parse_roman_numeral("X"), Some(10));
        assert_eq!(parse_roman_numeral("XI"), Some(11));
        assert_eq!(parse_roman_numeral("XII"), Some(12));
        assert_eq!(parse_roman_numeral("XIV"), Some(14));
        assert_eq!(parse_roman_numeral("XV"), Some(15));
        assert_eq!(parse_roman_numeral("XX"), Some(20));
        // Non-roman characters return None
        assert_eq!(parse_roman_numeral("ABC"), None);
    }

    #[test]
    fn parse_chapter_line_single() {
        let (nums, effect) = parse_chapter_line("I — Draw a card.").unwrap();
        assert_eq!(nums, vec![1]);
        assert_eq!(effect, "Draw a card.");
    }

    #[test]
    fn parse_chapter_line_multi() {
        let (nums, effect) = parse_chapter_line("I, II — Target creature gets +2/+0.").unwrap();
        assert_eq!(nums, vec![1, 2]);
        assert_eq!(effect, "Target creature gets +2/+0.");
    }

    #[test]
    fn parse_chapter_line_hyphen_fallback() {
        let (nums, effect) = parse_chapter_line("III - Destroy target creature.").unwrap();
        assert_eq!(nums, vec![3]);
        assert_eq!(effect, "Destroy target creature.");
    }

    #[test]
    fn parse_chapter_line_strips_flavor_title() {
        // FIN Summon saga pattern: "I — Crescent Fang — Search your library…"
        let (nums, effect) =
            parse_chapter_line("I — Crescent Fang — Search your library for a basic land card.")
                .unwrap();
        assert_eq!(nums, vec![1]);
        assert_eq!(effect, "Search your library for a basic land card.");

        // Multi-chapter with title: "I, II — Jecht Beam — Each opponent discards a card."
        let (nums, effect) =
            parse_chapter_line("I, II — Jecht Beam — Each opponent discards a card.").unwrap();
        assert_eq!(nums, vec![1, 2]);
        assert_eq!(effect, "Each opponent discards a card.");

        // Single-word title: Weatherseed Treaty "III — Domain — Target creature…"
        let (nums, effect) =
            parse_chapter_line("III — Domain — Target creature you control gets +X/+X.").unwrap();
        assert_eq!(nums, vec![3]);
        assert_eq!(effect, "Target creature you control gets +X/+X.");

        // No title: plain chapter still works
        let (nums, effect) = parse_chapter_line("II — Create a 1/1 green Saproling.").unwrap();
        assert_eq!(nums, vec![2]);
        assert_eq!(effect, "Create a 1/1 green Saproling.");
    }

    #[test]
    fn is_saga_chapter_extended() {
        assert!(is_saga_chapter("VI — Something"));
        assert!(is_saga_chapter("VII — Something"));
        assert!(is_saga_chapter("i — something"));
        assert!(!is_saga_chapter("Draw a card."));
    }

    /// CR 611.2b + CR 714.2b: Urza's Saga chapter I grants the Saga an activated
    /// mana ability with no Oracle-text duration. The chapter trigger's
    /// `GenericEffect` must carry `Duration::UntilHostLeavesPlay`, NOT the
    /// default `UntilEndOfTurn` — otherwise the granted `{T}: Add {C}` ability
    /// would vanish at the next cleanup step and never be activatable.
    #[test]
    fn urzas_saga_chapter_one_grants_persist_until_host_leaves_play() {
        let lines = vec![
            "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)",
            "I — This Saga gains \"{T}: Add {C}.\"",
        ];
        let (triggers, _etb, _consumed) = parse_saga_chapters(&lines, "Urza's Saga");
        assert_eq!(triggers.len(), 1, "expected one chapter trigger");
        let exec = triggers[0]
            .execute
            .as_ref()
            .expect("chapter trigger must have an execute body");
        match &*exec.effect {
            Effect::GenericEffect { duration, .. } => {
                assert_eq!(
                    duration.as_ref(),
                    Some(&Duration::UntilHostLeavesPlay),
                    "chapter-granted ability must persist while saga is in play"
                );
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
    }

    /// CR 611.2b + CR 714.2b: Urza's Saga chapter II grants `{2}, {T}: Create
    /// a 0/0 colorless Construct...`. Same persistence requirement as chapter I.
    #[test]
    fn urzas_saga_chapter_two_grants_persist_until_host_leaves_play() {
        let lines = vec![
            "II — This Saga gains \"{2}, {T}: Create a 0/0 colorless Construct artifact creature token with 'This token gets +1/+1 for each artifact you control.'\"",
        ];
        let (triggers, _etb, _consumed) = parse_saga_chapters(&lines, "Urza's Saga");
        assert_eq!(triggers.len(), 1);
        let exec = triggers[0].execute.as_ref().unwrap();
        match &*exec.effect {
            Effect::GenericEffect { duration, .. } => {
                assert_eq!(duration.as_ref(), Some(&Duration::UntilHostLeavesPlay));
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
    }

    /// CR 514.2: Roar of the Fifth People chapter IV explicitly says "until end
    /// of turn" — the explicit duration must NOT be promoted to
    /// `UntilHostLeavesPlay`. Regression guard for the promoter's
    /// "explicit-suffix → preserve" branch.
    #[test]
    fn explicit_until_end_of_turn_chapter_is_not_promoted() {
        let lines =
            vec!["IV — Dinosaurs you control gain double strike and trample until end of turn."];
        let (triggers, _etb, _consumed) = parse_saga_chapters(&lines, "Roar of the Fifth People");
        assert_eq!(triggers.len(), 1);
        let exec = triggers[0].execute.as_ref().unwrap();
        // If the parser produced something other than a GenericEffect (e.g. a
        // direct PumpAll), that's also acceptable — the regression we care
        // about is "GenericEffect with the wrong duration".
        if let Effect::GenericEffect { duration, .. } = &*exec.effect {
            assert_eq!(
                duration.as_ref(),
                Some(&Duration::UntilEndOfTurn),
                "explicit duration must be preserved by the saga-chapter promoter"
            );
        }
    }

    /// One-shot effect chapters (Search/Create/Destroy/Damage/etc.) don't go
    /// through `GenericEffect` at all, so the promoter must be a no-op. This
    /// test asserts the absence of regression: chapter III (search library) on
    /// Urza's Saga retains its `SearchLibrary` shape.
    #[test]
    fn one_shot_chapters_are_unaffected_by_promoter() {
        let lines = vec![
            "III — Search your library for an artifact card with mana cost {0} or {1}, put it onto the battlefield, then shuffle.",
        ];
        let (triggers, _etb, _consumed) = parse_saga_chapters(&lines, "Urza's Saga");
        assert_eq!(triggers.len(), 1);
        let exec = triggers[0].execute.as_ref().unwrap();
        // The exact effect kind is whatever the existing parser produced — we
        // only assert it is NOT a GenericEffect that the promoter touched.
        if let Effect::GenericEffect { duration, .. } = &*exec.effect {
            assert_ne!(
                duration.as_ref(),
                Some(&Duration::UntilHostLeavesPlay),
                "one-shot chapter must not be promoted to a persistent grant"
            );
        }
    }

    /// Detector unit test — `chapter_has_explicit_duration_suffix` must
    /// recognize the same suffix set that `oracle_effect::strip_trailing_duration`
    /// honors so the promoter never clobbers a parser-honored explicit duration.
    #[test]
    fn explicit_duration_suffix_detector() {
        assert!(chapter_has_explicit_duration_suffix(
            "Dinosaurs you control gain trample until end of turn."
        ));
        assert!(chapter_has_explicit_duration_suffix(
            "Target creature gains haste this turn."
        ));
        assert!(chapter_has_explicit_duration_suffix(
            "It gains flying until your next turn"
        ));
        assert!(chapter_has_explicit_duration_suffix(
            "Creatures you control get +1/+1 for as long as you control ~."
        ));
        assert!(!chapter_has_explicit_duration_suffix(
            "This Saga gains \"{T}: Add {C}.\""
        ));
        assert!(!chapter_has_explicit_duration_suffix(
            "Create a 1/1 green Saproling."
        ));
    }
}
