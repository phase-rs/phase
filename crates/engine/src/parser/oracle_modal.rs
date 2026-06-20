use crate::parser::oracle_nom::error::{OracleError, OracleResult};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::{map, opt, success, value};
use nom::multi::fold_many1;
use nom::sequence::{delimited, preceded, terminated};
use nom::Parser;

use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, AdditionalCostOrigin,
    AdditionalCostPaymentSource, ChoiceType, Effect, ModalChoice, ModalSelectionCondition,
    ModalSelectionConstraint, PlayerFilter, ReplacementDefinition, StaticCondition, TargetFilter,
    TargetSelectionMode, TriggerCondition,
};
use crate::types::replacements::ReplacementEvent;

use super::oracle::{find_activated_colon, strip_activated_constraints};
use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::{parse_effect_chain_with_context, try_parse_named_choice};
use super::oracle_ir::context::ParseContext;
use super::oracle_nom::condition as nom_condition;
use super::oracle_nom::primitives::{self as nom_primitives, scan_preceded};
use super::oracle_static::parse_static_line;
use super::oracle_trigger::parse_trigger_lines;
use super::oracle_util::{parse_mana_symbols, strip_reminder_text};
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

    ModeAst {
        raw: text.to_string(),
        label: None,
        body: text.to_string(),
        mode_cost: None,
        mode_pawprint: None,
    }
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
fn parse_modal_count_remainder(remainder: &str) -> Option<(usize, usize)> {
    let remainder = remainder.trim_start();
    if let Some(count) = scan_modal_count_override(remainder) {
        return Some(count);
    }
    nom_primitives::parse_number(remainder)
        .ok()
        .map(|(_, n)| (n as usize, n as usize))
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

    let (min_choices, max_choices) =
        if chooser == PlayerFilter::Controller && count_input == header_lower {
            // Bare `Choose …` header — unchanged path.
            parse_modal_choose_count(&header_lower)
        } else {
            // Chooser-prefixed remainder ("one —", "two —", …) — reuse the
            // shared count recognizer; `is_modal_header_text` already gated
            // that the remainder is a genuine count phrase.
            parse_modal_count_remainder(&count_input).unwrap_or((1, 1))
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
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("this spell was cast using teamwork"),
        tag("it was cast using teamwork"),
    ))
    .parse(input)
    {
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
            let mut modal_ability = build_modal_ability_with_subject(
                AbilityKind::Spell,
                &header,
                &modes,
                modal_subject,
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
fn build_modal_ability_with_subject(
    kind: AbilityKind,
    header: &ModalHeaderAst,
    modes: &[ModeAst],
    subject: Option<TargetFilter>,
    host_self_reference: Option<TargetFilter>,
) -> AbilityDefinition {
    AbilityDefinition::new(kind, modal_marker_effect(header)).with_modal(
        build_modal_choice(header, modes),
        lower_mode_abilities_with_subject(modes, kind, subject, host_self_reference),
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
/// CR 603.7c: For DamageDone triggers the damaged player is the triggering
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

/// Scan for modal count override phrases at word boundaries using nom combinators.
/// Returns (min_choices, max_choices) for matching phrases.
fn scan_modal_count_override(text: &str) -> Option<(usize, usize)> {
    super::oracle_nom::primitives::scan_at_word_boundaries(text, |input| {
        alt((
            value(
                (1, usize::MAX),
                tag::<_, _, OracleError<'_>>("choose any number instead"),
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
