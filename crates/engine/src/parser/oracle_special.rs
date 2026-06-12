use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::char;
use nom::combinator::opt;
use nom::combinator::value;
use nom::sequence::delimited;
use nom::Parser;

use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Comparator, DieResultBranch, Effect,
    SolveCondition, StaticDefinition, TargetFilter, TypedFilter,
};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
use crate::types::statics::StaticMode;

use super::oracle_cost::{parse_or_separated_mana_costs, parse_single_cost};
use super::oracle_effect::imperative::try_parse_die_result_line;
use super::oracle_effect::{capitalize, parse_effect_chain};
use super::oracle_nom::bridge::nom_on_lower;
use super::oracle_nom::condition::parse_inner_condition;
use super::oracle_nom::error::OracleResult;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_util::{
    normalize_card_name_refs, parse_mana_symbols, parse_subtype, strip_reminder_text,
};

/// CR 719.1: Parse a Case's "To solve" condition text into a typed `SolveCondition`.
/// Handles "you control no {filter}" and falls back to `Text` for others.
pub(super) fn parse_solve_condition(text: &str) -> SolveCondition {
    use crate::types::ability::{ControllerRef, FilterProp};

    if let Some(((), rest)) =
        nom_on_lower(text, text, |i| value((), tag("you control no ")).parse(i))
    {
        let rest = rest.trim_end_matches('.');
        let mut properties = Vec::new();

        let rest = if let Some(((), after)) =
            nom_on_lower(rest, rest, |i| value((), tag("suspected ")).parse(i))
        {
            properties.push(FilterProp::Suspected);
            after
        } else {
            rest
        };

        let rest_trimmed = rest.trim();
        let subtype = parse_subtype(rest_trimmed)
            .map(|(canonical, _)| canonical)
            .unwrap_or_else(|| capitalize(rest_trimmed.trim_end_matches('s')));

        let filter = TargetFilter::Typed(
            TypedFilter::creature()
                .subtype(subtype)
                .controller(ControllerRef::You)
                .properties(properties),
        );

        return SolveCondition::ObjectCount {
            filter,
            comparator: Comparator::EQ,
            threshold: 0,
        };
    }

    // CR 719.3a: A solve condition is a general game-state condition. Route it
    // through the single condition authority (`parse_inner_condition`) so every
    // condition shape the engine already understands (life totals, hand size,
    // control counts, event history, quantity comparisons) makes a Case
    // auto-solve. The caller lowercases the text (`oracle.rs` passes `rest_lower`);
    // bridge defensively through `nom_on_lower` exactly as the arm above.
    let trimmed = text.trim_end_matches('.').trim();
    if let Some((condition, rest)) = nom_on_lower(trimmed, trimmed, parse_inner_condition) {
        // Only accept a fully-consumed parse (trailing punctuation/whitespace
        // already stripped); a partial parse falls through to `Text`.
        if rest.trim().trim_end_matches('.').trim().is_empty() {
            return SolveCondition::Condition { condition };
        }
    }

    SolveCondition::Text {
        description: text.to_string(),
    }
}

/// Parse the Defiler cycle two-line pattern into a DefilerCostReduction static.
pub(super) fn parse_defiler_cost_reduction(
    lower: &str,
    has_next_line: bool,
    next_line_lower: impl FnOnce() -> Option<String>,
) -> Option<(StaticDefinition, bool)> {
    let (rest, (color, life_cost)) = parse_defiler_life_payment_sentence(lower.trim()).ok()?;
    let consumes_next_line = rest.is_empty();
    let reduction_text = if consumes_next_line {
        if !has_next_line {
            return None;
        }
        next_line_lower()?
    } else {
        rest.to_string()
    };
    let (rest, mana_reduction) =
        parse_defiler_reduction_sentence(reduction_text.trim(), color).ok()?;
    let (rest, mana_limit) = opt((
        tag::<_, _, OracleError<'_>>(". this effect reduces only the amount of "),
        parse_defiler_color,
        tag(" mana you pay"),
    ))
    .parse(rest)
    .ok()?;
    if let Some((_, limit_color, _)) = mana_limit {
        if limit_color != color {
            return None;
        }
    }
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(rest).ok()?;
    if !rest.is_empty() {
        return None;
    }

    Some((
        StaticDefinition::new(StaticMode::DefilerCostReduction {
            color,
            life_cost,
            mana_reduction,
        })
        .affected(TargetFilter::SelfRef)
        .description(format!(
            "As an additional cost to cast {} permanent spells, you may pay {} life. Those spells cost less to cast.",
            defiler_color_word(color), life_cost
        )),
        consumes_next_line,
    ))
}

fn parse_defiler_life_payment_sentence(input: &str) -> OracleResult<'_, (ManaColor, u32)> {
    let (rest, _) = tag("as an additional cost to cast ").parse(input)?;
    let (rest, color) = parse_defiler_color(rest)?;
    let (rest, _) = tag(" permanent spell").parse(rest)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(", you may pay ").parse(rest)?;
    let (rest, life_cost) = nom_primitives::parse_number(rest)?;
    let (rest, _) = tag(" life.").parse(rest)?;
    let (rest, _) = opt(tag(" ")).parse(rest)?;
    Ok((rest, (color, life_cost)))
}

fn parse_defiler_reduction_sentence(input: &str, color: ManaColor) -> OracleResult<'_, ManaCost> {
    let (rest, _) = tag("those spells cost ").parse(input)?;
    let (rest, mana_reduction) = parse_defiler_mana_reduction(rest, color)?;
    let (rest, _) = tag(" less to cast").parse(rest)?;
    let (rest, _) = opt(tag(" if you paid life this way")).parse(rest)?;
    Ok((rest, mana_reduction))
}

fn parse_defiler_color(input: &str) -> OracleResult<'_, ManaColor> {
    alt((
        value(ManaColor::White, tag("white")),
        value(ManaColor::Blue, tag("blue")),
        value(ManaColor::Black, tag("black")),
        value(ManaColor::Red, tag("red")),
        value(ManaColor::Green, tag("green")),
    ))
    .parse(input)
}

fn parse_defiler_mana_reduction(input: &str, color: ManaColor) -> OracleResult<'_, ManaCost> {
    let shard = defiler_mana_shard(color);
    let cost = ManaCost::Cost {
        shards: vec![shard],
        generic: 0,
    };
    match color {
        ManaColor::White => value(cost, tag("{w}")).parse(input),
        ManaColor::Blue => value(cost, tag("{u}")).parse(input),
        ManaColor::Black => value(cost, tag("{b}")).parse(input),
        ManaColor::Red => value(cost, tag("{r}")).parse(input),
        ManaColor::Green => value(cost, tag("{g}")).parse(input),
    }
}

fn defiler_mana_shard(color: ManaColor) -> ManaCostShard {
    match color {
        ManaColor::White => ManaCostShard::White,
        ManaColor::Blue => ManaCostShard::Blue,
        ManaColor::Black => ManaCostShard::Black,
        ManaColor::Red => ManaCostShard::Red,
        ManaColor::Green => ManaCostShard::Green,
    }
}

fn defiler_color_word(color: ManaColor) -> &'static str {
    match color {
        ManaColor::White => "white",
        ManaColor::Blue => "blue",
        ManaColor::Black => "black",
        ManaColor::Red => "red",
        ManaColor::Green => "green",
    }
}

/// Normalize self-references in a line for static ability parsing.
pub(crate) fn normalize_self_refs_for_static(text: &str, card_name: &str) -> String {
    normalize_card_name_refs(text, card_name)
}

/// CR 706: Walk the sub_ability chain of a parsed trigger/ability to find the
/// terminal `RollDie { results: [] }` node and attach die result branches
/// from subsequent oracle text lines.
pub(super) fn attach_die_result_branches_to_chain(
    def: &mut AbilityDefinition,
    lines: &[&str],
    start_line: usize,
) -> usize {
    let roll_die = find_terminal_roll_die(def);
    let roll_die = match roll_die {
        Some(roll_die) => roll_die,
        None => return start_line,
    };

    let mut branches = Vec::new();
    let mut j = start_line;
    while j < lines.len() {
        let table_line = strip_reminder_text(lines[j].trim());
        if table_line.is_empty() {
            j += 1;
            continue;
        }
        if let Some((min, max, effect_text)) = try_parse_die_result_line(&table_line) {
            let effect_text = strip_die_table_flavor_label(effect_text);
            let branch_def = parse_effect_chain(effect_text, AbilityKind::Spell);
            branches.push(DieResultBranch {
                min,
                max,
                effect: Box::new(branch_def),
            });
            j += 1;
        } else {
            break;
        }
    }

    if !branches.is_empty() {
        if let Effect::RollDie {
            ref mut results, ..
        } = roll_die
        {
            *results = branches;
        }
    }

    j
}

fn find_terminal_roll_die(def: &mut AbilityDefinition) -> Option<&mut Effect> {
    if matches!(&*def.effect, Effect::RollDie { results, .. } if results.is_empty()) {
        return Some(&mut *def.effect);
    }
    if let Some(ref mut sub) = def.sub_ability {
        return find_terminal_roll_die(sub);
    }
    None
}

/// CR 706: Try to parse a die roll table starting at line `i`.
/// CR 706.2: Also extracts an optional "and add/subtract X" modifier
/// from the header line so the resolver can shift the natural result before
/// branch lookup (Deck of Many Things, Diviner's Portent, Gale's Redirection).
pub(super) fn try_parse_die_roll_table(
    lines: &[&str],
    i: usize,
    line: &str,
    kind: AbilityKind,
) -> Option<(AbilityDefinition, usize)> {
    let lower = line.to_lowercase();
    let (sides, modifier) = parse_roll_die_sides_with_modifier(&lower)?;

    let mut branches = Vec::new();
    let mut has_branches = false;
    let mut j = i + 1;
    while j < lines.len() {
        let table_line = strip_reminder_text(lines[j].trim());
        if table_line.is_empty() {
            j += 1;
            continue;
        }
        if let Some((min, max, effect_text)) = try_parse_die_result_line(&table_line) {
            let effect_text = strip_die_table_flavor_label(effect_text);
            let branch_def = parse_effect_chain(effect_text, kind);
            branches.push(DieResultBranch {
                min,
                max,
                effect: Box::new(branch_def),
            });
            has_branches = true;
            j += 1;
        } else {
            break;
        }
    }

    let mut def = AbilityDefinition::new(
        kind,
        Effect::RollDie {
            // CR 706.1: result-table rolls are single-die ("roll a d20 ...").
            count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            sides,
            results: branches,
            modifier,
        },
    );
    def.description = Some(line.to_string());
    Some((def, if has_branches { j } else { i + 1 }))
}

/// CR 706.1a + CR 706.2: Parse the header line of a die-roll table, returning
/// `(sides, optional_modifier)`. The modifier captures "and add/subtract X"
/// suffixes attached to the roll command. Used by `try_parse_die_roll_table`
/// so the same shape works at every parsing entry point (spell text, trigger
/// text, activated effect text).
fn parse_roll_die_sides_with_modifier(
    lower: &str,
) -> Option<(u8, Option<crate::types::ability::DieRollModifier>)> {
    let ((), rest) = nom_on_lower(lower, lower, |i| {
        value((), alt((tag("roll a d"), tag("rolls a d")))).parse(i)
    })?;
    // Numeric form first; word-form below.
    let digit_end = rest
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(rest.len());
    if digit_end > 0 {
        if let Ok(sides) = rest[..digit_end].parse::<u8>() {
            let after = &rest[digit_end..];
            return Some((sides, parse_optional_modifier_suffix(after)));
        }
    }
    let sides = parse_roll_die_sides_word_form(lower)?;
    Some((sides, None))
}

/// CR 706.2: Parse an optional " and (add|subtract) X" suffix attached to a
/// die roll header. Returns `None` when the suffix is empty / purely
/// punctuation; returns `None` (suffix dropped) when the suffix is non-empty
/// but doesn't shape as a recognized modifier — the caller's outer parsing
/// has already captured `sides`, and any wider trailing clause is handled by
/// downstream chain parsing.
fn parse_optional_modifier_suffix(after: &str) -> Option<crate::types::ability::DieRollModifier> {
    let trimmed = after.trim().trim_end_matches(['.', ',', ';']).trim();
    if trimmed.is_empty() {
        return None;
    }
    let (after_and, _) = tag::<_, _, OracleError<'_>>("and ").parse(trimmed).ok()?;
    let (modifier_text, sign) = alt((
        value(true, tag::<_, _, OracleError<'_>>("add ")),
        value(false, tag("subtract ")),
    ))
    .parse(after_and)
    .ok()?;
    let qty = crate::parser::oracle_quantity::parse_quantity_ref(modifier_text)?;
    let value = crate::types::ability::QuantityExpr::Ref { qty };
    Some(if sign {
        crate::types::ability::DieRollModifier::Add { value }
    } else {
        crate::types::ability::DieRollModifier::Subtract { value }
    })
}

/// CR 706: Parse word-form die patterns like "roll a six-sided die".
fn parse_roll_die_sides_word_form(lower: &str) -> Option<u8> {
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("roll a "), tag("rolls a ")))
        .parse(lower)
        .ok()?;
    let (_, sides) = alt((
        value(
            4_u8,
            alt((tag::<_, _, OracleError<'_>>("four-sided"), tag("4-sided"))),
        ),
        value(6, alt((tag("six-sided"), tag("6-sided")))),
        value(8, alt((tag("eight-sided"), tag("8-sided")))),
        value(10, alt((tag("ten-sided"), tag("10-sided")))),
        value(12, alt((tag("twelve-sided"), tag("12-sided")))),
        value(20, alt((tag("twenty-sided"), tag("20-sided")))),
    ))
    .parse(rest)
    .ok()?;
    Some(sides)
}

fn strip_die_table_flavor_label(text: &str) -> &str {
    if let Some(idx) = text.find(" \u{2014} ") {
        let before = &text[..idx];
        if before.split_whitespace().count() <= 4 {
            return &text[idx + " \u{2014} ".len()..];
        }
    }
    text
}

pub(super) fn parse_escape_keyword(line: &str) -> Option<Keyword> {
    let (_, after_dash) = line.split_once('\u{2014}')?;
    let after_dash = after_dash.trim();
    let (cost, rest) = parse_mana_symbols(after_dash)?;
    let rest = rest.trim_start_matches(',').trim();
    let rest_lower = rest.to_lowercase();
    let ((), exile_part) = nom_on_lower(&rest_lower, &rest_lower, |i| {
        value((), tag("exile ")).parse(i)
    })?;
    let (exile_count, _) = super::oracle_util::parse_number(exile_part)?;
    Some(Keyword::Escape { cost, exile_count })
}

pub(super) fn parse_harmonize_keyword(line: &str) -> Option<Keyword> {
    let cost = parse_keyword_mana_cost_line(line, "harmonize ")?;
    Some(Keyword::Harmonize(cost))
}

fn parse_keyword_mana_cost_line(line: &str, keyword: &'static str) -> Option<ManaCost> {
    let lower = line.to_lowercase();
    let ((), rest) = nom_on_lower(line, &lower, |i| value((), tag(keyword)).parse(i))?;
    let (after_cost, cost) = nom_primitives::parse_mana_cost(rest.trim_start()).ok()?;
    let after_cost = after_cost.trim();
    if !after_cost.is_empty() {
        let lower_after_cost = after_cost.to_lowercase();
        let ((), remainder) = nom_on_lower(after_cost, &lower_after_cost, |i| {
            value((), delimited(char('('), take_until(")"), char(')'))).parse(i)
        })?;
        if !remainder.trim().is_empty() {
            return None;
        }
    }
    Some(cost)
}

/// CR 702.187b: Parse a "Mayhem {cost}" Oracle line into `Keyword::Mayhem`.
/// MTGJSON's keywords array carries only the bare "Mayhem" name, so the mana
/// cost is extracted here from the card's Oracle text (the cost precedes the
/// parenthesized reminder text). Mirrors `parse_harmonize_keyword`.
pub(super) fn parse_mayhem_keyword(line: &str) -> Option<Keyword> {
    let cost = parse_keyword_mana_cost_line(line, "mayhem ")?;
    Some(Keyword::Mayhem(cost))
}

/// CR 702.24a: Dispatch a cumulative-upkeep cost text into a typed
/// `AbilityCost`. Tries disjunctive mana (`"{G} or {W}"`), then pure mana
/// (`"{1}"`), then falls back to the generic single-cost parser (which handles
/// "Pay N life", "Sacrifice a land", etc.). Returns `None` only when no
/// recognized cost shape can be extracted, so the caller can suppress emitting
/// a malformed `Keyword::CumulativeUpkeep` entry.
fn parse_cumulative_upkeep_cost(text: &str) -> Option<AbilityCost> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    // Disjunctive mana: "{G} or {W}" → AbilityCost::OneOf { [Mana, Mana] }.
    if let Some(costs) = parse_or_separated_mana_costs(text) {
        return Some(AbilityCost::OneOf {
            costs: costs
                .into_iter()
                .map(|c| AbilityCost::Mana { cost: c })
                .collect(),
        });
    }

    // Pure mana: "{1}" / "{2}{U}" — must fully consume the input.
    if let Some((cost, rest)) = parse_mana_symbols(text) {
        if rest.trim().is_empty() {
            return Some(AbilityCost::Mana { cost });
        }
    }

    // Non-mana costs: "Pay 2 life", "Sacrifice a land", etc.
    let cost = parse_single_cost(text);
    if matches!(cost, AbilityCost::Unimplemented { .. }) {
        None
    } else {
        Some(cost)
    }
}

/// CR 702.24: Parse "Cumulative upkeep—[cost]" or "Cumulative upkeep {mana}" from Oracle text.
pub(super) fn parse_cumulative_upkeep_keyword(line: &str) -> Option<Keyword> {
    let lower = line.to_lowercase();

    let em_dash_rest = nom_on_lower(line, &lower, |i| {
        value(
            (),
            nom::sequence::pair(
                tag::<_, _, OracleError<'_>>("cumulative upkeep"),
                tag("\u{2014}"),
            ),
        )
        .parse(i)
    });
    if let Some(((), rest)) = em_dash_rest {
        let stripped = strip_reminder_text(rest);
        let cost_text = stripped.trim().trim_end_matches('.');
        let cost = parse_cumulative_upkeep_cost(cost_text)?;
        return Some(Keyword::CumulativeUpkeep(cost));
    }

    let ((), rest) = nom_on_lower(line, &lower, |i| {
        value((), tag("cumulative upkeep ")).parse(i)
    })?;
    let stripped = strip_reminder_text(rest);
    let cost_text = stripped.trim().trim_end_matches('.');
    let cost = parse_cumulative_upkeep_cost(cost_text)?;
    Some(Keyword::CumulativeUpkeep(cost))
}

#[cfg(test)]
mod solve_condition_tests {
    use super::parse_solve_condition;
    use crate::types::ability::{
        Comparator, PlayerScope, QuantityExpr, QuantityRef, SolveCondition, StaticCondition,
    };

    // CR 719.3a: "you have no cards in hand" (Case of the Crimson Pulse) decomposes
    // through the single condition authority into a hand-size comparison.
    #[test]
    fn hand_size_solve_condition_decomposes() {
        let cond = parse_solve_condition("you have no cards in hand");
        assert_eq!(
            cond,
            SolveCondition::Condition {
                condition: StaticCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::Controller,
                        },
                    },
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Fixed { value: 0 },
                },
            }
        );
    }

    // A life-total threshold decomposes to a quantity comparison.
    #[test]
    fn life_total_solve_condition_decomposes() {
        let cond = parse_solve_condition("you have 10 or more life");
        match cond {
            SolveCondition::Condition {
                condition:
                    StaticCondition::QuantityComparison {
                        lhs:
                            QuantityExpr::Ref {
                                qty: QuantityRef::LifeTotal { .. },
                            },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 10 },
                    },
            } => {}
            other => panic!("expected life-total QuantityComparison, got {other:?}"),
        }
    }

    // A control-count solve condition routes through the condition authority
    // (no longer the inert `Text` fallback).
    #[test]
    fn control_count_solve_condition_decomposes() {
        let cond = parse_solve_condition("you control three or more creatures");
        assert!(
            matches!(cond, SolveCondition::Condition { .. }),
            "expected a decomposed Condition, got {cond:?}"
        );
    }

    // The existing bespoke subtype-count arm still produces `ObjectCount`.
    #[test]
    fn subtype_count_solve_condition_uses_object_count() {
        let cond = parse_solve_condition("you control no suspected skeletons");
        assert!(
            matches!(cond, SolveCondition::ObjectCount { .. }),
            "expected ObjectCount for the suspected-subtype arm, got {cond:?}"
        );
    }

    // A condition the shared combinator cannot decompose falls back to `Text`.
    #[test]
    fn unparseable_solve_condition_falls_back_to_text() {
        let cond = parse_solve_condition("the stars align in your favor");
        assert!(
            matches!(cond, SolveCondition::Text { .. }),
            "expected Text fallback, got {cond:?}"
        );
    }
}
