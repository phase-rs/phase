use crate::parser::oracle_nom::error::{OracleError, OracleResult};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::{alpha1, multispace0};
use nom::combinator::{map, not, opt, success, value};
use nom::multi::fold_many1;
use nom::sequence::{delimited, preceded, terminated};
use nom::Parser;

use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, AdditionalCostOrigin,
    AdditionalCostPaymentSource, ChoiceType, Effect, ModalChoice, ModalSelectionCondition,
    ModalSelectionConstraint, PlayerFilter, QuantityExpr, QuantityRef, ReplacementDefinition,
    StaticCondition, TargetFilter, TargetSelectionMode, TriggerCondition,
};
use crate::types::replacements::ReplacementEvent;

use super::oracle::{find_activated_colon, strip_activated_constraints};
use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::{parse_effect_chain_with_context, try_parse_named_choice};
use super::oracle_ir::context::ParseContext;
use super::oracle_nom::bridge::nom_on_lower;
use super::oracle_nom::condition as nom_condition;
use super::oracle_nom::primitives::{self as nom_primitives, scan_preceded};
use super::oracle_static::parse_static_line;
use super::oracle_trigger::parse_trigger_lines;
use super::oracle_util::{parse_mana_symbols, strip_reminder_text, TextPair};
use crate::parser::oracle_ir::ast::{ModalHeaderAst, ModeAst, OracleBlockAst};

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
        let (effect_text, constraints) = strip_activated_constraints(effect_text);
        if let Some(header) = parse_modal_header_ast(&effect_text) {
            return Some((
                OracleBlockAst::ActivatedModal {
                    cost_text: cost_text.to_string(),
                    header,
                    modes,
                    constraints,
                },
                next,
            ));
        }
    }

    let candidate = strip_ability_word(&line).unwrap_or_else(|| line.clone());
    let lower = candidate.to_lowercase();

    // CR 614.12c + CR 607.2d: "As [this permanent] enters, choose <A> or <B>."
    // followed by bullet modes labeled with those anchor words. Detect before
    // the generic modal/triggered-modal arms so we route to the dedicated
    // anchor-word replacement-plus-linked-ability lowering instead of an
    // effect-less `TriggerMode::Unknown("As ~ enters")` modal trigger.
    if let Some(labels) = try_parse_as_enters_anchor_labels(&lower) {
        if anchor_modes_match_labels(&modes, &labels) {
            return Some((
                OracleBlockAst::AsEntersAnchorWordModal {
                    header_text: candidate.to_string(),
                    labels,
                    modes,
                },
                next,
            ));
        }
    }

    if let Some(header) = parse_modal_header_ast(&candidate) {
        // Reject trigger prefixes — these are triggered modals, not plain modals
        if alt((
            tag::<_, _, OracleError<'_>>("when "),
            tag("whenever "),
            tag("at "),
        ))
        .parse(lower.as_str())
        .is_err()
        {
            // CR 700.2e guard: an opponent-chooser modal that ALSO carries an
            // additional cost would re-emit `ModeChoice` through
            // `casting_costs.rs`, which threads `player` from the caster — the
            // re-emitted prompt would be mis-routed to the controller. Until
            // the casting-cost path threads the chooser, leave such a modal
            // unhandled (`modal: None`) rather than emit a mis-routed choice.
            // No in-scope corpus card hits this guard.
            if !header_is_opponent_chooser_with_additional_cost(&header, &modes) {
                // CR 700.2 + CR 601.2c: When the shared mode effect is phrased
                // once in the header ("Choose up to two. Return those cards …")
                // and each bullet is a bare target, distribute the shared effect
                // into every mode body so each chosen mode resolves its own
                // effect on its own target (Call Damage Control class).
                let modes = distribute_shared_mode_effect(&candidate, modes);
                return Some((OracleBlockAst::Modal { header, modes }, next));
            }
        }
    }

    if let Some((trigger_line, header)) = split_triggered_modal_header(&candidate) {
        if let Some(header) = parse_modal_header_ast(&header) {
            // CR 603.12 + CR 700.2b: A reflexive optional-cost header
            // ("Whenever you attack, you may sacrifice another creature. When
            // you do, choose ...") gates the modal behind the paid cost. Split
            // out the cost so the lowering builds an `Effect::Sacrifice` whose
            // `WhenYouDo` sub carries the modal, instead of firing the modes
            // unconditionally on the trigger.
            let (trigger_line, optional_cost) = match split_reflexive_optional_cost(&trigger_line) {
                Some((trigger, cost)) => (trigger, Some(cost)),
                None => (trigger_line, None),
            };
            return Some((
                OracleBlockAst::TriggeredModal {
                    trigger_line,
                    header,
                    modes,
                    optional_cost,
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
            chooser: PlayerFilter::Controller,
            selection: TargetSelectionMode::Chosen,
            dynamic_max_choices: None,
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
            chooser: PlayerFilter::Controller,
            selection: TargetSelectionMode::Chosen,
            dynamic_max_choices: None,
        };
        return Some((OracleBlockAst::Modal { header, modes }, next));
    }

    None
}

/// CR 700.2i: a run of one or more "{P}" pawprint symbols → weight as u8.
fn parse_pawprint_run(input: &str) -> OracleResult<'_, u8> {
    fold_many1(tag("{P}"), || 0u8, |acc, _| acc + 1).parse(input)
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
                    mode_pawprint: None,
                });
            } else {
                break; // Cost parse failure → stop collecting modes
            }
        } else if let Ok((rest, weight)) = parse_pawprint_run(line.as_str()) {
            // CR 700.2i: pawprint mode line "{P}{P} — effect"
            let body = strip_mode_separator(rest);
            modes.push(ModeAst {
                raw: body.to_string(),
                label: None,
                body: body.to_string(),
                mode_cost: None,
                mode_pawprint: Some(weight),
            });
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
                mode_pawprint: None,
            };
        }

        return ModeAst {
            raw: text.to_string(),
            label: Some(label.to_string()),
            body: body.to_string(),
            mode_cost: None,
            mode_pawprint: None,
        };
    }

    // CR 207.2c: A mode's flavor name ("Take 59 Flights of Stairs — …", Aerith
    // Rescue Mission) is italic flavor with no rules meaning. These names can
    // exceed the 4-word ability-word cap, so the short-label split above misses
    // them. A flavor label never contains a sentence terminator, a cost brace,
    // or an activation colon — those mark an actual effect/cost — so split on
    // the first " — " when the prefix is punctuation-free flavor text. The body
    // (the real rules text) is what `parse_effect_chain` lowers.
    if let Some((label, body)) = split_mode_flavor_label(text) {
        return ModeAst {
            raw: text.to_string(),
            label: Some(label.to_string()),
            body: body.to_string(),
            mode_cost: None,
            mode_pawprint: None,
        };
    }

    ModeAst {
        raw: text.to_string(),
        label: None,
        body: text.to_string(),
        mode_cost: None,
        mode_pawprint: None,
    }
}

/// CR 207.2c: Split a modal mode's flavor name from its rules text on the first
/// " — " / " – " separator. Unlike `split_short_label_prefix`, this imposes no
/// word-count cap (mode flavor names can be long), but requires the prefix to be
/// punctuation-free flavor text — no `.`, `:`, or `{` — so an actual effect
/// sentence or activation cost is never mistaken for a label. Returns
/// `(label, body)` with both trimmed, or `None`.
fn split_mode_flavor_label(text: &str) -> Option<(&str, &str)> {
    for sep in [" — ", " – "] {
        // allow-noncombinator: structural label/body split on the em-dash mode
        // separator (mirrors `split_short_label_prefix`), not parsing dispatch.
        if let Some(pos) = text.find(sep) {
            let prefix = text[..pos].trim();
            let rest = text[pos + sep.len()..].trim();
            // allow-noncombinator: punctuation guard distinguishing a flavor
            // label from an effect sentence / activation cost; structural, not
            // a parsing-dispatch substring scan.
            if !prefix.is_empty() && !rest.is_empty() && !prefix.contains(['.', ':', '{']) {
                return Some((prefix, rest));
            }
        }
    }
    None
}

fn strip_mode_separator(text: &str) -> &str {
    let trimmed = text.trim();
    alt((
        tag::<_, _, OracleError<'_>>("—"),
        tag::<_, _, OracleError<'_>>("–"),
    ))
    .parse(trimmed)
    .map(|(rest, _)| rest.trim())
    .unwrap_or(trimmed)
}

/// CR 700.2 + CR 601.2c: Distribute a header-level shared mode effect across
/// bare-target modes.
///
/// Some modal spells phrase the shared instruction once in the header and leave
/// each bullet mode as a bare target (Call Damage Control: "Choose up to two.
/// Return those cards from your graveyard to your hand. • Target artifact card.
/// • Target creature card. …"). The header's second sentence names a `those
/// <noun>` anaphor that resolves to the chosen modes' targets, and each mode
/// supplies only its target's card-type. Because the engine resolves every
/// chosen mode as its own `ResolvedAbility` (CR 700.2c — one target per chosen
/// mode), the rules-correct lowering rewrites each bare-target mode body into
/// the full shared effect with the anaphor replaced by that mode's target
/// phrase: `"Return target artifact card from your graveyard to your hand."`,
/// etc. Each mode then independently returns its own targeted card.
///
/// This builds for the CLASS: the card-type is the only per-mode axis; the
/// substitution is purely structural over the `those <noun>` slot, so it covers
/// any "Choose up to N. <verb> those <noun> …. • Target A. • Target B." spell.
///
/// Returns the rewritten modes when the class matches, otherwise the modes
/// unchanged.
fn distribute_shared_mode_effect(header_full_text: &str, modes: Vec<ModeAst>) -> Vec<ModeAst> {
    // The shared effect lives in the header sentence(s) after the choose-count.
    // `parse_modal_header_ast` keys off the first sentence; the remainder is the
    // shared template. Require at least two modes (CR 700.2 modal) and that
    // every mode is a bare target so we never clobber a mode that already
    // carries its own verb/effect.
    if modes.len() < 2 || !modes.iter().all(|m| mode_body_is_bare_target(&m.body)) {
        return modes;
    }

    let Some((prefix, suffix)) = parse_shared_those_template(header_full_text) else {
        return modes;
    };

    modes
        .into_iter()
        .map(|mode| {
            // The bare-target body retains the "target" keyword and card-type
            // phrase ("Target artifact card"); lowercase its leading article so
            // it reads mid-sentence inside the shared template. Drop a trailing
            // period — the suffix supplies sentence punctuation.
            let target_phrase = lowercase_first_word(mode.body.trim().trim_end_matches('.').trim());
            let distributed = format!("{prefix}{target_phrase}{suffix}");
            ModeAst {
                raw: mode.raw,
                label: mode.label,
                body: distributed,
                mode_cost: mode.mode_cost,
                mode_pawprint: mode.mode_pawprint,
            }
        })
        .collect()
}

/// True when a modal mode body is a bare target phrase using the "target"
/// keyword and nothing else — i.e. the body parses fully (modulo a trailing
/// period) as a single `target …` phrase with no leading or trailing verb. The
/// parser is the detector: `parse_target_with_syntax` reports `TargetKeyword`
/// only when the phrase opened with "target", and a leftover non-empty
/// remainder means there is an effect verb the shared template would wrongly
/// swallow.
fn mode_body_is_bare_target(body: &str) -> bool {
    let body = body.trim().trim_end_matches('.').trim();
    let lower = body.to_lowercase();
    // Must open with the "target" keyword (CR 601.2c) — a descriptor phrase
    // ("an artifact card") is not this class.
    if tag::<_, _, OracleError<'_>>("target ")
        .parse(lower.as_str())
        .is_err()
    {
        return false;
    }
    let mut ctx = ParseContext::default();
    let (_, rest, syntax) = crate::parser::oracle_target::parse_target_with_syntax(body, &mut ctx);
    syntax == crate::parser::oracle_target::TargetSyntax::TargetKeyword && rest.trim().is_empty()
}

/// CR 700.2: Parse a header's shared mode-effect template into the text on
/// either side of its `those <noun>` anaphor slot. The first header sentence is
/// the choose-count (handled by `parse_modal_header_ast`); the shared effect is
/// the following sentence whose object is `those <plural-noun>` referring to the
/// chosen modes' targets. Returns `(prefix, suffix)` such that
/// `format!("{prefix}{target}{suffix}")` reconstructs the per-mode effect, or
/// `None` when no such template exists.
fn parse_shared_those_template(header_full_text: &str) -> Option<(String, String)> {
    // Skip the choose-count sentence; the shared effect is the next non-empty
    // sentence. Splitting on the sentence terminator mirrors
    // `parse_modal_header_ast`'s own sentence segmentation.
    let mut sentences = header_full_text
        // allow-noncombinator: structural sentence segmentation on the sentence
        // terminator, mirroring `parse_modal_header_ast`; not parsing dispatch.
        .split('.')
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let _count_sentence = sentences.next()?;
    let effect_sentence = sentences.next()?;

    // Split the effect sentence around the `those ` anaphor slot, preserving the
    // original casing of the surrounding text via `TextPair`. The prefix is the
    // verb clause before the slot ("Return "); the anaphor noun ("cards") that
    // immediately follows is discarded, and the remainder is the suffix
    // ("from your graveyard to your hand").
    let lower = effect_sentence.to_lowercase();
    let pair = TextPair::new(effect_sentence, &lower);
    let (before, after) = pair.split_around("those ")?;
    let after = after.trim_start();
    // Consume the (plural) anaphor noun word with a nom combinator; the
    // remainder (with its leading space preserved) is the shared suffix, kept so
    // the reconstructed body reads "<target> from your graveyard …" rather than
    // gluing the target phrase onto the suffix. A bare "those" with no trailing
    // clause is not a usable template.
    let (_, suffix) = nom_on_lower(after.original, after.lower, |i| {
        value((), take_until::<_, _, OracleError<'_>>(" ")).parse(i)
    })?;
    Some((before.original.to_string(), format!("{}.", suffix)))
}

/// Lowercase only the first word of a phrase, leaving the remainder untouched.
/// Used to make a bullet mode's leading "Target …" read mid-sentence ("…
/// target …") inside a distributed shared-effect template.
fn lowercase_first_word(phrase: &str) -> String {
    let mut chars = phrase.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_lowercase(), chars.as_str()),
        None => String::new(),
    }
}

/// CR 614.12c + CR 607.2d: Recognise an anchor-word as-enters header sentence
/// — "as ~ enters, choose <A> or <B>" / "as ~ enters, choose <A>, <B>, or
/// <C>" — and return the labels in declaration order. Operates entirely on
/// already-normalised lowercase text using nom combinators so the per-card
/// label vocabulary (Khans/Dragons, Jeskai/Temur, …) doesn't need to be
/// hard-coded.
///
/// Returns `None` when the header isn't an as-enters-choose sentence or when
/// the choose clause doesn't reduce to a labeled-option list (per
/// `try_parse_labeled_choice`'s 1-2-word capitalisation/structure gates).
pub(crate) fn try_parse_as_enters_anchor_labels(lower: &str) -> Option<Vec<String>> {
    type E<'a> = OracleError<'a>;

    // "as <self-ref>, enters, choose ..." → strip the framing prefix. The
    // self-reference is always normalised to `~` by `normalize_self_refs`
    // before this function runs (see `oracle_util::SELF_REF_TYPE_PHRASES`
    // covers "this enchantment", "this permanent", etc.).
    let trimmed = lower.trim().trim_end_matches('.');
    let (rest, _) = tag::<_, _, E>("as ~ enters, ").parse(trimmed).ok()?;

    // Delegate to the shared named-choice recogniser to extract the labels.
    // Restricting to `Labeled` ensures we don't accidentally absorb "choose a
    // color" / "choose a creature type" / etc. — those have their own existing
    // `parse_as_enters_choose` replacement path.
    match try_parse_named_choice(rest)? {
        ChoiceType::Labeled { options } if options.len() >= 2 => Some(options),
        _ => None,
    }
}

/// CR 614.12c: True iff every collected bullet mode declares an anchor-word
/// label and the label set matches `labels` exactly (order-independent,
/// case-insensitive). Guards against false-positive matches on cards whose
/// header text accidentally resembles an anchor-word choose clause but whose
/// bullets aren't anchor-labeled (regular labeled modes).
fn anchor_modes_match_labels(modes: &[ModeAst], labels: &[String]) -> bool {
    if modes.len() != labels.len() {
        return false;
    }
    let mode_labels: Vec<String> = modes
        .iter()
        .filter_map(|m| m.label.as_ref().map(|s| s.to_lowercase()))
        .collect();
    if mode_labels.len() != modes.len() {
        return false;
    }
    let mut wanted: Vec<String> = labels.iter().map(|s| s.to_lowercase()).collect();
    for actual in &mode_labels {
        match wanted.iter().position(|w| w == actual) {
            Some(pos) => {
                wanted.swap_remove(pos);
            }
            None => return false,
        }
    }
    wanted.is_empty()
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

/// CR 700.2e: Recognise a chooser-subject prefix that precedes the `choose`
/// token of a modal header. The combinator consumes the subject **including**
/// the trailing `choose `/`chooses ` verb token, so the remainder begins
/// exactly where a bare `Choose one —` header's remainder begins.
///
/// Exactly two arms — `you choose ` (controller alias, CR 700.2a) and
/// `an opponent chooses ` (CR 700.2e, the single non-controller opponent).
/// `target opponent chooses ` and `each opponent chooses ` are deliberately
/// NOT handled (deferred — see plan 03 Pattern Coverage).
fn parse_modal_chooser_prefix(input: &str) -> nom::IResult<&str, PlayerFilter, OracleError<'_>> {
    alt((
        value(PlayerFilter::Controller, tag("you choose ")),
        value(PlayerFilter::Opponent, tag("an opponent chooses ")),
    ))
    .parse(input)
}

/// Recognise the count portion of a modal header **after** the `choose ` (or
/// chooser-prefix verb) token has been consumed. Returns the `(min, max)` pair
/// when the remainder is a genuine modal count phrase (`one —`, `two —`,
/// `up to two —`, `one or more —`, …), or `None` otherwise.
///
/// This is the single count authority shared by both the bare `Choose …`
/// header path and the chooser-prefixed path — neither enumerates its own
/// count vocabulary.
fn parse_modal_count_remainder(remainder: &str) -> Option<ModalCountSpec> {
    let remainder = remainder.trim_start();
    if let Some(spec) = scan_modal_count_override(remainder) {
        return Some(spec);
    }
    nom_primitives::parse_number(remainder)
        .ok()
        .map(|(_, n)| ModalCountSpec::Fixed {
            min: n as usize,
            max: n as usize,
        })
}

fn is_modal_header_text(lower: &str) -> bool {
    let lower = lower.trim();
    // Chooser-prefixed header (CR 700.2e): `you choose …` / `an opponent
    // chooses …`. Accept only when the post-prefix remainder is a genuine
    // count phrase — reuse `parse_modal_count_remainder`, never a second
    // count `alt()`.
    if let Ok((remainder, _)) = parse_modal_chooser_prefix(lower) {
        return parse_modal_count_remainder(remainder).is_some();
    }
    alt((
        tag::<_, _, OracleError<'_>>("choose "),
        tag("you may choose "),
    ))
    .parse(lower)
    .is_ok()
        || (tag::<_, _, OracleError<'_>>("if ").parse(lower).is_ok()
            && scan_preceded(lower, |i| tag::<_, _, OracleError<'_>>("choose ").parse(i)).is_some())
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

    // CR 700.2e: A chooser-subject prefix (`you choose …` / `an opponent
    // chooses …`) precedes the count phrase. Strip it, record the chooser,
    // and compute the count from the remainder so `an opponent chooses two —`
    // still yields `(2, 2)`.
    let (chooser, count_input) = match parse_modal_chooser_prefix(&header_lower) {
        Ok((remainder, chooser)) => (chooser, remainder.to_string()),
        Err(_) => (PlayerFilter::Controller, header_lower.clone()),
    };

    let count_spec = if chooser == PlayerFilter::Controller && count_input == header_lower {
        // Bare `Choose …` header — unchanged path.
        parse_modal_choose_count(&header_lower)
    } else {
        // Chooser-prefixed remainder ("one —", "two —", …) — reuse the
        // shared count recognizer; `is_modal_header_text` already gated
        // that the remainder is a genuine count phrase.
        parse_modal_count_remainder(&count_input)
            .unwrap_or(ModalCountSpec::Fixed { min: 1, max: 1 })
    };

    // CR 700.2 + CR 107.3m / CR 603.12a: a `Dynamic { qty }` header ("choose up
    // to X / up to that many") has min 0 (decline all modes) and a placeholder
    // max of `usize::MAX` that `build_modal_choice` clamps to `mode_count`; the
    // live cap is carried in `dynamic_max_choices` and resolved at runtime from
    // `qty` (cast {X} for CostXPaid, or the resolution-local repeated-payment
    // count for TimesCostPaidThisResolution).
    let (min_choices, max_choices, dynamic_max_choices) = match count_spec {
        ModalCountSpec::Fixed { min, max } => (min, max, None),
        ModalCountSpec::Dynamic { qty } => (0, usize::MAX, Some(QuantityExpr::Ref { qty })),
    };
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

    constraints.extend(parse_conditional_modal_max_constraints(
        &text.to_lowercase(),
        max_choices,
    ));

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

    // CR 700.2b (override) + CR 701.9b (analogous): "choose one at random" — the
    // game, not the chooser, selects the mode(s). The "at random" qualifier is a
    // word-boundary phrase flag on the header sentence, not parsing dispatch.
    let selection = if nom_primitives::scan_contains(&header_lower, "at random") {
        TargetSelectionMode::Random
    } else {
        TargetSelectionMode::Chosen
    };

    Some(ModalHeaderAst {
        raw: text.to_string(),
        min_choices,
        max_choices,
        allow_repeat_modes,
        constraints,
        chooser,
        selection,
        dynamic_max_choices,
    })
}

fn parse_conditional_modal_max_constraints(
    input: &str,
    otherwise_max_choices: usize,
) -> Vec<ModalSelectionConstraint> {
    match parse_conditional_modal_max(input.trim()) {
        Ok(("", (condition, max_choices))) => {
            vec![ModalSelectionConstraint::ConditionalMaxChoices {
                condition,
                max_choices,
                otherwise_max_choices,
            }]
        }
        _ => Vec::new(),
    }
}

fn parse_conditional_modal_max(
    input: &str,
) -> nom::IResult<&str, (ModalSelectionCondition, usize), OracleError<'_>> {
    let (rest, _) = parse_modal_base_sentence(input)?;
    let (rest, _) = tag(" if ").parse(rest)?;
    let (rest, condition) = parse_modal_condition(rest)?;
    let (rest, _) = tag(",").parse(rest)?;
    let (rest, _) = tag(" ").parse(rest)?;
    let (rest, _) = opt(tag("you may ")).parse(rest)?;
    let (rest, max_choices) = parse_modal_override_count(rest)?;
    let (rest, _) = opt(alt((tag("."), tag("—")))).parse(rest)?;
    Ok((rest, (condition, max_choices)))
}

fn parse_modal_base_sentence(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    let (rest, _) = alt((
        tag("choose one."),
        tag("choose two."),
        tag("choose three."),
        tag("choose one or both."),
        tag("choose one or more."),
        tag("choose any number of."),
    ))
    .parse(input)?;
    Ok((rest, ()))
}

fn parse_modal_condition(
    input: &str,
) -> nom::IResult<&str, ModalSelectionCondition, OracleError<'_>> {
    alt((
        parse_modal_additional_cost_condition,
        parse_modal_static_condition,
    ))
    .parse(input)
}

fn parse_modal_static_condition(
    input: &str,
) -> nom::IResult<&str, ModalSelectionCondition, OracleError<'_>> {
    let (rest, condition) = nom_condition::parse_inner_condition(input)?;
    let (rest, _) = opt(tag(" as you cast this spell")).parse(rest)?;
    Ok((rest, ModalSelectionCondition::Static { condition }))
}

fn parse_modal_additional_cost_condition(
    input: &str,
) -> nom::IResult<&str, ModalSelectionCondition, OracleError<'_>> {
    // CR 601.2b/f: Teamwork is an optional additional cast cost; the modal
    // "choose both instead" upgrade gates specifically on the Teamwork payment.
    // Stamping `origin: Some(Teamwork)` makes the rider test the Teamwork tap
    // payment, not any optional additional cost — so it composes correctly with
    // another object additional cost on the same spell.
    if let Ok((rest, ())) = nom_condition::parse_cast_using_teamwork_phrase(input) {
        return Ok((
            rest,
            ModalSelectionCondition::AdditionalCostPaid {
                source: AdditionalCostPaymentSource::Any,
                origin: Some(AdditionalCostOrigin::Teamwork),
                origin_ordinal: None,
                variant: None,
                kicker_cost: None,
                min_count: 1,
            },
        ));
    }

    if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("this spell's additional cost was paid").parse(input)
    {
        return Ok((
            rest,
            ModalSelectionCondition::AdditionalCostPaid {
                source: AdditionalCostPaymentSource::Any,
                origin: None,
                origin_ordinal: None,
                variant: None,
                kicker_cost: None,
                min_count: 1,
            },
        ));
    }

    let (rest, _) = alt((
        tag("this spell was kicked"),
        tag("it was kicked"),
        preceded(take_until(" was kicked"), tag(" was kicked")),
    ))
    .parse(input)?;

    alt((
        parse_modal_specific_kicker_cost_condition,
        value(
            ModalSelectionCondition::AdditionalCostPaid {
                source: AdditionalCostPaymentSource::Kicker,
                origin: None,
                origin_ordinal: None,
                variant: None,
                kicker_cost: None,
                min_count: 2,
            },
            tag(" twice"),
        ),
        map(
            preceded(
                tag(" "),
                terminated(nom_primitives::parse_number, tag(" times")),
            ),
            |min_count| ModalSelectionCondition::AdditionalCostPaid {
                source: AdditionalCostPaymentSource::Kicker,
                origin: None,
                origin_ordinal: None,
                variant: None,
                kicker_cost: None,
                min_count,
            },
        ),
        success(ModalSelectionCondition::AdditionalCostPaid {
            source: AdditionalCostPaymentSource::Kicker,
            origin: None,
            origin_ordinal: None,
            variant: None,
            kicker_cost: None,
            min_count: 1,
        }),
    ))
    .parse(rest)
}

fn parse_modal_specific_kicker_cost_condition(
    input: &str,
) -> nom::IResult<&str, ModalSelectionCondition, OracleError<'_>> {
    let (rest, _) = tag(" with its ").parse(input)?;
    let (rest, cost_text) = take_until(" kicker").parse(rest)?;
    let (rest, _) = tag(" kicker").parse(rest)?;
    let normalized_cost = cost_text.to_uppercase();
    let (_, kicker_cost) = nom_primitives::parse_mana_cost(normalized_cost.as_str())
        .map_err(|_| nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Fail)))?;
    Ok((
        rest,
        ModalSelectionCondition::AdditionalCostPaid {
            source: AdditionalCostPaymentSource::Kicker,
            origin: None,
            origin_ordinal: None,
            variant: None,
            kicker_cost: Some(kicker_cost),
            min_count: 1,
        },
    ))
}

fn parse_modal_override_count(input: &str) -> nom::IResult<&str, usize, OracleError<'_>> {
    // "choose <count> instead" — factor the shared prefix/suffix; only the
    // count word varies (PATTERNS.md §8b).
    delimited(
        tag("choose "),
        alt((
            value(2, alt((tag("both"), tag("two")))),
            value(3, tag("three")),
            value(usize::MAX, alt((tag("any number"), tag("one or more")))),
        )),
        tag(" instead"),
    )
    .parse(input)
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

/// CR 603.12 + CR 700.2b: Recognize a reflexive optional-cost trigger header of
/// the shape `"<trigger>, you may <cost>. When you do"` (Caesar, Legion's
/// Emperor) and split it into the bare trigger condition (`"Whenever you
/// attack"`) and the cost effect text (`"Sacrifice another creature"`, with the
/// `"you may "` optional marker and the trailing `". When you do"` reflexive
/// connector stripped). Returns `None` for a plain triggered modal (Pip-Boy
/// 3000's `"Whenever equipped creature attacks ..."`), which has neither a
/// `"you may "` optional cost nor a `"when you do"` reflexive connector — that
/// modal attaches directly to the trigger's execute.
///
/// The cost text is returned with an uppercased leading letter so it parses as
/// an imperative effect clause (`parse_effect_chain` expects sentence case).
fn split_reflexive_optional_cost(trigger_line: &str) -> Option<(String, String)> {
    // Combinator (run on lowercase, slice original by equal ASCII byte offset):
    //   <trigger> ", you may " <cost> ". " match_when_you_do
    let lower = trigger_line.to_lowercase();
    let (after_marker, trigger_lower): (&str, &str) = terminated(
        take_until::<_, _, OracleError<'_>>(", you may "),
        tag(", you may "),
    )
    .parse(lower.as_str())
    .ok()?;
    let (connector, cost_lower): (&str, &str) =
        terminated(take_until::<_, _, OracleError<'_>>(". "), tag(". "))
            .parse(after_marker)
            .ok()?;
    // The connector remainder must be exactly the reflexive "when you do".
    let (rest, ()) = nom_condition::match_when_you_do(connector).ok()?;
    if !rest.is_empty() {
        return None;
    }

    let trigger_len = trigger_lower.len();
    let cost_start = trigger_line.len() - after_marker.len();
    let cost_len = cost_lower.len();
    let trigger_orig = trigger_line.get(..trigger_len)?.trim();
    let cost_orig = trigger_line.get(cost_start..cost_start + cost_len)?.trim();

    // Sentence-case the cost so `parse_effect_chain` reads it as an imperative.
    let mut chars = cost_orig.chars();
    let first = chars.next()?;
    let cost_cased = first.to_uppercase().collect::<String>() + chars.as_str();
    Some((trigger_orig.to_string(), cost_cased))
}

pub(crate) fn lower_oracle_block(
    block: OracleBlockAst,
    card_name: &str,
    host_self_reference: Option<TargetFilter>,
    result: &mut super::oracle::ParsedAbilities,
) {
    match block {
        OracleBlockAst::ActivatedModal {
            cost_text,
            header,
            modes,
            constraints,
        } => {
            let mut def =
                build_modal_ability(AbilityKind::Activated, &header, &modes, host_self_reference)
                    .cost(parse_oracle_cost(&cost_text));
            def.activation_restrictions = constraints.restrictions;
            result.abilities.push(def);
        }
        OracleBlockAst::Modal { header, modes } => {
            let modal = build_modal_choice(&header, &modes);
            let mode_abilities =
                lower_mode_abilities(&modes, AbilityKind::Spell, host_self_reference);
            result.abilities.extend(mode_abilities);
            result.modal = Some(modal);
        }
        OracleBlockAst::TriggeredModal {
            trigger_line,
            header,
            modes,
            optional_cost,
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
            // CR 109.4 + CR 115.1 + CR 506.2: Derive the relative-
            // player scope the trigger condition establishes (e.g.
            // `TriggeringPlayer` for a "deals combat damage to a player" trigger)
            // so a `"that player controls"` / `"that player's library"` anaphor
            // in a BULLET-LINE mode body resolves to the damaged player, not the
            // caster. `trigger_line` is the bare trigger condition here (the
            // modal header was split off as the effect by
            // `split_triggered_modal_header`), so it is the condition text that
            // the single-authority scope resolver expects. Without this, bullet-
            // line modes hit the `unwrap_or(ControllerRef::You)` fallback in
            // `oracle_target.rs` while the inline `"; or"` form (which threads the
            // same scope via `try_parse_inline_modal`) resolved correctly — the
            // two modal surface forms of Grenzo, Havoc Raiser disagreed (#2346).
            let relative_player_scope = super::oracle_trigger::relative_player_scope_for_condition(
                &trigger_line.to_lowercase(),
            );
            let mut modal_ability = build_modal_ability_with_subject(
                AbilityKind::Spell,
                &header,
                &modes,
                modal_subject,
                relative_player_scope,
                host_self_reference,
            );

            let execute = match optional_cost {
                // CR 603.12 + CR 700.2b: The modal is gated behind a reflexive
                // optional cost. Build `Effect::Sacrifice { optional }` whose
                // `WhenYouDo` sub_ability carries the modal, so the modes are
                // chosen and resolved only after the controller pays the cost
                // (Caesar, Legion's Emperor). The decline path is handled by
                // `should_resolve_subability_on_optional_decline` (WhenYouDo →
                // false), so declining the sacrifice resolves no modes.
                Some(cost_text) => {
                    modal_ability.condition = Some(AbilityCondition::WhenYouDo);
                    let mut cost_ability = crate::parser::oracle_effect::parse_effect_chain(
                        &cost_text,
                        AbilityKind::Spell,
                    );
                    // CR 118.12 + CR 701.21: "you may sacrifice" makes the
                    // sacrifice cost optional during resolution; the controller
                    // is prompted before paying it.
                    cost_ability.optional = true;
                    cost_ability.sub_ability = Some(Box::new(modal_ability));
                    Box::new(cost_ability)
                }
                // Plain triggered modal (Pip-Boy): the modal attaches directly.
                None => Box::new(modal_ability),
            };

            for trigger in &mut triggers {
                trigger.execute = Some(execute.clone());
            }
            result.triggers.extend(triggers);
        }
        OracleBlockAst::AsEntersAnchorWordModal {
            header_text,
            labels,
            modes,
        } => {
            lower_as_enters_anchor_word_modal(header_text, labels, modes, card_name, result);
        }
    }
}

/// CR 614.12c + CR 607.2d: Lower an as-enters anchor-word modal block into:
///   1. A `Moved` `ReplacementDefinition` that asks the controller to choose
///      between the anchor-word labels and persists the answer as a
///      `ChosenAttribute::Label` on the entering permanent.
///   2. One `TriggerDefinition` or `StaticDefinition` per linked-ability mode
///      (CR 607.2d makes each linked ability), each gated on
///      `ChosenLabelIs { label }` so the linked ability functions only while
///      its anchor word was chosen.
///
/// Falls back to a no-op placeholder static with an `Unrecognized` condition
/// when a mode body parses to neither a trigger nor a static — preserves the
/// choice shape for the coverage report instead of silently dropping a mode.
fn lower_as_enters_anchor_word_modal(
    header_text: String,
    labels: Vec<String>,
    modes: Vec<ModeAst>,
    card_name: &str,
    result: &mut super::oracle::ParsedAbilities,
) {
    // 1. Synthesise the as-enters choose replacement. Mirrors the existing
    //    `parse_as_enters_choose` (oracle_replacement.rs) shape but uses the
    //    parsed labels directly so we don't re-run the labeled-choice
    //    recogniser on the header text.
    let choice_replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: ChoiceType::Labeled {
                    options: labels.clone(),
                },
                persist: true,
                selection: TargetSelectionMode::Chosen,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(crate::types::zones::Zone::Battlefield)
        // Description matches the printed header sentence so coverage and
        // log output show the original Oracle phrasing. Internal storage
        // detail ("persists as ChosenAttribute::Label") is documented on the
        // `ChosenAttribute::Label` variant and `lower_as_enters_anchor_word_modal`
        // itself, not duplicated here.
        .description(header_text.trim().to_string());
    result.replacements.push(choice_replacement);

    // 2. Lower each anchor-word mode into a continuous ability gated on
    //    `ChosenLabelIs { label }`. The mode body is fed back through the
    //    normal trigger / static parsers so it benefits from every parser
    //    primitive (Whenever / At / "creatures you control get +N/+M and have
    //    <keyword> and <keyword>" / etc.).
    for mode in &modes {
        let Some(label) = mode.label.as_ref() else {
            continue;
        };
        let body = mode.body.trim();
        if body.is_empty() {
            continue;
        }

        // Trigger first — "Whenever / When / At" patterns can only be
        // triggers, never statics.
        let trigger_lower = body.to_lowercase();
        let is_trigger_pattern = nom::Parser::parse(
            &mut alt((
                tag::<_, _, OracleError<'_>>("when "),
                tag("whenever "),
                tag("at "),
            )),
            trigger_lower.as_str(),
        )
        .is_ok();

        if is_trigger_pattern {
            let mut triggers = parse_trigger_lines(body, card_name);
            if !triggers.is_empty() {
                for trigger in &mut triggers {
                    attach_chosen_label_to_trigger(trigger, label);
                }
                result.triggers.extend(triggers);
                continue;
            }
        }

        // Static next — anthem-style "Creatures you control get +N/+M ..." or
        // "~ has flying" patterns. `parse_static_line` returns `None` when
        // the line isn't a recognised static, which falls through to the
        // unimplemented fallback below.
        if let Some(mut static_def) = parse_static_line(body) {
            attach_chosen_label_to_static(&mut static_def, label);
            result.statics.push(static_def);
            continue;
        }

        // Fallback: the mode body parsed to neither a trigger nor a static.
        // Emit a placeholder `StaticDefinition` with no modifications and
        // both the anchor-word gate and an `Unrecognized` marker on its
        // condition so the coverage report surfaces this specific anchor-word
        // mode (not the parent enchantment as a whole) as an unimplemented
        // pattern. The static has no continuous effect — the empty
        // `modifications` vector keeps layer evaluation a no-op even when
        // `ChosenLabelIs` is satisfied.
        let placeholder = crate::types::ability::StaticDefinition {
            mode: crate::types::statics::StaticMode::Continuous,
            affected: Some(TargetFilter::SelfRef),
            modifications: vec![],
            condition: Some(StaticCondition::And {
                conditions: vec![
                    StaticCondition::ChosenLabelIs {
                        label: label.clone(),
                    },
                    StaticCondition::Unrecognized {
                        text: body.to_string(),
                    },
                ],
            }),
            per_player_condition: None,
            affected_zone: None,
            effect_zone: None,
            active_zones: Vec::new(),
            characteristic_defining: false,
            description: Some(format!("CR 614.12c [{label}]: {body}")),
            attack_defended: None,
        };
        result.statics.push(placeholder);
    }
}

/// Attach a `ChosenLabelIs` intervening-if to a parsed trigger. Composes with
/// any pre-existing condition via `TriggerCondition::And` so the linked
/// ability remains rule-correct even if the body itself carries an "if"
/// clause (none in current corpus, future-safe).
fn attach_chosen_label_to_trigger(
    trigger: &mut crate::types::ability::TriggerDefinition,
    label: &str,
) {
    let gate = TriggerCondition::ChosenLabelIs {
        label: label.to_string(),
    };
    trigger.condition = Some(match trigger.condition.take() {
        None => gate,
        Some(existing) => TriggerCondition::And {
            conditions: vec![gate, existing],
        },
    });
    // CR 113.6 + CR 614.12c: Anchor-word linked abilities function only while
    // the source permanent is on the battlefield (same as any printed trigger
    // on a permanent). Leave `trigger_zones` untouched — the default
    // battlefield-only behavior is correct.
}

/// Attach a `ChosenLabelIs` gate to a parsed static. Composes with any
/// pre-existing condition via `StaticCondition::And`.
fn attach_chosen_label_to_static(
    static_def: &mut crate::types::ability::StaticDefinition,
    label: &str,
) {
    let gate = StaticCondition::ChosenLabelIs {
        label: label.to_string(),
    };
    static_def.condition = Some(match static_def.condition.take() {
        None => gate,
        Some(existing) => StaticCondition::And {
            conditions: vec![gate, existing],
        },
    });
}

pub(crate) fn build_modal_ability(
    kind: AbilityKind,
    header: &ModalHeaderAst,
    modes: &[ModeAst],
    host_self_reference: Option<TargetFilter>,
) -> AbilityDefinition {
    AbilityDefinition::new(kind, modal_marker_effect(header)).with_modal(
        build_modal_choice(header, modes),
        lower_mode_abilities(modes, kind, host_self_reference),
    )
}

/// Build a modal ability with a trigger-context subject so mode-body pronoun
/// anaphora resolve against the triggering object (CR 608.2k + CR 301.5a).
///
/// CR 303.4 + CR 702.103: `host_self_reference` propagates the enclosing
/// card's typed attachment-host self-reference into modal mode bodies.
///
/// CR 109.4 + CR 115.1 + CR 506.2: `relative_player_scope` threads
/// the trigger condition's player binding (e.g. `TriggeringPlayer` for a "deals
/// combat damage to a player" trigger) into every mode body so a `"that player
/// controls"` / `"that player's library"` anaphor resolves to the player the
/// condition introduced (the damaged player) rather than falling back to the
/// caster (`ControllerRef::You`). This mirrors the inline `"; or"` modal path
/// (`try_parse_inline_modal`); both must thread the same scope so bullet-line
/// and inline modal forms of the same trigger agree (issue #2346).
fn build_modal_ability_with_subject(
    kind: AbilityKind,
    header: &ModalHeaderAst,
    modes: &[ModeAst],
    subject: Option<TargetFilter>,
    relative_player_scope: Option<crate::types::ability::ControllerRef>,
    host_self_reference: Option<TargetFilter>,
) -> AbilityDefinition {
    AbilityDefinition::new(kind, modal_marker_effect(header)).with_modal(
        build_modal_choice(header, modes),
        lower_mode_abilities_with_scope(
            modes,
            kind,
            subject,
            relative_player_scope,
            host_self_reference,
        ),
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

/// CR 700.2e guard: true when the header is an opponent-chooser modal that
/// also carries an additional cost (per-mode Spree cost or an
/// `AdditionalCostPaid` conditional-max constraint). Such a modal would
/// re-emit `ModeChoice` through `casting_costs.rs` with the caster's `player`,
/// mis-routing the re-prompt. The parser declines to handle it (`modal: None`)
/// rather than ship a rules-incorrect routing.
fn header_is_opponent_chooser_with_additional_cost(
    header: &ModalHeaderAst,
    modes: &[ModeAst],
) -> bool {
    if header.chooser == PlayerFilter::Controller {
        return false;
    }
    let has_mode_cost = modes.iter().any(|m| m.mode_cost.is_some());
    let has_additional_cost_constraint = header.constraints.iter().any(|constraint| {
        matches!(
            constraint,
            ModalSelectionConstraint::ConditionalMaxChoices {
                condition: ModalSelectionCondition::AdditionalCostPaid { .. },
                ..
            }
        )
    });
    has_mode_cost || has_additional_cost_constraint
}

fn build_modal_choice(header: &ModalHeaderAst, modes: &[ModeAst]) -> ModalChoice {
    let mode_count = modes.len();
    let mode_pawprints: Vec<u8> = modes.iter().filter_map(|m| m.mode_pawprint).collect();
    // CR 700.2i: for a pawprint points-budget modal, `header.max_choices` is the
    // POINT BUDGET (Σ of chosen weights), NOT a mode count — do NOT clamp it to
    // `mode_count`. Bullet/Spree modals keep the existing count-cap behavior.
    let max_choices = if mode_pawprints.is_empty() {
        header.max_choices.min(mode_count)
    } else {
        header.max_choices
    };
    ModalChoice {
        min_choices: header.min_choices,
        max_choices,
        mode_count,
        mode_descriptions: modes.iter().map(|mode| mode.raw.clone()).collect(),
        allow_repeat_modes: header.allow_repeat_modes,
        constraints: cap_modal_constraints(&header.constraints, mode_count),
        mode_costs: modes.iter().filter_map(|m| m.mode_cost.clone()).collect(),
        mode_pawprints,
        entwine_cost: None,
        // CR 700.2e: the player who chooses the mode(s).
        chooser: header.chooser.clone(),
        // CR 700.2b (override): random mode selection ("choose one at random").
        selection: header.selection,
        // CR 700.2 + CR 107.3m: dynamic "choose up to X —" cap, resolved live
        // at runtime; the static `max_choices` above already holds the
        // `mode_count` clamp (usize::MAX.min(mode_count)) used pre-resolution.
        dynamic_max_choices: header.dynamic_max_choices.clone(),
    }
}

fn cap_modal_constraints(
    constraints: &[ModalSelectionConstraint],
    mode_count: usize,
) -> Vec<ModalSelectionConstraint> {
    constraints
        .iter()
        .cloned()
        .map(|constraint| match constraint {
            ModalSelectionConstraint::ConditionalMaxChoices {
                condition,
                max_choices,
                otherwise_max_choices,
            } => ModalSelectionConstraint::ConditionalMaxChoices {
                condition,
                max_choices: max_choices.min(mode_count),
                otherwise_max_choices: otherwise_max_choices.min(mode_count),
            },
            other => other,
        })
        .collect()
}

fn lower_mode_abilities(
    modes: &[ModeAst],
    kind: AbilityKind,
    host_self_reference: Option<TargetFilter>,
) -> Vec<AbilityDefinition> {
    lower_mode_abilities_with_subject(modes, kind, None, host_self_reference)
}

/// Variant of `lower_mode_abilities` that threads a trigger subject through
/// mode-body parsing so anaphoric pronouns ("that creature") resolve against
/// the triggering object (CR 608.2k + CR 301.5a). When `subject` is `None`,
/// behavior is identical to `lower_mode_abilities`.
///
/// CR 303.4 + CR 702.103: `host_self_reference` carries the enclosing card's
/// typed attachment-host self-reference so a `"that creature"` copy-token
/// anaphor inside a modal mode body of an Aura/bestow card remaps to the
/// enchanted host. `None` for non-Aura cards.
fn lower_mode_abilities_with_subject(
    modes: &[ModeAst],
    kind: AbilityKind,
    subject: Option<TargetFilter>,
    host_self_reference: Option<TargetFilter>,
) -> Vec<AbilityDefinition> {
    lower_mode_abilities_with_scope(modes, kind, subject, None, host_self_reference)
}

/// Variant of `lower_mode_abilities_with_subject` that additionally seeds
/// `relative_player_scope` on the parse context so mode-body "that player"
/// anaphora resolve to the correct player scope established by the trigger
/// condition (e.g. `TriggeringPlayer` for DamageDone triggers).
///
/// For DamageDone triggers the damaged player is the triggering
/// player; "that player" in each modal branch must resolve to them, not the
/// caster or `ParentTargetController`.
pub(crate) fn lower_mode_abilities_with_scope(
    modes: &[ModeAst],
    kind: AbilityKind,
    subject: Option<TargetFilter>,
    relative_player_scope: Option<crate::types::ability::ControllerRef>,
    host_self_reference: Option<TargetFilter>,
) -> Vec<AbilityDefinition> {
    let mut ctx = ParseContext {
        subject,
        host_self_reference,
        relative_player_scope,
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

/// CR 700.2 + CR 608.2d: Try to parse an inline modal trigger body of the form
/// `"choose one — <mode1>; or <mode2>[; or <modeN>]"` that appears as a single
/// sentence (semicolon-separated modes, no bullet lines).
///
/// This handles cards like Grenzo, Havoc Raiser where the entire trigger
/// including modal choices fits on one Oracle text line. Returns `None` if the
/// text does not start with a recognised modal header or contains no `; or `
/// separator.
///
/// The `relative_player_scope` from the trigger condition (e.g.
/// `TriggeringPlayer` for DamageDone triggers) is propagated into every mode
/// body so "that player" anaphora resolve to the correct player.
pub(crate) fn try_parse_inline_modal(
    effect_body: &str,
    relative_player_scope: Option<crate::types::ability::ControllerRef>,
) -> Option<AbilityDefinition> {
    let em_dash_pos = effect_body.find('\u{2014}')?;
    let header_text = effect_body[..em_dash_pos].trim();
    let modes_text = effect_body[em_dash_pos + '\u{2014}'.len_utf8()..].trim();

    let header = parse_modal_header_ast(header_text)?;

    let raw_modes: Vec<&str> = modes_text
        .split("; or ") // allow-noncombinator: structural delimiter split for modal modes
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if raw_modes.len() < 2 {
        return None;
    }

    let modes: Vec<ModeAst> = raw_modes
        .iter()
        .map(|body| {
            let body = body.trim_end_matches('.');
            ModeAst {
                raw: body.to_string(),
                label: None,
                body: body.to_string(),
                mode_cost: None,
                mode_pawprint: None,
            }
        })
        .collect();

    let mode_abilities = lower_mode_abilities_with_scope(
        &modes,
        AbilityKind::Spell,
        None,
        relative_player_scope,
        None,
    );
    Some(
        AbilityDefinition::new(AbilityKind::Spell, modal_marker_effect(&header))
            .with_modal(build_modal_choice(&header, &modes), mode_abilities),
    )
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
// `ModalCountSpec` is module-private; this helper is only ever called from
// within `oracle_modal.rs` (header parsing + its own tests), so it is module-
// private to keep the return type's visibility consistent (CR-neutral — no
// rules impact).
fn parse_modal_choose_count(lower: &str) -> ModalCountSpec {
    let lower = lower.trim();
    let lower = lower.strip_prefix("you may ").unwrap_or(lower).trim_start();

    // Scan for override phrases at word boundaries using nom combinators.
    if let Some(spec) = scan_modal_count_override(lower) {
        return spec;
    }
    // Extract the number word after "choose " using the shared nom combinator.
    if let Some(rest) = lower.strip_prefix("choose ") {
        if let Ok((_, n)) = nom_primitives::parse_number(rest) {
            return ModalCountSpec::Fixed {
                min: n as usize,
                max: n as usize,
            };
        }
    }
    // Default fallback
    ModalCountSpec::Fixed { min: 1, max: 1 }
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

/// CR 207.2d: flavor words (Universes Beyond) are italic ability-word prefixes
/// with no rules meaning; unlike the in-game ability words enumerated by CR
/// 207.2c (which are <=2 words), flavor-word names routinely run 5-6 words
/// ("Woman Who Walked the Earth", "Deal with the Black Guardian"). At the 4-word
/// cap these never strip, so the body behind them never reaches the relevant
/// sub-parser.
///
/// This heuristic cap governs the Priority-6b trigger-dispatch path (oracle.rs),
/// where the post-strip remainder is re-validated structurally rather than by a
/// length-independent guard: its activated branch is gated on
/// `ability_word_to_condition` (known ability words, <=2 words) and its trigger
/// branch re-validates via `has_trigger_prefix`. The 6-word ceiling bounds how
/// far an em-dash sentence may be treated as a label on that path.
///
/// The activated-ability cost-label path uses the wider
/// `FLAVOR_WORD_COST_LABEL_MAX_WORDS` instead — see that constant for why a word
/// count is the wrong guard there.
///
/// All other consumers keep the 4-word `strip_ability_word*` cap.
pub(super) const FLAVOR_WORD_MAX_WORDS: usize = 6;

/// CR 207.2d: the activated-ability cost-label path
/// (`oracle::strip_activated_cost_label`) re-validates the stripped remainder
/// through `cost_prefix_is_activated`, a length-independent guard requiring mana
/// symbols or a cost-starter verb. Because that guard — not the word count — is
/// what distinguishes a genuine flavor-word cost label from an ordinary em-dash
/// line, the word count is the wrong filter here: capping it merely drops valid
/// labels whose names happen to be long. Universes Beyond flavor names run
/// arbitrarily long ("I've Come Up with a New Recipe!", 7 words — Ignis
/// Scientia; "The Most Important Punch in History", 6 words — Duggan), so this
/// path is uncapped and leans entirely on `cost_prefix_is_activated`.
pub(super) const FLAVOR_WORD_COST_LABEL_MAX_WORDS: usize = usize::MAX;

pub(super) fn strip_flavor_word_with_name(line: &str) -> Option<(String, String)> {
    split_short_label_prefix(line, FLAVOR_WORD_MAX_WORDS)
        .map(|(name, rest)| (name.to_lowercase(), rest.to_string()))
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

/// CR 207.2c: Is `name` (already lowercased) a recognized ability word? Used by
/// callers that have already split an em-dash label and need to confirm it is a
/// flavor ability word (no rules meaning) before re-dispatching the body through
/// the ordinary trigger/static machinery — e.g. token-granted abilities whose
/// quoted text begins "Landfall — Whenever a land you control enters, ...".
pub(super) fn is_known_ability_word(name: &str) -> bool {
    ABILITY_WORD_NAMES.contains(&name)
}

/// Match a known ability-word name at the start of a lowercased input, enforcing
/// a trailing word boundary. Returns the remainder after the name.
///
/// CR 207.2c: Ability words have no rules meaning; this combinator is purely
/// for parser dispatch — it lets the reminder-body extractor distinguish
/// `Increment (Whenever ...)` from random lines that happen to start with an
/// open paren.
pub(super) fn parse_known_ability_word_name(
    input: &str,
) -> nom::IResult<&str, &'static str, OracleError<'_>> {
    for name in ABILITY_WORD_NAMES {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(*name).parse(input) {
            // Word-boundary guard: next char must be non-alphanumeric or end.
            if rest.is_empty() || !rest.chars().next().unwrap().is_alphanumeric() {
                return Ok((rest, *name));
            }
        }
    }
    Err(nom::Err::Error(nom::error::Error::new(
        "",
        nom::error::ErrorKind::Fail,
    )))
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

/// CR 700.2: The recognized shape of a modal header's count phrase. `Fixed`
/// holds a statically-resolved `(min, max)` pair; `Dynamic { qty }` marks a
/// "choose up to X / up to that many" header whose maximum resolves at runtime
/// from `qty` and is clamped to `mode_count` (CR 700.2d). `qty` is
/// `CostXPaid` for the cast-{X} subclass (CR 107.3m, The Ruinous Wrecking Crew)
/// and `TimesCostPaidThisResolution` for the repeated-optional-payment subclass
/// (CR 603.12a, Hawkeye, Master Marksman). LOW-1: `Copy` is dropped because
/// `QuantityRef` is not `Copy`.
///
/// Known coverage gap (empty in the current corpus): the
/// `TimesCostPaidThisResolution` cap is emitted cost-agnostically here, but the
/// runtime driver (`is_synchronous_mana_pay_cost`, effects/mod.rs) only handles a
/// pure static-mana repeated cost. A future "choose up to that many." +
/// `WhenYouDo` card whose repeated cost is non-mana / X-mana would parse a cap
/// (so the `Modal_DynamicMaxDropped` swallow-detector stays silent ⇒ "supported")
/// yet fall to the generic `repeat_for` path with the wrong cap — a false-green.
/// Before such a card lands: cross-check the carrying ability's cost is
/// synchronous mana in the coverage detector, or add driver pause-resume plumbing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ModalCountSpec {
    Fixed { min: usize, max: usize },
    Dynamic { qty: QuantityRef },
}

/// Scan for modal count override phrases at word boundaries using nom combinators.
/// Returns the recognized `ModalCountSpec` for matching phrases.
fn scan_modal_count_override(text: &str) -> Option<ModalCountSpec> {
    super::oracle_nom::primitives::scan_at_word_boundaries(text, |input| {
        alt((
            value(
                ModalCountSpec::Fixed {
                    min: 1,
                    max: usize::MAX,
                },
                tag::<_, _, OracleError<'_>>("choose any number instead"),
            ),
            value(
                ModalCountSpec::Fixed { min: 1, max: 2 },
                tag("choose both instead"),
            ),
            value(
                ModalCountSpec::Fixed { min: 1, max: 2 },
                tag("choose two instead"),
            ),
            value(
                ModalCountSpec::Fixed { min: 1, max: 3 },
                tag("choose three instead"),
            ),
            value(ModalCountSpec::Fixed { min: 1, max: 2 }, tag("one or both")),
            value(
                ModalCountSpec::Fixed {
                    min: 1,
                    max: usize::MAX,
                },
                alt((tag("one or more"), tag("any number"))),
            ),
            // CR 700.2 + CR 107.3m: "choose up to X —" — the maximum is the cast
            // {X}, resolved live at runtime; `parse_number` fails on bare "x" so
            // this arm cannot shadow the numeric "choose up to N" arm below.
            //
            // A trailing ", where X is <expr>" clause REDEFINES X to a different
            // quantity (e.g. Bumi "where X is the number of Lesson cards in your
            // graveyard"; Riku "where X is the number of times you chose a
            // mode") and the card carries no cast {X}. Such headers must NOT be
            // read as the cast {X} — the negative lookahead guards them out so
            // they fall through to the fixed default rather than resolving
            // `CostXPaid` (which is 0 for a card with no {X}, silently making
            // the modal choose nothing). Parsing the redefining quantity into
            // `dynamic_max_choices` is a follow-up; this PR's scope is the
            // cast-{X} subclass (The Ruinous Wrecking Crew).
            value(
                ModalCountSpec::Dynamic {
                    qty: QuantityRef::CostXPaid,
                },
                terminated(
                    tag::<_, _, OracleError<'_>>("choose up to x"),
                    not(preceded((opt(tag(",")), multispace0), tag("where"))),
                ),
            ),
            // CR 603.12a + CR 700.2d: "choose up to that many." (Hawkeye, Master
            // Marksman) caps the modal at the resolution-local count of repeated
            // optional payments. MED-1: match the PERIOD/bare/bullet-terminated
            // header form ONLY — the negative lookahead rejects a following
            // em-dash (Tranquil Frillback's "choose up to that many —", whose
            // reflexive condition is NOT WhenYouDo and so is not handled by the
            // repeated-optional-payment driver) and a following noun phrase (the
            // non-modal selection clauses "choose up to that many target
            // creatures you control" / "...creatures tapped this way"). Only a
            // clean termination (period / end / bullet) yields the dynamic cap,
            // so an unhandled card is never silently false-greened.
            value(
                ModalCountSpec::Dynamic {
                    qty: QuantityRef::TimesCostPaidThisResolution,
                },
                terminated(
                    tag::<_, _, OracleError<'_>>("choose up to that many"),
                    not(preceded(
                        multispace0,
                        alt((tag("\u{2014}"), tag("\u{2013}"), tag("-"), alpha1)),
                    )),
                ),
            ),
            // CR 700.2a / CR 700.2d: "choose up to N —" is a modal header where
            // min_choices = 0 (decline all modes) and max_choices = N.
            preceded(tag("choose up to "), nom_primitives::parse_number).map(|n: u32| {
                ModalCountSpec::Fixed {
                    min: 0,
                    max: n as usize,
                }
            }),
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
    fn strip_flavor_word_strips_five_and_six_word_prefixes() {
        // CR 207.2c: Universes-Beyond flavor words run 5-6 words and never strip
        // at the 4-word ability-word cap. The wider cap (used only by the
        // Priority-6b trigger dispatch) recovers the trigger body behind them.
        let (name, body) =
            strip_flavor_word_with_name("Woman Who Walked the Earth — When ~ enters, investigate.")
                .expect("5-word flavor prefix must strip at the wider cap");
        assert_eq!(name, "woman who walked the earth");
        assert_eq!(body, "When ~ enters, investigate.");

        let (name, body) = strip_flavor_word_with_name(
            "Deal with the Black Guardian — When ~ enters, you may have an opponent gain control of it.",
        )
        .expect("5-word flavor prefix must strip");
        assert_eq!(name, "deal with the black guardian");
        assert_eq!(
            body,
            "When ~ enters, you may have an opponent gain control of it."
        );

        // A genuine 6-word prefix also strips.
        let (_, body) =
            strip_flavor_word_with_name("One Two Three Four Five Six — When ~ dies, draw a card.")
                .expect("6-word prefix must strip");
        assert_eq!(body, "When ~ dies, draw a card.");
    }

    #[test]
    fn strip_ability_word_keeps_four_word_cap_for_flavor_lengths() {
        // The narrow ability-word helpers stay at the 4-word cap: a 5-word
        // prefix must NOT strip through them (only the dedicated flavor helper
        // and only on the trigger-dispatch path widens).
        assert_eq!(
            strip_ability_word_with_name(
                "Woman Who Walked the Earth — When ~ enters, investigate."
            ),
            None,
        );
        assert_eq!(
            strip_ability_word("Woman Who Walked the Earth — When ~ enters, investigate."),
            None,
        );
        // A 6-word prefix is beyond even the flavor cap and must not strip.
        assert_eq!(
            strip_flavor_word_with_name("One Two Three Four Five Six Seven — When ~ dies, draw."),
            None,
        );
    }

    fn bare_target_mode(body: &str) -> ModeAst {
        ModeAst {
            raw: format!("{body}."),
            label: None,
            body: body.to_string(),
            mode_cost: None,
            mode_pawprint: None,
        }
    }

    #[test]
    fn parse_shared_those_template_splits_around_anaphor() {
        let (prefix, suffix) = parse_shared_those_template(
            "Choose up to two. Return those cards from your graveyard to your hand.",
        )
        .expect("template must parse");
        assert_eq!(prefix, "Return ");
        assert_eq!(suffix, " from your graveyard to your hand.");
    }

    #[test]
    fn parse_shared_those_template_requires_effect_sentence() {
        // A bare choose-count header carries no shared effect.
        assert_eq!(parse_shared_those_template("Choose up to two."), None);
        // A second sentence without a `those <noun>` anaphor is not a template.
        assert_eq!(
            parse_shared_those_template("Choose one. You may choose the same mode more than once."),
            None
        );
    }

    #[test]
    fn distribute_shared_mode_effect_covers_card_type_axis() {
        // CR 700.2 + CR 601.2c: the card-type is the only per-mode axis; each
        // bare target gets the shared "Return … from your graveyard to your
        // hand" effect.
        let modes = vec![
            bare_target_mode("Target artifact card"),
            bare_target_mode("Target creature card"),
            bare_target_mode("Target enchantment card"),
            bare_target_mode("Target land card"),
        ];
        let out = distribute_shared_mode_effect(
            "Choose up to two. Return those cards from your graveyard to your hand.",
            modes,
        );
        assert_eq!(
            out.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(),
            vec![
                "Return target artifact card from your graveyard to your hand.",
                "Return target creature card from your graveyard to your hand.",
                "Return target enchantment card from your graveyard to your hand.",
                "Return target land card from your graveyard to your hand.",
            ]
        );
        // `raw` (the bullet text shown in `mode_descriptions`) is preserved.
        assert_eq!(out[0].raw, "Target artifact card.");
    }

    #[test]
    fn distribute_shared_mode_effect_leaves_full_effect_modes_unchanged() {
        // Standard modal modes already carry their own verb; the bare-target
        // gate must not clobber them.
        let modes = vec![
            bare_target_mode("Draw a card"),
            bare_target_mode("Gain 3 life"),
        ];
        let out = distribute_shared_mode_effect(
            "Choose one. Return those cards from your graveyard to your hand.",
            modes.clone(),
        );
        assert_eq!(out, modes, "non-bare-target modes must be left untouched");
    }

    #[test]
    fn distribute_shared_mode_effect_noop_without_shared_template() {
        // A plain modal with no shared-effect sentence is unchanged even when
        // every mode is a bare target.
        let modes = vec![
            bare_target_mode("Target artifact card"),
            bare_target_mode("Target creature card"),
        ];
        let out = distribute_shared_mode_effect("Choose up to two.", modes.clone());
        assert_eq!(out, modes);
    }

    #[test]
    fn mode_body_is_bare_target_discriminates_target_keyword() {
        assert!(mode_body_is_bare_target("Target artifact card"));
        assert!(mode_body_is_bare_target("Target creature card."));
        // A descriptor phrase is not a "target" mode.
        assert!(!mode_body_is_bare_target("Destroy target creature"));
        // A bare target with a trailing verb is not a bare target.
        assert!(!mode_body_is_bare_target("Target creature gets +1/+1"));
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

    fn fixed(min: usize, max: usize) -> ModalCountSpec {
        ModalCountSpec::Fixed { min, max }
    }

    #[test]
    fn parse_modal_choose_count_variants() {
        assert_eq!(parse_modal_choose_count("choose one —"), fixed(1, 1));
        assert_eq!(parse_modal_choose_count("choose two —"), fixed(2, 2));
        assert_eq!(parse_modal_choose_count("you may choose two."), fixed(2, 2));
        assert_eq!(parse_modal_choose_count("choose three —"), fixed(3, 3));
        assert_eq!(
            parse_modal_choose_count("choose one or both —"),
            fixed(1, 2)
        );
        assert_eq!(
            parse_modal_choose_count("choose one or more —"),
            fixed(1, usize::MAX)
        );
        assert_eq!(
            parse_modal_choose_count("choose any number of —"),
            fixed(1, usize::MAX)
        );
    }

    #[test]
    fn parse_modal_header_choose_one_at_random_sets_random_selection() {
        // CR 700.2b (override): "choose one at random" (Cult of Skaro) — the
        // game selects the mode, so the header records TargetSelectionMode::Random
        // while still parsing the count as one.
        let header =
            parse_modal_header_ast("choose one at random —").expect("modal header recognized");
        assert_eq!(header.min_choices, 1);
        assert_eq!(header.max_choices, 1);
        assert_eq!(header.selection, TargetSelectionMode::Random);
    }

    #[test]
    fn parse_modal_header_plain_choose_one_stays_chosen() {
        // Building-block regression: ordinary modal headers default to Chosen.
        let header = parse_modal_header_ast("choose one —").expect("modal header recognized");
        assert_eq!(header.selection, TargetSelectionMode::Chosen);
    }

    // B3: "choose up to N —" must parse as (0, N), not fall through to the
    // default (1, 1). Without this, players are forced to pick exactly one
    // mode when the CR allows zero. Affects Biblioplex Tomekeeper and ~96
    // other cards in the corpus (grep "choose up to" card-data.json).
    #[test]
    fn parse_modal_choose_count_up_to_variants() {
        assert_eq!(parse_modal_choose_count("choose up to one —"), fixed(0, 1));
        assert_eq!(parse_modal_choose_count("choose up to two —"), fixed(0, 2));
        assert_eq!(
            parse_modal_choose_count("choose up to seven —"),
            fixed(0, 7)
        );
        assert_eq!(
            parse_modal_choose_count("you may choose up to two."),
            fixed(0, 2)
        );
    }

    // CR 700.2 + CR 107.3m: "choose up to X —" is a cast-{X} dynamic header, not
    // a numeric fixed cap. `parse_number` fails on bare "x" so it cannot shadow
    // the numeric "choose up to N" arm.
    #[test]
    fn parse_modal_choose_count_up_to_x_is_dynamic() {
        assert_eq!(
            parse_modal_choose_count("choose up to x —"),
            ModalCountSpec::Dynamic {
                qty: QuantityRef::CostXPaid
            }
        );
    }

    // CR 603.12a + CR 700.2d: "choose up to that many." (Hawkeye, period/bare
    // form) is the repeated-optional-payment dynamic header. Revert
    // discriminator: dropping the new `value()/tag()` arm makes this fall to the
    // fixed `(1, 1)` default (`fixed(1, 1)`), failing the assertion.
    #[test]
    fn parse_modal_choose_count_up_to_that_many_period_is_dynamic() {
        assert_eq!(
            parse_modal_choose_count("choose up to that many"),
            ModalCountSpec::Dynamic {
                qty: QuantityRef::TimesCostPaidThisResolution
            }
        );
        assert_eq!(
            parse_modal_choose_count("choose up to that many."),
            ModalCountSpec::Dynamic {
                qty: QuantityRef::TimesCostPaidThisResolution
            }
        );
    }

    // MED-1 guard: the "that many" arm must NOT match the em-dash header
    // (Tranquil Frillback, whose reflexive condition is not WhenYouDo and so is
    // not handled by the repeated-optional-payment driver — matching it would
    // false-green an unhandled card) nor the non-modal selection clauses
    // ("choose up to that many target creatures you control"). These fall to the
    // fixed default. Revert the negative lookahead → both wrongly become Dynamic.
    #[test]
    fn parse_modal_choose_count_up_to_that_many_em_dash_and_noun_are_not_dynamic() {
        // Tranquil Frillback (em-dash continuation).
        assert_eq!(
            parse_modal_choose_count("choose up to that many \u{2014}"),
            fixed(1, 1)
        );
        // Heroic Feast (non-modal selection clause).
        assert_eq!(
            parse_modal_choose_count("choose up to that many target creatures you control"),
            fixed(1, 1)
        );
    }

    // CR 700.2 + CR 107.3m: a trailing ", where X is <expr>" clause REDEFINES X
    // to a quantity other than the cast {X} (Bumi → Lesson cards in graveyard;
    // Riku → number of times you chose a mode), and such cards carry no {X} in
    // their cost. These headers must NOT classify as `DynamicCostX` (which
    // resolves `CostXPaid` == 0 for them, silently choosing nothing); they fall
    // through to the fixed `(1, 1)` default. This negative discriminates the
    // word-boundary `not(... "where")` guard — reverting it makes both match
    // `DynamicCostX`.
    #[test]
    fn parse_modal_choose_count_up_to_x_redefined_is_not_dynamic() {
        // Bumi, King of Three Trials.
        assert_eq!(
            parse_modal_choose_count(
                "choose up to x, where x is the number of lesson cards in your graveyard —"
            ),
            fixed(1, 1)
        );
        // Riku of Many Paths.
        assert_eq!(
            parse_modal_choose_count(
                "choose up to x, where x is the number of times you chose a mode for that spell —"
            ),
            fixed(1, 1)
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

    /// Aerith Rescue Mission (std long-tail): a mode flavor name can exceed the
    /// 4-word ability-word cap ("Take 59 Flights of Stairs" = 5 words). The
    /// long-flavor split must strip the flavor label so the body
    /// ("Tap up to three target creatures...") reaches the effect parser,
    /// instead of the whole "Take 59 Flights of Stairs — Tap ..." text falling
    /// to `Effect::Unimplemented`. Revert-discriminating: the 3-word mode keeps
    /// parsing via `split_short_label_prefix`, but the 5-word mode's body would
    /// retain its flavor prefix without `split_mode_flavor_label`.
    /// CR 207.2c: ability/flavor words carry no rules meaning.
    #[test]
    fn collect_mode_asts_long_flavor_label_strips_to_body() {
        let lines = vec![
            "Choose one —",
            "• Take the Elevator — Create three 1/1 colorless Hero creature tokens.",
            "• Take 59 Flights of Stairs — Tap up to three target creatures. Put a stun counter on one of them.",
        ];
        let modes = collect_mode_asts(&lines, 1);
        assert_eq!(modes.len(), 2);
        assert_eq!(
            modes[0].label.as_deref(),
            Some("Take the Elevator"),
            "short (3-word) flavor label still splits"
        );
        assert_eq!(
            modes[0].body,
            "Create three 1/1 colorless Hero creature tokens."
        );
        assert_eq!(
            modes[1].label.as_deref(),
            Some("Take 59 Flights of Stairs"),
            "long (5-word) flavor label must be stripped via split_mode_flavor_label"
        );
        assert_eq!(
            modes[1].body, "Tap up to three target creatures. Put a stun counter on one of them.",
            "the long-flavor mode body must drop the flavor prefix"
        );
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

    #[test]
    fn parse_pawprint_run_counts_symbols() {
        // CR 700.2i: one or more "{P}" → weight.
        assert_eq!(parse_pawprint_run("{P}").unwrap().1, 1);
        assert_eq!(parse_pawprint_run("{P}{P}").unwrap().1, 2);
        assert_eq!(parse_pawprint_run("{P}{P}{P}").unwrap().1, 3);
        // A non-pawprint line (bullet) must NOT match.
        assert!(parse_pawprint_run("•").is_err());
        assert!(parse_pawprint_run("Draw a card.").is_err());
    }

    #[test]
    fn collect_mode_asts_pawprint_lines_extract_weight_and_body() {
        let lines = vec![
            "Choose up to five {P} worth of modes. You may choose the same mode more than once.",
            "{P} — Draw a card.",
            "{P}{P} — Gain 3 life.",
            "{P}{P}{P} — Deal 3 damage to any target.",
        ];
        let modes = collect_mode_asts(&lines, 1);
        assert_eq!(modes.len(), 3);
        assert_eq!(modes[0].mode_pawprint, Some(1));
        assert_eq!(modes[0].body, "Draw a card.");
        assert_eq!(modes[1].mode_pawprint, Some(2));
        assert_eq!(modes[2].mode_pawprint, Some(3));
        // Pawprint modes never carry a Spree mode cost.
        assert!(modes.iter().all(|m| m.mode_cost.is_none()));
    }

    #[test]
    fn build_modal_choice_leaves_pawprint_budget_unclamped() {
        // CR 700.2i: `max_choices` is the 5-point budget, NOT clamped to the
        // mode_count of 3. This is the cap-bug regression guard at the parser
        // level (the bug clamped 5 → 3 via `header.max_choices.min(mode_count)`).
        let lines = vec![
            "Choose up to five {P} worth of modes. You may choose the same mode more than once.",
            "{P} — Draw a card.",
            "{P}{P} — Gain 3 life.",
            "{P}{P}{P} — Deal 3 damage to any target.",
        ];
        let modes = collect_mode_asts(&lines, 1);
        let header = parse_modal_header_ast(lines[0]).expect("header should parse");
        let modal = build_modal_choice(&header, &modes);
        assert_eq!(modal.mode_count, 3);
        assert_eq!(modal.mode_pawprints, vec![1u8, 2, 3]);
        assert_eq!(modal.max_choices, 5, "budget is uncapped, not clamped to 3");
    }

    /// T1 — CR 700.2 + CR 107.3m: The Ruinous Wrecking Crew's "choose up to X —"
    /// ETB modal (4 modes) parses to `min_choices == 0`, a `CostXPaid` dynamic
    /// max, and a static `max_choices` clamped to the mode_count of 4 (NOT the
    /// `usize::MAX` placeholder). Before the fix this header fell through to the
    /// fixed `(1, 1)` / `dynamic_max_choices: None` path.
    #[test]
    fn build_modal_choice_ruinous_choose_up_to_x_is_dynamic() {
        let lines = vec![
            "choose up to x —",
            "• Discard a card, then draw a card.",
            "• Target opponent loses 2 life.",
            "• Destroy target token.",
            "• Each player sacrifices a creature of their choice.",
        ];
        let modes = collect_mode_asts(&lines, 1);
        assert_eq!(modes.len(), 4, "all four bulleted modes collected");
        let header = parse_modal_header_ast(lines[0]).expect("header should parse");
        let modal = build_modal_choice(&header, &modes);

        assert_eq!(modal.min_choices, 0, "choose up to X declines all modes");
        assert_eq!(modal.mode_count, 4);
        assert_eq!(
            modal.dynamic_max_choices,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid
            }),
            "dynamic cap references the cast {{X}}"
        );
        assert_eq!(
            modal.max_choices, 4,
            "static placeholder is mode_count (usize::MAX clamped), not usize::MAX"
        );
        assert_ne!(
            modal.max_choices,
            usize::MAX,
            "must not serialize usize::MAX"
        );
    }

    /// T4 — serde round-trip: `dynamic_max_choices` survives serialization when
    /// `Some(CostXPaid)` and is omitted from the JSON (and round-trips to `None`)
    /// when absent, matching `skip_serializing_if = "Option::is_none"`.
    #[test]
    fn modal_choice_dynamic_max_choices_serde_round_trip() {
        let dynamic = ModalChoice {
            min_choices: 0,
            max_choices: 4,
            mode_count: 4,
            dynamic_max_choices: Some(QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&dynamic).expect("serialize dynamic modal");
        assert!(
            // allow-noncombinator: serde JSON-output assertion in a test, not parser dispatch.
            json.contains("dynamic_max_choices"),
            "Some(..) field is serialized: {json}"
        );
        let back: ModalChoice = serde_json::from_str(&json).expect("deserialize dynamic modal");
        assert_eq!(back, dynamic, "dynamic round-trips identically");

        let fixed = ModalChoice {
            min_choices: 0,
            max_choices: 2,
            mode_count: 3,
            dynamic_max_choices: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&fixed).expect("serialize fixed modal");
        assert!(
            // allow-noncombinator: serde JSON-output assertion in a test, not parser dispatch.
            !json.contains("dynamic_max_choices"),
            "None omits the key: {json}"
        );
        let back: ModalChoice = serde_json::from_str(&json).expect("deserialize fixed modal");
        assert_eq!(back.dynamic_max_choices, None, "absent key → None");
    }

    // ---- Ao, the Dawn Sky (SOC) — modal dies trigger integration ----

    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{
        ChoiceType, Effect, StaticCondition, TargetFilter, TriggerCondition,
    };
    use crate::types::replacements::ReplacementEvent;
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

    const RUINOUS_ORACLE: &str = "The Ruinous Wrecking Crew enters with X +1/+1 counters on it.\n\
When The Ruinous Wrecking Crew enters, choose up to X —\n\
• Discard a card, then draw a card.\n\
• Target opponent loses 2 life.\n\
• Destroy target token.\n\
• Each player sacrifices a creature of their choice.";

    /// T1 (production path) — CR 700.2b + CR 107.3m: the full oracle pipeline
    /// lowers The Ruinous Wrecking Crew's "choose up to X —" ETB into a modal
    /// triggered ability whose `ModalChoice` carries `min_choices == 0`, the
    /// `CostXPaid` dynamic max, and a static `max_choices` of `mode_count` (4),
    /// not the `usize::MAX` placeholder. Before the fix the same header lowered
    /// to a fixed `(1, 1)` cap with `dynamic_max_choices: None`.
    #[test]
    fn ruinous_choose_up_to_x_lowers_to_dynamic_modal() {
        let parsed = parse_oracle_text(
            RUINOUS_ORACLE,
            "The Ruinous Wrecking Crew",
            &[],
            &["Creature".to_string()],
            &[],
        );
        let modal = parsed
            .triggers
            .iter()
            .find_map(|t| t.execute.as_deref().and_then(|e| e.modal.as_ref()))
            .expect("ETB modal trigger with modal metadata");
        assert_eq!(modal.min_choices, 0, "choose up to X declines all modes");
        assert_eq!(modal.mode_count, 4);
        assert_eq!(
            modal.dynamic_max_choices,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid
            }),
            "live cap references the cast {{X}}"
        );
        assert_eq!(
            modal.max_choices, 4,
            "static placeholder clamped to mode_count"
        );
        assert_ne!(modal.max_choices, usize::MAX);
    }

    const FROSTCLIFF_SIEGE_ORACLE: &str = "As this enchantment enters, choose Jeskai or Temur.\n\
• Jeskai — Whenever one or more creatures you control deal combat damage to a player, draw a card.\n\
• Temur — Creatures you control get +1/+0 and have trample and haste.";

    #[test]
    fn frostcliff_siege_anchor_word_modal_lowers_choice_and_linked_gates() {
        // CR 614.12c + CR 607.2d: anchor-word permanents lower to one
        // as-enters labeled choice and one chosen-label gate on each linked
        // ability. This is parser-only so it does not depend on generated
        // card-data.json being present in the checkout.
        let parsed = parse_oracle_text(FROSTCLIFF_SIEGE_ORACLE, "Frostcliff Siege", &[], &[], &[]);

        assert_eq!(parsed.replacements.len(), 1);
        let replacement = &parsed.replacements[0];
        assert_eq!(replacement.event, ReplacementEvent::Moved);
        assert_eq!(replacement.destination_zone, Some(Zone::Battlefield));
        let execute = replacement.execute.as_ref().expect("choice execute");
        match execute.effect.as_ref() {
            Effect::Choose {
                choice_type: ChoiceType::Labeled { options },
                persist,
                ..
            } => {
                assert!(*persist);
                assert_eq!(options, &vec!["Jeskai".to_string(), "Temur".to_string()]);
            }
            other => panic!("expected persisted labeled choose, got {other:?}"),
        }

        assert_eq!(parsed.triggers.len(), 1);
        assert_eq!(
            parsed.triggers[0].mode,
            TriggerMode::DamageDoneOnceByController
        );
        assert!(matches!(
            parsed.triggers[0]
                .execute
                .as_ref()
                .map(|ability| ability.effect.as_ref()),
            Some(Effect::Draw { .. })
        ));
        assert!(matches!(
            parsed.triggers[0].condition.as_ref(),
            Some(TriggerCondition::ChosenLabelIs { label }) if label == "Jeskai"
        ));

        assert_eq!(parsed.statics.len(), 1);
        assert!(matches!(
            parsed.statics[0].condition.as_ref(),
            Some(StaticCondition::ChosenLabelIs { label }) if label == "Temur"
        ));
        assert_eq!(parsed.statics[0].modifications.len(), 4);
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

    // ---- Chooser-prefixed modal headers (CR 700.2e) ----

    #[test]
    fn you_choose_one_modal_parses_as_controller_chooser() {
        // CR 700.2a: "You choose one —" is the controller-chooser alias of a
        // bare `Choose one —`. On HEAD this produces `modal: None`.
        const ORACLE: &str = "You choose one —\n• Draw a card.\n• You gain 3 life.";
        let parsed = parse_oracle_text(ORACLE, "Test You Choose", &[], &[], &[]);
        let modal = parsed.modal.as_ref().expect("should parse as modal");
        assert_eq!(modal.chooser, PlayerFilter::Controller);
        assert_eq!((modal.min_choices, modal.max_choices), (1, 1));
        assert_eq!(parsed.abilities.len(), 2);
        for ability in &parsed.abilities {
            assert!(
                !matches!(*ability.effect, Effect::Unimplemented { .. }),
                "mode should lower to a concrete effect: {:?}",
                ability.effect
            );
        }
    }

    #[test]
    fn an_opponent_chooses_one_modal() {
        // CR 700.2e: "An opponent chooses one —" routes the mode choice to the
        // opponent. On HEAD this produces `modal: None`.
        const ORACLE: &str = "An opponent chooses one —\n• Draw a card.\n• You gain 3 life.";
        let parsed = parse_oracle_text(ORACLE, "Test Opponent Choose", &[], &[], &[]);
        let modal = parsed.modal.as_ref().expect("should parse as modal");
        assert_eq!(modal.chooser, PlayerFilter::Opponent);
        assert_eq!((modal.min_choices, modal.max_choices), (1, 1));
        assert_eq!(modal.mode_count, 2);
        assert_eq!(parsed.abilities.len(), 2);
    }

    #[test]
    fn an_opponent_chooses_two_modal() {
        // The shared count recognizer still resolves the count on the
        // post-prefix remainder: "an opponent chooses two —" → (2, 2).
        const ORACLE: &str = "An opponent chooses two —\n\
• Draw a card.\n• You gain 3 life.\n• You lose 3 life.";
        let parsed = parse_oracle_text(ORACLE, "Test Opponent Choose Two", &[], &[], &[]);
        let modal = parsed.modal.as_ref().expect("should parse as modal");
        assert_eq!(modal.chooser, PlayerFilter::Opponent);
        assert_eq!((modal.min_choices, modal.max_choices), (2, 2));
        assert_eq!(modal.mode_count, 3);
    }

    #[test]
    fn target_opponent_chooses_stays_unhandled() {
        // DEFERRED (plan 03): "Target opponent chooses one —" needs a real
        // `TargetRef::Player` declared in the casting flow before the CR
        // 601.2b mode choice. Plan 03 adds no `target opponent chooses` arm —
        // the card keeps `modal: None`. Regression guard, not a HEAD-failing
        // test.
        const ORACLE: &str = "Target opponent chooses one —\n• Draw a card.\n• You gain 3 life.";
        let parsed = parse_oracle_text(ORACLE, "Test Target Opponent", &[], &[], &[]);
        assert!(
            parsed.modal.is_none(),
            "targeted-chooser modal is deferred and must stay unhandled"
        );
    }

    #[test]
    fn each_opponent_chooses_stays_unhandled() {
        // DEFERRED (plan 03): "Each opponent chooses one —" has one
        // independent chooser per opponent — a single-`PlayerId` chooser
        // cannot represent it. Plan 03 adds no `each opponent chooses` arm.
        const ORACLE: &str = "Each opponent chooses one —\n• Draw a card.\n• You gain 3 life.";
        let parsed = parse_oracle_text(ORACLE, "Test Each Opponent", &[], &[], &[]);
        assert!(
            parsed.modal.is_none(),
            "each-opponent-chooser modal is deferred and must stay unhandled"
        );
    }

    #[test]
    fn chooser_prefix_without_bullets_is_not_modal() {
        // Biggest Risk mitigation: a non-bulleted sentence containing
        // "you choose …" must NOT be misclassified as a modal block —
        // `parse_oracle_block` gates on a non-empty bullet list.
        const ORACLE: &str =
            "When this creature enters, you choose a card in your hand and discard it.";
        let parsed = parse_oracle_text(ORACLE, "Test No Bullets", &[], &[], &[]);
        assert!(
            parsed.modal.is_none(),
            "a chooser clause with no bulleted modes must not parse as modal"
        );
    }

    #[test]
    fn parse_modal_chooser_prefix_recognizes_both_arms() {
        assert_eq!(
            parse_modal_chooser_prefix("you choose one —").map(|(_, c)| c),
            Ok(PlayerFilter::Controller)
        );
        assert_eq!(
            parse_modal_chooser_prefix("an opponent chooses two —").map(|(_, c)| c),
            Ok(PlayerFilter::Opponent)
        );
        // Deferred forms are not recognized.
        assert!(parse_modal_chooser_prefix("target opponent chooses one —").is_err());
        assert!(parse_modal_chooser_prefix("each opponent chooses one —").is_err());
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

    // --- Caesar, Legion's Emperor (issue #2857): reflexive optional-cost
    // gated modal trigger ---

    const CAESAR_ORACLE: &str = "Whenever you attack, you may sacrifice another creature. When you do, choose two —\n\
        • Create two 1/1 red and white Soldier creature tokens with haste that are tapped and attacking.\n\
        • You draw a card and you lose 1 life.\n\
        • Caesar deals damage equal to the number of creature tokens you control to target opponent.";

    #[test]
    fn match_when_you_do_absorbs_trailing_comma() {
        // CR 603.12: the reflexive connector combinator consumes "when you do"
        // and an optional trailing ", ".
        let (rest, ()) =
            nom_condition::match_when_you_do("when you do, choose two").expect("matches connector");
        assert_eq!(rest, "choose two");
        let (rest, ()) = nom_condition::match_when_you_do("when you do").expect("matches bare");
        assert_eq!(rest, "");
        assert!(nom_condition::match_when_you_do("if you do").is_err());
    }

    #[test]
    fn split_reflexive_optional_cost_extracts_trigger_and_cost() {
        // CR 603.12 + CR 700.2b: Caesar's trigger header splits into the bare
        // attack trigger and the sentence-cased sacrifice cost; the "you may "
        // marker and the trailing ". When you do" connector are stripped.
        let (trigger, cost) = split_reflexive_optional_cost(
            "Whenever you attack, you may sacrifice another creature. When you do",
        )
        .expect("Caesar's reflexive header must split");
        assert_eq!(trigger, "Whenever you attack");
        assert_eq!(cost, "Sacrifice another creature");
    }

    #[test]
    fn split_reflexive_optional_cost_rejects_plain_triggered_modal() {
        // Pip-Boy 3000's trigger has no "you may" optional cost nor a
        // "when you do" reflexive connector — it stays a plain triggered modal.
        assert_eq!(
            split_reflexive_optional_cost("Whenever equipped creature attacks"),
            None
        );
        // A "you may" with no reflexive connector is not a gated modal header.
        assert_eq!(
            split_reflexive_optional_cost("Whenever you attack, you may draw a card"),
            None
        );
    }

    #[test]
    fn caesar_lowers_to_reflexive_gated_modal() {
        // CR 603.12 + CR 700.2b: Caesar's attack trigger must lower to an
        // `Effect::Sacrifice { optional }` whose `WhenYouDo` sub_ability carries
        // the choose-two modal with three modes — NOT a bare modal attached
        // directly to the trigger (which would fire the modes unconditionally).
        let parsed = parse_oracle_text(
            CAESAR_ORACLE,
            "Caesar, Legion's Emperor",
            &[],
            &["Legendary".to_string(), "Creature".to_string()],
            &["Human".to_string(), "Soldier".to_string()],
        );
        let trigger = parsed
            .triggers
            .first()
            .expect("Caesar has an attack trigger");
        let execute = trigger
            .execute
            .as_deref()
            .expect("attack trigger has an execute chain");

        assert!(
            matches!(&*execute.effect, Effect::Sacrifice { .. }),
            "the attack trigger executes an optional Sacrifice, got {:?}",
            execute.effect
        );
        assert!(
            execute.optional,
            "the sacrifice is optional ('you may'), not a mandatory PayCost gate"
        );

        let modal_sub = execute
            .sub_ability
            .as_deref()
            .expect("the sacrifice has a WhenYouDo modal sub-ability");
        assert_eq!(
            modal_sub.condition,
            Some(AbilityCondition::WhenYouDo),
            "the modal is gated on WhenYouDo so it resolves only after the sacrifice"
        );
        let modal = modal_sub
            .modal
            .as_ref()
            .expect("the WhenYouDo sub carries the modal choice");
        assert_eq!(modal.mode_count, 3, "choose-two over three modes");
        assert_eq!(modal.min_choices, 2);
        assert_eq!(modal.max_choices, 2);
        assert_eq!(
            modal_sub.mode_abilities.len(),
            3,
            "one AbilityDefinition per mode"
        );
    }
}
