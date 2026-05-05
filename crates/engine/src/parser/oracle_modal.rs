use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::Parser;
use nom_language::error::VerboseError;

use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ModalChoice, ModalSelectionCondition,
    ModalSelectionConstraint,
};

use super::oracle::find_activated_colon;
use super::oracle_effect::{parse_effect_chain, parse_effect_chain_with_context};
use super::oracle_ir::context::ParseContext;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_util::{parse_mana_symbols, strip_reminder_text};
use crate::parser::oracle_ir::ast::{ModalHeaderAst, ModeAst, OracleBlockAst};
use crate::types::ability::TargetFilter;

pub(crate) fn parse_oracle_block(lines: &[&str], start: usize) -> Option<(OracleBlockAst, usize)> {
    let line = strip_reminder_text(lines.get(start)?.trim());
    if line.is_empty() {
        return None;
    }

    let modes = collect_mode_asts(lines, start + 1);
    if modes.is_empty() {
        return None;
    }

    let next = start + 1 + modes.len();

    if let Some(colon_pos) = find_activated_colon(&line) {
        let cost_text = line[..colon_pos].trim();
        let effect_text = line[colon_pos + 1..].trim();
        if let Some(header) = parse_modal_header_ast(effect_text) {
            return Some((
                OracleBlockAst::ActivatedModal {
                    cost_text: cost_text.to_string(),
                    header,
                    modes,
                },
                next,
            ));
        }
    }

    let candidate = strip_ability_word(&line).unwrap_or_else(|| line.clone());
    let lower = candidate.to_lowercase();

    if let Some(header) = parse_modal_header_ast(&candidate) {
        // Reject trigger prefixes — these are triggered modals, not plain modals
        if alt((
            tag::<_, _, VerboseError<&str>>("when "),
            tag("whenever "),
            tag("at "),
        ))
        .parse(lower.as_str())
        .is_err()
        {
            return Some((OracleBlockAst::Modal { header, modes }, next));
        }
    }

    if let Some((trigger_line, header)) = split_triggered_modal_header(&candidate) {
        if let Some(header) = parse_modal_header_ast(&header) {
            return Some((
                OracleBlockAst::TriggeredModal {
                    trigger_line,
                    header,
                    modes,
                },
                next,
            ));
        }
    }

    // CR 702.172: Spree keyword line + all modes have per-mode costs
    if line.eq_ignore_ascii_case("spree")
        && !modes.is_empty()
        && modes.iter().all(|m| m.mode_cost.is_some())
    {
        let header = ModalHeaderAst {
            raw: line.to_string(),
            min_choices: 1,
            max_choices: modes.len(),
            allow_repeat_modes: false,
            constraints: vec![],
        };
        return Some((OracleBlockAst::Modal { header, modes }, next));
    }

    if line.eq_ignore_ascii_case("tiered")
        && !modes.is_empty()
        && modes.iter().all(|m| m.mode_cost.is_some())
    {
        let header = ModalHeaderAst {
            raw: line.to_string(),
            min_choices: 1,
            max_choices: 1,
            allow_repeat_modes: false,
            constraints: vec![],
        };
        return Some((OracleBlockAst::Modal { header, modes }, next));
    }

    None
}

pub(crate) fn collect_mode_asts(lines: &[&str], start: usize) -> Vec<ModeAst> {
    let mut modes = Vec::new();

    for raw in lines.iter().skip(start) {
        let line = strip_reminder_text(raw.trim());
        if let Some(stripped) = line.strip_prefix('•') {
            modes.push(parse_mode_ast(stripped.trim()));
        } else if let Some(stripped) = line.strip_prefix('+') {
            // CR 702.172: Spree mode lines use `+ {cost} — effect` format
            let stripped = stripped.trim();
            if let Some((cost, rest)) = parse_mana_symbols(stripped) {
                // Strip " — " or " – " separator between cost and effect text
                let body = strip_mode_separator(rest);
                modes.push(ModeAst {
                    raw: body.to_string(),
                    label: None,
                    body: body.to_string(),
                    mode_cost: Some(cost),
                });
            } else {
                break; // Cost parse failure → stop collecting modes
            }
        } else {
            break;
        }
    }

    modes
}

fn parse_mode_ast(text: &str) -> ModeAst {
    if let Some((label, body)) = split_short_label_prefix(text, 4) {
        if let Some((cost, rest)) = parse_mana_symbols(body) {
            let body = strip_mode_separator(rest);
            return ModeAst {
                raw: text.to_string(),
                label: Some(label.to_string()),
                body: body.to_string(),
                mode_cost: Some(cost),
            };
        }

        return ModeAst {
            raw: text.to_string(),
            label: Some(label.to_string()),
            body: body.to_string(),
            mode_cost: None,
        };
    }

    ModeAst {
        raw: text.to_string(),
        label: None,
        body: text.to_string(),
        mode_cost: None,
    }
}

fn strip_mode_separator(text: &str) -> &str {
    let trimmed = text.trim();
    alt((
        tag::<_, _, VerboseError<&str>>("—"),
        tag::<_, _, VerboseError<&str>>("–"),
    ))
    .parse(trimmed)
    .map(|(rest, _)| rest.trim())
    .unwrap_or(trimmed)
}

pub(super) fn split_short_label_prefix(text: &str, max_words: usize) -> Option<(&str, &str)> {
    for sep in [" — ", " – ", " - "] {
        if let Some(pos) = text.find(sep) {
            let prefix = text[..pos].trim();
            let rest = text[pos + sep.len()..].trim();
            let word_count = prefix.split_whitespace().count();
            if (1..=max_words).contains(&word_count)
                && !prefix.contains('{')
                && !prefix.contains(':')
                && !rest.is_empty()
            {
                return Some((prefix, rest));
            }
        }
    }

    None
}

fn is_modal_header_text(lower: &str) -> bool {
    let lower = lower.trim();
    alt((
        tag::<_, _, VerboseError<&str>>("choose "),
        tag("you may choose "),
    ))
    .parse(lower)
    .is_ok()
        || (tag::<_, _, VerboseError<&str>>("if ").parse(lower).is_ok()
            && lower.contains("choose "))
}

pub(crate) fn parse_modal_header_ast(text: &str) -> Option<ModalHeaderAst> {
    let sentences: Vec<&str> = text
        .split('.')
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        .collect();
    let header_text = sentences.first().copied().unwrap_or(text).trim();
    let header_lower = header_text.to_lowercase();
    if !is_modal_header_text(&header_lower) {
        return None;
    }

    let (min_choices, max_choices) = parse_modal_choose_count(&text.to_lowercase());
    let mut allow_repeat_modes = false;
    let mut constraints = Vec::new();

    // CR 700.2: Detect cross-resolution mode restrictions from Oracle text.
    // The constraint phrase is part of the header sentence, not a period-delimited sub-sentence.
    // Order matters — "this turn" is the more specific substring.
    if header_lower.contains("that hasn't been chosen this turn") {
        constraints.push(ModalSelectionConstraint::NoRepeatThisTurn);
    } else if header_lower.contains("that hasn't been chosen") {
        constraints.push(ModalSelectionConstraint::NoRepeatThisGame);
    }

    if parse_commander_conditional_choose_both(&text.to_lowercase()).is_ok() {
        constraints.push(ModalSelectionConstraint::ConditionalMaxChoices {
            condition: ModalSelectionCondition::ControlsCommander,
            max_choices: 2,
            otherwise_max_choices: 1,
        });
    }

    for sentence in sentences.iter().skip(1) {
        let lower = sentence.to_lowercase();
        if lower == "you may choose the same mode more than once" {
            allow_repeat_modes = true;
            continue;
        }
        if lower == "each mode must target a different player" {
            constraints.push(ModalSelectionConstraint::DifferentTargetPlayers);
        }
    }

    Some(ModalHeaderAst {
        raw: text.to_string(),
        min_choices,
        max_choices,
        allow_repeat_modes,
        constraints,
    })
}

fn parse_commander_conditional_choose_both(
    input: &str,
) -> nom::IResult<&str, (), VerboseError<&str>> {
    let (rest, _) = tag("choose one.").parse(input.trim())?;
    let (rest, _) = tag(" if ").parse(rest)?;
    let (rest, _) = tag("you control a commander").parse(rest)?;
    let (rest, _) = tag(" as you cast this spell,").parse(rest)?;
    let (rest, _) = tag(" you may ").parse(rest)?;
    let (rest, _) = tag("choose both instead").parse(rest)?;
    Ok((rest, ()))
}

fn split_triggered_modal_header(line: &str) -> Option<(String, String)> {
    for (comma_pos, _) in line.match_indices(", ") {
        let trigger_line = line[..comma_pos].trim();
        let header = line[comma_pos + 2..].trim();
        if is_modal_header_text(&header.to_lowercase()) {
            return Some((trigger_line.to_string(), header.to_string()));
        }
    }

    None
}

pub(crate) fn lower_oracle_block(
    block: OracleBlockAst,
    card_name: &str,
    result: &mut super::oracle::ParsedAbilities,
) {
    use super::oracle_cost::parse_oracle_cost;
    use super::oracle_trigger::parse_trigger_lines;

    match block {
        OracleBlockAst::ActivatedModal {
            cost_text,
            header,
            modes,
        } => {
            let def = build_modal_ability(AbilityKind::Activated, &header, &modes)
                .cost(parse_oracle_cost(&cost_text));
            result.abilities.push(def);
        }
        OracleBlockAst::Modal { header, modes } => {
            let modal = build_modal_choice(&header, &modes);
            let mode_abilities = lower_mode_abilities(&modes, AbilityKind::Spell);
            result.abilities.extend(mode_abilities);
            result.modal = Some(modal);
        }
        OracleBlockAst::TriggeredModal {
            trigger_line,
            header,
            modes,
        } => {
            let mut triggers = parse_trigger_lines(&trigger_line, card_name);
            // CR 608.2k + CR 301.5a: Derive the trigger subject from the parsed
            // trigger so modal-mode pronoun anaphora ("that creature") binds to
            // `TriggeringSource` instead of an unbound `ParentTarget`. Pip-Boy
            // 3000's "Whenever equipped creature attacks ... put a +1/+1 counter
            // on that creature" is the canonical case; the modal parent is a
            // `GenericEffect` with no target, so without this threading the
            // "Pick a Perk" mode emits an unresolvable `ParentTarget`.
            let modal_subject = derive_modal_subject(&triggers);
            let modal_execute = Box::new(build_modal_ability_with_subject(
                AbilityKind::Spell,
                &header,
                &modes,
                modal_subject,
            ));
            for trigger in &mut triggers {
                trigger.execute = Some(modal_execute.clone());
            }
            result.triggers.extend(triggers);
        }
    }
}

pub(crate) fn build_modal_ability(
    kind: AbilityKind,
    header: &ModalHeaderAst,
    modes: &[ModeAst],
) -> AbilityDefinition {
    AbilityDefinition::new(kind, modal_marker_effect(header)).with_modal(
        build_modal_choice(header, modes),
        lower_mode_abilities(modes, kind),
    )
}

/// Build a modal ability with a trigger-context subject so mode-body pronoun
/// anaphora resolve against the triggering object (CR 608.2k + CR 301.5a).
fn build_modal_ability_with_subject(
    kind: AbilityKind,
    header: &ModalHeaderAst,
    modes: &[ModeAst],
    subject: Option<TargetFilter>,
) -> AbilityDefinition {
    AbilityDefinition::new(kind, modal_marker_effect(header)).with_modal(
        build_modal_choice(header, modes),
        lower_mode_abilities_with_subject(modes, kind, subject),
    )
}

/// CR 608.2k: Pick the trigger subject used to thread anaphoric pronoun
/// resolution into modal mode bodies. Returns `None` when the trigger has no
/// `valid_card` filter, when the filter is `SelfRef`/`Any`, or when there are
/// no triggers (defensive — the parser always emits at least one). Mirrors
/// `resolve_it_pronoun`'s gating: only non-self, non-Any subjects route mode-
/// body "that creature" to `TriggeringSource`; self-triggers (like Saga
/// chapters that name themselves) keep the legacy `ParentTarget` semantics.
fn derive_modal_subject(
    triggers: &[crate::types::ability::TriggerDefinition],
) -> Option<TargetFilter> {
    let trigger = triggers.first()?;
    let subject = trigger.valid_card.as_ref()?;
    match subject {
        TargetFilter::SelfRef | TargetFilter::Any => None,
        other => Some(other.clone()),
    }
}

fn modal_marker_effect(_header: &ModalHeaderAst) -> Effect {
    Effect::GenericEffect {
        static_abilities: vec![],
        duration: None,
        target: None,
    }
}

fn build_modal_choice(header: &ModalHeaderAst, modes: &[ModeAst]) -> ModalChoice {
    ModalChoice {
        min_choices: header.min_choices,
        max_choices: header.max_choices.min(modes.len()),
        mode_count: modes.len(),
        mode_descriptions: modes.iter().map(|mode| mode.raw.clone()).collect(),
        allow_repeat_modes: header.allow_repeat_modes,
        constraints: header.constraints.clone(),
        mode_costs: modes.iter().filter_map(|m| m.mode_cost.clone()).collect(),
        entwine_cost: None,
    }
}

fn lower_mode_abilities(modes: &[ModeAst], kind: AbilityKind) -> Vec<AbilityDefinition> {
    modes
        .iter()
        .map(|mode| {
            let parsed = parse_effect_chain(&mode.body, kind);
            guard_unsupported_mode_qualifiers(&mode.body, parsed, kind)
        })
        .collect()
}

/// Variant of `lower_mode_abilities` that threads a trigger subject through
/// mode-body parsing so anaphoric pronouns ("that creature") resolve against
/// the triggering object (CR 608.2k + CR 301.5a). When `subject` is `None`,
/// behavior is identical to `lower_mode_abilities`.
fn lower_mode_abilities_with_subject(
    modes: &[ModeAst],
    kind: AbilityKind,
    subject: Option<TargetFilter>,
) -> Vec<AbilityDefinition> {
    let mut ctx = ParseContext {
        subject,
        ..Default::default()
    };
    modes
        .iter()
        .map(|mode| {
            let parsed = parse_effect_chain_with_context(&mode.body, kind, &mut ctx);
            guard_unsupported_mode_qualifiers(&mode.body, parsed, kind)
        })
        .collect()
}

/// Replace a parsed mode ability with `Effect::Unimplemented` when the mode body
/// contains a filter qualifier that the current parser silently drops, which
/// would otherwise produce a rules-incorrect (overly-permissive) effect at
/// resolution time.
///
/// CR 700.2 (modal): A mode's effect must faithfully represent the printed
/// text. If the parser consumes a filter core but discards a restrictive
/// qualifier (e.g. "with total mana value 4 or less", "that's a creature or
/// Vehicle"), the resulting effect would execute against a broader class of
/// objects than the card allows. Marking such modes as Unimplemented is the
/// rules-safe fallback — the trigger/modal structure is preserved for the
/// coverage report, but the unsupported mode body does not execute.
///
/// The guard is intentionally conservative: it fires only on phrases that the
/// `parse_target` / `parse_dig_from_among` pipelines do not currently lower
/// into a typed constraint. When the relevant selection primitives
/// (e.g. `TotalManaValueAtMost`) or filter extensions (core-type + subtype
/// disjunction in `that's a X or Y`) are introduced, this guard will be
/// tightened to only fire on the residual unsupported forms.
fn guard_unsupported_mode_qualifiers(
    body: &str,
    parsed: AbilityDefinition,
    kind: AbilityKind,
) -> AbilityDefinition {
    let lower = body.to_lowercase();

    // Budgeted-selection qualifier on Dig-class modes — currently unsupported.
    // Example (Ao, the Dawn Sky): "Put any number of nonland permanent cards
    // with total mana value 4 or less from among them onto the battlefield."
    // Presence check only (word-boundary scan); not a parsing-dispatch `contains`.
    let dig_with_total_mv = matches!(&*parsed.effect, Effect::Dig { .. })
        && nom_primitives::scan_contains(&lower, "with total mana value");

    // "that's a X or Y" relative-clause narrowing on PutCounterAll/PutCounter
    // targets — parser drops the clause, producing an overly-permissive filter.
    // Example (Ao mode 2): "Put two +1/+1 counters on each permanent you control
    // that's a creature or Vehicle."
    let put_counter_with_thats_a = matches!(
        &*parsed.effect,
        Effect::PutCounterAll { .. } | Effect::PutCounter { .. }
    ) && nom_primitives::scan_contains(&lower, "that's a ");

    if dig_with_total_mv || put_counter_with_thats_a {
        return AbilityDefinition::new(
            kind,
            Effect::Unimplemented {
                name: "modal_mode_unsupported_qualifier".into(),
                description: Some(body.to_string()),
            },
        )
        .description(body.to_string());
    }

    parsed
}

/// Parse the "choose N" count from the modal header line.
///
/// Returns (min_choices, max_choices). Examples:
/// - "choose one —" → (1, 1)
/// - "choose two —" → (2, 2)
/// - "choose one or both —" → (1, 2)
/// - "choose one or more —" → (1, usize::MAX) (capped to mode_count at construction)
/// - "choose any number of —" → (1, usize::MAX)
pub(crate) fn parse_modal_choose_count(lower: &str) -> (usize, usize) {
    let lower = lower.trim();
    let lower = lower.strip_prefix("you may ").unwrap_or(lower).trim_start();

    // Scan for override phrases at word boundaries using nom combinators.
    if let Some(count) = scan_modal_count_override(lower) {
        return count;
    }
    // Extract the number word after "choose " using the shared nom combinator.
    if let Some(rest) = lower.strip_prefix("choose ") {
        if let Ok((_, n)) = nom_primitives::parse_number(rest) {
            return (n as usize, n as usize);
        }
    }
    // Default fallback
    (1, 1)
}

/// Strip an "ability word — " prefix from a line.
/// Ability words are italicized flavor prefixes before an em dash, e.g.:
/// "Landfall — Whenever a land enters..." → "Whenever a land enters..."
/// "Spell mastery — If there are two or more..." → "If there are two or more..."
pub(super) fn strip_ability_word(line: &str) -> Option<String> {
    split_short_label_prefix(line, 4).map(|(_, rest)| rest.to_string())
}

/// Strip an ability word prefix and also return the ability word name (lowercased).
/// Used for mapping known ability words to typed conditions (B7).
pub(super) fn strip_ability_word_with_name(line: &str) -> Option<(String, String)> {
    split_short_label_prefix(line, 4).map(|(name, rest)| (name.to_lowercase(), rest.to_string()))
}

/// Known ability-word names. Per CR 207.2c, ability words are italicized flavor
/// markers that tie together cards with similar functionality but have no rules
/// meaning — their body text must parse through ordinary trigger/effect/static
/// machinery. The list below unions CR 207.2c (the rulebook enumeration) with
/// the five new SOS ability words whose bodies carry real rules text inside
/// the parenthesized reminder. Paradigm is NOT an ability word — it's a real
/// keyword and lives in `oracle_keyword.rs`.
///
/// Used exclusively by parser dispatch (Pattern A: `<word> (body)` reminder
/// extraction). The list must stay lowercase and pre-trimmed so nom `tag()`
/// can match it on a lowercased input slice.
pub(super) const ABILITY_WORD_NAMES: &[&str] = &[
    // CR 207.2c
    "adamant",
    "addendum",
    "alliance",
    "battalion",
    "bloodrush",
    "celebration",
    "channel",
    "chroma",
    "cohort",
    "constellation",
    "converge",
    "council's dilemma",
    "coven",
    "delirium",
    "descend 4",
    "descend 8",
    "disappear",
    "domain",
    "eerie",
    "eminence",
    "enrage",
    "fateful hour",
    "fathomless descent",
    "ferocious",
    "flurry",
    "formidable",
    "grandeur",
    "hellbent",
    "heroic",
    "imprint",
    "inspired",
    "join forces",
    "kinship",
    "landfall",
    "lieutenant",
    "magecraft",
    "metalcraft",
    "morbid",
    "pack tactics",
    "paradox",
    "parley",
    "radiance",
    "raid",
    "rally",
    "renew",
    "revolt",
    "secret council",
    "spell mastery",
    "strive",
    "survival",
    "sweep",
    "tempting offer",
    "threshold",
    "undergrowth",
    "valiant",
    "vivid",
    "void",
    "will of the council",
    // SOS additions (flavor markers only — all rules live inside the reminder)
    "increment",
    "infusion",
    "opus",
    "repartee",
];

/// Match a known ability-word name at the start of a lowercased input, enforcing
/// a trailing word boundary. Returns the remainder after the name.
///
/// CR 207.2c: Ability words have no rules meaning; this combinator is purely
/// for parser dispatch — it lets the reminder-body extractor distinguish
/// `Increment (Whenever ...)` from random lines that happen to start with an
/// open paren.
pub(super) fn parse_known_ability_word_name(
    input: &str,
) -> nom::IResult<&str, &'static str, VerboseError<&str>> {
    for name in ABILITY_WORD_NAMES {
        if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>(*name).parse(input) {
            // Word-boundary guard: next char must be non-alphanumeric or end.
            if rest.is_empty() || !rest.chars().next().unwrap().is_alphanumeric() {
                return Ok((rest, *name));
            }
        }
    }
    Err(nom::Err::Error(VerboseError { errors: vec![] }))
}

/// Pattern A (CR 207.2c): Detect a line of the form `<ability-word> (<body>)`
/// where the body text lives ONLY inside the reminder parentheses and nothing
/// follows the closing paren. This is the SOS Increment/Opus/Repartee form
/// where the printed reminder IS the rules body. Returns the extracted body
/// (contents between the parens, trimmed) so the caller can dispatch it
/// through the normal per-line parser pipeline as if the ability word
/// weren't present.
///
/// Returns `None` for:
/// - lines without a recognized ability-word prefix,
/// - lines where text follows the closing `)`,
/// - bodies containing nested parens (current Oracle text does not nest),
/// - empty bodies.
pub(super) fn extract_ability_word_reminder_body(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let lower = trimmed.to_lowercase();
    let (after_name, _name) = parse_known_ability_word_name(&lower).ok()?;
    // Require exactly " (" between the name and the body — no em-dash, no colon.
    let after_space = after_name.strip_prefix(' ')?;
    let body_start_lower = after_space.strip_prefix('(')?;
    // Body must end with ')' and nothing (besides optional whitespace) after it.
    let (body_lower, tail_lower) = body_start_lower.rsplit_once(')')?;
    if !tail_lower.trim().is_empty() {
        return None;
    }
    if body_lower.trim().is_empty() {
        return None;
    }
    // structural: not dispatch — nested-paren guard. Oracle text does not nest
    // reminder text, so this rejects only malformed input.
    if body_lower.contains('(') {
        return None;
    }
    // Compute the matching byte range in the original-case string so we return
    // the body with original capitalization preserved.
    let body_start_byte = trimmed.len() - body_start_lower.len();
    let body_end_byte = body_start_byte + body_lower.len();
    Some(trimmed[body_start_byte..body_end_byte].trim().to_string())
}

/// Scan for modal count override phrases at word boundaries using nom combinators.
/// Returns (min_choices, max_choices) for matching phrases.
fn scan_modal_count_override(text: &str) -> Option<(usize, usize)> {
    use nom::sequence::preceded;
    super::oracle_nom::primitives::scan_at_word_boundaries(text, |input| {
        alt((
            value(
                (1, usize::MAX),
                tag::<_, _, VerboseError<&str>>("choose any number instead"),
            ),
            value((1, 2), tag("choose both instead")),
            value((1, 2), tag("choose two instead")),
            value((1, 3), tag("choose three instead")),
            value((1, 2), tag("one or both")),
            value(
                (1, usize::MAX),
                alt((tag("one or more"), tag("any number"))),
            ),
            // CR 700.2a / CR 700.2d: "choose up to N —" is a modal header where
            // min_choices = 0 (decline all modes) and max_choices = N.
            preceded(tag("choose up to "), nom_primitives::parse_number)
                .map(|n: u32| (0usize, n as usize)),
        ))
        .parse(input)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_ability_word_reminder_body_increment() {
        // CR 207.2c: SOS Increment — reminder body IS the rules text.
        let raw = "Increment (Whenever you cast a spell, if the amount of mana you spent is greater than this creature's power or toughness, put a +1/+1 counter on this creature.)";
        let body = extract_ability_word_reminder_body(raw).expect("should extract Increment body");
        assert!(body.starts_with("Whenever you cast a spell"));
        assert!(body.ends_with("put a +1/+1 counter on this creature."));
    }

    #[test]
    fn extract_ability_word_reminder_body_rejects_em_dash_form() {
        // The Infusion em-dash form is handled by `strip_ability_word_with_name`,
        // not by this extractor.
        let raw = "Infusion — If you gained life this turn, destroy all creatures instead.";
        assert_eq!(extract_ability_word_reminder_body(raw), None);
    }

    #[test]
    fn extract_ability_word_reminder_body_rejects_trailing_text() {
        // Body must be ONLY inside the parens; text after the closing paren
        // indicates a different pattern (e.g. a keyword with inline reminder).
        let raw = "Increment (reminder) extra text";
        assert_eq!(extract_ability_word_reminder_body(raw), None);
    }

    #[test]
    fn extract_ability_word_reminder_body_rejects_unknown_word() {
        // Non-ability-word prefixes must not trigger extraction, otherwise
        // keyword lines like "Ward (reminder)" would be falsely swallowed.
        let raw = "Wardwalk (When this creature enters, ...)";
        assert_eq!(extract_ability_word_reminder_body(raw), None);
    }

    #[test]
    fn extract_ability_word_reminder_body_preserves_original_case() {
        let raw =
            "Opus (Whenever you cast an instant or sorcery spell, put a +1/+1 counter on it.)";
        let body = extract_ability_word_reminder_body(raw).expect("should extract Opus body");
        assert!(body.starts_with("Whenever you cast an instant"));
    }

    #[test]
    fn parse_known_ability_word_enforces_word_boundary() {
        // "landfall" must match, but "landfallen" must not (different word).
        assert!(parse_known_ability_word_name("landfall — whenever").is_ok());
        assert!(parse_known_ability_word_name("landfallen").is_err());
    }

    #[test]
    fn parse_modal_choose_count_variants() {
        assert_eq!(parse_modal_choose_count("choose one —"), (1, 1));
        assert_eq!(parse_modal_choose_count("choose two —"), (2, 2));
        assert_eq!(parse_modal_choose_count("you may choose two."), (2, 2));
        assert_eq!(parse_modal_choose_count("choose three —"), (3, 3));
        assert_eq!(parse_modal_choose_count("choose one or both —"), (1, 2));
        assert_eq!(
            parse_modal_choose_count("choose one or more —"),
            (1, usize::MAX)
        );
        assert_eq!(
            parse_modal_choose_count("choose any number of —"),
            (1, usize::MAX)
        );
    }

    // B3: "choose up to N —" must parse as (0, N), not fall through to the
    // default (1, 1). Without this, players are forced to pick exactly one
    // mode when the CR allows zero. Affects Biblioplex Tomekeeper and ~96
    // other cards in the corpus (grep "choose up to" card-data.json).
    #[test]
    fn parse_modal_choose_count_up_to_variants() {
        assert_eq!(parse_modal_choose_count("choose up to one —"), (0, 1));
        assert_eq!(parse_modal_choose_count("choose up to two —"), (0, 2));
        assert_eq!(parse_modal_choose_count("choose up to seven —"), (0, 7));
        assert_eq!(
            parse_modal_choose_count("you may choose up to two."),
            (0, 2)
        );
    }

    #[test]
    fn modal_header_tracks_repeatable_modes() {
        let header = parse_modal_header_ast(
            "Choose up to five {P} worth of modes. You may choose the same mode more than once.",
        )
        .expect("header should parse");
        assert!(header.allow_repeat_modes);
    }

    #[test]
    fn modal_header_detects_no_repeat_this_turn_constraint() {
        let header = parse_modal_header_ast("choose one that hasn't been chosen this turn —")
            .expect("header should parse");
        assert_eq!(
            header.constraints,
            vec![ModalSelectionConstraint::NoRepeatThisTurn]
        );
    }

    #[test]
    fn modal_header_detects_no_repeat_this_game_constraint() {
        let header = parse_modal_header_ast("choose one that hasn't been chosen —")
            .expect("header should parse");
        assert_eq!(
            header.constraints,
            vec![ModalSelectionConstraint::NoRepeatThisGame]
        );
    }

    #[test]
    fn collect_mode_asts_plus_prefix_extracts_cost_and_body() {
        let lines = vec![
            "Spree",
            "+ {2} — Draw a card.",
            "+ {R} — Deal 3 damage to target creature.",
        ];
        let modes = collect_mode_asts(&lines, 1);
        assert_eq!(modes.len(), 2);
        assert!(modes[0].mode_cost.is_some());
        assert_eq!(modes[0].body, "Draw a card.");
        assert!(modes[1].mode_cost.is_some());
    }

    #[test]
    fn collect_mode_asts_standard_bullet_has_no_mode_cost() {
        let lines = vec!["Choose one —", "• Draw a card.", "• Gain 3 life."];
        let modes = collect_mode_asts(&lines, 1);
        assert_eq!(modes.len(), 2);
        assert!(modes[0].mode_cost.is_none());
        assert!(modes[1].mode_cost.is_none());
    }

    #[test]
    fn collect_mode_asts_malformed_plus_line_stops_collection() {
        // A `+` line without valid mana cost should break mode collection
        let lines = vec![
            "Spree",
            "+ Draw a card.", // no mana cost after +
        ];
        let modes = collect_mode_asts(&lines, 1);
        assert!(modes.is_empty());
    }

    // ---- Ao, the Dawn Sky (SOC) — modal dies trigger integration ----

    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{Effect, TargetFilter};
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    const AO_ORACLE: &str = "Flying, vigilance\nWhen Ao dies, choose one —\n\
• Look at the top seven cards of your library. Put any number of nonland permanent cards with total mana value 4 or less from among them onto the battlefield. Put the rest on the bottom of your library in a random order.\n\
• Put two +1/+1 counters on each permanent you control that's a creature or Vehicle.";

    #[test]
    fn ao_dies_trigger_parses_as_changeszone_graveyard() {
        // CR 700.4: "dies" == "is put into a graveyard from the battlefield".
        // CR 603.6c + CR 603.10a: dies triggers look back to before-death state.
        // Verifies the self-ref fix for 2-char comma-form legendary names
        // ("Ao" in "Ao, the Dawn Sky").
        let parsed = parse_oracle_text(AO_ORACLE, "Ao, the Dawn Sky", &[], &[], &[]);
        assert_eq!(parsed.triggers.len(), 1, "expected a single dies trigger");
        let trigger = &parsed.triggers[0];
        assert_eq!(trigger.mode, TriggerMode::ChangesZone);
        assert_eq!(trigger.origin, Some(Zone::Battlefield));
        assert_eq!(trigger.destination, Some(Zone::Graveyard));
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));
        assert_eq!(trigger.trigger_zones, vec![Zone::Graveyard]);
    }

    #[test]
    fn ao_dies_trigger_wraps_modal_with_two_modes() {
        // CR 700.2: modal triggered ability — the "choose one —" header binds
        // to the dies trigger and produces a ModalChoice with two modes.
        let parsed = parse_oracle_text(AO_ORACLE, "Ao, the Dawn Sky", &[], &[], &[]);
        let trigger = parsed.triggers.first().expect("expected a dies trigger");
        let execute = trigger
            .execute
            .as_deref()
            .expect("trigger should have an execute body");
        let modal = execute
            .modal
            .as_ref()
            .expect("execute should carry modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 2);
        assert_eq!(execute.mode_abilities.len(), 2);
    }

    #[test]
    fn ao_mode_bodies_guarded_as_unimplemented() {
        // Both modes carry filter qualifiers the parser silently drops:
        //   - mode 1: "with total mana value 4 or less" (no budgeted-selection
        //     primitive yet; Dig would otherwise admit unlimited-MV cards).
        //   - mode 2: "that's a creature or Vehicle" (relative clause dropped;
        //     PutCounterAll would otherwise apply to every permanent you
        //     control, not just creatures/Vehicles).
        // CR 700.2 requires the mode effect to faithfully match printed text;
        // the guard replaces each mode with Effect::Unimplemented preserving
        // the original body for coverage reporting.
        let parsed = parse_oracle_text(AO_ORACLE, "Ao, the Dawn Sky", &[], &[], &[]);
        let execute = parsed.triggers[0]
            .execute
            .as_deref()
            .expect("trigger execute");
        for mode in &execute.mode_abilities {
            assert!(
                matches!(*mode.effect, Effect::Unimplemented { .. }),
                "mode should be guarded as Unimplemented: {:?}",
                mode.effect
            );
        }
    }

    // ---- Final Act (SOC / M3C) — "Choose one or more —" modal spell ----

    const FINAL_ACT_ORACLE: &str = "Choose one or more —\n\
• Destroy all creatures.\n\
• Destroy all planeswalkers.\n\
• Destroy all battles.\n\
• Exile all graveyards.\n\
• Each opponent loses all counters.";

    #[test]
    fn final_act_parses_as_one_or_more_modal_with_five_modes() {
        // CR 700.2 + CR 700.2d: "Choose one or more —" produces a modal with
        // min_choices = 1 and max_choices = mode_count (all five). Each mode
        // lowers to a concrete, supported effect — no Unimplemented fallbacks.
        let parsed = parse_oracle_text(FINAL_ACT_ORACLE, "Final Act", &[], &[], &[]);
        let modal = parsed.modal.as_ref().expect("Final Act is modal");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 5);
        assert_eq!(modal.mode_count, 5);
        assert!(!modal.allow_repeat_modes);
        assert_eq!(parsed.abilities.len(), 5);

        // Mode 1: Destroy all creatures
        assert!(matches!(
            &*parsed.abilities[0].effect,
            Effect::DestroyAll { .. }
        ));
        // Mode 2: Destroy all planeswalkers
        assert!(matches!(
            &*parsed.abilities[1].effect,
            Effect::DestroyAll { .. }
        ));
        // Mode 3: Destroy all battles
        assert!(matches!(
            &*parsed.abilities[2].effect,
            Effect::DestroyAll { .. }
        ));
        // Mode 4: Exile all graveyards (ChangeZoneAll from graveyard to exile)
        assert!(matches!(
            &*parsed.abilities[3].effect,
            Effect::ChangeZoneAll { .. }
        ));
        // Mode 5: Each opponent loses all counters
        assert!(
            matches!(
                &*parsed.abilities[4].effect,
                Effect::LoseAllPlayerCounters { .. }
            ),
            "mode 5 should parse as LoseAllPlayerCounters, got {:?}",
            parsed.abilities[4].effect
        );
    }

    #[test]
    fn pip_boy_modal_that_creature_resolves_to_triggering_source() {
        // CR 608.2k + CR 301.5a: Pip-Boy 3000's "Whenever equipped creature
        // attacks ... • Pick a Perk — Put a +1/+1 counter on that creature."
        // The modal parent is a `GenericEffect` with no target, so binding
        // "that creature" to `ParentTarget` would leave the counter unbound.
        // The trigger subject (`AttachedTo`) must thread through modal mode
        // parsing so anaphora resolve to `TriggeringSource`.
        const PIP_BOY: &str = "Whenever equipped creature attacks, choose one —\n\
• Sort Inventory — Draw a card, then discard a card.\n\
• Pick a Perk — Put a +1/+1 counter on that creature.\n\
• Check Map — Untap up to two target lands.\nEquip {2}";
        let parsed = parse_oracle_text(PIP_BOY, "Pip-Boy 3000", &[], &[], &[]);
        let trigger = parsed.triggers.first().expect("attacks trigger");
        let execute = trigger.execute.as_deref().expect("modal execute");
        let mode2 = &execute.mode_abilities[1];
        match &*mode2.effect {
            Effect::PutCounter { target, .. } => assert_eq!(
                target,
                &TargetFilter::TriggeringSource,
                "mode 2 'that creature' must bind to TriggeringSource, not ParentTarget"
            ),
            other => panic!("expected PutCounter, got {other:?}"),
        }
    }

    #[test]
    fn final_act_mode5_is_player_scoped_to_each_opponent() {
        // CR 608.2: "Each opponent loses all counters" — the outer
        // `player_scope = Opponent` drives per-opponent iteration; the inner
        // target is `TargetFilter::Controller` so the iterating player is
        // addressed.
        use crate::types::ability::PlayerFilter;
        let parsed = parse_oracle_text(FINAL_ACT_ORACLE, "Final Act", &[], &[], &[]);
        let mode5 = &parsed.abilities[4];
        assert_eq!(mode5.player_scope, Some(PlayerFilter::Opponent));
        assert!(matches!(
            &*mode5.effect,
            Effect::LoseAllPlayerCounters {
                target: TargetFilter::Controller,
            }
        ));
    }
}
