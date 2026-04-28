use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::char;
use nom::combinator::value;
use nom::multi::many1;
use nom::sequence::{delimited, preceded, terminated};
use nom::Parser;
use nom_language::error::VerboseError;

use crate::parser::oracle_nom::error::OracleResult;
use crate::parser::oracle_nom::primitives as nom_primitives;
use crate::types::ability::{
    Effect, LinkedExileScope, ManaContribution, ManaProduction, ManaSpendRestriction, QuantityExpr,
    QuantityRef,
};
use crate::types::keywords::KeywordKind;
use crate::types::mana::{ManaColor, ManaSpellGrant};

use super::super::oracle_quantity::{parse_cda_quantity, parse_event_context_quantity};
use super::super::oracle_target::parse_type_phrase;
use super::super::oracle_util::{parse_mana_production, parse_number, TextPair};
use crate::types::ability::TargetFilter;

/// Bridge: run a nom combinator on a lowercase copy, mapping the consumed length
/// back to the original-case text to compute the correct remainder.
fn nom_on_lower<'a, T, F>(text: &'a str, lower: &str, mut parser: F) -> Option<(T, &'a str)>
where
    F: FnMut(&str) -> OracleResult<'_, T>,
{
    let (rest, result) = parser(lower).ok()?;
    let consumed = lower.len() - rest.len();
    Some((result, &text[consumed..]))
}

/// Public wrapper for the upstream clause dispatcher: accepts original-cased
/// text and lowercases internally. See `try_parse_for_each_color_mana`.
pub(super) fn try_parse_for_each_color_mana_public(text: &str) -> Option<Effect> {
    let lower = text.to_lowercase();
    try_parse_for_each_color_mana(text, &lower)
}

/// CR 106.1 + CR 109.1: Recognize "For each color among [type-phrase], add one
/// mana of that color" — the Faeburrow Elder class. Emits
/// `ManaProduction::DistinctColorsAmongPermanents { filter }`, which resolves
/// at activation time to one mana of each distinct color (W/U/B/R/G) present
/// among matching permanents.
fn try_parse_for_each_color_mana(text: &str, lower: &str) -> Option<Effect> {
    use nom::bytes::complete::take_until;
    let lower_trimmed = lower.trim_end_matches('.').trim();
    // Prefix: "for each color among "
    let (rest, _) = tag::<_, _, VerboseError<&str>>("for each color among ")
        .parse(lower_trimmed)
        .ok()?;
    // Boundary: the type phrase runs until ", add one mana of that color".
    let (_, type_text_lower) =
        take_until::<_, _, VerboseError<&str>>(", add one mana of that color")
            .parse(rest)
            .ok()?;
    // Recover original-cased slice for parse_type_phrase.
    let offset = lower_trimmed.len() - rest.len();
    let original_trimmed = text.trim_end_matches('.').trim();
    let type_text = &original_trimmed[offset..offset + type_text_lower.len()];
    let (filter, remainder) = parse_type_phrase(type_text);
    if !remainder.trim().is_empty() || matches!(filter, TargetFilter::Any) {
        return None;
    }
    Some(Effect::Mana {
        produced: ManaProduction::DistinctColorsAmongPermanents { filter },
        restrictions: vec![],
        grants: vec![],
        expiry: None,
        target: None,
    })
}

pub(super) fn try_parse_add_mana_effect(text: &str) -> Option<Effect> {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    // Match "add " prefix via nom
    let (_, clause) = nom_on_lower(trimmed, &lower, |i| value((), tag("add ")).parse(i))?;
    let clause = clause.trim();
    let clause_lower = clause.to_lowercase();
    let clause_tp = TextPair::new(clause, &clause_lower);
    let (without_where_x, where_x_expression) = super::strip_trailing_where_x(clause_tp);
    let clause = without_where_x.original.trim().trim_end_matches(['.', '"']);
    // CR 605.1a + CR 107.4a: Track whether the "an additional " prefix was present
    // so that `ChosenColor`/`AnyOneColor` variants record their contribution role
    // rather than silently dropping the additive qualifier (e.g. Utopia Sprawl,
    // Fertile Ground). Typed enum — never a bool.
    let clause_lower_trimmed = clause.to_lowercase();
    let (clause, contribution) = match nom_on_lower(clause, &clause_lower_trimmed, |i| {
        value((), tag("an additional ")).parse(i)
    }) {
        Some((_, rest)) => (rest, ManaContribution::Additional),
        None => (clause, ManaContribution::Base),
    };

    if let Some((produced, target)) = parse_mana_production_clause(clause, contribution) {
        return Some(Effect::Mana {
            produced,
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target,
        });
    }

    // CR 605.3b + CR 106.1a: Filter-land pattern — `{X}{X}, {X}{Y}, or {Y}{Y}`
    // (Shadowmoor/Eventide filter lands). Two or more comma-separated
    // combinations of pure-color mana symbols joined with `or`. Must be tried
    // before the count-prefix fallback since the clause has no leading count.
    if let Some(options) = parse_mana_combinations_clause(clause) {
        return Some(Effect::Mana {
            produced: ManaProduction::ChoiceAmongCombinations { options },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        });
    }

    // CR 106.1 / CR 106.3: "an amount of {color} equal to [quantity]"
    // e.g. "an amount of {G} equal to ~'s power"
    if let Some(effect) = try_parse_amount_equal_to(clause, contribution) {
        return Some(effect);
    }

    if let Some((count, rest)) = parse_mana_count_prefix(clause) {
        let count = apply_where_x_count_expression(count, where_x_expression.as_deref());
        let rest = rest.trim().trim_end_matches(['.', '"']).trim();
        let rest_lower = rest.to_lowercase();

        // CR 603.7c + CR 106.3: "add one mana of any type that land produced"
        // (Vorinclex, Voice of Hunger; Dictate of Karametra). Only meaningful
        // inside a TapsForMana trigger context; resolves the mana color from
        // the triggering `ManaAdded` event at resolution time.
        if let Some((_, _)) = nom_on_lower(rest, &rest_lower, |i| {
            preceded(
                tag("mana of any type that "),
                alt((
                    value((), tag("land produced")),
                    value((), tag("permanent produced")),
                )),
            )
            .parse(i)
        }) {
            // Count is fixed at 1 for this pattern (Oracle says "one mana");
            // CR 106.5: if the trigger event is absent the resolver returns
            // empty mana, so the count here is irrelevant for N>1.
            let _ = count;
            return Some(Effect::Mana {
                produced: ManaProduction::TriggerEventManaType,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            });
        }

        // CR 106.7 + CR 106.1b: "mana of any type that a land [scope] could
        // produce" — Reflecting Pool, Naga Vitalist, Incubation Druid, Cactus
        // Preserve, Horizon of Progress. The trailing scope phrase is
        // dispatched via `alt()` over the printed variants so future
        // opponent-/player-scoped printings slot in by adding a tag without
        // touching the runtime. Per "build for the class": the resulting
        // `TargetFilter` carries `ControllerRef` so a single primitive covers
        // every scoping variant.
        if let Some((controller_ref, _)) = nom_on_lower(rest, &rest_lower, |i| {
            preceded(
                tag("mana of any type that a land "),
                terminated(
                    alt((
                        value(
                            crate::types::ability::ControllerRef::You,
                            tag("you control"),
                        ),
                        value(
                            crate::types::ability::ControllerRef::Opponent,
                            tag("an opponent controls"),
                        ),
                    )),
                    tag(" could produce"),
                ),
            )
            .parse(i)
        }) {
            let land_filter = TargetFilter::Typed(
                crate::types::ability::TypedFilter::land().controller(controller_ref),
            );
            return Some(Effect::Mana {
                produced: ManaProduction::AnyTypeProduceableBy { count, land_filter },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            });
        }

        if let Some((_, after_color)) = nom_on_lower(rest, &rest_lower, |i| {
            alt((
                value((), tag("mana of any one color")),
                value((), tag("mana of any color")),
            ))
            .parse(i)
        }) {
            let after_lower = after_color.trim().to_lowercase();
            // CR 106.7: "that a land an opponent controls could produce"
            // CR 115.1 + CR 115.7: When the for-each branch resolves a player
            // target filter (e.g., "for each card in target opponent's hand"),
            // surface it on the returned `Effect::Mana::target` so the caller
            // attaches a player target slot. All other any-color variants have
            // no player target — `mana_target` defaults to `None`.
            let mut mana_target: Option<TargetFilter> = None;
            let produced = if nom_on_lower(after_color.trim(), &after_lower, |i| {
                value((), tag("that a land an opponent controls could produce")).parse(i)
            })
            .is_some()
            {
                ManaProduction::OpponentLandColors { count }
            } else if nom_on_lower(after_color.trim(), &after_lower, |i| {
                // CR 605.1a + CR 406.1 + CR 610.3: "mana of any color among the
                // exiled cards" — read colors dynamically from `state.exile_links`.
                value((), tag("among the exiled cards")).parse(i)
            })
            .is_some()
            {
                ManaProduction::ChoiceAmongExiledColors {
                    source: LinkedExileScope::ThisObject,
                }
            } else if nom_on_lower(after_color.trim(), &after_lower, |i| {
                // CR 903.4 + CR 903.4f: "any color in your commander('s/s')
                // color identity" — Path of Ancestry, Study Hall. Colors
                // resolve dynamically from the activator's commander(s)'
                // combined color identity. The `alt()` covers both singular
                // and plural possessive apostrophe placements.
                value(
                    (),
                    alt((
                        tag("in your commander's color identity"),
                        tag("in your commanders' color identity"),
                        tag("in your commanders color identity"),
                    )),
                )
                .parse(i)
            })
            .is_some()
            {
                ManaProduction::AnyInCommandersColorIdentity {
                    count,
                    contribution,
                }
            } else if let Some((dynamic_qty, target)) =
                try_parse_any_color_for_each_suffix(&after_lower)
            {
                // CR 106.1: "mana of any color for each [filter]" — dynamic
                // count of any-color mana, with one color choice per mana
                // produced. Mirrors the fixed-color "for each" handling in
                // `parse_mana_production_clause` (e.g., "Add {R} for each card
                // in target opponent's hand"); the only delta is that the
                // color options are the full any-color set instead of a fixed
                // list. Class: Coalition Relic, Storage Counter cycle
                // (Saprazzan Cove, Dwarven Hold, Hollow Trees, Mercadian
                // Bazaar).
                mana_target = target;
                ManaProduction::AnyOneColor {
                    count: QuantityExpr::Ref { qty: dynamic_qty },
                    color_options: all_mana_colors(),
                    contribution,
                }
            } else {
                ManaProduction::AnyOneColor {
                    count,
                    color_options: all_mana_colors(),
                    contribution,
                }
            };
            return Some(Effect::Mana {
                produced,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: mana_target,
            });
        }

        if let Some((_, _)) = nom_on_lower(rest, &rest_lower, |i| {
            value((), tag("mana in any combination of colors")).parse(i)
        }) {
            return Some(Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count,
                    color_options: all_mana_colors(),
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            });
        }

        if let Some((_, _)) = nom_on_lower(rest, &rest_lower, |i| {
            alt((
                value((), tag("mana of the chosen color")),
                value((), tag("mana of that color")),
            ))
            .parse(i)
        }) {
            return Some(Effect::Mana {
                produced: ManaProduction::ChosenColor {
                    count,
                    contribution,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            });
        }

        // CR 106.1: "[count] {color}" -> single color repeated (e.g., "six {G}" -> 6 Green)
        if let Some((colors, after)) = parse_mana_production(rest) {
            let after = after.trim();
            if !colors.is_empty() && (after.is_empty() || after == ".") {
                // Single color repeated N times
                if colors.len() == 1 {
                    return Some(Effect::Mana {
                        produced: ManaProduction::AnyOneColor {
                            count,
                            color_options: colors,
                            contribution,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    });
                }
            }
        }

        if let Some((_, after_combo)) = nom_on_lower(rest, &rest_lower, |i| {
            value((), tag("mana in any combination of ")).parse(i)
        }) {
            let color_set_text = after_combo.trim();
            if let Some(color_options) = parse_mana_color_set(color_set_text) {
                return Some(Effect::Mana {
                    produced: ManaProduction::AnyCombination {
                        count,
                        color_options,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                });
            }
        }
    }

    let clause_lower = clause.to_lowercase();
    let fallback_count = parse_mana_count_prefix(clause)
        .map(|(count, _)| count)
        .unwrap_or(QuantityExpr::Fixed { value: 1 });
    let fallback_count =
        apply_where_x_count_expression(fallback_count, where_x_expression.as_deref());

    // Scan for mana production type at word boundaries using nom combinators.
    let produced = scan_mana_production_type(&clause_lower, fallback_count.clone(), contribution)?;
    Some(Effect::Mana {
        produced,
        restrictions: vec![],
        grants: vec![],
        expiry: None,
        target: None,
    })
}

pub(super) fn try_parse_activate_only_condition(text: &str) -> Option<Effect> {
    let trimmed = text.trim().trim_end_matches('.');
    let lower = trimmed.to_ascii_lowercase();
    let (_, raw) = nom_on_lower(trimmed, &lower, |i| {
        value((), tag("activate only if you control ")).parse(i)
    })?;
    let raw_lower = raw.to_lowercase();
    let mut subtypes = Vec::new();
    for part in raw_lower.split(" or ") {
        let token = part
            .trim()
            .trim_start_matches("a ")
            .trim_start_matches("an ")
            .trim();
        let subtype = match token {
            "plains" => "Plains",
            "island" => "Island",
            "swamp" => "Swamp",
            "mountain" => "Mountain",
            "forest" => "Forest",
            _ => return None,
        };
        if !subtypes.contains(&subtype) {
            subtypes.push(subtype);
        }
    }

    if subtypes.is_empty() {
        return None;
    }

    Some(Effect::Unimplemented {
        name: "activate_only_if_controls_land_subtype_any".to_string(),
        description: Some(subtypes.join("|")),
    })
}

/// CR 115.1 + CR 115.7: Detect a player target filter inside a for-each clause.
///
/// When the for-each tail mentions "target opponent" or "target player", surface
/// the corresponding `TargetFilter` so the wrapping ability can attach a player
/// target slot. The actual count is resolved separately via `TargetZoneCardCount`
/// or `TargetLifeTotal` against `ability.targets` at resolution time.
///
/// Returns `None` when the clause refers to a non-target subject (e.g. "Swamp
/// you control" — Cabal Coffers' `ObjectCount`-class), in which case the parent
/// `Effect::Mana` keeps `target: None`.
fn for_each_clause_target_filter(for_each_rest: &str) -> Option<TargetFilter> {
    use crate::types::ability::{ControllerRef, TypedFilter};
    let lower = for_each_rest.to_lowercase();
    if nom_primitives::scan_contains(&lower, "target opponent") {
        // CR 115.1: "target opponent" — same encoding as `parse_target` uses
        // (TypedFilter with `ControllerRef::Opponent`) so target legality and
        // multiplayer filtering reuse the existing opponent-only path.
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ))
    } else if nom_primitives::scan_contains(&lower, "target player") {
        Some(TargetFilter::Player)
    } else {
        None
    }
}

/// CR 106.1: Detect a `for each [filter]` suffix on the "any color" branch and
/// dispatch the inner clause to the shared `parse_for_each_clause` quantity
/// dispatcher. Leading whitespace is skipped so the suffix is recognized whether
/// the input begins with a literal space or has been pre-trimmed. The for-each
/// clause is passed lowercase-normalized — `parse_for_each_clause` itself does
/// its own lowercasing for type-phrase parsing, and the clause never contains
/// a card name (which would already be `~`-normalized upstream by the same
/// pipeline that built the `lower` view passed in here).
///
/// Returns the resolved `QuantityRef` paired with an optional player
/// `TargetFilter` so the parent `Effect::Mana` can attach a player target slot
/// when the for-each clause references "target opponent" / "target player"
/// (CR 115.1 + CR 115.7). Mirrors `parse_mana_production_clause`'s
/// `for_each_clause_target_filter` call so future printings of
/// "Add one mana of any color for each card in target opponent's hand"
/// surface the player target via the same primitive.
///
/// Returns `None` when no for-each suffix is present or the inner clause does
/// not parse as a known quantity.
fn try_parse_any_color_for_each_suffix(lower: &str) -> Option<(QuantityRef, Option<TargetFilter>)> {
    let (rest, _) = preceded(
        nom::character::complete::multispace0::<_, VerboseError<&str>>,
        tag("for each "),
    )
    .parse(lower.trim_start())
    .ok()?;
    let for_each_rest = rest.trim().trim_end_matches('.').trim();
    let qty = super::super::oracle_quantity::parse_for_each_clause(for_each_rest)?;
    let target = for_each_clause_target_filter(for_each_rest);
    Some((qty, target))
}

pub(super) fn parse_mana_production_clause(
    text: &str,
    contribution: ManaContribution,
) -> Option<(ManaProduction, Option<TargetFilter>)> {
    if let Some(color_options) = parse_mana_color_set(text) {
        if color_options.len() > 1 {
            return Some((
                ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options,
                    contribution,
                },
                None,
            ));
        }
    }

    if let Some((colors, remainder)) = parse_mana_production(text) {
        let remainder = remainder.trim().trim_end_matches(['.', '"']).trim();
        if remainder.is_empty() {
            return Some((
                ManaProduction::Fixed {
                    colors,
                    contribution,
                },
                None,
            ));
        }
        // CR 106.1: "{color} for each [filter]" -> dynamic mana count
        let remainder_lower = remainder.to_lowercase();
        if let Some((_, for_each_rest)) = nom_on_lower(remainder, &remainder_lower, |i| {
            value((), tag("for each ")).parse(i)
        }) {
            let qty = super::super::oracle_quantity::parse_for_each_clause(for_each_rest)?;
            // CR 115.1 + CR 115.7: Surface a player target filter when the
            // for-each clause references a target player/opponent (Jeska's Will
            // mode 1: "Add {R} for each card in target opponent's hand"). The
            // count itself is `TargetZoneCardCount` / `TargetLifeTotal`, which
            // resolves against `ability.targets` at resolution time.
            let target = for_each_clause_target_filter(for_each_rest);
            return Some((
                ManaProduction::AnyOneColor {
                    count: QuantityExpr::Ref { qty },
                    color_options: colors,
                    contribution,
                },
                target,
            ));
        }
        // Unknown trailing text -- don't silently discard it
        return None;
    }

    if let Some((colorless_count, remainder)) = parse_colorless_mana_production(text) {
        let remainder = remainder.trim().trim_end_matches(['.', '"']).trim();
        if remainder.is_empty() {
            return Some((
                ManaProduction::Colorless {
                    count: colorless_count,
                },
                None,
            ));
        }
        // CR 106.1: "{C} for each [filter]" -> dynamic colorless mana count
        let remainder_lower = remainder.to_lowercase();
        if let Some((_, for_each_rest)) = nom_on_lower(remainder, &remainder_lower, |i| {
            value((), tag("for each ")).parse(i)
        }) {
            let qty = super::super::oracle_quantity::parse_for_each_clause(for_each_rest)?;
            let target = for_each_clause_target_filter(for_each_rest);
            return Some((
                ManaProduction::Colorless {
                    count: QuantityExpr::Ref { qty },
                },
                target,
            ));
        }
        // CR 106.1: Mixed colorless + colored: {C}{W}, {C}{C}{R}, etc.
        // (e.g. Karoo, Azorius Chancery, Grinning Ignus)
        if let Some((colors, after_colors)) = parse_mana_production(remainder) {
            let after_colors = after_colors.trim().trim_end_matches(['.', '"']).trim();
            if after_colors.is_empty() {
                if let QuantityExpr::Fixed { value: n } = colorless_count {
                    return Some((
                        ManaProduction::Mixed {
                            colorless_count: n as u32,
                            colors,
                        },
                        None,
                    ));
                }
            }
        }
        return None;
    }

    None
}

pub(super) fn parse_colorless_mana_production(text: &str) -> Option<(QuantityExpr, &str)> {
    let rest = text.trim_start();
    // Nom combinator: count consecutive {C} symbols.
    let result: Result<(&str, Vec<()>), _> = many1(delimited(
        tag::<_, _, VerboseError<&str>>("{"),
        value((), alt((tag("C"), tag("c")))),
        terminated(
            tag("}"),
            nom::combinator::opt(nom::character::complete::multispace0),
        ),
    ))
    .parse(rest);

    match result {
        Ok((after, symbols)) => {
            let count = symbols.len() as i32;
            Some((QuantityExpr::Fixed { value: count }, after))
        }
        Err(_) => None,
    }
}

/// Parse a count prefix for mana amounts: "X ", "x ", or an English/digit number.
///
/// Uses nom combinators for the "X"/"x" prefix matching, falling back to
/// `oracle_util::parse_number` for English words and digits.
pub(super) fn parse_mana_count_prefix(text: &str) -> Option<(QuantityExpr, &str)> {
    let trimmed = text.trim_start();
    let lower = trimmed.to_lowercase();

    // Try "x " via nom (case-insensitive via lowercase)
    if let Some((_, rest)) = nom_on_lower(trimmed, &lower, |i| value((), tag("x ")).parse(i)) {
        return Some((
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            rest.trim_start(),
        ));
    }

    let (count, rest) = parse_number(trimmed)?;
    Some((
        QuantityExpr::Fixed {
            value: count as i32,
        },
        rest,
    ))
}

pub(super) fn apply_where_x_count_expression(
    count: QuantityExpr,
    where_x_expression: Option<&str>,
) -> QuantityExpr {
    match (&count, where_x_expression) {
        (
            QuantityExpr::Ref {
                qty: QuantityRef::Variable { ref name },
            },
            Some(expression),
        ) if name.eq_ignore_ascii_case("X") => {
            crate::parser::oracle_quantity::parse_cda_quantity(expression).unwrap_or_else(|| {
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: expression.to_string(),
                    },
                }
            })
        }
        _ => count,
    }
}

/// Parse a set of mana color symbols separated by conjunctions.
///
/// Uses nom combinators for separator matching ("and/or", "or", "and", ",", "/"),
/// delegating color symbol extraction to `parse_mana_color_symbol`.
pub(super) fn parse_mana_color_set(text: &str) -> Option<Vec<ManaColor>> {
    let mut rest = text.trim().trim_end_matches(['.', '"']).trim();
    if rest.is_empty() {
        return None;
    }

    let mut colors = Vec::new();
    loop {
        let (parsed, after_symbol) = parse_mana_color_symbol(rest)?;
        for color in parsed {
            if !colors.contains(&color) {
                colors.push(color);
            }
        }

        let next = after_symbol.trim_start();
        if next.is_empty() {
            break;
        }

        // Use nom for separator matching
        let next_lower = next.to_lowercase();
        if let Some((_, after_sep)) = nom_on_lower(next, &next_lower, |i| {
            alt((
                value((), tag("and/or ")),
                value((), tag("or ")),
                value((), tag("and ")),
            ))
            .parse(i)
        }) {
            rest = after_sep.trim_start();
            continue;
        }

        // Comma-separated: ",[ and/or | or | and ] ..."
        if let Some((_, after_comma)) =
            nom_on_lower(next, &next_lower, |i| value((), tag(",")).parse(i))
        {
            let stripped = after_comma.trim_start();
            let stripped_lower = stripped.to_lowercase();
            if let Some((_, after_conj)) = nom_on_lower(stripped, &stripped_lower, |i| {
                alt((
                    value((), tag("and/or ")),
                    value((), tag("or ")),
                    value((), tag("and ")),
                ))
                .parse(i)
            }) {
                rest = after_conj.trim_start();
                continue;
            }
            rest = stripped;
            continue;
        }

        // Slash separator
        if let Some((_, after_slash)) =
            nom_on_lower(next, &next_lower, |i| value((), tag("/")).parse(i))
        {
            rest = after_slash.trim_start();
            continue;
        }

        return None;
    }

    if colors.is_empty() {
        None
    } else {
        Some(colors)
    }
}

/// Parse a single mana color symbol like `{W}`, `{U/B}`, returning the color(s)
/// and the remaining text after the closing brace.
///
/// Delegates brace-delimited extraction to `nom_primitives::parse_mana_symbol`
/// for single-color symbols, falling back to manual `/`-split parsing for
/// hybrid color symbols like `{W/U}` which need multi-color extraction.
pub(super) fn parse_mana_color_symbol(text: &str) -> Option<(Vec<ManaColor>, &str)> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let end = trimmed.find('}')?;
    let symbol = &trimmed[1..end];
    let colors = parse_mana_color_symbol_set(symbol)?;
    Some((colors, &trimmed[end + 1..]))
}

pub(super) fn parse_mana_color_symbol_set(symbol: &str) -> Option<Vec<ManaColor>> {
    fn parse_single(code: &str) -> Option<ManaColor> {
        match code {
            "W" => Some(ManaColor::White),
            "U" => Some(ManaColor::Blue),
            "B" => Some(ManaColor::Black),
            "R" => Some(ManaColor::Red),
            "G" => Some(ManaColor::Green),
            _ => None,
        }
    }

    let symbol = symbol.trim().to_ascii_uppercase();
    if let Some(color) = parse_single(&symbol) {
        return Some(vec![color]);
    }

    let mut colors = Vec::new();
    for part in symbol.split('/') {
        let color = parse_single(part.trim())?;
        if !colors.contains(&color) {
            colors.push(color);
        }
    }

    if colors.is_empty() {
        None
    } else {
        Some(colors)
    }
}

/// Scan for mana production type at word boundaries using nom combinators.
fn scan_mana_production_type(
    text: &str,
    count: QuantityExpr,
    contribution: ManaContribution,
) -> Option<ManaProduction> {
    use nom_language::error::VerboseError;
    crate::parser::oracle_nom::primitives::scan_at_word_boundaries(text, |input| {
        alt((
            // CR 106.7: "mana of any color that a land an opponent controls could produce"
            // must be checked before the shorter "mana of any color" to avoid partial match.
            value(
                ManaProduction::OpponentLandColors {
                    count: count.clone(),
                },
                alt((
                    tag::<_, _, VerboseError<&str>>(
                        "mana of any one color that a land an opponent controls could produce",
                    ),
                    tag("mana of any color that a land an opponent controls could produce"),
                )),
            ),
            // CR 605.1a + CR 406.1 + CR 610.3: "one mana of any of the exiled
            // cards' colors" / "mana of any color among the exiled cards"
            // (Pit of Offerings). Must precede the shorter "mana of any (one)
            // color" arm below so the longer phrase wins. The leading "one " is
            // stripped by `parse_mana_count_prefix` upstream, so the scanner
            // only needs to recognize the post-count tail.
            value(
                ManaProduction::ChoiceAmongExiledColors {
                    source: LinkedExileScope::ThisObject,
                },
                alt((
                    tag::<_, _, VerboseError<&str>>("mana of any of the exiled cards' colors"),
                    tag("mana of any of the exiled cards’ colors"),
                    tag("mana of any of the exiled card's colors"),
                    tag("mana of any of the exiled card’s colors"),
                    tag("mana of any color among the exiled cards"),
                )),
            ),
            value(
                ManaProduction::AnyOneColor {
                    count: count.clone(),
                    color_options: all_mana_colors(),
                    contribution,
                },
                alt((tag("mana of any one color"), tag("mana of any color"))),
            ),
            value(
                ManaProduction::AnyCombination {
                    count: count.clone(),
                    color_options: all_mana_colors(),
                },
                tag("mana in any combination of colors"),
            ),
            value(
                ManaProduction::ChosenColor {
                    count: count.clone(),
                    contribution,
                },
                alt((tag("mana of the chosen color"), tag("mana of that color"))),
            ),
        ))
        .parse(input)
    })
}

pub(super) fn all_mana_colors() -> Vec<ManaColor> {
    vec![
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ]
}

/// Parse a "Spend this mana only to cast..." clause into a `ManaSpendRestriction`.
/// Parse a "Spend this mana only to cast..." clause into a restriction and optional spell grants.
///
/// CR 106.6: Some abilities that produce mana have an additional effect on the spell
/// the mana is spent on (e.g., "that spell can't be countered").
///
/// Uses nom combinators for prefix matching: "spend this mana only", "to activate
/// abilities", "on costs that include", "to cast".
///
/// Handles patterns like:
/// - "spend this mana only to cast creature spells" -> `SpellType("Creature")`
/// - "spend this mana only to cast a creature spell of the chosen type" -> `ChosenCreatureType`
/// - "spend this mana only to activate abilities" -> `ActivateOnly`
///
/// Returns `(restriction, grants)` where grants are properties conferred to the spell.
pub(crate) fn parse_mana_spend_restriction(
    lower: &str,
) -> Option<(ManaSpendRestriction, Vec<ManaSpellGrant>)> {
    let (_, base) = nom_on_lower(lower, lower, |i| {
        value((), tag("spend this mana only ")).parse(i)
    })?;
    let base = base.trim_end_matches(['.', '"']);
    let base_lower = base.to_lowercase();

    // "spend this mana only to activate abilities" -- activation-only
    if nom_on_lower(base, &base_lower, |i| {
        value((), tag("to activate abilities")).parse(i)
    })
    .is_some()
    {
        return Some((ManaSpendRestriction::ActivateOnly, vec![]));
    }

    // "spend this mana only on costs that include" -- X-cost restriction
    if nom_on_lower(base, &base_lower, |i| {
        value((), tag("on costs that include")).parse(i)
    })
    .is_some()
    {
        return Some((ManaSpendRestriction::XCostOnly, vec![]));
    }

    let (_, rest) = nom_on_lower(base, &base_lower, |i| value((), tag("to cast ")).parse(i))?;
    let rest = rest.trim();

    // CR 106.6: Extract "and that spell can't be countered" grant before parsing restriction.
    let (rest, grants) = extract_spell_grants(rest);
    let rest = rest.trim();
    if matches!(rest, "spells with flashback" | "a spell with flashback") {
        return Some((
            ManaSpendRestriction::SpellWithKeywordKind(KeywordKind::Flashback),
            grants,
        ));
    }

    if matches!(
        rest,
        "spells with flashback from a graveyard" | "a spell with flashback from a graveyard"
    ) {
        return Some((
            ManaSpendRestriction::SpellWithKeywordKindFromZone {
                kind: KeywordKind::Flashback,
                zone: crate::types::zones::Zone::Graveyard,
            },
            grants,
        ));
    }

    if matches!(
        rest,
        "spells with flashback from your graveyard" | "a spell with flashback from your graveyard"
    ) {
        return Some((
            ManaSpendRestriction::SpellWithKeywordKindFromZone {
                kind: KeywordKind::Flashback,
                zone: crate::types::zones::Zone::Graveyard,
            },
            grants,
        ));
    }

    // CR 106.12: Check for "or activate abilities of [type]" suffix.
    // If present, emit a combined SpellTypeOrAbilityActivation restriction.
    let has_ability_activation = rest.contains(" or activate abilities");
    let spell_part = rest
        .split(" or activate abilities")
        .next()
        .unwrap_or(rest)
        .trim();

    if spell_part.contains("of the chosen type") {
        return Some((ManaSpendRestriction::ChosenCreatureType, grants));
    }

    // "creature spells" / "a creature spell" / "artifact spells" etc.
    let spell_part_lower = spell_part.to_lowercase();
    let spell_part = nom_on_lower(spell_part, &spell_part_lower, nom_primitives::parse_article)
        .map(|(_, rest)| rest)
        .unwrap_or(spell_part);

    // Handle compound type: "instant or sorcery spells" -> "Instant or Sorcery"
    // Check for "[type] or [type] spell(s)" pattern
    if let Some((first, second_with_spells)) = spell_part.split_once(" or ") {
        let second = second_with_spells
            .strip_suffix(" spells")
            .or_else(|| second_with_spells.strip_suffix(" spell"))
            .unwrap_or(second_with_spells);
        // Only treat as compound if second part is a single type word
        if !second.contains(' ') || second.ends_with("creature") {
            let compound = format!(
                "{} or {}",
                super::capitalize(first),
                super::capitalize(second)
            );
            if has_ability_activation {
                return Some((
                    ManaSpendRestriction::SpellTypeOrAbilityActivation(compound),
                    grants,
                ));
            }
            return Some((ManaSpendRestriction::SpellType(compound), grants));
        }
    }

    let type_word = spell_part.split_whitespace().next()?;
    let type_name = super::capitalize(type_word);

    if has_ability_activation {
        Some((
            ManaSpendRestriction::SpellTypeOrAbilityActivation(type_name),
            grants,
        ))
    } else {
        Some((ManaSpendRestriction::SpellType(type_name), grants))
    }
}

/// CR 106.6: Parse a standalone "that spell can't be countered" clause.
///
/// Used when comma-splitting separates the grant from the restriction text,
/// producing a standalone clause like "that spell can't be countered".
pub(super) fn parse_mana_spell_grant(lower: &str) -> Option<Vec<ManaSpellGrant>> {
    let trimmed = lower.trim().trim_end_matches('.');
    // Use nom tag for matching
    if value::<_, _, nom_language::error::VerboseError<&str>, _>(
        (),
        tag("that spell can't be countered"),
    )
    .parse(trimmed)
    .is_ok()
    {
        return Some(vec![ManaSpellGrant::CantBeCountered]);
    }
    None
}

/// CR 106.6: Extract trailing spell grants from a mana restriction clause.
///
/// Recognizes patterns like:
/// - ", and that spell can't be countered"
/// - ", and that spell can't be countered."
///
/// Returns the text before the grant clause and the list of grants found.
/// Uses suffix stripping (structural, not dispatch) since the grant clause
/// is always a fixed trailing phrase.
fn extract_spell_grants(text: &str) -> (&str, Vec<ManaSpellGrant>) {
    let lower = text.to_lowercase();
    // structural: not dispatch — suffix stripping of fixed trailing clause
    for suffix in [
        ", and that spell can't be countered.",
        ", and that spell can't be countered",
    ] {
        if let Some(before) = lower.strip_suffix(suffix) {
            let before_len = before.len();
            return (
                text[..before_len].trim(),
                vec![ManaSpellGrant::CantBeCountered],
            );
        }
    }
    (text, vec![])
}

/// CR 605.3b + CR 106.1a: Parse a filter-land-style combinations clause.
///
/// Recognises a list of two or more pure-color mana-symbol combinations
/// joined by `, ` / `, or ` / ` or ` (case-insensitive). Each combination
/// must be a run of at least one pure-color mana symbol (`{W}`, `{U}`, etc. —
/// no hybrid, phyrexian, colorless, generic, `{X}`, or snow symbols).
///
/// Returns `Some(Vec<Vec<ManaColor>>)` with at least two combinations on a
/// successful parse; `None` when the clause doesn't match (e.g., single
/// sequence, presence of non-pure-color symbols, trailing text).
///
/// Delegates symbol extraction to `parse_pure_color_symbol` (nom combinator,
/// word-boundary safe via `char('{')` / `char('}')` delimiters) rather than
/// the legacy `parse_mana_color_symbol` to keep parsing consistent with
/// `oracle_nom` primitives.
fn parse_mana_combinations_clause(clause: &str) -> Option<Vec<Vec<ManaColor>>> {
    let trimmed = clause.trim().trim_end_matches(['.', '"']).trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_lowercase();

    let (options, rest) = nom_on_lower(trimmed, &lower, parse_combinations_list)?;
    // The clause must be fully consumed (no trailing text).
    if !rest.trim().is_empty() {
        return None;
    }
    if options.len() < 2 {
        return None;
    }
    Some(options)
}

/// Parse a sequence of pure-color combinations separated by
/// `, or ` / `, ` / ` or ` (in longest-match-first order). Runs on the
/// lowercase copy produced by `nom_on_lower`, so all `tag`s are lowercase.
fn parse_combinations_list(
    i: &str,
) -> crate::parser::oracle_nom::error::OracleResult<'_, Vec<Vec<ManaColor>>> {
    let (mut rest, first) = parse_single_combination(i)?;
    let mut out = vec![first];
    while let Ok((after_sep, _)) = parse_combination_separator(rest) {
        match parse_single_combination(after_sep) {
            Ok((after_combo, combo)) => {
                out.push(combo);
                rest = after_combo;
            }
            Err(_) => break,
        }
    }
    Ok((rest, out))
}

fn parse_combination_separator(i: &str) -> crate::parser::oracle_nom::error::OracleResult<'_, ()> {
    value((), alt((tag(", or "), tag(", "), tag(" or ")))).parse(i)
}

fn parse_single_combination(
    i: &str,
) -> crate::parser::oracle_nom::error::OracleResult<'_, Vec<ManaColor>> {
    many1(parse_pure_color_symbol).parse(i)
}

/// Parse a single pure-color mana symbol (`{w}`/`{u}`/`{b}`/`{r}`/`{g}`) from
/// lowercase text. Rejects hybrid, phyrexian, colorless, generic, `{X}`, and
/// snow — those have no place in a filter-land combination.
fn parse_pure_color_symbol(
    i: &str,
) -> crate::parser::oracle_nom::error::OracleResult<'_, ManaColor> {
    delimited(
        char('{'),
        alt((
            value(ManaColor::White, tag("w")),
            value(ManaColor::Blue, tag("u")),
            value(ManaColor::Black, tag("b")),
            value(ManaColor::Red, tag("r")),
            value(ManaColor::Green, tag("g")),
        )),
        char('}'),
    )
    .parse(i)
}

/// CR 106.1 / CR 106.3: Parse "an amount of {color} equal to [quantity]"
/// e.g. "an amount of {G} equal to ~'s power" -> AnyOneColor { count: SelfPower, [Green] }
fn try_parse_amount_equal_to(clause: &str, contribution: ManaContribution) -> Option<Effect> {
    let clause_lower = clause.to_lowercase();
    let (_, rest) = nom_on_lower(clause, &clause_lower, |i| {
        value((), tag("an amount of ")).parse(i)
    })?;
    let rest = rest.trim_start();

    // CR 106.1: Colorless-mana production ({C}). `parse_mana_production`
    // only recognizes the five colored symbols (W/U/B/R/G) and returns
    // `None` for `{C}`, so route colorless separately to
    // `ManaProduction::Colorless` before falling through to the colored path.
    if let Some(after_c) = rest.strip_prefix("{C}") {
        let after_c = after_c.trim();
        let after_c_lower = after_c.to_lowercase();
        let (_, quantity_text) = nom_on_lower(after_c, &after_c_lower, |i| {
            value((), tag("equal to ")).parse(i)
        })?;
        let quantity_text = quantity_text.trim().trim_end_matches(['.', '"']);
        // CR 601.2h + CR 603.7c: "the amount of mana spent to cast that spell"
        // resolves via `parse_event_context_quantity` to
        // `ManaSpentOnTriggeringSpell`; fall back to `parse_cda_quantity` for
        // non-event quantities (e.g. "~'s power").
        let count = parse_event_context_quantity(quantity_text)
            .or_else(|| parse_cda_quantity(quantity_text))?;
        return Some(Effect::Mana {
            produced: ManaProduction::Colorless { count },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        });
    }

    // Parse the mana color symbol(s): "{G}", "{R}", etc.
    let (colors, after_color) = parse_mana_production(rest)?;
    if colors.is_empty() {
        return None;
    }

    // Expect "equal to [quantity]"
    let after_color = after_color.trim();
    let after_color_lower = after_color.to_lowercase();
    let (_, quantity_text) = nom_on_lower(after_color, &after_color_lower, |i| {
        value((), tag("equal to ")).parse(i)
    })?;
    let quantity_text = quantity_text.trim().trim_end_matches(['.', '"']);

    let count = parse_event_context_quantity(quantity_text)
        .or_else(|| parse_cda_quantity(quantity_text))?;

    let color_options: Vec<ManaColor> = colors;
    Some(Effect::Mana {
        produced: ManaProduction::AnyOneColor {
            count,
            color_options,
            contribution,
        },
        restrictions: vec![],
        grants: vec![],
        expiry: None,
        target: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_combinations(oracle: &str) -> Option<Vec<Vec<ManaColor>>> {
        match try_parse_add_mana_effect(oracle) {
            Some(Effect::Mana {
                produced: ManaProduction::ChoiceAmongCombinations { options },
                ..
            }) => Some(options),
            _ => None,
        }
    }

    #[test]
    fn sunken_ruins_pattern_parses_as_combinations() {
        // CR 605.3b: Shadowmoor/Eventide filter land shape.
        let options = extract_combinations("Add {U}{U}, {U}{B}, or {B}{B}")
            .expect("should parse filter-land pattern");
        assert_eq!(
            options,
            vec![
                vec![ManaColor::Blue, ManaColor::Blue],
                vec![ManaColor::Blue, ManaColor::Black],
                vec![ManaColor::Black, ManaColor::Black],
            ]
        );
    }

    #[test]
    fn all_ten_filter_land_color_pairs_parse() {
        // Exhaustively cover the Shadowmoor/Eventide cycle.
        let pairs: &[(&str, ManaColor, ManaColor)] = &[
            (
                "{W}{W}, {W}{U}, or {U}{U}",
                ManaColor::White,
                ManaColor::Blue,
            ),
            (
                "{W}{W}, {W}{B}, or {B}{B}",
                ManaColor::White,
                ManaColor::Black,
            ),
            (
                "{U}{U}, {U}{B}, or {B}{B}",
                ManaColor::Blue,
                ManaColor::Black,
            ),
            ("{U}{U}, {U}{R}, or {R}{R}", ManaColor::Blue, ManaColor::Red),
            (
                "{B}{B}, {B}{R}, or {R}{R}",
                ManaColor::Black,
                ManaColor::Red,
            ),
            (
                "{B}{B}, {B}{G}, or {G}{G}",
                ManaColor::Black,
                ManaColor::Green,
            ),
            (
                "{R}{R}, {R}{G}, or {G}{G}",
                ManaColor::Red,
                ManaColor::Green,
            ),
            (
                "{R}{R}, {R}{W}, or {W}{W}",
                ManaColor::Red,
                ManaColor::White,
            ),
            (
                "{G}{G}, {G}{W}, or {W}{W}",
                ManaColor::Green,
                ManaColor::White,
            ),
            (
                "{G}{G}, {G}{U}, or {U}{U}",
                ManaColor::Green,
                ManaColor::Blue,
            ),
        ];
        for (text, a, b) in pairs {
            let oracle = format!("Add {text}");
            let options = extract_combinations(&oracle)
                .unwrap_or_else(|| panic!("expected combinations for {oracle}"));
            assert_eq!(
                options,
                vec![vec![*a, *a], vec![*a, *b], vec![*b, *b]],
                "combination options mismatch for {oracle}",
            );
        }
    }

    #[test]
    fn single_mana_symbol_sequence_is_not_combinations() {
        // A plain `Add {G}{G}` is `Fixed`, not `ChoiceAmongCombinations` —
        // parse_mana_production_clause catches it first.
        assert!(extract_combinations("Add {G}{G}").is_none());
    }

    #[test]
    fn hybrid_symbols_reject_combinations_parse() {
        // Hybrid `{W/U}` is not a pure-color symbol — must not parse.
        assert!(extract_combinations("Add {W/U}{W}, {W}{U}, or {U}{U}").is_none());
    }

    #[test]
    fn filter_land_trailing_text_rejects_parse() {
        // The clause must be fully consumed — trailing words indicate a
        // different shape that must fall through to other arms.
        assert!(extract_combinations("Add {U}{U}, {U}{B}, or {B}{B} to your mana pool").is_none());
    }

    #[test]
    fn trailing_period_is_tolerated() {
        assert!(extract_combinations("Add {U}{U}, {U}{B}, or {B}{B}.").is_some());
    }

    /// CR 106.7 + CR 106.1b: Reflecting Pool — "any type that a land you
    /// control could produce" must parse to `AnyTypeProduceableBy` with a
    /// `ControllerRef::You`-scoped land filter. This is the building-block
    /// test (one parser arm covering the entire 5-card class).
    #[test]
    fn reflecting_pool_parses_any_type_you_control() {
        use crate::types::ability::{ControllerRef, TargetFilter};
        let effect = try_parse_add_mana_effect(
            "Add one mana of any type that a land you control could produce",
        )
        .expect("Reflecting Pool clause must parse");
        let Effect::Mana { produced, .. } = effect else {
            panic!("expected Effect::Mana, got something else");
        };
        let ManaProduction::AnyTypeProduceableBy { count, land_filter } = produced else {
            panic!("expected AnyTypeProduceableBy, got {produced:?}");
        };
        assert_eq!(count, QuantityExpr::Fixed { value: 1 });
        let TargetFilter::Typed(typed) = land_filter else {
            panic!("expected Typed land filter, got {land_filter:?}");
        };
        assert_eq!(typed.controller, Some(ControllerRef::You));
    }

    /// CR 106.7: Future opponent-scoped "type" printings must dispatch via
    /// the same primitive — this guards the parser's class generality even
    /// though no current card prints this exact phrase.
    #[test]
    fn any_type_opponent_controls_routes_to_opponent_scope() {
        use crate::types::ability::{ControllerRef, TargetFilter};
        let effect = try_parse_add_mana_effect(
            "Add one mana of any type that a land an opponent controls could produce",
        )
        .expect("opponent-scoped type clause must parse");
        let Effect::Mana { produced, .. } = effect else {
            panic!("expected Effect::Mana");
        };
        let ManaProduction::AnyTypeProduceableBy { land_filter, .. } = produced else {
            panic!("expected AnyTypeProduceableBy, got {produced:?}");
        };
        let TargetFilter::Typed(typed) = land_filter else {
            panic!("expected Typed land filter");
        };
        assert_eq!(typed.controller, Some(ControllerRef::Opponent));
    }

    /// CR 106.1 + CR 601.2h + CR 603.7c: "add an amount of {C} equal to the
    /// amount of mana spent to cast that spell" — Mana Sculpt's sub_ability.
    /// The `{C}` colorless branch routes to `ManaProduction::Colorless`
    /// (since `parse_mana_production` only recognizes W/U/B/R/G and would
    /// otherwise silently fail), and the quantity clause routes through
    /// `parse_event_context_quantity` to `ManaSpentOnTriggeringSpell`.
    #[test]
    fn amount_equal_to_mana_spent_on_triggering_spell() {
        let effect = try_parse_add_mana_effect(
            "Add an amount of {C} equal to the amount of mana spent to cast that spell",
        )
        .expect("Mana Sculpt amount clause must parse");
        let Effect::Mana { produced, .. } = effect else {
            panic!("expected Effect::Mana, got something else");
        };
        match produced {
            ManaProduction::Colorless { count } => {
                assert_eq!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ManaSpentOnTriggeringSpell
                    },
                    "count must reference mana spent on the triggering spell"
                );
            }
            other => panic!("expected Colorless mana production, got {other:?}"),
        }
    }

    /// CR 106.1 + CR 115.1 + CR 115.7: Jeska's Will mode 1 — "Add {R} for each
    /// card in target opponent's hand". The for-each clause references a
    /// player target, so the resulting `Effect::Mana` carries:
    /// 1. `produced: AnyOneColor { count: TargetZoneCardCount{Hand}, [Red] }`,
    /// 2. `target: Some(TypedFilter::default().controller(Opponent))` so
    ///    `collect_target_slots` surfaces a player target slot at cast time.
    #[test]
    fn jeskas_will_for_each_card_in_target_opponents_hand() {
        use crate::types::ability::{ControllerRef, TargetFilter, ZoneRef};
        let effect = try_parse_add_mana_effect("Add {R} for each card in target opponent's hand.")
            .expect("Jeska's Will mode 1 must parse");
        let Effect::Mana {
            produced, target, ..
        } = effect
        else {
            panic!("expected Effect::Mana");
        };
        match produced {
            ManaProduction::AnyOneColor {
                count,
                color_options,
                ..
            } => {
                assert_eq!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::TargetZoneCardCount {
                            zone: ZoneRef::Hand
                        }
                    },
                );
                assert_eq!(color_options, vec![ManaColor::Red]);
            }
            other => panic!("expected AnyOneColor, got {other:?}"),
        }
        let target = target.expect("target opponent should surface a player target filter");
        let TargetFilter::Typed(typed) = target else {
            panic!("expected Typed filter for target opponent, got {target:?}");
        };
        assert_eq!(typed.controller, Some(ControllerRef::Opponent));
    }

    /// CR 106.1 + CR 115.1: "Add {U} for each card in target player's hand"
    /// — generalized printing variant. Routes to `TargetFilter::Player`.
    #[test]
    fn add_mana_for_each_card_in_target_players_hand() {
        use crate::types::ability::{TargetFilter, ZoneRef};
        let effect = try_parse_add_mana_effect("Add {U} for each card in target player's hand.")
            .expect("target-player variant must parse");
        let Effect::Mana {
            produced, target, ..
        } = effect
        else {
            panic!("expected Effect::Mana");
        };
        let ManaProduction::AnyOneColor { count, .. } = produced else {
            panic!("expected AnyOneColor");
        };
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::TargetZoneCardCount {
                    zone: ZoneRef::Hand
                }
            },
        );
        assert_eq!(target, Some(TargetFilter::Player));
    }

    /// Cabal Coffers — "Add {B} for each Swamp you control" — must continue to
    /// route through `ObjectCount` (no target field). Regression for the
    /// non-target arm of `parse_mana_production_clause`.
    #[test]
    fn cabal_coffers_for_each_controlled_swamp_no_target() {
        let effect = try_parse_add_mana_effect("Add {B} for each Swamp you control.")
            .expect("Cabal Coffers must parse");
        let Effect::Mana {
            produced, target, ..
        } = effect
        else {
            panic!("expected Effect::Mana");
        };
        match produced {
            ManaProduction::AnyOneColor { count, .. } => match count {
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. },
                } => {}
                other => panic!("expected ObjectCount, got {other:?}"),
            },
            other => panic!("expected AnyOneColor, got {other:?}"),
        }
        assert!(
            target.is_none(),
            "Cabal Coffers does not target a player; target must be None",
        );
    }

    /// CR 106.1 + CR 609.3 + CR 122.1: Coalition Relic — "add one mana of any
    /// color for each charge counter removed this way". This is the AnyOneColor
    /// equivalent of the fixed-color "Add {R} for each X" pattern. Class also
    /// includes the Storage Counter cycle (Saprazzan Cove, Dwarven Hold, etc.).
    /// Without this the bare "any color" branch produces `count: Fixed(1)` and
    /// silently drops the for-each tail.
    #[test]
    fn coalition_relic_any_color_for_each_charge_counter_removed_this_way() {
        let effect = try_parse_add_mana_effect(
            "Add one mana of any color for each charge counter removed this way.",
        )
        .expect("any-color + for-each must parse");
        let Effect::Mana {
            produced, target, ..
        } = effect
        else {
            panic!("expected Effect::Mana, got {effect:?}");
        };
        match produced {
            ManaProduction::AnyOneColor {
                count,
                color_options,
                ..
            } => {
                assert_eq!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::PreviousEffectAmount
                    },
                    "for-each tail must dispatch to PreviousEffectAmount"
                );
                assert_eq!(
                    color_options.len(),
                    5,
                    "any-color must offer all five colors"
                );
            }
            other => panic!("expected AnyOneColor, got {other:?}"),
        }
        assert!(
            target.is_none(),
            "for-each-counters-removed has no player target",
        );
    }

    /// CR 106.1 + CR 115.1 + CR 115.7: Symmetry test for the new AnyOneColor
    /// for-each branch — when the for-each clause references a player target,
    /// the parsed `Effect::Mana::target` must surface that filter so the
    /// surrounding ability attaches a player target slot. Mirrors the
    /// fixed-color analogue (`add_mana_for_each_card_in_target_players_hand`)
    /// for "any color".
    #[test]
    fn add_any_color_mana_for_each_card_in_target_opponents_hand() {
        use crate::types::ability::{ControllerRef, TargetFilter, TypedFilter};
        let effect = try_parse_add_mana_effect(
            "Add one mana of any color for each card in target opponent's hand.",
        )
        .expect("any-color + for-each + target-opponent must parse");
        let Effect::Mana {
            produced, target, ..
        } = effect
        else {
            panic!("expected Effect::Mana, got {effect:?}");
        };
        match produced {
            ManaProduction::AnyOneColor { color_options, .. } => {
                assert_eq!(
                    color_options.len(),
                    5,
                    "any-color must offer all five colors"
                );
            }
            other => panic!("expected AnyOneColor, got {other:?}"),
        }
        // CR 115.1: target must be the opponent player filter so the engine
        // surfaces a player target slot at cast/trigger time.
        let target = target.expect("target opponent must surface a player target filter");
        let TargetFilter::Typed(typed) = target else {
            panic!("expected TargetFilter::Typed, got {target:?}");
        };
        assert_eq!(typed.controller, Some(ControllerRef::Opponent));
        // Sanity: this is a player target (no type filter).
        assert_eq!(
            typed,
            TypedFilter::default().controller(ControllerRef::Opponent)
        );
    }
}
