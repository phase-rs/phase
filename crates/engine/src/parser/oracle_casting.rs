use crate::parser::oracle_nom::bridge::nom_on_lower;
use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::{all_consuming, map, opt, value};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::oracle_cost::parse_oracle_cost;
use super::oracle_util::{parse_mana_symbols, parse_ordinal, TextPair};
use crate::parser::oracle_condition::parse_restriction_condition;
use crate::types::ability::{
    AbilityCost, AdditionalCost, CastingRestriction, Comparator, ParsedCondition, QuantityExpr,
    QuantityRef, SpellCastingOption,
};

/// Split a combined additional-cost line from its trailing self-spell cost
/// reduction (Rottenmouth Viper class: "...sacrifice N. This spell costs {1}
/// less to cast for each permanent sacrificed this way.").
pub(crate) fn split_additional_cost_trailing_spell_reduction<'a>(
    line: &'a str,
    lower: &'a str,
) -> (&'a str, Option<&'a str>) {
    let Some(((), reduction_text)) = nom_on_lower(line, lower, |input| {
        value((), (take_until(". this spell costs "), tag(". "))).parse(input)
    }) else {
        return (line, None);
    };
    let activation_len = line.len() - ". ".len() - reduction_text.len();
    (line[..activation_len].trim(), Some(reduction_text))
}

/// Parse "As an additional cost to cast this spell, ..." into an `AdditionalCost`.
///
/// Recognized patterns:
/// - "you may blight N" → `Optional(Blight { count: N })`
/// - "blight N or pay {M}" → `Choice(Blight { count: N }, Mana { cost: M })`
/// - General "X or Y" → `Choice(X, Y)` using `parse_single_cost` for each fragment
pub fn parse_additional_cost_line(lower: &str, raw: &str) -> Option<AdditionalCost> {
    // Strip the standard additional-cost prefix.
    let after_prefix = tag::<_, _, OracleError<'_>>("as an additional cost to cast this spell, ")
        .parse(lower)
        .map_or(lower, |(rest, _)| rest);
    // Use TextPair for case-preserving parallel slicing, then strip trailing period.
    let tp = TextPair::new(&raw[raw.len() - after_prefix.len()..], after_prefix);
    let tp = tp.trim_end_matches('.');
    let body_lower = tp.lower;
    let body_raw = tp.original;

    // CR 701.4a: A spelled-out "choose … you control or reveal … from your hand"
    // behold cost (Monstrous Emergence) is a single cohesive cost whose internal
    // " or " separates the two legs of ONE behold action — not two independent
    // alternative costs. It must be recognized as a whole BEFORE the general
    // "X or Y" split below would fragment it into a spurious `Choice`.
    let behold = super::oracle_cost::parse_single_cost(body_raw);
    if matches!(behold, AbilityCost::Behold { .. }) {
        return Some(AdditionalCost::Required(behold));
    }

    // CR 701.4a + CR 601.2b/f: A line that unambiguously opens the spelled-out
    // choose-behold cost ("choose a/an <type> you control or ...") but whose
    // alternative leg is NOT a recognized behold-reveal alternative is NOT a real
    // `Behold` (the behold check above declined it) and must not be allowed to
    // misparse. Without this guard the general "X or Y" split below — or the
    // single-cost effect fallback — silently swallows only the "choose ... you
    // control" leg as a `TargetOnly` cost and drops the alternative entirely
    // (Close Encounter's "or a warped creature card you own in exile":
    // exile-zone selection plus the "warped" property are unsupported by
    // `eligible_behold_choices`). That leaves the card falsely green while the
    // damage clause references a chosen object no cost ever produces. Surface an
    // honest unimplemented cost so coverage stays red. Scoped to exactly this
    // prefix shape so ordinary "X or Y" alternative costs are unaffected.
    if is_choose_behold_prefix(body_lower) {
        return Some(AdditionalCost::Required(AbilityCost::Unimplemented {
            description: body_raw.to_string(),
        }));
    }

    // "you may [cost]" → Optional wrapping
    if let Ok((opt_lower, _)) = tag::<_, _, OracleError<'_>>("you may ").parse(body_lower) {
        let opt_raw = &body_raw[body_raw.len() - opt_lower.len()..];
        let cost = super::oracle_cost::parse_single_cost(opt_raw);
        if !matches!(cost, AbilityCost::Unimplemented { .. }) {
            return Some(AdditionalCost::Optional {
                cost,
                repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
            });
        }
    }

    // "X or pay {M}" → Choice between cost X and mana payment.
    // Uses the raw text for mana symbols (case-sensitive).
    if let Some((left_lower, right_lower)) = body_lower.split_once(" or pay ") {
        let right_raw = &body_raw[body_raw.len() - right_lower.len()..];
        if let Some((mana_cost, _)) = parse_mana_symbols(right_raw.trim()) {
            let cost_a = super::oracle_cost::parse_single_cost(left_lower.trim());
            if !matches!(cost_a, AbilityCost::Unimplemented { .. }) {
                return Some(AdditionalCost::Choice(
                    cost_a,
                    AbilityCost::Mana { cost: mana_cost },
                ));
            }
        }
    }

    // General "X or Y" choice pattern using parse_single_cost for each fragment.
    if let Some((left, right)) = body_lower.split_once(" or ") {
        let cost_a = super::oracle_cost::parse_single_cost(left.trim());
        let cost_b = super::oracle_cost::parse_single_cost(right.trim());
        // Both fragments must parse to known costs — Unimplemented means the split was wrong
        // (e.g. "sacrifice an artifact or creature" splits incorrectly on " or ").
        if !matches!(cost_a, AbilityCost::Unimplemented { .. })
            && !matches!(cost_b, AbilityCost::Unimplemented { .. })
        {
            return Some(AdditionalCost::Choice(cost_a, cost_b));
        }
    }

    // Mandatory single cost: "sacrifice a creature", "discard a card", "pay 3 life", etc.
    // Delegates to parse_single_cost which handles all standard cost patterns.
    let cost = super::oracle_cost::parse_single_cost(body_raw);
    if !matches!(cost, AbilityCost::Unimplemented { .. }) {
        return Some(AdditionalCost::Required(cost));
    }

    None
}

/// CR 701.4a: Detect the *opening* of a spelled-out choose-behold cost —
/// "choose a/an <type> you control or " — on an already-lowercase body slice.
///
/// `parse_choose_or_reveal_behold_cost` (oracle_cost.rs) recognizes the FULL
/// shape "choose a/an <type> you control or reveal a/an <type> card from your
/// hand" and yields a `Behold`. When only this prefix matches but the full
/// behold parse declined (an unrecognized alternative leg such as Close
/// Encounter's "a warped creature card you own in exile"), the line is
/// unambiguously a choose-behold cost the engine cannot model. This guard lets
/// the caller surface an honest unimplemented cost instead of misparsing the
/// fragment. The bare `take_until` for the type phrase keeps the prefix as
/// narrow as possible — any line lacking " you control or " falls through.
fn is_choose_behold_prefix(body_lower: &str) -> bool {
    fn parse(i: &str) -> nom::IResult<&str, (), OracleError<'_>> {
        let (i, _) = tag("choose ").parse(i)?;
        let (i, _) = alt((tag("a "), tag("an "))).parse(i)?;
        let (i, _) = take_until(" you control or ").parse(i)?;
        let (i, _) = tag(" you control or ").parse(i)?;
        Ok((i, ()))
    }
    parse(body_lower).is_ok()
}

pub(crate) fn parse_spell_casting_option_line(
    text: &str,
    card_name: &str,
) -> Option<SpellCastingOption> {
    let trimmed = text.trim().trim_end_matches('.');
    let (condition, body) = split_leading_if_clause(trimmed);
    let primary_body = body.split_once(". ").map_or(body, |(head, _)| head).trim();
    let body_lower = primary_body.to_lowercase();

    parse_self_flash_option(primary_body, &body_lower, card_name)
        .or_else(|| parse_self_alternative_cost_option(primary_body, &body_lower, card_name))
        .and_then(|mut option| {
            if option.condition.is_none() {
                if let Some(condition_text) = condition {
                    // CR 118.9 + CR 601.3d: A leading-if gate on a casting option
                    // (alternative cost / flash permission) must NOT be dropped
                    // silently when the predicate is unrecognized — that would
                    // emit the option unconditionally, strictly more permissive
                    // than the printed text. Refuse to emit the option entirely,
                    // matching the trailing-if `?` contract in
                    // `parse_self_flash_option` / `parse_self_alternative_cost_option`.
                    option.condition = Some(parse_restriction_condition(condition_text)?);
                }
            }
            Some(option)
        })
}

fn split_leading_if_clause(text: &str) -> (Option<&str>, &str) {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    if tag::<_, _, OracleError<'_>>("if ")
        .parse(lower.as_str())
        .is_err()
    {
        return (None, trimmed);
    }

    if let Some((condition, rest)) = trimmed.split_once(", ") {
        return (
            Some(condition.trim_start_matches("If ").trim()),
            rest.trim(),
        );
    }

    (None, trimmed)
}

fn parse_self_flash_option(
    body: &str,
    body_lower: &str,
    card_name: &str,
) -> Option<SpellCastingOption> {
    let self_ref = self_spell_phrase(body_lower, card_name)?;
    let prefix = format!("you may cast {self_ref} as though it had flash");
    let r = body_lower.strip_prefix(&*prefix)?;
    let rest = body[body.len() - r.len()..].trim();
    let mut option = SpellCastingOption::as_though_had_flash();

    if rest.is_empty() {
        return Some(option);
    }

    if let Ok((after, _)) = tag::<_, _, OracleError<'_>>("if you pay ").parse(rest) {
        if let Some(cost_text) = after.strip_suffix(" more to cast it") {
            option = option.cost(parse_oracle_cost(cost_text));
            return Some(option);
        }
    }

    if let Ok((after, _)) = tag::<_, _, OracleError<'_>>("by ").parse(rest) {
        if let Some(cost_text) = after.strip_suffix(" in addition to paying its other costs") {
            option = option.cost(parse_oracle_cost(cost_text));
            return Some(option);
        }
    }

    if let Ok((after, _)) = tag::<_, _, OracleError<'_>>("if you ").parse(rest) {
        if let Ok((_, cost_text)) = all_consuming(terminated(
            take_until::<_, _, OracleError<'_>>(" as an additional cost to cast it"),
            tag(" as an additional cost to cast it"),
        ))
        .parse(after)
        {
            let cost = parse_oracle_cost(cost_text);
            if !matches!(cost, AbilityCost::Unimplemented { .. }) {
                option = option.cost(cost);
                return Some(option);
            }
        }
    }

    if let Ok((condition_text, _)) = tag::<_, _, OracleError<'_>>("if ").parse(rest) {
        // CR 601.3d: A target-dependent flash permission ("if it targets a commander")
        // must NOT degrade to an unconditional permission when the predicate is not
        // recognized — that would let the spell be cast at instant speed against any
        // target, strictly more permissive than the printed text. Refuse to emit the
        // option entirely so the spell stays sorcery-speed; the SwallowedClause /
        // Condition_If swallow detector then flags the dropped clause for the parser
        // gap-finder rather than fail-silently authorizing an over-permissive cast.
        let parsed = parse_restriction_condition(condition_text.trim())?;
        option = option.condition(parsed);
        return Some(option);
    }

    Some(option)
}

/// CR 118.9 (verified `docs/MagicCompRules.txt:1014`): "Some spells have alternative costs.
/// An alternative cost is a cost listed in a spell's text, or applied to it from another
/// effect, that its controller may pay rather than paying the spell's mana cost. Alternative
/// costs are usually phrased, 'You may [action] rather than pay [this object's] mana cost,'
/// or 'You may cast [this object] without paying its mana cost.'"
///
/// Parses both forms. The `"you may [verb-cost] rather than pay this spell's mana cost"`
/// form is verb-agnostic: the cost text (with verb intact) is delegated to `parse_oracle_cost`,
/// the single authority for cost parsing. This composes `pay {N}{C}`, `tap [filter]`,
/// `sacrifice [filter]`, and any future cost verb uniformly without per-verb arms.
fn parse_self_alternative_cost_option(
    body: &str,
    body_lower: &str,
    card_name: &str,
) -> Option<SpellCastingOption> {
    if let Some((cost_text, trailing_if)) = extract_rather_than_pay_alt_cost(body, body_lower) {
        let mut option = SpellCastingOption::alternative_cost(parse_oracle_cost(cost_text));
        if let Some(condition_text) = trailing_if {
            // CR 118.9 + CR 601.3d: A conditional alternative cost must NOT be
            // emitted unconditionally when the if-clause predicate is not
            // recognized — that would let the player pay the alt-cost
            // (typically cheaper or zero-mana) without the gating condition
            // holding, strictly more permissive than the printed text. Refuse
            // to emit the option entirely so the spell may only be cast at its
            // printed cost; the SwallowedClause / Condition_If swallow detector
            // then flags the unparsed predicate for the parser gap-finder
            // rather than fail-silently authorizing an over-permissive cast.
            let parsed = parse_restriction_condition(condition_text)?;
            option = option.condition(parsed);
        }
        return Some(option);
    }

    if let Some(self_ref) = self_spell_phrase(body_lower, card_name) {
        let without_cost = format!("you may cast {self_ref} without paying its mana cost");
        if body_lower == without_cost {
            return Some(SpellCastingOption::free_cast());
        }

        let for_cost = format!("you may cast {self_ref} for ");
        if let Some(rest) = body_lower.strip_prefix(&*for_cost) {
            let cost_text = body[body.len() - rest.len()..].trim();
            return Some(SpellCastingOption::alternative_cost(parse_oracle_cost(
                cost_text,
            )));
        }
    }

    None
}

/// Extract the cost-text and optional trailing-`if` condition from a
/// `"you may [verb-cost] rather than pay this spell's mana cost[ if [condition]]"` line.
///
/// Composed via nom `tag()` + `take_until()`: prefix is verb-agnostic so a single combinator
/// handles `pay`, `tap`, `sacrifice`, and any future cost verb that `parse_oracle_cost`
/// recognizes. The cost text is returned in original case (preserves mana symbol casing for
/// `parse_mana_symbols`); the optional trailing condition is returned as a raw slice for
/// downstream `parse_restriction_condition`.
fn extract_rather_than_pay_alt_cost<'a>(
    body: &'a str,
    body_lower: &str,
) -> Option<(&'a str, Option<&'a str>)> {
    const PREFIX: &str = "you may ";
    const SUFFIX: &str = " rather than pay this spell's mana cost";

    let (after_prefix_lower, _) = tag::<_, _, OracleError<'_>>(PREFIX)
        .parse(body_lower)
        .ok()?;
    let prefix_len = body_lower.len() - after_prefix_lower.len();

    let (after_suffix_lower, _) = take_until::<_, _, OracleError<'_>>(SUFFIX)
        .parse(after_prefix_lower)
        .ok()?;
    let cost_end = body_lower.len() - after_suffix_lower.len();

    let cost_text = body[prefix_len..cost_end].trim();
    let after_suffix_pos = cost_end + SUFFIX.len();
    let remainder_lower = &body_lower[after_suffix_pos..];
    let trailing_if =
        if let Ok((cond_lower, _)) = tag::<_, _, OracleError<'_>>(" if ").parse(remainder_lower) {
            let cond_start = body.len() - cond_lower.len();
            Some(body[cond_start..].trim())
        } else {
            None
        };

    Some((cost_text, trailing_if))
}

fn self_spell_phrase(lower: &str, card_name: &str) -> Option<String> {
    let card_name_lower = card_name.to_lowercase();
    if let Ok((_, phrase)) = alt((
        value(
            "this spell",
            tag::<_, _, OracleError<'_>>("you may cast this spell "),
        ),
        value("it", tag("you may cast it ")),
    ))
    .parse(lower)
    {
        return Some(phrase.to_string());
    }
    // Dynamic card name prefix — must use strip_prefix (runtime string)
    let card_prefix = format!("you may cast {card_name_lower} ");
    if lower.strip_prefix(&*card_prefix).is_some() {
        return Some(card_name_lower);
    }

    None
}

/// CR 601.3: Parse "Cast this spell only [condition]" into typed restrictions.
/// Handles ability word prefixes (e.g., "Tragic Backstory — Cast this spell only if...").
pub(crate) fn parse_casting_restriction_line(text: &str) -> Option<Vec<CastingRestriction>> {
    let trimmed = text.trim().trim_end_matches('.');
    // Try direct match first, then fall back to stripping ability word prefix
    let trimmed_lower = trimmed.to_lowercase();
    if let Some(restriction) = parse_negative_self_casting_restriction(&trimmed_lower) {
        return Some(vec![restriction]);
    }
    // Also try after stripping an ability word prefix (e.g., "From the Future — You can't cast ~...").
    if let Some(after_word) = super::oracle_modal::strip_ability_word(trimmed) {
        let after_word_lower = after_word.to_lowercase();
        if let Some(restriction) = parse_negative_self_casting_restriction(&after_word_lower) {
            return Some(vec![restriction]);
        }
    }
    let effective = if tag::<_, _, OracleError<'_>>("cast this spell only ")
        .parse(trimmed_lower.as_str())
        .is_ok()
    {
        trimmed.to_lowercase()
    } else {
        super::oracle_modal::strip_ability_word(trimmed)?.to_lowercase()
    };
    let rest = match tag::<_, _, OracleError<'_>>("cast this spell only ").parse(effective.as_str())
    {
        Ok((r, _)) => r,
        Err(_) => return None,
    };
    let mut restrictions = scan_timing_restrictions(rest);

    // Extract condition clauses: "if ...", "only if ...", or "... and only if ..."
    if let Ok((condition, _)) =
        alt((tag::<_, _, OracleError<'_>>("only if "), tag("if "))).parse(rest)
    {
        let condition_text = strip_casting_condition_suffixes(condition);
        restrictions.push(CastingRestriction::RequiresCondition {
            condition: parse_restriction_condition(condition_text),
        });
    }
    if let Some(condition) = rest.split(" and only if ").nth(1) {
        let condition_text = strip_casting_condition_suffixes(condition);
        restrictions.push(CastingRestriction::RequiresCondition {
            condition: parse_restriction_condition(condition_text),
        });
    }

    (!restrictions.is_empty()).then_some(restrictions)
}

fn parse_negative_self_casting_restriction(text: &str) -> Option<CastingRestriction> {
    // Strip the "you can't cast" prefix first.
    let after_prefix: &str = preceded(
        alt((
            tag::<_, _, OracleError<'_>>("you can't cast "),
            tag("you cannot cast "),
            tag("you can\u{2019}t cast "),
        )),
        nom::combinator::rest,
    )
    .parse(text)
    .map(|(_, rest)| rest)
    .ok()?;

    // "you can't cast ~ during your first[, second, ...] turn[s] of the game"
    // CR 601.3a: The prohibition window is the caster's own first N turns.
    // Uses TurnsTaken (per-player, CR 500) — NOT turn_number (global), which
    // would incorrectly count opponent turns toward the threshold.
    if let Some(condition) = parse_during_your_nth_turns_of_game_condition(after_prefix) {
        return Some(CastingRestriction::RequiresCondition {
            condition: Some(condition),
        });
    }

    // "you can't cast ~ if/unless [condition]"
    let (condition_text, (subject, negated)) = alt((
        map(
            terminated(take_until::<_, _, OracleError<'_>>(" if "), tag(" if ")),
            |subject| (subject, true),
        ),
        map(
            terminated(
                take_until::<_, _, OracleError<'_>>(" unless "),
                tag(" unless "),
            ),
            |subject| (subject, false),
        ),
    ))
    .parse(after_prefix)
    .ok()?;
    let subject = subject.trim();
    if all_consuming(alt((
        value((), tag::<_, _, OracleError<'_>>("~")),
        value((), tag("this spell")),
    )))
    .parse(subject)
    .is_err()
    {
        return None;
    }
    let condition = parse_restriction_condition(condition_text.trim())?;
    let condition = if negated {
        ParsedCondition::Not {
            condition: Box::new(condition),
        }
    } else {
        condition
    };
    Some(CastingRestriction::RequiresCondition {
        condition: Some(condition),
    })
}

/// Parse `"[~|this spell] during your first[, second, or third] turn[s] of the game"`
/// (where `text` is everything after `"you can't cast "`) and return a condition that
/// is **false** (i.e., blocks casting) while the caster's `turns_taken` ≤ max ordinal.
///
/// CR 500 + CR 601.3a: uses `TurnsTaken` (per-player) — NOT `turn_number` (global),
/// which would incorrectly count opponent turns toward the threshold.
///
/// Returns `None` if the phrase doesn't match so the caller falls through to
/// the `if`/`unless` branch.
fn parse_during_your_nth_turns_of_game_condition(text: &str) -> Option<ParsedCondition> {
    // Consume "~" or "this spell", then " during your ".
    let after_subject: &str = alt((tag::<_, _, OracleError<'_>>("~"), tag("this spell")))
        .parse(text)
        .map(|(rest, _)| rest)
        .ok()?;
    let after_during: &str = tag::<_, _, OracleError<'_>>(" during your ")
        .parse(after_subject)
        .map(|(rest, _)| rest)
        .ok()?;

    // Parse a comma/or-separated ordinal list: "first", "first or second",
    // "first, second, or third", etc. Take the maximum ordinal as the threshold.
    let mut max_ordinal: u32 = 0;
    let mut remaining = after_during;
    loop {
        remaining = alt((
            tag::<_, _, OracleError<'_>>(", or "),
            tag(", "),
            tag(" or "),
            tag("or "),
        ))
        .parse(remaining)
        .map_or(remaining, |(rest, _)| rest);
        if let Some((val, rest)) = parse_ordinal(remaining) {
            max_ordinal = max_ordinal.max(val);
            remaining = rest;
        } else {
            break;
        }
    }
    if max_ordinal == 0 {
        return None;
    }

    // Expect "turns" or "turn" (optionally followed by " of the game") and
    // reject trailing conjuncts so they do not become swallowed restrictions.
    all_consuming((
        alt((tag::<_, _, OracleError<'_>>("turns"), tag("turn"))),
        opt(tag(" of the game")),
    ))
    .parse(remaining.trim_start())
    .ok()?;

    // Casting is allowed only when turns_taken > max_ordinal.
    // Represented as Not(turns_taken <= max_ordinal) so RequiresCondition
    // blocks casting while the condition evaluates to false.
    Some(ParsedCondition::Not {
        condition: Box::new(ParsedCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::TurnsTaken,
            },
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed {
                value: max_ordinal as i32,
            },
        }),
    })
}

fn strip_casting_condition_suffixes(text: &str) -> &str {
    text.trim()
        .trim_end_matches(" and only as a sorcery")
        .trim_end_matches(" and only during any upkeep step")
        .trim_end_matches(" and only during any upkeep")
        .trim()
}

/// Nom combinator: parse a single timing restriction phrase from the current position.
///
/// Structured by prefix dispatch: `during` → sub-dispatch by possessive/phase,
/// `before`/`after`/`on`/`as` each dispatch independently. This avoids redundant
/// prefix matching across the 15 timing variants.
fn parse_timing_restriction(
    input: &str,
) -> nom::IResult<&str, CastingRestriction, OracleError<'_>> {
    use nom::sequence::preceded;
    alt((
        preceded(tag("during "), parse_during_phrase),
        preceded(tag("before "), parse_before_phrase),
        preceded(
            tag("on "),
            alt((
                parse_opponent_possessive_turn,
                value(CastingRestriction::DuringYourTurn, tag("your turn")),
            )),
        ),
        value(CastingRestriction::AfterCombat, tag("after combat")),
        value(CastingRestriction::AsSorcery, tag("as a sorcery")),
    ))
    .parse(input)
}

/// Sub-dispatch for "during [rest]" — declare steps, opponent/your phases, combat, upkeep.
fn parse_during_phrase(input: &str) -> nom::IResult<&str, CastingRestriction, OracleError<'_>> {
    use nom::sequence::preceded;
    alt((
        // Declare steps (most specific combat sub-phases)
        value(
            CastingRestriction::DeclareAttackersStep,
            alt((
                tag("the declare attackers step"),
                tag("your declare attackers step"),
                tag("declare attackers step"),
            )),
        ),
        value(
            CastingRestriction::DeclareBlockersStep,
            alt((
                tag("the declare blockers step"),
                tag("your declare blockers step"),
                tag("declare blockers step"),
            )),
        ),
        // Opponent phases: "during an opponent's [phase]" — dispatch on phase after possessive
        preceded(parse_opponent_possessive, parse_opponent_phase),
        // Your phases (must try specific phases before generic "your turn")
        value(CastingRestriction::DuringYourUpkeep, tag("your upkeep")),
        value(CastingRestriction::DuringYourEndStep, tag("your end step")),
        value(CastingRestriction::DuringYourTurn, tag("your turn")),
        // Generic upkeep (any player)
        value(
            CastingRestriction::DuringAnyUpkeep,
            alt((tag("any upkeep step"), tag("any upkeep"))),
        ),
        value(CastingRestriction::DuringCombat, tag("combat")),
    ))
    .parse(input)
}

/// Match "an opponent's " / "an opponents " possessive prefix (handles curly apostrophe).
fn parse_opponent_possessive(input: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
    alt((
        tag("an opponent\u{2019}s "),
        tag("an opponent's "),
        tag("an opponents "),
    ))
    .parse(input)
}

/// After "an opponent's", dispatch on the phase keyword.
fn parse_opponent_phase(input: &str) -> nom::IResult<&str, CastingRestriction, OracleError<'_>> {
    alt((
        value(CastingRestriction::DuringOpponentsUpkeep, tag("upkeep")),
        value(CastingRestriction::DuringOpponentsEndStep, tag("end step")),
        value(CastingRestriction::DuringOpponentsTurn, tag("turn")),
    ))
    .parse(input)
}

/// "on an opponent's turn" — reuses the opponent possessive combinator.
fn parse_opponent_possessive_turn(
    input: &str,
) -> nom::IResult<&str, CastingRestriction, OracleError<'_>> {
    use nom::sequence::preceded;
    value(
        CastingRestriction::DuringOpponentsTurn,
        preceded(parse_opponent_possessive, tag("turn")),
    )
    .parse(input)
}

/// Sub-dispatch for "before [rest]" — attackers, blockers, combat damage.
fn parse_before_phrase(input: &str) -> nom::IResult<&str, CastingRestriction, OracleError<'_>> {
    alt((
        value(
            CastingRestriction::BeforeAttackersDeclared,
            tag("attackers are declared"),
        ),
        value(
            CastingRestriction::BeforeBlockersDeclared,
            tag("blockers are declared"),
        ),
        value(
            CastingRestriction::BeforeCombatDamage,
            alt((tag("the combat damage step"), tag("combat damage"))),
        ),
    ))
    .parse(input)
}

/// Walk `text` word-by-word, collecting all timing restrictions found via nom combinators.
/// Tries `parse_timing_restriction` at each word boundary — on match, consumes the phrase
/// and advances; on miss, skips to the next word.
fn scan_timing_restrictions(text: &str) -> Vec<CastingRestriction> {
    let mut results = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if let Ok((rest, restriction)) = parse_timing_restriction(remaining) {
            if !results.contains(&restriction) {
                results.push(restriction);
            }
            remaining = rest.trim_start();
        } else {
            // Advance past the current word to the next word boundary
            remaining = remaining
                .find(' ')
                .map_or("", |i| remaining[i + 1..].trim_start());
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AdditionalCostRepeatability, BeholdCostAction, CardSelectionMode, Comparator,
        ControllerRef, FilterProp, ParsedCondition, PlayerFilter, QuantityExpr, QuantityRef,
        TargetFilter, TypeFilter,
    };
    use crate::types::mana::{ManaColor, ManaCost};
    use crate::types::zones::Zone;

    #[test]
    fn spell_cast_restriction_condition_is_preserved() {
        let restrictions = parse_casting_restriction_line(
            "Cast this spell only during the declare attackers step and only if you've been attacked this step.",
        )
        .expect("restrictions should parse");
        assert_eq!(
            restrictions,
            vec![
                CastingRestriction::DeclareAttackersStep,
                CastingRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::BeenAttackedThisStep),
                },
            ]
        );
    }

    #[test]
    fn spell_cast_restriction_parses_end_step_window() {
        let restrictions =
            parse_casting_restriction_line("Cast this spell only during your end step.")
                .expect("restrictions should parse");
        assert_eq!(restrictions, vec![CastingRestriction::DuringYourEndStep]);
    }

    #[test]
    fn spell_cast_restriction_parses_opponent_upkeep_window() {
        let restrictions =
            parse_casting_restriction_line("Cast this spell only during an opponent's upkeep.")
                .expect("restrictions should parse");
        assert_eq!(
            restrictions,
            vec![CastingRestriction::DuringOpponentsUpkeep]
        );
    }

    #[test]
    fn spell_cast_restriction_parses_any_upkeep_window() {
        let restrictions =
            parse_casting_restriction_line("Cast this spell only during any upkeep step.")
                .expect("restrictions should parse");
        assert_eq!(restrictions, vec![CastingRestriction::DuringAnyUpkeep]);
    }

    #[test]
    fn spell_cast_restriction_parses_plain_only_if_condition() {
        let restrictions = parse_casting_restriction_line(
            "Cast this spell only if you control two or more Vampires.",
        )
        .expect("restrictions should parse");
        assert_eq!(
            restrictions,
            vec![CastingRestriction::RequiresCondition {
                condition: Some(ParsedCondition::YouControlSubtypeCountAtLeast {
                    subtype: "vampire".to_string(),
                    count: 2,
                }),
            }]
        );
    }

    #[test]
    fn spell_cast_restriction_splits_as_sorcery_from_condition() {
        let restrictions = parse_casting_restriction_line(
            "Cast this spell only if there are four or more card types among cards in your graveyard and only as a sorcery.",
        )
        .expect("restrictions should parse");
        assert_eq!(
            restrictions,
            vec![
                CastingRestriction::AsSorcery,
                CastingRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::ZoneCardTypeCountAtLeast {
                        zone: Zone::Graveyard,
                        count: 4
                    }),
                },
            ]
        );
    }

    #[test]
    fn spell_cast_restriction_parses_your_declare_attackers_step_variant() {
        let restrictions = parse_casting_restriction_line(
            "Cast this spell only during your declare attackers step.",
        )
        .expect("restrictions should parse");
        assert_eq!(restrictions, vec![CastingRestriction::DeclareAttackersStep]);
    }

    #[test]
    fn spell_cast_restriction_handles_on_your_turn_variant() {
        // "on your turn" (vs "during your turn") appears in compound restrictions
        let restrictions =
            parse_casting_restriction_line("Cast this spell only during combat on your turn.")
                .expect("restrictions should parse");
        assert!(restrictions.contains(&CastingRestriction::DuringCombat));
        assert!(restrictions.contains(&CastingRestriction::DuringYourTurn));
    }

    #[test]
    fn spell_cast_restriction_handles_ability_word_prefix() {
        // Ability word prefixed casting restrictions (e.g., Tragic Backstory)
        let restrictions = parse_casting_restriction_line(
            "Tragic Backstory \u{2014} Cast this spell only if a creature died this turn.",
        )
        .expect("restrictions should parse");
        assert_eq!(
            restrictions,
            vec![CastingRestriction::RequiresCondition {
                condition: Some(ParsedCondition::CreatureDiedThisTurn),
            }]
        );
    }

    #[test]
    fn spell_cast_restriction_cast_another_spell_this_turn() {
        let restrictions = parse_casting_restriction_line(
            "Cast this spell only if you've cast another spell this turn.",
        )
        .expect("restrictions should parse");
        assert_eq!(
            restrictions,
            vec![CastingRestriction::RequiresCondition {
                condition: Some(ParsedCondition::YouCastSpellCountAtLeast { count: 1 }),
            }]
        );
    }

    #[test]
    fn spell_cast_restriction_parses_negative_self_condition() {
        for text in [
            "You can't cast ~ if you've played a land this turn.",
            "You cannot cast this spell if you have played a land this turn.",
            "You can\u{2019}t cast ~ if you played a land this turn.",
        ] {
            let restrictions =
                parse_casting_restriction_line(text).expect("restrictions should parse");
            assert_eq!(
                restrictions,
                vec![CastingRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::Not {
                        condition: Box::new(ParsedCondition::YouPlayedLandThisTurn),
                    }),
                }],
                "text={text:?}"
            );
        }
    }

    #[test]
    fn spell_cast_restriction_does_not_consume_generic_spell_subject() {
        assert_eq!(
            parse_casting_restriction_line(
                "You can't cast creature spells if you've played a land this turn.",
            ),
            None
        );
        assert_eq!(
            parse_casting_restriction_line(
                "You can't cast cards from graveyards if you've played a land this turn.",
            ),
            None
        );
    }

    #[test]
    fn spell_cast_restriction_parses_negative_self_unless_condition() {
        let restrictions = parse_casting_restriction_line(
            "You can't cast ~ unless an opponent lost life this turn.",
        )
        .expect("restrictions should parse");

        assert_eq!(
            restrictions,
            vec![CastingRestriction::RequiresCondition {
                condition: Some(ParsedCondition::PlayerCountAtLeast {
                    filter: PlayerFilter::OpponentLostLife,
                    minimum: 1,
                }),
            }]
        );
    }

    #[test]
    fn spell_cast_restriction_handles_combat_on_your_turn_before_blockers() {
        let restrictions = parse_casting_restriction_line(
            "Cast this spell only during combat on your turn before blockers are declared.",
        )
        .expect("restrictions should parse");
        assert!(restrictions.contains(&CastingRestriction::DuringCombat));
        assert!(restrictions.contains(&CastingRestriction::DuringYourTurn));
        assert!(restrictions.contains(&CastingRestriction::BeforeBlockersDeclared));
    }

    #[test]
    fn parse_additional_cost_optional_blight() {
        let lower = "as an additional cost to cast this spell, you may blight 1.";
        let raw = "As an additional cost to cast this spell, you may blight 1.";
        let result = parse_additional_cost_line(lower, raw);
        assert_eq!(
            result,
            Some(AdditionalCost::Optional {
                cost: AbilityCost::Blight { count: 1 },
                repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
            })
        );
    }

    #[test]
    fn parse_additional_cost_optional_blight_2() {
        let lower = "as an additional cost to cast this spell, you may blight 2.";
        let raw = "As an additional cost to cast this spell, you may blight 2.";
        let result = parse_additional_cost_line(lower, raw);
        assert_eq!(
            result,
            Some(AdditionalCost::Optional {
                cost: AbilityCost::Blight { count: 2 },
                repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
            })
        );
    }

    #[test]
    fn parse_additional_cost_optional_behold() {
        let lower =
            "as an additional cost to cast this spell, you may behold a dragon. (you may choose a dragon you control or reveal a dragon card from your hand.)";
        let raw =
            "As an additional cost to cast this spell, you may behold a Dragon. (You may choose a Dragon you control or reveal a Dragon card from your hand.)";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Optional {
                cost:
                    AbilityCost::Behold {
                        count: 1,
                        filter: TargetFilter::Typed(filter),
                        action: BeholdCostAction::ChooseOrReveal,
                    },
                repeatability: AdditionalCostRepeatability::Once,
            }) => {
                assert!(filter
                    .type_filters
                    .iter()
                    .any(|tf| matches!(tf, TypeFilter::Subtype(name) if name == "Dragon")));
            }
            other => panic!("Expected Optional(Behold Dragon), got {other:?}"),
        }
    }

    #[test]
    fn parse_additional_cost_behold_or_pay() {
        let lower =
            "as an additional cost to cast this spell, behold an elf or pay {2}. (to behold an elf, choose an elf you control or reveal an elf card from your hand.)";
        let raw =
            "As an additional cost to cast this spell, behold an Elf or pay {2}. (To behold an Elf, choose an Elf you control or reveal an Elf card from your hand.)";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Choice(
                AbilityCost::Behold {
                    count: 1,
                    filter: TargetFilter::Typed(filter),
                    action: BeholdCostAction::ChooseOrReveal,
                },
                AbilityCost::Mana { cost },
            )) => {
                assert!(filter
                    .type_filters
                    .iter()
                    .any(|tf| matches!(tf, TypeFilter::Subtype(name) if name == "Elf")));
                assert_eq!(cost, ManaCost::generic(2));
            }
            other => panic!("Expected Choice(Behold Elf, Mana {{2}}), got {other:?}"),
        }
    }

    #[test]
    fn parse_additional_cost_mandatory_behold_and_exile() {
        let lower =
            "as an additional cost to cast this spell, behold an elemental and exile it. (exile an elemental you control or an elemental card from your hand.)";
        let raw =
            "As an additional cost to cast this spell, behold an Elemental and exile it. (Exile an Elemental you control or an Elemental card from your hand.)";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Required(AbilityCost::Behold {
                count: 1,
                filter: TargetFilter::Typed(filter),
                action: BeholdCostAction::ExileChosen,
            })) => {
                assert!(filter
                    .type_filters
                    .iter()
                    .any(|tf| matches!(tf, TypeFilter::Subtype(name) if name == "Elemental")));
            }
            other => panic!("Expected Required(Behold Elemental exile), got {other:?}"),
        }
    }

    /// CR 701.4a + CR 601.2b/f: the SPELLED-OUT choose-or-reveal behold cost
    /// printed without the "behold" keyword (Monstrous Emergence) parses to the
    /// same `Behold { ChooseOrReveal }` shape as the keyword form.
    #[test]
    fn parse_additional_cost_spelled_out_choose_or_reveal_behold() {
        let lower =
            "as an additional cost to cast this spell, choose a creature you control or reveal a creature card from your hand.";
        let raw =
            "As an additional cost to cast this spell, choose a creature you control or reveal a creature card from your hand.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Required(AbilityCost::Behold {
                count: 1,
                filter: TargetFilter::Typed(filter),
                action: BeholdCostAction::ChooseOrReveal,
            })) => {
                assert!(
                    filter
                        .type_filters
                        .iter()
                        .any(|tf| matches!(tf, TypeFilter::Creature)),
                    "spelled-out behold must carry the bare creature type filter: {filter:?}"
                );
            }
            other => panic!("Expected Required(Behold creature ChooseOrReveal), got {other:?}"),
        }
    }

    /// CR 701.4a + CR 601.2b/f (coverage honesty): a choose-behold cost whose
    /// alternative leg is NOT a recognized behold-reveal alternative (Close
    /// Encounter: "or a warped creature card you own in exile") must surface an
    /// honest unimplemented cost — NOT silently drop the alternative leg and
    /// misparse only "choose a creature you control" as a `TargetOnly` cost.
    /// Reverting the prefix guard regresses this assertion: the line would parse
    /// to `Required(EffectCost { TargetOnly { .. } })` (false green).
    #[test]
    fn parse_additional_cost_choose_behold_unrecognized_alternative_is_unimplemented() {
        let lower =
            "as an additional cost to cast this spell, choose a creature you control or a warped creature card you own in exile.";
        let raw =
            "As an additional cost to cast this spell, choose a creature you control or a warped creature card you own in exile.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Required(AbilityCost::Unimplemented { description })) => {
                assert_eq!(
                    description,
                    "choose a creature you control or a warped creature card you own in exile",
                    "unimplemented cost must preserve the full unrecognized line"
                );
            }
            other => panic!("Close Encounter must surface Required(Unimplemented), got {other:?}"),
        }
    }

    #[test]
    fn parse_additional_cost_behold_multiple_objects() {
        let lower = "as an additional cost to cast this spell, behold three elementals.";
        let raw = "As an additional cost to cast this spell, behold three Elementals.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Required(AbilityCost::Behold {
                count: 3,
                filter: TargetFilter::Typed(filter),
                action: BeholdCostAction::ChooseOrReveal,
            })) => {
                assert!(filter
                    .type_filters
                    .iter()
                    .any(|tf| matches!(tf, TypeFilter::Subtype(name) if name == "Elemental")));
            }
            other => panic!("Expected Required(Behold three Elementals), got {other:?}"),
        }
    }

    #[test]
    fn parse_additional_cost_choice_blight_or_pay() {
        let lower = "as an additional cost to cast this spell, blight 2 or pay {1}.";
        let raw = "As an additional cost to cast this spell, blight 2 or pay {1}.";
        let result = parse_additional_cost_line(lower, raw);
        assert_eq!(
            result,
            Some(AdditionalCost::Choice(
                AbilityCost::Blight { count: 2 },
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        generic: 1,
                        shards: vec![]
                    }
                }
            ))
        );
    }

    #[test]
    fn parse_additional_cost_choice_blight_or_pay_3() {
        let lower = "as an additional cost to cast this spell, blight 1 or pay {3}.";
        let raw = "As an additional cost to cast this spell, blight 1 or pay {3}.";
        let result = parse_additional_cost_line(lower, raw);
        assert_eq!(
            result,
            Some(AdditionalCost::Choice(
                AbilityCost::Blight { count: 1 },
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        generic: 3,
                        shards: vec![]
                    }
                }
            ))
        );
    }

    #[test]
    fn parse_additional_cost_mandatory_blight() {
        let lower = "as an additional cost to cast this spell, blight 2.";
        let raw = "As an additional cost to cast this spell, blight 2.";
        let result = parse_additional_cost_line(lower, raw);
        assert_eq!(
            result,
            Some(AdditionalCost::Required(AbilityCost::Blight { count: 2 }))
        );
    }

    #[test]
    fn parse_additional_cost_discard_or_pay_life() {
        let lower = "as an additional cost to cast this spell, discard a card or pay 3 life.";
        let raw = "As an additional cost to cast this spell, discard a card or pay 3 life.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Choice(
                AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    selection: CardSelectionMode::Chosen,
                    ..
                },
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                },
            )) => {}
            other => panic!("Expected Choice(Discard, PayLife), got {:?}", other),
        }
    }

    #[test]
    fn parse_additional_cost_sacrifice_or_mana() {
        let lower = "as an additional cost to cast this spell, sacrifice a creature or pay {2}.";
        let raw = "As an additional cost to cast this spell, sacrifice a creature or pay {2}.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Choice(AbilityCost::Sacrifice(_), AbilityCost::Mana { .. })) => {}
            other => panic!("Expected Choice(Sacrifice, Mana), got {:?}", other),
        }
    }

    #[test]
    fn parse_additional_cost_sacrifice_compound_type_not_choice() {
        // "sacrifice an artifact or creature" is a single sacrifice cost, not a choice.
        // The " or " split fails because "creature" alone is Unimplemented, correctly
        // falling through to the mandatory single-cost path which parses the full filter.
        let lower = "as an additional cost to cast this spell, sacrifice an artifact or creature.";
        let raw = "As an additional cost to cast this spell, sacrifice an artifact or creature.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Required(AbilityCost::Sacrifice(ref sac))) => {
                assert_eq!(sac.requirement.fixed_count(), Some(1));
                assert!(
                    matches!(&sac.target, TargetFilter::Or { .. }),
                    "Expected Or filter, got {:?}",
                    sac.target
                );
            }
            other => panic!("Expected Required(Sacrifice {{ Or, 1 }}), got {:?}", other),
        }
    }

    #[test]
    fn parse_additional_cost_sacrifice_creature() {
        let lower = "as an additional cost to cast this spell, sacrifice a creature.";
        let raw = "As an additional cost to cast this spell, sacrifice a creature.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Required(AbilityCost::Sacrifice(ref sac)))
                if sac.requirement.fixed_count() == Some(1) => {}
            other => panic!("Expected Required(Sacrifice), got {:?}", other),
        }
    }

    #[test]
    fn parse_additional_cost_discard_card() {
        let lower = "as an additional cost to cast this spell, discard a card.";
        let raw = "As an additional cost to cast this spell, discard a card.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Required(AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            })) => {}
            other => panic!("Expected Required(Discard), got {:?}", other),
        }
    }

    #[test]
    fn parse_additional_cost_pay_life() {
        let lower = "as an additional cost to cast this spell, pay 3 life.";
        let raw = "As an additional cost to cast this spell, pay 3 life.";
        let result = parse_additional_cost_line(lower, raw);
        assert_eq!(
            result,
            Some(AdditionalCost::Required(AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 3 }
            }))
        );
    }

    #[test]
    fn parse_additional_cost_pay_x_life() {
        let lower = "as an additional cost to cast this spell, pay x life.";
        let raw = "As an additional cost to cast this spell, pay X life.";
        let result = parse_additional_cost_line(lower, raw);
        assert_eq!(
            result,
            Some(AdditionalCost::Required(AbilityCost::PayLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string()
                    }
                }
            }))
        );
    }

    #[test]
    fn parse_additional_cost_exile_x_cards_from_graveyard() {
        let lower = "as an additional cost to cast this spell, exile x cards from your graveyard.";
        let raw = "As an additional cost to cast this spell, exile X cards from your graveyard.";
        let result = parse_additional_cost_line(lower, raw);
        assert_eq!(
            result,
            Some(AdditionalCost::Required(AbilityCost::Exile {
                count: crate::types::ability::EXILE_COST_X,
                zone: Some(crate::types::zones::Zone::Graveyard),
                filter: None,
            }))
        );
    }

    #[test]
    fn parse_additional_cost_optional_sacrifice() {
        let lower = "as an additional cost to cast this spell, you may sacrifice an artifact.";
        let raw = "As an additional cost to cast this spell, you may sacrifice an artifact.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Optional {
                cost: AbilityCost::Sacrifice(ref sac),
                repeatability: AdditionalCostRepeatability::Once,
            }) if sac.requirement.fixed_count() == Some(1) => {}
            other => panic!("Expected Optional(Sacrifice), got {:?}", other),
        }
    }

    /// Issue #2415: Rottenmouth Viper — optional sacrifice any number + trailing reduction.
    #[test]
    fn parse_additional_cost_optional_sacrifice_any_number_nonland() {
        let lower = "as an additional cost to cast this spell, you may sacrifice any number of nonland permanents.";
        let raw =
            "As an additional cost to cast this spell, you may sacrifice any number of nonland permanents.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Optional {
                cost: AbilityCost::Sacrifice(ref sac),
                repeatability: AdditionalCostRepeatability::Once,
            }) if sac.requirement.fixed_count() == Some(u32::MAX) => {}
            other => panic!("Expected Optional(Sacrifice any number), got {:?}", other),
        }
    }

    #[test]
    fn split_rottenmouth_additional_cost_trailing_reduction() {
        let raw = "As an additional cost to cast this spell, you may sacrifice any number of nonland permanents. This spell costs {1} less to cast for each permanent sacrificed this way.";
        let lower = raw.to_lowercase();
        let (cost_line, trailing) = split_additional_cost_trailing_spell_reduction(raw, &lower);
        let trailing = trailing.expect("trailing cost-reduction sentence");
        assert_eq!(
            cost_line,
            "As an additional cost to cast this spell, you may sacrifice any number of nonland permanents"
        );
        assert_eq!(
            trailing,
            "This spell costs {1} less to cast for each permanent sacrificed this way."
        );
    }

    #[test]
    fn parse_additional_cost_reveal_type_or_pay() {
        let lower =
            "as an additional cost to cast this spell, reveal a dragon card from your hand or pay {1}.";
        let raw =
            "As an additional cost to cast this spell, reveal a Dragon card from your hand or pay {1}.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Choice(
                AbilityCost::Reveal {
                    count: 1,
                    filter: Some(_),
                },
                AbilityCost::Mana { .. },
            )) => {}
            other => panic!("Expected Choice(Reveal, Mana), got {:?}", other),
        }
    }

    #[test]
    fn parse_additional_cost_reveal_type_mandatory() {
        let lower =
            "as an additional cost to cast this spell, reveal a creature card from your hand.";
        let raw =
            "As an additional cost to cast this spell, reveal a creature card from your hand.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Required(AbilityCost::Reveal {
                count: 1,
                filter: Some(_),
            })) => {}
            other => panic!("Expected Required(Reveal with filter), got {:?}", other),
        }
    }

    #[test]
    fn parse_additional_cost_sacrifice_land() {
        let lower = "as an additional cost to cast this spell, sacrifice a land.";
        let raw = "As an additional cost to cast this spell, sacrifice a land.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Required(AbilityCost::Sacrifice(ref sac)))
                if sac.requirement.fixed_count() == Some(1) => {}
            other => panic!("Expected Required(Sacrifice), got {:?}", other),
        }
    }

    // CR 118.9: Alternative-cost arms — verb-agnostic prefix delegates to `parse_oracle_cost`.
    //
    // Class: ~23 cards in card-data.json including Ramosian Rally, Lashknife, Orim's Cure,
    // Angelic Favor, Sivvi's Valor, The Lady of Otaria (tap arm); Fireblast, Pulverize,
    // Mogg Alarm, Crash, Hand of Emrakul, Delraich, Dark Triumph, Flare of Denial, Salvage
    // Titan, Mind Swords, Mine Collapse, Thunderclap, Downhill Charge, Flare of Cultivation,
    // Flare of Duplication, Flare of Fortitude, Flare of Malice (sacrifice arm); the
    // pre-existing pay-mana arm covers Archive Trap, Force of Will, etc.

    #[test]
    fn alt_cost_tap_creature_arm() {
        let option = parse_spell_casting_option_line(
            "you may tap an untapped creature you control rather than pay this spell's mana cost.",
            "Ramosian Rally",
        )
        .expect("alt-cost should parse");
        match option {
            SpellCastingOption {
                kind: crate::types::ability::SpellCastingOptionKind::AlternativeCost,
                cost:
                    Some(AbilityCost::TapCreatures {
                        ref requirement, ..
                    }),
                condition: None,
            } if requirement.fixed_count() == Some(1) => {}
            other => panic!("expected TapCreatures alt-cost, got {other:?}"),
        }
    }

    #[test]
    fn alt_cost_tap_creature_arm_with_count() {
        // The Lady of Otaria — "tap three untapped Dwarves you control"
        let option = parse_spell_casting_option_line(
            "You may tap three untapped Dwarves you control rather than pay this spell's mana cost.",
            "The Lady of Otaria",
        )
        .expect("alt-cost should parse");
        match option {
            SpellCastingOption {
                kind: crate::types::ability::SpellCastingOptionKind::AlternativeCost,
                cost:
                    Some(AbilityCost::TapCreatures {
                        ref requirement, ..
                    }),
                condition: None,
            } if requirement.fixed_count() == Some(3) => {}
            other => panic!("expected TapCreatures(count=3) alt-cost, got {other:?}"),
        }
    }

    #[test]
    fn alt_cost_sacrifice_arm() {
        // Fireblast — "sacrifice two Mountains"
        let option = parse_spell_casting_option_line(
            "You may sacrifice two Mountains rather than pay this spell's mana cost.",
            "Fireblast",
        )
        .expect("alt-cost should parse");
        match option {
            SpellCastingOption {
                kind: crate::types::ability::SpellCastingOptionKind::AlternativeCost,
                cost: Some(AbilityCost::Sacrifice(ref sac)),
                condition: None,
            } if sac.requirement.fixed_count() == Some(2) => {}
            other => panic!("expected Sacrifice(count=2) alt-cost, got {other:?}"),
        }
    }

    #[test]
    fn alt_cost_sacrifice_typed_creature_arm() {
        // Delraich — "sacrifice three black creatures"
        let option = parse_spell_casting_option_line(
            "You may sacrifice three black creatures rather than pay this spell's mana cost.",
            "Delraich",
        )
        .expect("alt-cost should parse");
        match option {
            SpellCastingOption {
                kind: crate::types::ability::SpellCastingOptionKind::AlternativeCost,
                cost: Some(AbilityCost::Sacrifice(ref sac)),
                condition: None,
            } if sac.requirement.fixed_count() == Some(3) => {}
            other => panic!("expected Sacrifice(count=3) alt-cost, got {other:?}"),
        }
    }

    /// Issue #3677: Flare of Denial — "sacrifice a nontoken blue creature" must
    /// keep BOTH the `NonToken` negation and the `blue creature` type/color
    /// filter. Before the fix to `parse_type_phrase`'s color-prefix scan (which
    /// only ran before the `non-` negation loop), the color and creature type
    /// were silently dropped, leaving a filter that matched any nontoken
    /// permanent — including a land — as a valid alternative-cost payment.
    #[test]
    fn alt_cost_sacrifice_nontoken_colored_creature_arm() {
        let option = parse_spell_casting_option_line(
            "You may sacrifice a nontoken blue creature rather than pay this spell's mana cost.",
            "Flare of Denial",
        )
        .expect("alt-cost should parse");
        match option {
            SpellCastingOption {
                kind: crate::types::ability::SpellCastingOptionKind::AlternativeCost,
                cost: Some(AbilityCost::Sacrifice(ref sac)),
                condition: None,
            } if sac.requirement.fixed_count() == Some(1) => match &sac.target {
                TargetFilter::Typed(tf) => {
                    assert!(
                        tf.type_filters.contains(&TypeFilter::Creature),
                        "expected Creature type filter, got {tf:?}"
                    );
                    assert!(
                        tf.properties.contains(&FilterProp::NonToken),
                        "expected NonToken property, got {tf:?}"
                    );
                    assert!(
                        tf.properties.contains(&FilterProp::HasColor {
                            color: ManaColor::Blue
                        }),
                        "expected blue HasColor property, got {tf:?}"
                    );
                }
                other => panic!("expected Typed sacrifice target, got {other:?}"),
            },
            other => panic!("expected Sacrifice(count=1) alt-cost, got {other:?}"),
        }
    }

    #[test]
    fn alt_cost_tap_with_leading_if_condition_binds() {
        // Ramosian Rally — leading "If you control a Plains, " binds via the outer
        // `split_leading_if_clause` + `parse_restriction_condition` pipeline.
        let option = parse_spell_casting_option_line(
            "If you control a Plains, you may tap an untapped creature you control rather than pay this spell's mana cost.",
            "Ramosian Rally",
        )
        .expect("alt-cost should parse with leading-if condition");
        match option {
            SpellCastingOption {
                kind: crate::types::ability::SpellCastingOptionKind::AlternativeCost,
                cost:
                    Some(AbilityCost::TapCreatures {
                        ref requirement, ..
                    }),
                condition:
                    Some(ParsedCondition::YouControlSubtypeCountAtLeast {
                        ref subtype,
                        count: 1,
                    }),
            } if subtype == "plains" && requirement.fixed_count() == Some(1) => {}
            other => panic!("expected TapCreatures + Plains-control condition, got {other:?}"),
        }
    }

    #[test]
    fn alt_cost_pay_mana_regression_unchanged() {
        // Existing class — Archive Trap shape. Verifies the verb-agnostic prefix still
        // routes "pay {N}" through `parse_oracle_cost` to `Mana { cost }`.
        let option = parse_spell_casting_option_line(
            "you may pay {0} rather than pay this spell's mana cost.",
            "Archive Trap",
        )
        .expect("alt-cost should parse");
        match option {
            SpellCastingOption {
                kind: crate::types::ability::SpellCastingOptionKind::AlternativeCost,
                cost:
                    Some(AbilityCost::Mana {
                        cost:
                            ManaCost::Cost {
                                generic: 0,
                                ref shards,
                            },
                    }),
                condition: None,
            } if shards.is_empty() => {}
            other => panic!("expected Mana(0) alt-cost, got {other:?}"),
        }
    }

    #[test]
    fn alt_cost_opponent_had_artifact_enter_condition() {
        let option = parse_spell_casting_option_line(
            "If an opponent had an artifact enter the battlefield under their control this turn, you may pay {1}{G} rather than pay this spell's mana cost.",
            "Baloth Cage Trap",
        )
        .expect("trap alt-cost should parse");
        match option.condition {
            Some(ParsedCondition::BattlefieldEntriesThisTurn {
                filter: TargetFilter::Typed(filter),
                count: 1,
            }) => {
                assert_eq!(filter.controller, Some(ControllerRef::Opponent));
                assert!(filter.type_filters.contains(&TypeFilter::Artifact));
            }
            other => panic!("expected opponent artifact entry condition, got {other:?}"),
        }
    }

    #[test]
    fn alt_cost_opponent_had_two_lands_enter_condition() {
        let option = parse_spell_casting_option_line(
            "If an opponent had two or more lands enter the battlefield under their control this turn, you may pay {3}{R}{R} rather than pay this spell's mana cost.",
            "Lavaball Trap",
        )
        .expect("trap alt-cost should parse");
        match option.condition {
            Some(ParsedCondition::BattlefieldEntriesThisTurn {
                filter: TargetFilter::Typed(filter),
                count: 2,
            }) => {
                assert_eq!(filter.controller, Some(ControllerRef::Opponent));
                assert!(filter.type_filters.contains(&TypeFilter::Land));
            }
            other => panic!("expected opponent land entry condition, got {other:?}"),
        }
    }

    #[test]
    fn alt_cost_nourishing_shoal_exile_green_card_with_mana_value_x() {
        use crate::types::ability::{
            Comparator, FilterProp, QuantityExpr, QuantityRef, TargetFilter,
        };

        let option = parse_spell_casting_option_line(
            "You may exile a green card with mana value X from your hand rather than pay this spell's mana cost.",
            "Nourishing Shoal",
        )
        .expect("Nourishing Shoal alt-cost should parse (#2372)");
        match option {
            SpellCastingOption {
                kind: crate::types::ability::SpellCastingOptionKind::AlternativeCost,
                cost:
                    Some(AbilityCost::Exile {
                        filter: Some(filter),
                        zone,
                        ..
                    }),
                condition: None,
            } => {
                assert_eq!(zone, Some(crate::types::zones::Zone::Hand));
                let TargetFilter::Typed(typed) = filter else {
                    panic!("expected typed exile filter, got {filter:?}");
                };
                assert!(typed.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Cmc {
                        comparator: Comparator::EQ,
                        value: QuantityExpr::Ref {
                            qty: QuantityRef::Variable { name },
                        },
                    } if name == "X"
                )));
            }
            other => panic!("expected AlternativeCost(Exile), got {other:?}"),
        }
    }

    #[test]
    fn alt_cost_pay_mana_composite_regression_unchanged() {
        // Force of Will shape — composite cost via " and " split.
        let option = parse_spell_casting_option_line(
            "You may pay 1 life and exile a blue card from your hand rather than pay this spell's mana cost.",
            "Force of Will",
        )
        .expect("alt-cost should parse");
        match option {
            SpellCastingOption {
                kind: crate::types::ability::SpellCastingOptionKind::AlternativeCost,
                cost: Some(AbilityCost::Composite { .. }),
                condition: None,
            } => {}
            other => panic!("expected Composite alt-cost, got {other:?}"),
        }
    }

    #[test]
    fn alt_cost_trailing_if_condition_drops_option_when_predicate_unrecognized() {
        // Blasphemous Edict — only card in dataset with trailing "if [condition]" suffix.
        // CR 118.9 + CR 601.3d: when the if-clause predicate cannot be parsed into a
        // typed `ParsedCondition`, the alt-cost option is dropped entirely so the spell
        // may only be cast at its printed cost. Strictly-safe degradation: a fail-silent
        // unconditional alt-cost emission would let the player pay {B} regardless of
        // the 13-creature threshold — strictly more permissive than the printed text.
        // The unrecognized predicate is surfaced by the SwallowedClause / Condition_If
        // detector for the parser gap-finder.
        let option = parse_spell_casting_option_line(
            "You may pay {B} rather than pay this spell's mana cost if there are thirteen or more creatures on the battlefield.",
            "Blasphemous Edict",
        );
        assert!(
            option.is_none(),
            "unrecognized if-clause must drop the alt-cost option entirely, got: {option:?}"
        );
    }

    #[test]
    fn alt_cost_leading_if_not_your_turn_condition_binds() {
        // Force of Despair — leading "If it's not your turn, " gates the alt-cost.
        // CR 102.1: the active player is the player whose turn it is. Nom parses
        // "it's not your turn" → `StaticCondition::Not(DuringYourTurn)`, which the
        // restriction bridge maps to `Not(IsYourTurn)`.
        let option = parse_spell_casting_option_line(
            "If it's not your turn, you may exile a black card from your hand rather than pay this spell's mana cost.",
            "Force of Despair",
        )
        .expect("alt-cost should parse with leading-if not-your-turn condition");
        match option {
            SpellCastingOption {
                kind: crate::types::ability::SpellCastingOptionKind::AlternativeCost,
                condition: Some(ParsedCondition::Not { condition }),
                ..
            } if matches!(*condition, ParsedCondition::IsYourTurn) => {}
            other => panic!("expected Not(IsYourTurn) condition, got {other:?}"),
        }
    }

    #[test]
    fn alt_cost_leading_if_unrecognized_predicate_drops_option() {
        // CR 118.9 + CR 601.3d: when the leading-if predicate cannot decompose
        // into a typed `ParsedCondition`, the casting option must be dropped
        // entirely — not emitted unconditionally. This mirrors the trailing-if
        // `?` contract; the prior `.map()` silently assigned `None` and emitted
        // the alt-cost regardless of the gate.
        let option = parse_spell_casting_option_line(
            "If the sky is green, you may exile a black card from your hand rather than pay this spell's mana cost.",
            "Test Card",
        );
        assert!(
            option.is_none(),
            "unrecognized leading-if predicate must drop the alt-cost option, got: {option:?}"
        );
    }

    #[test]
    fn spell_flash_option_targets_commander_condition_attaches() {
        // CR 601.3d + CR 702.8a: "as though it had flash if it targets a commander"
        // — Timely Ward class. The if-clause must populate the option's `condition`
        // slot with a typed `SpellTargetsFilter` rather than being dropped.
        let option = parse_spell_casting_option_line(
            "You may cast this spell as though it had flash if it targets a commander.",
            "Timely Ward",
        )
        .expect("flash-conditional should parse");
        assert!(matches!(
            option.kind,
            crate::types::ability::SpellCastingOptionKind::AsThoughHadFlash
        ));
        match option.condition {
            Some(ParsedCondition::SpellTargetsFilter {
                filter: TargetFilter::Typed(ref f),
            }) => {
                assert!(f.properties.contains(&FilterProp::IsCommander));
            }
            other => panic!("expected SpellTargetsFilter(IsCommander), got {other:?}"),
        }
    }

    #[test]
    fn spell_flash_option_behold_additional_cost_attaches() {
        let option = parse_spell_casting_option_line(
            "You may cast this spell as though it had flash if you behold a Dragon as an additional cost to cast it.",
            "Molten Exhale",
        )
        .expect("behold flash option should parse");
        assert!(matches!(
            option.kind,
            crate::types::ability::SpellCastingOptionKind::AsThoughHadFlash
        ));
        match option.cost {
            Some(AbilityCost::Behold {
                count: 1,
                filter: TargetFilter::Typed(filter),
                action: BeholdCostAction::ChooseOrReveal,
            }) => {
                assert!(filter
                    .type_filters
                    .iter()
                    .any(|tf| matches!(tf, TypeFilter::Subtype(name) if name == "Dragon")));
            }
            other => panic!("expected Behold Dragon flash cost, got {other:?}"),
        }
    }

    #[test]
    fn spell_flash_option_unrecognized_if_clause_drops_option() {
        // CR 601.3d: when the if-clause predicate cannot be parsed, the flash option
        // is dropped so the spell stays sorcery-speed. A fail-silent unconditional
        // flash emission would let the player cast at instant speed regardless of
        // the printed gating condition — strictly more permissive than the text.
        let option = parse_spell_casting_option_line(
            "You may cast this spell as though it had flash if frob is wobble.",
            "Imaginary Card",
        );
        assert!(
            option.is_none(),
            "unrecognized if-clause must drop the flash option, got: {option:?}"
        );
    }

    // CR 500 + CR 601.3a: "You can't cast ~ during your first[, second, or third] turn[s] of
    // the game" must use per-player TurnsTaken, NOT the global turn_number.
    // Regression for issue #2002: Spider-Man 2099 was castable on the player's 3rd turn
    // because the global turn counter counts both players' turns (my turn 3 = global turn 5).
    #[test]
    fn spell_cast_restriction_parses_first_n_turns_of_game_per_player() {
        // Spider-Man 2099 exact oracle text (with ability-word prefix and curly apostrophe,
        // after card-name normalization to "~").
        let restrictions = parse_casting_restriction_line(
            "From the Future \u{2014} You can\u{2019}t cast ~ during your first, second, or third turns of the game.",
        )
        .expect("Spider-Man 2099 restriction should parse");
        assert_eq!(
            restrictions,
            vec![CastingRestriction::RequiresCondition {
                condition: Some(ParsedCondition::Not {
                    condition: Box::new(ParsedCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::TurnsTaken,
                        },
                        comparator: Comparator::LE,
                        rhs: QuantityExpr::Fixed { value: 3 },
                    }),
                }),
            }],
            "must block casting on turns 1–3 using per-player TurnsTaken, not global turn_number"
        );
    }

    #[test]
    fn spell_cast_restriction_parses_first_two_turns_of_game() {
        // "first or second" variant — max_ordinal = 2.
        let restrictions = parse_casting_restriction_line(
            "You can't cast ~ during your first or second turns of the game.",
        )
        .expect("two-turn restriction should parse");
        assert_eq!(
            restrictions,
            vec![CastingRestriction::RequiresCondition {
                condition: Some(ParsedCondition::Not {
                    condition: Box::new(ParsedCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::TurnsTaken,
                        },
                        comparator: Comparator::LE,
                        rhs: QuantityExpr::Fixed { value: 2 },
                    }),
                }),
            }]
        );
    }

    #[test]
    fn spell_cast_restriction_parses_first_turn_of_game() {
        // Singular "turn" variant — max_ordinal = 1.
        let restrictions = parse_casting_restriction_line(
            "You can't cast this spell during your first turn of the game.",
        )
        .expect("single-turn restriction should parse");
        assert_eq!(
            restrictions,
            vec![CastingRestriction::RequiresCondition {
                condition: Some(ParsedCondition::Not {
                    condition: Box::new(ParsedCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::TurnsTaken,
                        },
                        comparator: Comparator::LE,
                        rhs: QuantityExpr::Fixed { value: 1 },
                    }),
                }),
            }]
        );
    }

    #[test]
    fn spell_cast_restriction_rejects_trailing_turn_clause_text() {
        let restrictions = parse_casting_restriction_line(
            "You can't cast ~ during your first turn of the game and only if you control a Forest.",
        );
        assert_eq!(
            restrictions, None,
            "trailing conjunct must not be swallowed into an unconditional turn restriction"
        );
    }
}
