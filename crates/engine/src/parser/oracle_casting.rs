use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::value;
use nom::Parser;
use nom_language::error::VerboseError;

use super::oracle_cost::parse_oracle_cost;
use super::oracle_util::{parse_mana_symbols, TextPair};
use crate::parser::oracle_condition::parse_restriction_condition;
use crate::types::ability::{AbilityCost, AdditionalCost, CastingRestriction, SpellCastingOption};

/// Parse "As an additional cost to cast this spell, ..." into an `AdditionalCost`.
///
/// Recognized patterns:
/// - "you may blight N" → `Optional(Blight { count: N })`
/// - "blight N or pay {M}" → `Choice(Blight { count: N }, Mana { cost: M })`
/// - General "X or Y" → `Choice(X, Y)` using `parse_single_cost` for each fragment
pub fn parse_additional_cost_line(lower: &str, raw: &str) -> Option<AdditionalCost> {
    // Strip the standard additional-cost prefix.
    let after_prefix =
        tag::<_, _, VerboseError<&str>>("as an additional cost to cast this spell, ")
            .parse(lower)
            .map_or(lower, |(rest, _)| rest);
    // Use TextPair for case-preserving parallel slicing, then strip trailing period.
    let tp = TextPair::new(&raw[raw.len() - after_prefix.len()..], after_prefix);
    let tp = tp.trim_end_matches('.');
    let body_lower = tp.lower;
    let body_raw = tp.original;

    // "you may [cost]" → Optional wrapping
    if let Ok((opt_lower, _)) = tag::<_, _, VerboseError<&str>>("you may ").parse(body_lower) {
        let opt_raw = &body_raw[body_raw.len() - opt_lower.len()..];
        let cost = super::oracle_cost::parse_single_cost(opt_raw);
        if !matches!(cost, AbilityCost::Unimplemented { .. }) {
            return Some(AdditionalCost::Optional(cost));
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
        .map(|mut option| {
            if option.condition.is_none() {
                if let Some(condition_text) = condition {
                    option.condition = parse_restriction_condition(condition_text);
                }
            }
            option
        })
}

fn split_leading_if_clause(text: &str) -> (Option<&str>, &str) {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    if tag::<_, _, VerboseError<&str>>("if ")
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

    if let Ok((after, _)) = tag::<_, _, VerboseError<&str>>("if you pay ").parse(rest) {
        if let Some(cost_text) = after.strip_suffix(" more to cast it") {
            option = option.cost(parse_oracle_cost(cost_text));
            return Some(option);
        }
    }

    if let Ok((after, _)) = tag::<_, _, VerboseError<&str>>("by ").parse(rest) {
        if let Some(cost_text) = after.strip_suffix(" in addition to paying its other costs") {
            option = option.cost(parse_oracle_cost(cost_text));
            return Some(option);
        }
    }

    if let Ok((condition_text, _)) = tag::<_, _, VerboseError<&str>>("if ").parse(rest) {
        if let Some(parsed) = parse_restriction_condition(condition_text.trim()) {
            option = option.condition(parsed);
        }
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
            if let Some(parsed) = parse_restriction_condition(condition_text) {
                option = option.condition(parsed);
            }
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

    let (after_prefix_lower, _) = tag::<_, _, VerboseError<&str>>(PREFIX)
        .parse(body_lower)
        .ok()?;
    let prefix_len = body_lower.len() - after_prefix_lower.len();

    let (after_suffix_lower, _) = take_until::<_, _, VerboseError<&str>>(SUFFIX)
        .parse(after_prefix_lower)
        .ok()?;
    let cost_end = body_lower.len() - after_suffix_lower.len();

    let cost_text = body[prefix_len..cost_end].trim();
    let after_suffix_pos = cost_end + SUFFIX.len();
    let remainder_lower = &body_lower[after_suffix_pos..];
    let trailing_if = if let Ok((cond_lower, _)) =
        tag::<_, _, VerboseError<&str>>(" if ").parse(remainder_lower)
    {
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
            tag::<_, _, VerboseError<&str>>("you may cast this spell "),
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
    let effective = if tag::<_, _, VerboseError<&str>>("cast this spell only ")
        .parse(trimmed_lower.as_str())
        .is_ok()
    {
        trimmed.to_lowercase()
    } else {
        super::oracle_modal::strip_ability_word(trimmed)?.to_lowercase()
    };
    let rest =
        match tag::<_, _, VerboseError<&str>>("cast this spell only ").parse(effective.as_str()) {
            Ok((r, _)) => r,
            Err(_) => return None,
        };
    let mut restrictions = scan_timing_restrictions(rest);

    // Extract condition clauses: "if ...", "only if ...", or "... and only if ..."
    if let Ok((condition, _)) =
        alt((tag::<_, _, VerboseError<&str>>("only if "), tag("if "))).parse(rest)
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
) -> nom::IResult<&str, CastingRestriction, VerboseError<&str>> {
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
fn parse_during_phrase(input: &str) -> nom::IResult<&str, CastingRestriction, VerboseError<&str>> {
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
fn parse_opponent_possessive(input: &str) -> nom::IResult<&str, &str, VerboseError<&str>> {
    alt((
        tag("an opponent\u{2019}s "),
        tag("an opponent's "),
        tag("an opponents "),
    ))
    .parse(input)
}

/// After "an opponent's", dispatch on the phase keyword.
fn parse_opponent_phase(input: &str) -> nom::IResult<&str, CastingRestriction, VerboseError<&str>> {
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
) -> nom::IResult<&str, CastingRestriction, VerboseError<&str>> {
    use nom::sequence::preceded;
    value(
        CastingRestriction::DuringOpponentsTurn,
        preceded(parse_opponent_possessive, tag("turn")),
    )
    .parse(input)
}

/// Sub-dispatch for "before [rest]" — attackers, blockers, combat damage.
fn parse_before_phrase(input: &str) -> nom::IResult<&str, CastingRestriction, VerboseError<&str>> {
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
    use crate::types::ability::{ParsedCondition, QuantityExpr, TargetFilter};
    use crate::types::mana::ManaCost;
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
            Some(AdditionalCost::Optional(AbilityCost::Blight { count: 1 }))
        );
    }

    #[test]
    fn parse_additional_cost_optional_blight_2() {
        let lower = "as an additional cost to cast this spell, you may blight 2.";
        let raw = "As an additional cost to cast this spell, you may blight 2.";
        let result = parse_additional_cost_line(lower, raw);
        assert_eq!(
            result,
            Some(AdditionalCost::Optional(AbilityCost::Blight { count: 2 }))
        );
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
                    random: false,
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
            Some(AdditionalCost::Choice(
                AbilityCost::Sacrifice { .. },
                AbilityCost::Mana { .. },
            )) => {}
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
            Some(AdditionalCost::Required(AbilityCost::Sacrifice { target, count: 1 })) => {
                assert!(
                    matches!(target, TargetFilter::Or { .. }),
                    "Expected Or filter, got {target:?}"
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
            Some(AdditionalCost::Required(AbilityCost::Sacrifice { count: 1, .. })) => {}
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
    fn parse_additional_cost_optional_sacrifice() {
        let lower = "as an additional cost to cast this spell, you may sacrifice an artifact.";
        let raw = "As an additional cost to cast this spell, you may sacrifice an artifact.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Optional(AbilityCost::Sacrifice { count: 1, .. })) => {}
            other => panic!("Expected Optional(Sacrifice), got {:?}", other),
        }
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
            Some(AdditionalCost::Required(AbilityCost::Sacrifice { count: 1, .. })) => {}
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
                        count: 1,
                        filter: _,
                    }),
                condition: None,
            } => {}
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
                        count: 3,
                        filter: _,
                    }),
                condition: None,
            } => {}
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
                cost: Some(AbilityCost::Sacrifice { count: 2, .. }),
                condition: None,
            } => {}
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
                cost: Some(AbilityCost::Sacrifice { count: 3, .. }),
                condition: None,
            } => {}
            other => panic!("expected Sacrifice(count=3) alt-cost, got {other:?}"),
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
                        count: 1,
                        filter: _,
                    }),
                condition:
                    Some(ParsedCondition::YouControlSubtypeCountAtLeast {
                        ref subtype,
                        count: 1,
                    }),
            } if subtype == "plains" => {}
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
    fn alt_cost_trailing_if_condition_does_not_drop_cost() {
        // Blasphemous Edict — only card in dataset with trailing "if [condition]" suffix.
        // The mana cost {B} must still parse cleanly even with a trailing if-clause.
        // (The condition itself, "there are thirteen or more creatures on the battlefield",
        // is currently outside `parse_restriction_condition`'s recognized patterns; that's
        // a separate parser gap and pre-existed this fix — see card-data.json's existing
        // `condition: null` on Blasphemous Edict's casting_options.)
        let option = parse_spell_casting_option_line(
            "You may pay {B} rather than pay this spell's mana cost if there are thirteen or more creatures on the battlefield.",
            "Blasphemous Edict",
        )
        .expect("alt-cost should parse even when trailing-if condition is unrecognized");
        assert!(matches!(
            option.kind,
            crate::types::ability::SpellCastingOptionKind::AlternativeCost
        ));
        assert!(matches!(option.cost, Some(AbilityCost::Mana { .. })));
    }
}
