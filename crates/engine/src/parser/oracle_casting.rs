use crate::types::ability::{AbilityCost, AdditionalCost, CastingRestriction, SpellCastingOption};

use super::oracle_cost::parse_oracle_cost;
use super::oracle_util::parse_mana_symbols;

/// Parse "As an additional cost to cast this spell, ..." into an `AdditionalCost`.
///
/// Recognized patterns:
/// - "you may blight N" → `Optional(Blight { count: N })`
/// - "blight N or pay {M}" → `Choice(Blight { count: N }, Mana { cost: M })`
/// - General "X or Y" → `Choice(X, Y)` using `parse_single_cost` for each fragment
pub fn parse_additional_cost_line(lower: &str, _raw: &str) -> Option<AdditionalCost> {
    // Pattern: "you may blight N" → Optional
    if let Some(pos) = lower.find("you may blight ") {
        let after = &lower[pos + "you may blight ".len()..];
        let count = parse_blight_count(after);
        return Some(AdditionalCost::Optional(AbilityCost::Blight { count }));
    }

    // Pattern: "blight N or pay {M}" → Choice (specific pattern with case-sensitive mana)
    if let Some(pos) = lower.find("blight ") {
        let after_blight = &lower[pos + "blight ".len()..];
        let count = parse_blight_count(after_blight);

        if let Some(or_pos) = after_blight.find(" or pay ") {
            let or_abs_pos = pos + "blight ".len() + or_pos + " or pay ".len();
            let mana_part = &_raw[or_abs_pos..];
            if let Some((mana_cost, _)) = parse_mana_symbols(mana_part.trim_end_matches('.')) {
                return Some(AdditionalCost::Choice(
                    AbilityCost::Blight { count },
                    AbilityCost::Mana { cost: mana_cost },
                ));
            }
        }
    }

    // Strip the standard additional-cost prefix and trailing period.
    let body = lower
        .strip_prefix("as an additional cost to cast this spell, ")
        .unwrap_or(lower)
        .trim_end_matches('.');

    // "waterbend {N}" as mandatory additional cost
    if let Some(rest) = body.strip_prefix("waterbend ") {
        if let Some((mana_cost, _)) = parse_mana_symbols(rest.trim()) {
            return Some(AdditionalCost::Required(AbilityCost::Waterbend {
                cost: mana_cost,
            }));
        }
    }

    // General "X or Y" choice pattern using parse_single_cost for each fragment.

    if let Some((left, right)) = body.split_once(" or ") {
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
                if let Some(condition) = condition {
                    option.condition = Some(condition.to_string());
                }
            }
            option
        })
}

fn split_leading_if_clause(text: &str) -> (Option<&str>, &str) {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    if !lower.starts_with("if ") {
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
    if !body_lower.starts_with(&prefix) {
        return None;
    }

    let rest = body[prefix.len()..].trim();
    let mut option = SpellCastingOption::as_though_had_flash();

    if rest.is_empty() {
        return Some(option);
    }

    if let Some(cost_text) = rest
        .strip_prefix("if you pay ")
        .and_then(|rest| rest.strip_suffix(" more to cast it"))
    {
        option = option.cost(parse_oracle_cost(cost_text));
        return Some(option);
    }

    if let Some(cost_text) = rest
        .strip_prefix("by ")
        .and_then(|rest| rest.strip_suffix(" in addition to paying its other costs"))
    {
        option = option.cost(parse_oracle_cost(cost_text));
        return Some(option);
    }

    if let Some(condition) = rest.strip_prefix("if ") {
        option = option.condition(condition.trim());
        return Some(option);
    }

    Some(option)
}

fn parse_self_alternative_cost_option(
    body: &str,
    body_lower: &str,
    card_name: &str,
) -> Option<SpellCastingOption> {
    if let Some(cost_text) = extract_alternative_cost(
        body,
        body_lower,
        "you may pay ",
        " rather than pay this spell's mana cost",
    ) {
        return Some(SpellCastingOption::alternative_cost(parse_oracle_cost(
            cost_text,
        )));
    }

    if let Some((cost_text, condition)) = extract_alternative_cost_with_trailing_condition(
        body,
        body_lower,
        "you may pay ",
        " rather than pay this spell's mana cost if ",
    ) {
        return Some(
            SpellCastingOption::alternative_cost(parse_oracle_cost(cost_text)).condition(condition),
        );
    }

    if let Some(self_ref) = self_spell_phrase(body_lower, card_name) {
        let without_cost = format!("you may cast {self_ref} without paying its mana cost");
        if body_lower == without_cost {
            return Some(SpellCastingOption::free_cast());
        }

        let for_cost = format!("you may cast {self_ref} for ");
        if body_lower.starts_with(&for_cost) {
            let cost_text = body[for_cost.len()..].trim();
            return Some(SpellCastingOption::alternative_cost(parse_oracle_cost(
                cost_text,
            )));
        }
    }

    None
}

fn extract_alternative_cost<'a>(
    raw: &'a str,
    lower: &str,
    prefix: &str,
    suffix: &str,
) -> Option<&'a str> {
    if lower.starts_with(prefix) && lower.ends_with(suffix) {
        let cost_end = raw.len() - suffix.len();
        return Some(raw[prefix.len()..cost_end].trim());
    }

    None
}

fn extract_alternative_cost_with_trailing_condition<'a>(
    raw: &'a str,
    lower: &str,
    prefix: &str,
    marker: &str,
) -> Option<(&'a str, &'a str)> {
    if !lower.starts_with(prefix) {
        return None;
    }

    let marker_pos = lower.find(marker)?;
    let cost_text = raw[prefix.len()..marker_pos].trim();
    let condition = raw[marker_pos + marker.len()..].trim();
    Some((cost_text, condition))
}

fn self_spell_phrase(lower: &str, card_name: &str) -> Option<String> {
    let card_name_lower = card_name.to_lowercase();
    if lower.starts_with("you may cast this spell ") {
        return Some("this spell".to_string());
    }
    if lower.starts_with("you may cast it ") {
        return Some("it".to_string());
    }
    if lower.starts_with(&format!("you may cast {card_name_lower} ")) {
        return Some(card_name_lower);
    }

    None
}

pub(crate) fn parse_casting_restriction_line(text: &str) -> Option<Vec<CastingRestriction>> {
    let lower = text.trim().trim_end_matches('.').to_lowercase();
    let rest = lower.strip_prefix("cast this spell only ")?;
    let mut restrictions = Vec::new();

    if rest.contains("as a sorcery") {
        restrictions.push(CastingRestriction::AsSorcery);
    }
    if rest.contains("during combat") {
        restrictions.push(CastingRestriction::DuringCombat);
    }
    if rest.contains("during an opponent's turn")
        || rest.contains("during an opponents turn")
        || rest.contains("on an opponent's turn")
        || rest.contains("on an opponents turn")
    {
        restrictions.push(CastingRestriction::DuringOpponentsTurn);
    }
    if rest.contains("during your turn") {
        restrictions.push(CastingRestriction::DuringYourTurn);
    }
    if rest.contains("during your upkeep") {
        restrictions.push(CastingRestriction::DuringYourUpkeep);
    }
    if rest.contains("during any upkeep step") || rest.contains("during any upkeep") {
        restrictions.push(CastingRestriction::DuringAnyUpkeep);
    }
    if rest.contains("during an opponent's upkeep") || rest.contains("during an opponents upkeep") {
        restrictions.push(CastingRestriction::DuringOpponentsUpkeep);
    }
    if rest.contains("during your end step") {
        restrictions.push(CastingRestriction::DuringYourEndStep);
    }
    if rest.contains("during an opponent's end step")
        || rest.contains("during an opponents end step")
    {
        restrictions.push(CastingRestriction::DuringOpponentsEndStep);
    }
    if rest.contains("during the declare attackers step")
        || rest.contains("during your declare attackers step")
        || rest.contains("during declare attackers step")
    {
        restrictions.push(CastingRestriction::DeclareAttackersStep);
    }
    if rest.contains("during the declare blockers step")
        || rest.contains("during your declare blockers step")
        || rest.contains("during declare blockers step")
    {
        restrictions.push(CastingRestriction::DeclareBlockersStep);
    }
    if rest.contains("before attackers are declared") {
        restrictions.push(CastingRestriction::BeforeAttackersDeclared);
    }
    if rest.contains("before blockers are declared") {
        restrictions.push(CastingRestriction::BeforeBlockersDeclared);
    }
    if rest.contains("before the combat damage step") || rest.contains("before combat damage") {
        restrictions.push(CastingRestriction::BeforeCombatDamage);
    }
    if rest.contains("after combat") {
        restrictions.push(CastingRestriction::AfterCombat);
    }

    if let Some(condition) = rest.strip_prefix("if ") {
        restrictions.push(CastingRestriction::RequiresCondition {
            text: strip_casting_condition_suffixes(condition).to_string(),
        });
    }
    if let Some(condition) = rest.strip_prefix("only if ") {
        restrictions.push(CastingRestriction::RequiresCondition {
            text: strip_casting_condition_suffixes(condition).to_string(),
        });
    }
    if let Some(condition) = rest.split(" and only if ").nth(1) {
        restrictions.push(CastingRestriction::RequiresCondition {
            text: strip_casting_condition_suffixes(condition).to_string(),
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

/// Extract the blight count (N) from text starting after "blight ".
fn parse_blight_count(text: &str) -> u32 {
    text.split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::mana::ManaCost;

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
                    text: "you've been attacked this step".to_string(),
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
                text: "you control two or more vampires".to_string(),
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
                    text: "there are four or more card types among cards in your graveyard"
                        .to_string(),
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
    fn parse_additional_cost_mandatory_blight_skipped() {
        // Mandatory blight (no "you may", no "or") — not yet modeled
        let lower = "as an additional cost to cast this spell, blight 2.";
        let raw = "As an additional cost to cast this spell, blight 2.";
        let result = parse_additional_cost_line(lower, raw);
        // Mandatory without "or" currently falls through (no choice to present)
        assert!(result.is_none());
    }

    #[test]
    fn parse_additional_cost_discard_or_pay_life() {
        let lower = "as an additional cost to cast this spell, discard a card or pay 3 life.";
        let raw = "As an additional cost to cast this spell, discard a card or pay 3 life.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Choice(
                AbilityCost::Discard {
                    count: 1,
                    random: false,
                    ..
                },
                AbilityCost::PayLife { amount: 3 },
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
        // "sacrifice an artifact or creature" is a single sacrifice cost, not a choice
        let lower = "as an additional cost to cast this spell, sacrifice an artifact or creature.";
        let raw = "As an additional cost to cast this spell, sacrifice an artifact or creature.";
        let result = parse_additional_cost_line(lower, raw);
        // Should return None — "creature" alone is Unimplemented, rejecting the split
        assert!(result.is_none());
    }
}
