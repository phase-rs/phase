use std::collections::HashSet;

use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::Parser;

use crate::types::ability::{
    AbilityDefinition, AbilityKind, CounterTriggerFilter, Duration, Effect, QuantityExpr,
    ReplacementDefinition, TargetFilter, TriggerDefinition,
};
use crate::types::counter::CounterType;
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
/// composable combinator with structured `OracleError` diagnostics, rather than
/// chained `split_once` calls.
fn split_on_chapter_separator(line: &str) -> Option<(&str, &str)> {
    for sep in [" — ", " - "] {
        let parse =
            nom::bytes::complete::take_until::<_, _, OracleError<'_>>(sep).and(tag::<
                _,
                _,
                OracleError<'_>,
            >(sep));
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
    let normalized_title = title.trim_end_matches(['!', '?']).trim_end();
    let looks_like_title = !normalized_title.is_empty()
        && normalized_title.len() < 40
        && normalized_title
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        && !normalized_title.contains(['.', ',', ';', ':']);
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
    let result: nom::IResult<&str, &str, OracleError<'_>> = alt((tag("•"), tag("·"))).parse(line);
    result.is_ok()
}

/// Return shape of [`parse_saga_chapters`]: source-line-tagged chapter triggers,
/// the `(line, ETB replacement)` pair, and the set of consumed line indices.
type SagaChaptersParse = (
    Vec<(usize, TriggerDefinition)>,
    (usize, ReplacementDefinition),
    HashSet<usize>,
);

/// CR 714: Parse all chapter lines from a Saga's Oracle text.
/// Returns (chapter_triggers, etb_replacement, consumed_line_indices).
pub(crate) fn parse_saga_chapters(lines: &[&str], _card_name: &str) -> SagaChaptersParse {
    // Each chapter carries its source line index so `parse_oracle_ir` can emit its
    // trigger(s) in printed source order (unit-4 c2). A multi-numeral chapter line
    // (CR 714.2c: "I, II — [Effect]") yields one trigger per numeral, all on the
    // SAME line; emitting them in numeral order gives ascending ordinals on the
    // shared `(first_line, start_byte)` key, i.e. correct printed order.
    let mut chapters: Vec<(Vec<u32>, String, usize)> = Vec::new();
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
            chapters.push((nums, effect, idx));
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
    for (nums, effect_text, line_idx) in &chapters {
        for &n in nums {
            // CR 701.38 (Council's-dilemma / Will-of-the-council vote): a saga
            // chapter may itself be a vote (Trial of a Time Lord IV: "Starting
            // with you, each player votes for innocent or guilty. If guilty
            // gets more votes, ..."). The vote dispatcher recognizes the entire
            // opener + outcome clauses as one synthesized Vote effect; chain
            // parsing would mis-split the opener and leave the outcome clauses
            // Unimplemented. Try it first, mirroring the spell-line dispatch in
            // `oracle.rs`.
            let mut execute =
                match crate::parser::oracle_vote::parse_vote_block(effect_text, AbilityKind::Spell)
                {
                    Some(vote_def) => vote_def,
                    None => parse_effect_chain(effect_text, AbilityKind::Spell),
                };
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
            triggers.push((*line_idx, trigger));
        }
    }

    // CR 714.3a: As a Saga enters the battlefield, its controller puts a lore counter on it.
    let etb_replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Lore,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Battlefield)
        .description("Saga ETB lore counter".to_string());

    // CR 714.3a: the ETB lore-counter replacement has no printed line of its own;
    // anchor it at the FIRST chapter's line so it emits at/near the front of the
    // document (preserving today's `replacements[0]` position). Falls back to line
    // 0 for a degenerate Saga with no parsed chapters.
    let etb_line = chapters.first().map_or(0, |(_, _, idx)| *idx);

    (triggers, (etb_line, etb_replacement), consumed)
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

    /// u4-c2 test shim: map the source-line-tagged preprocessor return back to the
    /// bare-def shape the existing assertions read.
    fn saga_test_chapters(
        lines: &[&str],
        name: &str,
    ) -> (
        Vec<TriggerDefinition>,
        ReplacementDefinition,
        std::collections::HashSet<usize>,
    ) {
        let (triggers, (_, etb), consumed) = parse_saga_chapters(lines, name);
        (
            triggers.into_iter().map(|(_, t)| t).collect(),
            etb,
            consumed,
        )
    }
    use crate::types::ability::{
        ContinuousModification, ControllerRef, FilterProp, PtValue, TypeFilter,
    };

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

        // FIN Summon titles can carry emphatic punctuation.
        let (nums, effect) = parse_chapter_line(
            "I, II, III, IV — Stampede! — Other creatures you control get +1/+0 until end of turn.",
        )
        .unwrap();
        assert_eq!(nums, vec![1, 2, 3, 4]);
        assert_eq!(
            effect,
            "Other creatures you control get +1/+0 until end of turn."
        );

        // No title: plain chapter still works
        let (nums, effect) = parse_chapter_line("II — Create a 1/1 green Saproling.").unwrap();
        assert_eq!(nums, vec![2]);
        assert_eq!(effect, "Create a 1/1 green Saproling.");
    }

    #[test]
    fn emphatic_chapter_title_keeps_mass_pump_subject() {
        let lines = vec![
            "I, II, III, IV — Stampede! — Other creatures you control get +1/+0 until end of turn.",
        ];
        let (triggers, _etb, _consumed) = saga_test_chapters(&lines, "Summon: Choco/Mog");
        assert_eq!(triggers.len(), 4);

        for trigger in triggers {
            let exec = trigger.execute.expect("chapter should have execute effect");
            match &*exec.effect {
                Effect::PumpAll {
                    power,
                    toughness,
                    target,
                } => {
                    assert_eq!(*power, PtValue::Fixed(1));
                    assert_eq!(*toughness, PtValue::Fixed(0));
                    match target {
                        TargetFilter::Typed(filter) => {
                            assert_eq!(filter.controller, Some(ControllerRef::You));
                            assert!(filter.type_filters.contains(&TypeFilter::Creature));
                            assert!(filter.properties.contains(&FilterProp::Another));
                        }
                        other => panic!("expected typed creature target, got {other:?}"),
                    }
                }
                other => panic!("expected PumpAll, got {other:?}"),
            }
            assert_eq!(exec.duration, Some(Duration::UntilEndOfTurn));
        }
    }

    #[test]
    fn summon_yojimbo_chapter_combat_tax_parses() {
        use crate::parser::oracle_effect::parse_effect;
        use crate::types::ability::{ContinuousModification, StaticCondition};
        use crate::types::statics::StaticMode;

        let lines = vec!["II, III — Until your next turn, creatures can't attack you unless their controller pays {2} for each of those creatures."];
        let (triggers, _etb, _consumed) = saga_test_chapters(&lines, "Summon: Yojimbo");
        assert_eq!(triggers.len(), 2);

        for trigger in &triggers {
            let exec = trigger.execute.as_ref().expect("chapter execute");
            assert!(
                matches!(exec.duration, Some(Duration::UntilNextTurnOf { .. })),
                "expected UntilNextTurnOf, got {:?}",
                exec.duration
            );
            match &*exec.effect {
                Effect::GenericEffect {
                    static_abilities,
                    target,
                    ..
                } => {
                    assert_eq!(target, &Some(TargetFilter::SelfRef));
                    let ContinuousModification::GrantStaticAbility { definition } =
                        &static_abilities[0].modifications[0]
                    else {
                        panic!("expected GrantStaticAbility combat tax");
                    };
                    assert!(matches!(definition.mode, StaticMode::CantAttack));
                    assert!(matches!(
                        definition.condition,
                        Some(StaticCondition::UnlessPay { .. })
                    ));
                }
                other => panic!("expected GenericEffect combat tax, got {other:?}"),
            }
        }

        let effect = parse_effect(
            "Until your next turn, creatures can't attack you unless their controller pays {2} for each of those creatures.",
        );
        assert!(
            matches!(effect, Effect::GenericEffect { .. }),
            "peeled duration combat tax must not be Unimplemented"
        );
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
        let (triggers, _etb, _consumed) = saga_test_chapters(&lines, "Urza's Saga");
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
        let (triggers, _etb, _consumed) = saga_test_chapters(&lines, "Urza's Saga");
        assert_eq!(triggers.len(), 1);
        let exec = triggers[0].execute.as_ref().unwrap();
        match &*exec.effect {
            Effect::GenericEffect { duration, .. } => {
                assert_eq!(duration.as_ref(), Some(&Duration::UntilHostLeavesPlay));
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
    }

    /// CR 111.3 (issue #4605): Urza's Saga chapter II grants
    /// `{2}, {T}: Create a 0/0 colorless Construct artifact creature token with
    /// 'This token gets +1/+1 for each artifact you control.'`. Because the
    /// create-token clause is nested inside the double-quoted granted ability,
    /// its inner token ability uses SINGLE quotes. The granted ability's effect
    /// must be a token-creation effect — NOT the inner `Pump` lifted out of the
    /// single-quoted span (which is what made activating it create no token).
    #[test]
    fn urzas_saga_chapter_two_granted_ability_creates_token() {
        let lines = vec![
            "II — This Saga gains \"{2}, {T}: Create a 0/0 colorless Construct artifact creature token with 'This token gets +1/+1 for each artifact you control.'\"",
        ];
        let (triggers, _etb, _consumed) = saga_test_chapters(&lines, "Urza's Saga");
        assert_eq!(triggers.len(), 1);
        let exec = triggers[0].execute.as_ref().unwrap();
        let Effect::GenericEffect {
            static_abilities, ..
        } = &*exec.effect
        else {
            panic!("expected GenericEffect, got {:?}", exec.effect);
        };
        let granted = static_abilities
            .iter()
            .flat_map(|s| s.modifications.iter())
            .find_map(|m| match m {
                ContinuousModification::GrantAbility { definition } => Some(definition),
                _ => None,
            })
            .expect("chapter II must grant an activated ability");
        assert!(
            matches!(&*granted.effect, Effect::Token { .. }),
            "granted ability must create a token, got {:?}",
            granted.effect
        );
    }

    /// CR 611.2b + CR 714.2b + CR 602.5: Roar of the Fifth People chapter II
    /// grants the Saga a static ability whose text is "Creatures you control have
    /// '{T}: Add {R}, {G}, or {W}.'". The nested single-quoted tap ability must
    /// parse as a `GrantStaticAbility` on creatures you control — NOT a broken
    /// activated ability on the Saga itself (#5978).
    #[test]
    fn roar_chapter_two_grants_creature_tap_mana_ability() {
        use crate::game::mana_abilities::is_mana_ability;
        use crate::types::ability::{
            ContinuousModification, ControllerRef, ManaProduction, TypeFilter,
        };
        use crate::types::mana::ManaColor;

        let lines = vec![
            "II — This Saga gains \"Creatures you control have '{T}: Add {R}, {G}, or {W}.'\"",
        ];
        let (triggers, _etb, _consumed) = saga_test_chapters(&lines, "Roar of the Fifth People");
        assert_eq!(triggers.len(), 1);
        let exec = triggers[0].execute.as_ref().unwrap();
        let Effect::GenericEffect {
            static_abilities,
            duration,
            ..
        } = &*exec.effect
        else {
            panic!("expected GenericEffect, got {:?}", exec.effect);
        };
        assert_eq!(
            duration.as_ref(),
            Some(&Duration::UntilHostLeavesPlay),
            "chapter-granted ability must persist while saga is in play"
        );
        let ContinuousModification::GrantStaticAbility { definition } =
            &static_abilities[0].modifications[0]
        else {
            panic!(
                "expected GrantStaticAbility for nested static grant, got {:?}",
                static_abilities[0].modifications
            );
        };
        let affected = definition
            .affected
            .as_ref()
            .expect("inner static must scope creatures you control");
        match affected {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("expected typed creature filter, got {other:?}"),
        }
        let ContinuousModification::GrantAbility {
            definition: granted,
        } = &definition
            .modifications
            .iter()
            .find(|m| matches!(m, ContinuousModification::GrantAbility { .. }))
            .expect("inner static must grant the tap mana ability")
        else {
            unreachable!();
        };
        assert!(
            is_mana_ability(granted),
            "granted ability must be a mana ability"
        );
        assert!(
            matches!(&*granted.effect, Effect::Mana { .. }),
            "granted ability must add mana, got {:?}",
            granted.effect
        );
        if let Effect::Mana {
            produced: ManaProduction::AnyOneColor { color_options, .. },
            ..
        } = &*granted.effect
        {
            assert!(color_options.contains(&ManaColor::Red));
            assert!(color_options.contains(&ManaColor::Green));
            assert!(color_options.contains(&ManaColor::White));
        } else {
            panic!("expected AnyOneColor mana production");
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
        let (triggers, _etb, _consumed) = saga_test_chapters(&lines, "Roar of the Fifth People");
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
        let (triggers, _etb, _consumed) = saga_test_chapters(&lines, "Urza's Saga");
        assert_eq!(triggers.len(), 1);
        let exec = triggers[0].execute.as_ref().unwrap();
        let Effect::SearchLibrary { filter, .. } = &*exec.effect else {
            panic!("expected SearchLibrary, got {:?}", exec.effect);
        };
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected typed artifact filter, got {filter:?}");
        };
        assert!(typed
            .type_filters
            .contains(&crate::types::ability::TypeFilter::Artifact));
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            crate::types::ability::FilterProp::ManaCostIn { costs }
                if costs == &vec![
                    crate::types::mana::ManaCost::zero(),
                    crate::types::mana::ManaCost::generic(1)
                ]
        )));
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

    /// Fable of the Mirror-Breaker chapter III: exile then return transformed.
    #[test]
    fn fable_chapter_three_exiles_then_returns_transformed() {
        let lines = vec![
            "III — Exile this Saga, then return it to the battlefield transformed under your control.",
        ];
        let (triggers, _etb, _consumed) = saga_test_chapters(&lines, "Fable of the Mirror-Breaker");
        assert_eq!(triggers.len(), 1);
        let exec = triggers[0].execute.as_ref().expect("chapter III execute");
        match &*exec.effect {
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            } => {}
            other => panic!("expected exile SelfRef clause 1, got {other:?}"),
        }
        let sub = exec.sub_ability.as_ref().expect("return transformed sub");
        assert!(
            !matches!(&*sub.effect, Effect::ChangeZoneAll { .. }),
            "chapter III return must be single-object ChangeZone so enter_transformed propagates"
        );
        match &*sub.effect {
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                target,
                enter_transformed,
                enters_under,
                ..
            } => {
                assert!(
                    matches!(
                        target,
                        TargetFilter::SelfRef
                            | TargetFilter::TrackedSet { .. }
                            | TargetFilter::ParentTarget
                    ),
                    "return target must refer to the exiled saga, got {target:?}"
                );
                assert!(*enter_transformed, "chapter III must return transformed");
                assert_eq!(
                    enters_under.as_ref(),
                    Some(&ControllerRef::You),
                    "chapter III must enter under your control"
                );
            }
            other => panic!("expected return transformed clause 2, got {other:?}"),
        }
    }

    /// CR 714.2 + CR 400.7i: The Legend of Roku chapter I (issue #1549) —
    /// exile top three, then grant play-from-exile until end of controller's
    /// next turn. The permission sub-ability must bind to `TrackedSet`, not
    /// the saga source.
    #[test]
    fn legend_of_roku_chapter_one_exiles_and_grants_play_permission() {
        use crate::types::ability::{
            CastingPermission, Duration, PlayerScope, QuantityExpr, TargetFilter,
        };
        use crate::types::identifiers::TrackedSetId;

        let lines = vec![
            "I — Exile the top three cards of your library. Until the end of your next turn, you may play those cards.",
        ];
        let (triggers, _etb, _consumed) = saga_test_chapters(&lines, "The Legend of Roku");
        assert_eq!(triggers.len(), 1);
        let exec = triggers[0].execute.as_ref().expect("chapter I execute");
        match &*exec.effect {
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 3 },
                face_down: false,
            } => {}
            other => panic!("expected ExileTop(controller, 3), got {other:?}"),
        }
        let sub = exec
            .sub_ability
            .as_ref()
            .expect("play permission sub-ability");
        match &*sub.effect {
            Effect::GrantCastingPermission {
                permission:
                    CastingPermission::PlayFromExile {
                        duration:
                            Duration::UntilEndOfNextTurnOf {
                                player: PlayerScope::Controller,
                            },
                        ..
                    },
                target:
                    TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
                ..
            } => {}
            other => panic!("expected PlayFromExile grant on TrackedSet, got {other:?}"),
        }
    }

    /// Issue #588: Good King Mog XII chapter IV mass counter placement must
    /// lower to PutCounterAll scoped to other Moogles you control.
    #[test]
    fn good_king_mog_chapter_four_counters_other_moogles_issue_588() {
        use crate::types::ability::{ControllerRef, FilterProp, QuantityExpr, TypeFilter};
        use crate::types::counter::CounterType;

        let lines = vec!["IV — Put two +1/+1 counters on each other Moogle you control."];
        let (triggers, _etb, _consumed) = saga_test_chapters(&lines, "Summon: Good King Mog XII");
        assert_eq!(triggers.len(), 1);
        let exec = triggers[0].execute.as_ref().expect("chapter IV execute");
        match &*exec.effect {
            Effect::PutCounterAll {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(*counter_type, CounterType::Plus1Plus1);
                assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
                let TargetFilter::Typed(tf) = target else {
                    panic!("expected Typed target, got {target:?}");
                };
                assert!(
                    tf.type_filters
                        .iter()
                        .any(|f| matches!(f, TypeFilter::Subtype(s) if s == "Moogle")),
                    "Moogle subtype must survive saga chapter lowering, got {:?}",
                    tf.type_filters
                );
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::Another));
            }
            other => panic!("expected PutCounterAll, got {other:?}"),
        }
    }
}
