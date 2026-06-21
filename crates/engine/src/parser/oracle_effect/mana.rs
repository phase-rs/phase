use crate::parser::oracle_nom::error::{oracle_err, OracleError};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::{anychar, char};
use nom::combinator::{all_consuming, map, map_opt, not, opt, recognize, rest as nom_rest, value};
use nom::multi::{many1, separated_list1};
use nom::sequence::{delimited, preceded, separated_pair, terminated};
use nom::Parser;

use crate::parser::oracle_nom::error::OracleResult;
use crate::parser::oracle_nom::primitives as nom_primitives;
use crate::types::ability::{
    AbilityKind, AbilityTag, Comparator, Effect, LinkedExileScope, ManaContribution,
    ManaProduction, ManaSpendRestriction, QuantityExpr, QuantityRef,
};
use crate::types::keywords::KeywordKind;
use crate::types::mana::{
    AbilityActivationScope, ManaColor, ManaRestriction, ManaSpellGrant, SpellCostCriterion,
    ZoneSpend, ZoneSpendPolarity,
};
use crate::types::zones::Zone;

use super::super::oracle_keyword::parse_keyword_from_oracle;
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

/// CR 106.1 + CR 109.1: Parse the permanent filter tail of
/// "mana of any color among [type-phrase]" (Mox Amber class).
fn try_parse_any_color_among_permanents_filter(
    after_color: &str,
    after_lower: &str,
) -> Option<TargetFilter> {
    let trimmed_lower = after_lower.trim().trim_end_matches('.').trim();
    let (rest, _) = tag::<_, _, OracleError<'_>>("among ")
        .parse(trimmed_lower)
        .ok()?;
    let type_lower = rest.trim();
    if type_lower.is_empty() {
        return None;
    }
    let prefix_len = trimmed_lower.len() - rest.len();
    let trimmed_original = after_color.trim().trim_end_matches('.').trim();
    let type_text = trimmed_original.get(prefix_len..)?.trim();
    let (filter, remainder) = parse_type_phrase(type_text);
    if !remainder.trim().is_empty() || matches!(filter, TargetFilter::Any) {
        return None;
    }
    Some(filter)
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
    let (rest, _) = tag::<_, _, OracleError<'_>>("for each color among ")
        .parse(lower_trimmed)
        .ok()?;
    // Boundary: the type phrase runs until ", add one mana of that color".
    let (_, type_text_lower) = take_until::<_, _, OracleError<'_>>(", add one mana of that color")
        .parse(rest)
        .ok()?;
    // CR 702.167c + CR 105.1: "For each color among the exiled cards used to craft
    // this creature, add one mana of that color" (Sunbird Effigy) — the iteration
    // source is the craft-material linked-exile pool, not a battlefield type
    // phrase. Tried first so the craft noun phrase wins over `parse_type_phrase`.
    if let Ok((craft_rest, filter)) =
        crate::parser::oracle_nom::quantity::parse_craft_materials_filter(type_text_lower.trim())
    {
        if craft_rest.trim().is_empty() {
            return Some(Effect::Mana {
                produced: ManaProduction::DistinctColorsAmongPermanents { filter },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            });
        }
    }
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

/// CR 505.1 + CR 106.4: Recognize a leading player-subject before the mana
/// verb so subject-led mana clauses ("the active player adds {C}{C} …", "that
/// player adds {G}") reach the mana dispatcher. Returns the recipient
/// `TargetFilter` and the remainder beginning at the mana symbols, with the
/// subject's "adds" verb normalized away.
///
/// "the active player" is the active player whose phase began (CR 505.1) — for
/// the Phase triggers that carry these clauses (Belbe, Corrupted Observer) the
/// active player is the trigger's scoped player, so the recipient resolves via
/// `TargetFilter::ScopedPlayer`. "that player" is the same anaphor.
fn strip_mana_subject_prefix(text: &str) -> Option<(TargetFilter, &str)> {
    let lower = text.to_lowercase();
    nom_on_lower(text, &lower, |i| {
        value(
            TargetFilter::ScopedPlayer,
            (
                alt((tag("the active player "), tag("that player "))),
                tag("adds "),
            ),
        )
        .parse(i)
    })
}

pub(super) fn try_parse_add_mana_effect(text: &str) -> Option<Effect> {
    // CR 505.1 + CR 106.4: A subject-led mana clause routes the produced mana
    // to the named player. Strip the subject, parse the bare "add …" clause,
    // and stamp the recipient onto the resulting `Effect::Mana.target`.
    if let Some((recipient, rest)) = strip_mana_subject_prefix(text.trim()) {
        let synthetic = format!("add {rest}");
        let mut effect = try_parse_add_mana_effect(&synthetic)?;
        if let Effect::Mana { target, .. } = &mut effect {
            if target.is_none() {
                *target = Some(recipient);
            }
        }
        return Some(effect);
    }
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

    // CR 106.1: Count-prefixed disjunctive color choice — `"X {C1} or X {C2}"`
    // (Brigid, Doun's Mind). The combinator declines when there is no leading
    // count token, so count-free `"{G}{G} or {W}{W}"` text still routes to
    // `parse_mana_combinations_clause` below. Tried before
    // `parse_mana_production_clause` so the where-X count is resolved here,
    // co-located with `apply_where_x_count_expression`.
    if let Some((count, color_options)) = parse_repeated_count_color_choice(clause) {
        let (count, target) = apply_where_x_count_expression(count, where_x_expression.as_deref());
        return Some(Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count,
                color_options,
                contribution,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target,
        });
    }

    // CR 106.1 + CR 106.3: disjunctive color choice scaled by a "for each"
    // count -- "{C1} or {C2} [...] for each [clause]" (Culling Ritual: "Add
    // {B} or {G} for each permanent destroyed this way"). Each unit is chosen
    // independently from the color set, so this is AnyCombination, not
    // AnyOneColor. The single-color "{C} for each X" form is handled by
    // parse_mana_production_clause; this branch covers the >1-color set, which
    // parse_mana_color_set rejects today because of the trailing "for each".
    if let Ok((for_each_rest, before)) = terminated(
        take_until::<_, _, OracleError<'_>>(" for each "),
        tag::<_, _, OracleError<'_>>(" for each "),
    )
    .parse(clause)
    {
        if let Some(color_options) = parse_mana_color_set(before.trim()) {
            if color_options.len() > 1 {
                if let Some(qty) =
                    super::super::oracle_quantity::parse_for_each_clause(for_each_rest.trim())
                {
                    return Some(Effect::Mana {
                        produced: ManaProduction::AnyCombination {
                            count: QuantityExpr::Ref { qty },
                            color_options,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: for_each_clause_target_filter(for_each_rest.trim()),
                    });
                }
            }
        }
    }

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
        let (count, where_x_target) =
            apply_where_x_count_expression(count, where_x_expression.as_deref());
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
                target: where_x_target,
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
            } else if let Some(filter) =
                try_parse_any_color_among_permanents_filter(after_color.trim(), &after_lower)
            {
                ManaProduction::AnyOneColorAmongPermanents {
                    count,
                    filter,
                    contribution,
                }
            } else if nom_on_lower(after_color.trim(), &after_lower, |i| {
                value((), tag("among ")).parse(i)
            })
            .is_some()
            {
                return None;
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
                target: mana_target.or(where_x_target),
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
                target: where_x_target,
            });
        }

        // CR 605.3b + CR 106.1a: "N mana of different colors" — produce N mana,
        // each a different color from WUBRG. Materialize every distinct unordered
        // color combination of size N; the existing `ChoiceAmongCombinations`
        // machinery prompts among them (10 options for N=2). Only a fixed N is
        // enumerable into a static option set; X/dynamic counts decline (no such
        // printing exists). Class: Firemind Vessel, Component Pouch, Guild Globe,
        // Interplanar Beacon.
        if nom_on_lower(rest, &rest_lower, |i| {
            value((), tag("mana of different colors")).parse(i)
        })
        .is_some()
        {
            if let QuantityExpr::Fixed { value } = count {
                if value >= 1 {
                    let options = combinations_of_distinct_colors(value as usize);
                    if !options.is_empty() {
                        return Some(Effect::Mana {
                            produced: ManaProduction::ChoiceAmongCombinations { options },
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: where_x_target,
                        });
                    }
                }
            }
            // non-fixed / out-of-range count: fall through to later arms.
        }

        // CR 106.1: "{fixed} or one mana of the chosen color" after a count
        // prefix must retain the fixed-color alternative. Scan `rest` before
        // the bare chosen-color arm so a leading count token does not drop the
        // `{B}`/`{G}` branch (Gate lands with a count-qualified tail).
        if let Some(produced) = scan_mana_production_type(&rest_lower, count.clone(), contribution)
        {
            if matches!(
                produced,
                ManaProduction::ChosenColor {
                    fixed_alternative: Some(_),
                    ..
                }
            ) {
                return Some(Effect::Mana {
                    produced,
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: where_x_target,
                });
            }
        }

        if let Some((_, after_color)) = nom_on_lower(rest, &rest_lower, |i| {
            alt((
                value((), tag("mana of the chosen color")),
                value((), tag("mana of that color")),
            ))
            .parse(i)
        }) {
            let after_lower = after_color.trim().to_lowercase();
            let mut mana_target: Option<TargetFilter> = None;
            let count = if let Some((dynamic_qty, target)) =
                try_parse_any_color_for_each_suffix(after_lower.as_str())
            {
                mana_target = target;
                QuantityExpr::Ref { qty: dynamic_qty }
            } else {
                count
            };
            return Some(Effect::Mana {
                produced: ManaProduction::ChosenColor {
                    count,
                    contribution,
                    fixed_alternative: None,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: mana_target.or(where_x_target),
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
                        target: where_x_target,
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
                    target: where_x_target,
                });
            }
        }
    }

    let clause_lower = clause.to_lowercase();
    let fallback_count = parse_mana_count_prefix(clause)
        .map(|(count, _)| count)
        .unwrap_or(QuantityExpr::Fixed { value: 1 });
    let (fallback_count, fallback_target) =
        apply_where_x_count_expression(fallback_count, where_x_expression.as_deref());

    // Scan for mana production type at word boundaries using nom combinators.
    let produced = scan_mana_production_type(&clause_lower, fallback_count.clone(), contribution)?;
    Some(Effect::Mana {
        produced,
        restrictions: vec![],
        grants: vec![],
        expiry: None,
        target: fallback_target,
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
        nom::character::complete::multispace0::<_, OracleError<'_>>,
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
        // CR 106.1: "{C}{C} for each [filter]" -> dynamic colorless mana count.
        // The literal `{C}` symbol count is a per-iteration multiplier — it
        // must NOT be discarded. `{C} for each X` yields a bare `Ref`;
        // `{C}{C} for each X` yields `Multiply { factor: 2, inner: Ref }`
        // (Belbe, Corrupted Observer adds two colorless per qualifying opponent).
        let remainder_lower = remainder.to_lowercase();
        if let Some((_, for_each_rest)) = nom_on_lower(remainder, &remainder_lower, |i| {
            value((), tag("for each ")).parse(i)
        }) {
            let qty = super::super::oracle_quantity::parse_for_each_clause(for_each_rest)?;
            let target = for_each_clause_target_filter(for_each_rest);
            let count = scale_for_each_count(colorless_count, QuantityExpr::Ref { qty });
            return Some((ManaProduction::Colorless { count }, target));
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

/// CR 106.1: Combine the literal mana-symbol count with a dynamic "for each"
/// quantity. `literal` is the `QuantityExpr::Fixed` symbol count produced by
/// `parse_colorless_mana_production`; `dynamic` is the per-iteration quantity.
/// `N == 1` yields the bare `dynamic` (no redundant `Multiply { factor: 1 }`);
/// `N > 1` wraps it as `Multiply { factor: N, inner: dynamic }`.
fn scale_for_each_count(literal: QuantityExpr, dynamic: QuantityExpr) -> QuantityExpr {
    match literal {
        QuantityExpr::Fixed { value } if value > 1 => QuantityExpr::Multiply {
            factor: value,
            inner: Box::new(dynamic),
        },
        _ => dynamic,
    }
}

pub(super) fn parse_colorless_mana_production(text: &str) -> Option<(QuantityExpr, &str)> {
    let rest = text.trim_start();
    // Nom combinator: count consecutive {C} symbols.
    let result: Result<(&str, Vec<()>), _> = many1(delimited(
        tag::<_, _, OracleError<'_>>("{"),
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

/// Parse a count prefix for mana amounts: "that much", "that many", "X", or
/// an English/digit number.
///
/// Uses nom combinators for the "X"/"x" prefix matching, falling back to
/// `oracle_util::parse_number` for English words and digits.
pub(super) fn parse_mana_count_prefix(text: &str) -> Option<(QuantityExpr, &str)> {
    let trimmed = text.trim_start();
    let lower = trimmed.to_lowercase();

    if let Some((_, rest)) = nom_on_lower(trimmed, &lower, |i| {
        value((), alt((tag("that much "), tag("that many ")))).parse(i)
    }) {
        return Some((
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            rest.trim_start(),
        ));
    }

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
) -> (QuantityExpr, Option<TargetFilter>) {
    match (&count, where_x_expression) {
        (
            QuantityExpr::Ref {
                qty: QuantityRef::Variable { ref name },
            },
            Some(expression),
        ) if name.eq_ignore_ascii_case("X") => {
            if let Some(count) = super::parse_where_x_quantity_expression(expression) {
                return (count, where_x_expression_target_filter(expression));
            }
            (
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: expression.to_string(),
                    },
                },
                None,
            )
        }
        _ => (count, None),
    }
}

/// CR 115.1: Extract target player filters from where-X expressions.
fn where_x_expression_target_filter(expression: &str) -> Option<TargetFilter> {
    let lower = expression.to_ascii_lowercase();
    let clause = tag::<_, _, OracleError<'_>>("the number of ")
        .parse(lower.as_str())
        .map(|(rest, _)| rest)
        .unwrap_or(lower.as_str());
    for_each_clause_target_filter(clause)
}

/// CR 106.1: Recognize a count-prefixed disjunctive color choice of the shape
/// `"<count> {C1} or <count> {C2} [or <count> {Cn} ...]"` — the same count
/// token repeated before each color (Brigid, Doun's Mind:
/// `"Add X {G} or X {W}, where X is the number of other creatures you control"`).
///
/// Returns `Some((count, color_options))` where `count` is the raw
/// `QuantityExpr` from the leading count prefix (the caller resolves any
/// `where X is …` tail via `apply_where_x_count_expression`) and
/// `color_options` is the distinct list of choosable colors. The caller maps
/// this onto the existing `ManaProduction::AnyOneColor` variant — no new enum
/// variant is introduced.
///
/// Declines (returns `None`) when:
/// - there is no leading count token (so `"{G}{G} or {W}{W}"`-style text still
///   routes to `parse_mana_combinations_clause`);
/// - a later disjunct repeats a count that differs from the first (typed
///   `QuantityExpr` equality — `"X {G} or 2 {W}"` is a different grammar);
/// - a later disjunct omits the count entirely (`"X {G} or {W}"`);
/// - fewer than two distinct colors are collected (a single color is the
///   existing fixed/`AnyOneColor`-single path and must not be intercepted);
/// - any trailing text remains unconsumed.
///
/// Separator and count parsing delegate to nom combinators
/// (`parse_mana_count_prefix`, `alt`/`tag` over the joiners) and color symbol
/// extraction to `parse_mana_color_symbol` — no string-matching dispatch.
fn parse_repeated_count_color_choice(clause: &str) -> Option<(QuantityExpr, Vec<ManaColor>)> {
    let trimmed = clause.trim().trim_end_matches(['.', '"']).trim();
    if trimmed.is_empty() {
        return None;
    }

    // First disjunct: a leading count prefix is mandatory. If absent, decline
    // so count-free disjunctive forms route to `parse_mana_combinations_clause`.
    let (count, rest) = parse_mana_count_prefix(trimmed)?;
    let (first_colors, rest) = parse_mana_color_symbol(rest)?;

    let push_colors = |parsed: Vec<ManaColor>, colors: &mut Vec<ManaColor>| {
        for color in parsed {
            if !colors.contains(&color) {
                colors.push(color);
            }
        }
    };
    let mut colors: Vec<ManaColor> = Vec::new();
    push_colors(first_colors, &mut colors);

    // Do NOT trim leading whitespace here: the separator tags below carry a
    // leading space (` or `, `, or `), so the gap between a color symbol and
    // its joiner must be preserved for the `alt`/`tag` match.
    let mut rest = rest.trim_end();
    loop {
        if rest.is_empty() {
            break;
        }
        // Separator: ", or " / ", and/or " / " and/or " / " or " / ", " —
        // nom `alt`/`tag`, longest-match-first so ", or " wins over ", ".
        let rest_lower = rest.to_lowercase();
        let (_, after_sep) = nom_on_lower(rest, &rest_lower, |i| {
            value(
                (),
                alt((
                    tag(", or "),
                    tag(", and/or "),
                    tag(" and/or "),
                    tag(" or "),
                    tag(", "),
                )),
            )
            .parse(i)
        })?;

        // Each subsequent disjunct must repeat the SAME count token. A missing
        // or differing count is a different grammar — decline.
        let (next_count, after_count) = parse_mana_count_prefix(after_sep)?;
        if next_count != count {
            return None;
        }
        let (next_colors, after_colors) = parse_mana_color_symbol(after_count)?;
        push_colors(next_colors, &mut colors);
        // Keep leading whitespace — the next separator tag needs it.
        rest = after_colors.trim_end();
    }

    // Require at least two distinct colors to qualify as a *choice*.
    if colors.len() < 2 {
        return None;
    }
    Some((count, colors))
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
    use crate::parser::oracle_nom::error::OracleError;
    crate::parser::oracle_nom::primitives::scan_at_word_boundaries(text, |input| {
        alt((
            // CR 106.7: "mana of any color that a land an opponent controls could produce"
            // must be checked before the shorter "mana of any color" to avoid partial match.
            value(
                ManaProduction::OpponentLandColors {
                    count: count.clone(),
                },
                alt((
                    tag::<_, _, OracleError<'_>>(
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
                    tag::<_, _, OracleError<'_>>("mana of any of the exiled cards' colors"),
                    tag("mana of any of the exiled cards’ colors"),
                    tag("mana of any of the exiled card's colors"),
                    tag("mana of any of the exiled card’s colors"),
                    tag("mana of any color among the exiled cards"),
                )),
            ),
            // CR 106.1 + CR 109.1: Parse "mana of any [one] color among [permanents]"
            map_opt(
                preceded(
                    alt((
                        tag::<_, _, OracleError<'_>>("mana of any one color among "),
                        tag("mana of any color among "),
                    )),
                    nom_rest,
                ),
                |type_text: &str| {
                    let (filter, remainder) = parse_type_phrase(type_text.trim());
                    if !remainder.trim().is_empty() || matches!(filter, TargetFilter::Any) {
                        return None;
                    }
                    Some(ManaProduction::AnyOneColorAmongPermanents {
                        count: count.clone(),
                        filter,
                        contribution,
                    })
                },
            ),
            value(
                ManaProduction::AnyOneColor {
                    count: count.clone(),
                    color_options: all_mana_colors(),
                    contribution,
                },
                terminated(
                    alt((tag("mana of any one color"), tag("mana of any color"))),
                    not(tag(" among ")),
                ),
            ),
            value(
                ManaProduction::AnyCombination {
                    count: count.clone(),
                    color_options: all_mana_colors(),
                },
                tag("mana in any combination of colors"),
            ),
            // CR 106.1: "{g} or one mana of the chosen color" — a fixed-color
            // alternative to the chosen color (Cycle of Gates). Must precede the
            // bare `ChosenColor` arm so it wins at the earlier `{g}` word
            // boundary; the bare arm would otherwise skip the `{g}` and drop it.
            map(
                (
                    parse_pure_color_symbol,
                    alt((tag(" or one "), tag(" or "))),
                    alt((tag("mana of the chosen color"), tag("mana of that color"))),
                ),
                |(fixed, _, _)| ManaProduction::ChosenColor {
                    count: count.clone(),
                    contribution,
                    fixed_alternative: Some(fixed),
                },
            ),
            value(
                ManaProduction::ChosenColor {
                    count: count.clone(),
                    contribution,
                    fixed_alternative: None,
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

/// CR 605.3b + CR 106.1a: Enumerate every distinct (unordered) combination of
/// `n` different colors drawn from WUBRG, in canonical lexicographic order.
/// Expands "Add N mana of different colors" into the explicit option set for
/// `ManaProduction::ChoiceAmongCombinations` (Firemind Vessel, Component Pouch,
/// Guild Globe, Interplanar Beacon — all N=2 → 10 pairs). Empty when `n == 0`
/// or `n > 5` so callers decline rather than emit an empty choice.
fn combinations_of_distinct_colors(n: usize) -> Vec<Vec<ManaColor>> {
    let colors = ManaColor::ALL; // [ManaColor; 5], canonical WUBRG order
    if n == 0 || n > colors.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // `idx` holds the current strictly-increasing combination of indices into
    // `colors`. Emit it, then advance to the next combination in lexicographic
    // order (standard algorithm-L style k-combination successor).
    let mut idx: Vec<usize> = (0..n).collect();
    loop {
        out.push(idx.iter().map(|&i| colors[i]).collect());
        let mut i = n;
        loop {
            if i == 0 {
                return out;
            }
            i -= 1;
            if idx[i] != i + colors.len() - n {
                idx[i] += 1;
                for j in (i + 1)..n {
                    idx[j] = idx[j - 1] + 1;
                }
                break;
            }
        }
    }
}

fn parse_restricted_spell_type_phrase(spell_part: &str) -> Option<String> {
    let (rest, phrase) = terminated(
        take_until::<_, _, OracleError<'_>>(" spell"),
        alt((tag(" spells"), tag(" spell"))),
    )
    .parse(spell_part)
    .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(
        phrase
            .split_whitespace()
            .map(|word| match word {
                "and" | "or" => word.to_string(),
                _ => super::capitalize(word),
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// CR 106.6: Parse the negative spend restriction "this mana can't be spent to
/// cast [a/an] non<TYPE> spell(s)" into a `SpellTypeOrAbilityActivation` whose
/// `spell_type` is `<TYPE>` (the phrase with the leading "non" stripped) and
/// whose ability scope is `Any`. The double-negative restricts spell-casting to
/// `<TYPE>` spells while leaving every ability activation payable (CR 605/602) —
/// Karn, Legacy Reforged; Hydraulic Helper. Returns `None` for any other
/// phrasing so the positive-form parser and the existing gap behavior are
/// untouched.
fn parse_negative_mana_spend_restriction(lower: &str) -> Option<ManaSpendRestriction> {
    let (_, rest) = nom_on_lower(lower, lower, |i| {
        // MTGJSON Oracle text is not apostrophe-normalized, so accept both the
        // ASCII (') and curly (U+2019) apostrophe forms of "can't".
        let (i, _) = tag("this mana ca").parse(i)?;
        let (i, _) = alt((tag("n't"), tag("n\u{2019}t"))).parse(i)?;
        let (i, _) = tag(" be spent to cast ").parse(i)?;
        let (i, _) = opt(nom_primitives::parse_article).parse(i)?;
        value((), alt((tag("non-"), tag("non")))).parse(i)
    })?;
    let rest = rest.trim().trim_end_matches(['.', '"']).trim();
    // `rest` is now "<type> spell(s)" (the article and "non" prefix already
    // consumed); reuse the shared type-phrase combinator to canonicalize the
    // spell type.
    let spell_type = parse_restricted_spell_type_phrase(rest)?;
    Some(ManaSpendRestriction::SpellTypeOrAbilityActivation {
        spell_type,
        ability: AbilityActivationScope::Any,
    })
}

fn normalize_restricted_source_phrase(phrase: &str) -> String {
    phrase
        .split_whitespace()
        .map(|word| {
            let singular = if word != "colorless" && word.len() > 1 && word.ends_with('s') {
                &word[..word.len() - 1]
            } else {
                word
            };
            match singular {
                "and" | "or" => singular.to_string(),
                _ => super::capitalize(singular),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_activation_source_quality(input: &str) -> OracleResult<'_, String> {
    let (input, _) = opt(nom_primitives::parse_article).parse(input)?;
    let (rest, phrase) = alt((
        terminated(
            take_until::<_, _, OracleError<'_>>(" source"),
            alt((tag(" sources"), tag(" source"))),
        ),
        nom_rest,
    ))
    .parse(input)?;
    Ok((rest, normalize_restricted_source_phrase(phrase.trim())))
}

fn parse_activation_tail_after_or(input: &str) -> OracleResult<'_, Option<String>> {
    let (input, _) = opt(tag("to ")).parse(input)?;
    let (input, _) = tag("activate ").parse(input)?;
    let (input, _) = alt((tag("an ability"), tag("abilities"))).parse(input)?;
    let (input, source_quality) =
        opt(preceded(tag(" of "), parse_activation_source_quality)).parse(input)?;
    Ok((input, source_quality))
}

/// The ability-activation tail of a "cast [X] spell …" spend restriction.
enum ActivationTail {
    /// No "or activate …" tail — a plain spell-type restriction.
    None,
    /// "… or (to) activate an ability" with no type qualifier — any ability.
    Any,
    /// "… or activate abilities of [source quality]" — abilities of that type.
    OfType(String),
}

fn split_restricted_spell_and_activation(rest: &str) -> (&str, ActivationTail) {
    // Anchor on the activation suffix prefix instead of the first " or " so
    // spell type unions ("instant or sorcery") stay inside the spell half.
    let activation_tail = all_consuming(alt((
        separated_pair(
            take_until::<_, _, OracleError<'_>>(" or to activate "),
            tag(" or "),
            parse_activation_tail_after_or,
        ),
        separated_pair(
            take_until::<_, _, OracleError<'_>>(" or activate "),
            tag(" or "),
            parse_activation_tail_after_or,
        ),
    )))
    .parse(rest)
    .map(|(_, (spell_part, source_quality))| (spell_part.trim(), source_quality));
    if let Ok((spell_part, source_quality)) = activation_tail {
        return (
            spell_part,
            source_quality.map_or(ActivationTail::Any, ActivationTail::OfType),
        );
    }

    (rest.trim(), ActivationTail::None)
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
    // CR 106.6: Negative spend restriction — "this mana can't be spent to cast
    // non<TYPE> spells" (Karn, Legacy Reforged). The double negative ("can't
    // cast non<TYPE>") is the spell-side equivalent of "only to cast <TYPE>",
    // but — unlike the positive "spend this mana only …" form — it places NO
    // restriction on ability activation: the clause forbids only the *casting*
    // of non-<TYPE> spells. Lower it to `SpellTypeOrAbilityActivation` (any
    // ability stays payable) rather than `SpellType` (spells-only), which would
    // wrongly forbid paying for abilities.
    if let Some(restriction) = parse_negative_mana_spend_restriction(lower) {
        return Some((restriction, vec![]));
    }

    let (_, base) = nom_on_lower(lower, lower, |i| {
        value((), tag("spend this mana only ")).parse(i)
    })?;
    let base = base.trim_end_matches(['.', '"']);
    let base_lower = base.to_lowercase();

    // CR 106.6: "spend this mana only to activate power-up abilities" -- tag-scoped.
    // Try BEFORE the broader "to activate abilities" arm (more-specific-first) so
    // Quinjet's {R}{R} does not collapse to ActivateOnly.
    if nom_on_lower(base, &base_lower, |i| {
        value((), tag("to activate power-up abilities")).parse(i)
    })
    .is_some()
    {
        return Some((
            ManaSpendRestriction::ActivateTagged(AbilityTag::PowerUp),
            vec![],
        ));
    }

    // "spend this mana only to activate abilities" -- activation-only
    if nom_on_lower(base, &base_lower, |i| {
        value((), tag("to activate abilities")).parse(i)
    })
    .is_some()
    {
        return Some((ManaSpendRestriction::ActivateOnly, vec![]));
    }

    // "spend this mana only on costs that include/contain {X}" -- X-cost restriction
    if nom_on_lower(base, &base_lower, |i| {
        value(
            (),
            alt((tag("on costs that include"), tag("on costs that contain"))),
        )
        .parse(i)
    })
    .is_some()
    {
        return Some((ManaSpendRestriction::XCostOnly, vec![]));
    }

    // CR 106.6: Activation-first disjunction — "to activate X or cast Y" (Automated
    // Artificer). The "to cast " prefix check below would reject this ordering, so
    // detect the activation-first pattern and route through the disjunction parser
    // with the full base text (stripped of the leading "to ").
    if nom_on_lower(base, &base_lower, |i| {
        value((), (tag("to activate "), take_until(" or cast "))).parse(i)
    })
    .is_some()
    {
        // Strip leading "to " so each clause starts bare for parse_disjunctive_clause.
        let without_to = nom_on_lower(base, &base_lower, |i| value((), tag("to ")).parse(i))
            .map_or(base, |(_, rest)| rest);
        if let Some(restriction) = parse_disjunctive_cast_clauses(without_to.trim()) {
            return Some((restriction, vec![]));
        }
    }

    let (_, rest) = nom_on_lower(base, &base_lower, |i| value((), tag("to cast ")).parse(i))?;
    let rest = rest.trim();

    // CR 106.6: Extract "and that spell can't be countered" grant before parsing restriction.
    let (rest, grants) = extract_spell_grants(rest);
    let rest = rest.trim();

    // CR 106.6: Prefer the whole-remainder single-clause reading first, so a
    // type union inside one clause ("instant or sorcery spells") stays a single
    // `SpellType` and only genuinely heterogeneous disjunctions fall through to
    // the multi-clause path below.
    if let Some(restriction) = parse_single_cast_clause(rest) {
        return Some((restriction, grants));
    }

    // CR 106.6: Disjunctive spend restriction ("cast X or Y", "cast X, Y, or
    // activate Z"). Each top-level clause is parsed independently; only when ≥2
    // clauses all parse to self-evaluable restrictions do we emit `Any`.
    if let Some(restriction) = parse_disjunctive_cast_clauses(rest) {
        return Some((restriction, grants));
    }

    None
}

/// CR 106.6: Parse a single post-"to cast " clause (no grant extraction, no
/// disjunction) into a `ManaSpendRestriction`. This is the body of the legacy
/// single-clause logic, extracted verbatim so the disjunction path can reuse it
/// per clause. Returns `None` for any phrase it does not recognize.
fn parse_single_cast_clause(rest: &str) -> Option<ManaSpendRestriction> {
    let rest = rest.trim();
    let rest_lower = rest.to_lowercase();
    if nom_on_lower(rest, &rest_lower, |i| {
        value((), all_consuming(tag("spells"))).parse(i)
    })
    .is_some()
    {
        return Some(ManaSpendRestriction::SpellOnly);
    }

    // CR 106.6 + CR 107.3 + CR 202.3: "[creature ]spells with mana value N or
    // greater or [creature ]spells with {X} in their mana costs" — the
    // disjunctive MV/X reading (Helga, Troyan). Checked before the single
    // MV-threshold arm because it begins with the same "spells with mana value"
    // prefix but continues with the " or … {X} …" disjunct.
    if let Some((spell_type, criteria)) = parse_mv_or_x_cost_criteria(rest) {
        return Some(ManaSpendRestriction::SpellMatchingCostCriteria {
            spell_type,
            criteria,
        });
    }

    // CR 106.6: "spells with mana value N or greater" / "a spell with mana
    // value N or less" — parameterized over Comparator by the threshold suffix.
    if let Some((comparator, value)) = parse_mana_value_threshold(rest) {
        return Some(ManaSpendRestriction::SpellWithManaValue { comparator, value });
    }

    // CR 105.2 + CR 106.6: "spells with exactly N colors" / "a spell with N or
    // more colors" — parameterized over Comparator by the color-count suffix.
    if let Some((comparator, count)) = parse_color_count(rest) {
        return Some(ManaSpendRestriction::SpellWithColorCount { comparator, count });
    }

    // CR 106.6 + CR 702: "[a spell|spells] [with|that has|that have] <keyword>"
    // — keyword-gated spend, optionally with a "from <zone>" tail. Parameterized
    // over the keyword name so a single arm covers flashback, freerunning, etc.
    if let Some((kind, zone)) = parse_spell_with_keyword(rest) {
        return Some(match zone {
            Some(zone) => ManaSpendRestriction::SpellWithKeywordKindFromZone { kind, zone },
            None => ManaSpendRestriction::SpellWithKeywordKind(kind),
        });
    }

    // CR 106.6 + CR 400.7: "[a spell|spells] from [your] <zone>" / "from anywhere
    // other than [your] <zone>" — zone-gated spend (no keyword required), where
    // the leading "anywhere other than" upgrades the reading to the `NotFrom`
    // polarity (Mm'menon, the Right Hand). Checked before the type-phrase
    // fallback, which does not recognize a "from <zone>" tail.
    if let Some((zone, polarity)) = parse_spell_from_zone(rest) {
        return Some(ManaSpendRestriction::SpellFromZone(ZoneSpend {
            zone,
            polarity,
        }));
    }

    // CR 106.6: Check for an "or activate …" ability-activation suffix. If
    // present, emit a combined SpellTypeOrAbilityActivation restriction whose
    // `ability` scope is `OfSpellType` (typed suffix) or `Any` (generic suffix).
    let (spell_part, activation_tail) = split_restricted_spell_and_activation(rest);

    if spell_part.contains("of the chosen type") {
        return Some(ManaSpendRestriction::ChosenCreatureType);
    }

    // "creature spells" / "a creature spell" / "artifact spells" etc.
    let spell_part_lower = spell_part.to_lowercase();
    let spell_part = nom_on_lower(spell_part, &spell_part_lower, nom_primitives::parse_article)
        .map(|(_, rest)| rest)
        .unwrap_or(spell_part);

    let type_phrase = parse_restricted_spell_type_phrase(spell_part)?;

    match activation_tail {
        ActivationTail::OfType(source_quality)
            if source_quality.eq_ignore_ascii_case(&type_phrase) =>
        {
            Some(ManaSpendRestriction::SpellTypeOrAbilityActivation {
                spell_type: type_phrase,
                ability: AbilityActivationScope::OfSpellType,
            })
        }
        // A typed activation suffix whose source quality differs from the spell
        // type is an unsupported compound — gap it rather than guess.
        ActivationTail::OfType(_) => None,
        ActivationTail::Any => Some(ManaSpendRestriction::SpellTypeOrAbilityActivation {
            spell_type: type_phrase,
            ability: AbilityActivationScope::Any,
        }),
        ActivationTail::None => Some(ManaSpendRestriction::SpellType(type_phrase)),
    }
}

/// CR 106.6: Parse one clause of a disjunctive spend restriction. A clause is
/// either a CAST clause (handled by [`parse_single_cast_clause`], tolerating a
/// leading "to cast "/"cast " that the split may have left on a trailing
/// clause) or an ACTIVATE clause ("to activate an ability of an X source"),
/// which is lowered to a `SpellTypeOrAbilityActivation`/`ActivateOnly` reading.
///
/// Returns `None` for any clause that does not independently parse — the caller
/// then drops the whole disjunction rather than guessing.
/// CR 106.6 + CR 116.2m + CR 709.5e: Recognize the non-cast "unlock [a ]door[s]"
/// special-action clause of a disjunctive spend restriction (Smoky Lounge: "cast
/// Room spells and unlock doors"). Tolerates an optional leading "to " (the
/// split may leave "to unlock ..." on a trailing clause) and the
/// singular/plural article forms. Returns the `UnlockDoor` leaf, which lowers to
/// the door-unlock special-action runtime gate. Pure combinator — no string
/// dispatch.
fn parse_unlock_door_clause(clause: &str, clause_lower: &str) -> Option<ManaSpendRestriction> {
    nom_on_lower(clause, clause_lower, |i| {
        let (i, _) = opt(tag("to ")).parse(i)?;
        let (i, _) = tag("unlock ").parse(i)?;
        let (i, _) = opt(alt((tag("a "), tag("an ")))).parse(i)?;
        value(
            ManaSpendRestriction::UnlockDoor,
            all_consuming(alt((tag("doors"), tag("door")))),
        )
        .parse(i)
    })
    .map(|(restriction, _)| restriction)
}

fn parse_disjunctive_clause(clause: &str) -> Option<ManaSpendRestriction> {
    let clause = clause.trim();
    let clause_lower = clause.to_lowercase();

    // CR 116.2m + CR 709.5e: Non-cast door-unlock special-action clause — tried
    // before the cast/activate arms so "unlock doors" isn't mistaken for a cast
    // clause (it has no " spell" terminator and would otherwise fail to parse).
    if let Some(restriction) = parse_unlock_door_clause(clause, &clause_lower) {
        return Some(restriction);
    }

    // ACTIVATE clause: "to activate an ability of an X source" / "activate an
    // equip ability" / "to activate an ability". Reuse the existing activation
    // tail combinator by treating the clause itself as the post-" or " tail.
    if nom_on_lower(clause, &clause_lower, |i| {
        value((), alt((tag("to activate "), tag("activate ")))).parse(i)
    })
    .is_some()
    {
        let (_, source_quality) = all_consuming(parse_activation_tail_after_or)
            .parse(clause_lower.as_str())
            .ok()?;
        return Some(match source_quality {
            // CR 106.6: "activate an ability of an X source" — abilities of
            // permanents of type X. There is no pure "activate abilities of type
            // X" `ManaRestriction`; the closest self-evaluable reading is
            // `SpellTypeOrAbilityActivation { X, OfSpellType }`, whose activation
            // half is exactly "abilities of type X" (and whose spell half also
            // allows X spells — harmless inside a disjunction that already lists
            // the matching X cast clause, e.g. Brotherhood Headquarters).
            Some(quality) => ManaSpendRestriction::SpellTypeOrAbilityActivation {
                spell_type: quality,
                ability: AbilityActivationScope::OfSpellType,
            },
            // CR 106.6: "to activate an ability" with no qualifier — any ability.
            None => ManaSpendRestriction::ActivateOnly,
        });
    }

    // CAST clause: tolerate a leading "to cast "/"cast " the split may have left.
    let cast_clause = nom_on_lower(clause, &clause_lower, |i| {
        value((), alt((tag("to cast "), tag("cast ")))).parse(i)
    })
    .map_or(clause, |(_, rest)| rest);

    parse_single_cast_clause(cast_clause)
}

/// CR 106.6: Split a post-"to cast " remainder into top-level disjunction
/// clauses and, if every clause parses to a self-evaluable restriction (and
/// there are ≥2 of them), emit a `ManaSpendRestriction::Any`. Returns `None`
/// when the remainder is not a multi-clause disjunction or any clause fails to
/// parse / is an `XCostOnly` (deferred-eval) reading — leaving the restriction
/// as a known gap rather than guessing.
///
/// Splitting is done on " or " and the Oxford-comma forms (", " / ", or "); a
/// candidate split is only accepted when it yields ≥2 fragments that EACH
/// independently parse via [`parse_disjunctive_clause`]. Because the caller
/// already tried the whole remainder as a single clause first, a type union
/// inside one clause ("instant or sorcery spells") never reaches this path.
/// CR 106.6: Recognize one disjunction clause — the run of input up to (but not
/// including) the next clause delimiter (" or " / ", " / ", or ") or end of
/// input. nom combinator (a `not`-guarded `anychar` repetition) per the
/// parser-combinator mandate, rather than `str::split`.
fn parse_one_disjunction_clause(input: &str) -> OracleResult<'_, &str> {
    recognize(many1(preceded(
        // CR 106.6: " or ", " and ", and the Oxford-comma forms each separate
        // distinct acceptable actions in a spend restriction. " and " joins
        // heterogeneous spend clauses too (Smoky Lounge: "cast Room spells and
        // unlock doors") — a same-clause type union ("instant and sorcery
        // spells") never reaches this splitter because the caller tries the
        // whole remainder as a single clause first.
        not(alt((
            tag(", or "),
            tag(", and "),
            tag(", "),
            tag(" or "),
            tag(" and "),
        ))),
        anychar,
    )))
    .parse(input)
}

fn parse_disjunctive_cast_clauses(rest: &str) -> Option<ManaSpendRestriction> {
    // CR 106.6: Split the remainder into top-level disjunction clauses with a nom
    // separated list — delimiters are " or ", " and ", and the Oxford-comma forms
    // (", " / ", or " / ", and "). Longest delimiter first so ", or "/", and "
    // win over their ", " prefix. Each clause is the run of input up to the next
    // delimiter; a type union inside one clause ("instant or sorcery spells",
    // "instant and sorcery spells") never reaches here because the caller tries
    // the whole remainder as a single clause first.
    let (_, fragments) = all_consuming(separated_list1(
        alt((
            tag(", or "),
            tag(", and "),
            tag(", "),
            tag(" or "),
            tag(" and "),
        )),
        parse_one_disjunction_clause,
    ))
    .parse(rest)
    .ok()?;

    if fragments.len() < 2 {
        return None;
    }

    let mut subs = Vec::with_capacity(fragments.len());
    for fragment in fragments {
        let restriction = parse_disjunctive_clause(fragment)?;
        // CR 106.6: XCostOnly defers full {X}-cost detection to its call site and
        // cannot be self-evaluated inside a disjunction — drop the whole thing.
        if matches!(restriction, ManaSpendRestriction::XCostOnly) {
            return None;
        }
        subs.push(restriction);
    }

    // Only emit a disjunction for ≥2 successfully-parsed clauses.
    (subs.len() >= 2).then_some(ManaSpendRestriction::Any(subs))
}

/// CR 106.6 + CR 702: Parse "[a spell|spells] [with|that has|that have]
/// <keyword> [from <zone>]" into the keyword kind and optional origin zone.
/// Parameterized over the keyword name (an `alt` of keyword tags) so one arm
/// covers every keyword-gated spend filter (flashback, freerunning, …) rather
/// than a hardcoded per-keyword `matches!`. The optional "from <zone>" tail
/// upgrades the result to a keyword+zone reading (Flashback from graveyard).
fn parse_spell_with_keyword(rest: &str) -> Option<(KeywordKind, Option<Zone>)> {
    let rest_lower = rest.to_lowercase();
    let (kind, after_kw) = nom_on_lower(rest, &rest_lower, |i| {
        let (i, _) = alt((tag("spells"), tag("a spell"))).parse(i)?;
        let (i, _) = alt((tag(" with "), tag(" that has "), tag(" that have "))).parse(i)?;
        // CR 702: keyword name → KeywordKind. Extend this `alt` per keyword.
        alt((
            value(KeywordKind::Flashback, tag("flashback")),
            value(KeywordKind::Freerunning, tag("freerunning")),
        ))
        .parse(i)
    })?;

    let after_lower = after_kw.to_lowercase();
    if after_kw.trim().is_empty() {
        return Some((kind, None));
    }
    // Optional "from [your|a] <zone>" tail.
    let (_, after_from) = nom_on_lower(after_kw, &after_lower, |i| {
        value((), (tag(" from "), opt(alt((tag("your "), tag("a ")))))).parse(i)
    })?;
    let after_from_lower = after_from.to_lowercase();
    let (zone, _) = nom_on_lower(after_from, &after_from_lower, |i| {
        all_consuming(alt((
            value(Zone::Graveyard, tag("graveyard")),
            value(Zone::Exile, tag("exile")),
            value(Zone::Hand, tag("hand")),
        )))
        .parse(i)
    })?;
    Some((kind, Some(zone)))
}

/// CR 106.6 + CR 400.7: Parse "[a spell|spells] from [anywhere other than ]<zone>"
/// (the post-"to cast" remainder of a zone-gated spend restriction) into the
/// origin `Zone` and its inclusion/exclusion polarity. Handles graveyard / exile
/// / hand with the usual "your"/"a" determiners. The optional "anywhere other
/// than " prefix flips the reading to [`ZoneSpendPolarity::NotFrom`] (Mm'menon,
/// the Right Hand — "from anywhere other than your hand"). Returns `None` when
/// the remainder is not a bare spell-from-zone phrase (e.g. it carries a keyword
/// or type qualifier handled by other arms).
fn parse_spell_from_zone(rest: &str) -> Option<(Zone, ZoneSpendPolarity)> {
    let rest_lower = rest.to_lowercase();
    // Consume "[a spell|spells] from " then, optionally, the "anywhere other
    // than " exclusion marker; the determiner ("your"/"a") follows in both
    // readings. The presence of the exclusion marker selects the polarity.
    let (polarity, after_prefix) = nom_on_lower(rest, &rest_lower, |i| {
        let (i, _) = alt((tag("a spell"), tag("spells"))).parse(i)?;
        let (i, _) = tag(" from ").parse(i)?;
        let (i, exclusion) = opt(tag("anywhere other than ")).parse(i)?;
        let polarity = if exclusion.is_some() {
            ZoneSpendPolarity::NotFrom
        } else {
            ZoneSpendPolarity::From
        };
        let (i, _) = opt(alt((tag("your "), tag("a ")))).parse(i)?;
        Ok((i, polarity))
    })?;
    let after_lower = after_prefix.to_lowercase();
    let (zone, _) = nom_on_lower(after_prefix, &after_lower, |i| {
        all_consuming(alt((
            value(Zone::Graveyard, tag("graveyard")),
            value(Zone::Exile, tag("exile")),
            value(Zone::Hand, tag("hand")),
        )))
        .parse(i)
    })?;
    Some((zone, polarity))
}

/// CR 106.6: Parse the "[spells|a spell] with mana value N [or greater|or
/// more|or less]" tail of a spend restriction into a `(Comparator, value)`.
/// Bare "mana value N" with no comparator suffix reads as exact (`EQ`).
///
/// This file's `nom_on_lower` returns `(value, remainder)`, so the consumed
/// remainder is the second tuple element.
fn parse_mana_value_threshold(rest: &str) -> Option<(Comparator, u32)> {
    let rest_lower = rest.to_lowercase();
    let (_, after_prefix) = nom_on_lower(rest, &rest_lower, |i| {
        alt((
            value((), tag("spells with mana value ")),
            value((), tag("a spell with mana value ")),
            value((), tag("spells with a mana value of ")),
            value((), tag("a spell with a mana value of ")),
        ))
        .parse(i)
    })?;
    // parse_number consumes the leading integer N (returns u32).
    let after_prefix_lower = after_prefix.to_lowercase();
    let (value_n, after_num) = nom_on_lower(
        after_prefix,
        &after_prefix_lower,
        nom_primitives::parse_number,
    )?;
    let after_num = after_num.trim();
    let after_num_lower = after_num.to_lowercase();
    // Threshold suffix → comparator. Empty/all-consumed remainder = exact (EQ).
    let comparator = if after_num.is_empty() {
        Comparator::EQ
    } else if nom_on_lower(after_num, &after_num_lower, |i| {
        all_consuming(alt((
            value((), tag("or greater")),
            value((), tag("or more")),
        )))
        .parse(i)
    })
    .is_some()
    {
        Comparator::GE
    } else if nom_on_lower(after_num, &after_num_lower, |i| {
        all_consuming(value((), tag("or less"))).parse(i)
    })
    .is_some()
    {
        Comparator::LE
    } else {
        return None;
    };
    Some((comparator, value_n))
}

/// CR 106.6 + CR 107.3 + CR 202.3: Parse the disjunctive "[<type> ]spells with
/// mana value N <threshold> or [<type> ]spells with {X} in their mana cost[s]"
/// spend restriction (Helga, Skittish Seer — `Some("Creature")`; Troyan, Gutsy
/// Explorer — `None`). Returns `(optional spell type, criteria)` where the
/// criteria are `[ManaValue { .. }, HasXInCost]` in oracle order.
///
/// The optional type word that prefixes "spells" must be identical on both
/// disjuncts (both "creature spells" or both bare "spells"); a mismatch is an
/// unsupported compound and yields `None` so the clause stays a loud gap.
fn parse_mv_or_x_cost_criteria(rest: &str) -> Option<(Option<String>, Vec<SpellCostCriterion>)> {
    let rest_lower = rest.to_lowercase();
    nom_on_lower(rest, &rest_lower, |i| {
        all_consuming(parse_mv_or_x_cost_criteria_inner).parse(i)
    })
    .map(|(parsed, _)| parsed)
}

/// CR 202.3: Comparator suffix shared by mana-value thresholds within the
/// disjunctive criteria combinator. `or greater`/`or more` → `GE`,
/// `or less`/`or fewer` → `LE`.
fn parse_mana_value_comparator(input: &str) -> OracleResult<'_, Comparator> {
    alt((
        value(Comparator::GE, alt((tag("or greater"), tag("or more")))),
        value(Comparator::LE, alt((tag("or less"), tag("or fewer")))),
    ))
    .parse(input)
}

/// CR 106.6: Consume the optional article, optional shared type word, and the
/// "spell(s) with " opener of one disjunct. Returns the captured type word
/// (`None` for bare "spells with "). Reuses `parse_article` so "a creature spell
/// with" and "creature spells with" both reduce to the bare type word.
///
/// The bare "spell(s) with " opener is tried first; only when it does not match
/// is a single non-"spell" type word captured ahead of the opener. This keeps a
/// type-less disjunct from swallowing later " spell" occurrences via `take_until`.
fn parse_typed_spells_with_opener(input: &str) -> OracleResult<'_, Option<String>> {
    let (input, _) = opt(nom_primitives::parse_article).parse(input)?;
    let spells_with = || alt((tag(" spells with "), tag(" spell with ")));
    // Bare opener (no type word). `take_until` over the empty-type case would
    // otherwise consume across the disjunct, so handle it explicitly first.
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("spells with "),
        tag("spell with "),
    ))
    .parse(input)
    {
        return Ok((rest, None));
    }
    let (input, type_word) = map(take_until(" spell"), |t: &str| t.to_string()).parse(input)?;
    let (input, _) = spells_with().parse(input)?;
    Ok((input, Some(type_word)))
}

/// CR 106.6 + CR 107.3 + CR 202.3: Inner all-consuming combinator for
/// [`parse_mv_or_x_cost_criteria`]. Parses both disjuncts and validates that the
/// optional type prefix matches across them.
fn parse_mv_or_x_cost_criteria_inner(
    input: &str,
) -> OracleResult<'_, (Option<String>, Vec<SpellCostCriterion>)> {
    // First disjunct: "[<type> ]spell(s) with mana value N <threshold>".
    let (input, type_a) = parse_typed_spells_with_opener(input)?;
    let (input, _) = tag("mana value ").parse(input)?;
    let (input, value) = nom_primitives::parse_number(input)?;
    let (input, _) = char(' ').parse(input)?;
    let (input, comparator) = parse_mana_value_comparator(input)?;
    // Connective.
    let (input, _) = tag(" or ").parse(input)?;
    // Second disjunct: "[<type> ]spell(s) with {x} in their mana cost[s]".
    let (input, type_b) = parse_typed_spells_with_opener(input)?;
    let (input, _) = (
        tag("{x} in "),
        alt((tag("their"), tag("its"))),
        tag(" mana cost"),
        opt(tag("s")),
    )
        .parse(input)?;

    if type_a != type_b {
        return Err(oracle_err(input));
    }
    let spell_type = type_a.map(|t| super::capitalize(t.trim()));
    let criteria = vec![
        SpellCostCriterion::ManaValue { comparator, value },
        SpellCostCriterion::HasXInCost,
    ];
    Ok((input, (spell_type, criteria)))
}

/// CR 105.2 + CR 106.6: Parse the "[spells|a spell] with [exactly] N [or more|or
/// fewer] color(s)" tail of a spend restriction into a `(Comparator, count)`.
/// "exactly N color(s)" and bare "N color(s)" read as exact (`EQ`); "or more /
/// or greater color(s)" reads as `GE`; "or fewer / or less color(s)" reads as `LE`.
/// Colorless spells have a color count of 0, so `count` may be 0.
///
/// This file's `nom_on_lower` returns `(value, remainder)`, so the consumed
/// remainder is the second tuple element. Mirrors `parse_mana_value_threshold`.
fn parse_color_count(rest: &str) -> Option<(Comparator, u32)> {
    let rest_lower = rest.to_lowercase();
    let (_, after_prefix) = nom_on_lower(rest, &rest_lower, |i| {
        value((), alt((tag("spells with "), tag("a spell with ")))).parse(i)
    })?;
    // Optional "exactly " forces an exact (EQ) reading regardless of suffix.
    let after_prefix_lower = after_prefix.to_lowercase();
    let (exactly, after_exactly) = nom_on_lower(after_prefix, &after_prefix_lower, |i| {
        opt(value((), tag("exactly "))).parse(i)
    })
    .map(|(exactly, rest)| (exactly.is_some(), rest))?;
    // parse_number consumes the leading integer N (returns u32, handles word numbers).
    let after_exactly_lower = after_exactly.to_lowercase();
    let (count, after_num) = nom_on_lower(
        after_exactly,
        &after_exactly_lower,
        nom_primitives::parse_number,
    )?;
    let after_num = after_num.trim();
    let after_num_lower = after_num.to_lowercase();
    // Suffix -> comparator. Bare "color(s)" or "exactly N color(s)" = exact (EQ).
    let comparator = if exactly {
        if nom_on_lower(after_num, &after_num_lower, |i| {
            all_consuming(parse_color_word).parse(i)
        })
        .is_some()
        {
            Comparator::EQ
        } else {
            return None;
        }
    } else if nom_on_lower(after_num, &after_num_lower, |i| {
        all_consuming(parse_color_word).parse(i)
    })
    .is_some()
    {
        Comparator::EQ
    } else if nom_on_lower(after_num, &after_num_lower, |i| {
        all_consuming(alt((
            value((), (tag("or more "), parse_color_word)),
            value((), (tag("or greater "), parse_color_word)),
        )))
        .parse(i)
    })
    .is_some()
    {
        Comparator::GE
    } else if nom_on_lower(after_num, &after_num_lower, |i| {
        all_consuming(alt((
            value((), (tag("or fewer "), parse_color_word)),
            value((), (tag("or less "), parse_color_word)),
        )))
        .parse(i)
    })
    .is_some()
    {
        Comparator::LE
    } else {
        return None;
    };
    Some((comparator, count))
}

fn parse_color_word(input: &str) -> OracleResult<'_, ()> {
    value((), (tag("color"), opt(tag("s")))).parse(input)
}

/// CR 106.6: Parse a standalone "that spell can't be countered" clause.
///
/// Used when comma-splitting separates the grant from the restriction text,
/// producing a standalone clause like "that spell can't be countered".
pub(super) fn parse_mana_spell_grant(lower: &str) -> Option<Vec<ManaSpellGrant>> {
    let trimmed = lower.trim().trim_end_matches('.');
    if let Some(grant) = parse_conditional_keyword_grant(trimmed) {
        return Some(vec![grant]);
    }
    // Use nom tag for matching
    if value::<_, _, OracleError<'_>, _>((), tag("that spell can't be countered"))
        .parse(trimmed)
        .is_ok()
    {
        return Some(vec![ManaSpellGrant::CantBeCountered]);
    }
    None
}

/// CR 106.6 + CR 702: Parse mana-rider keyword grants:
/// "If that mana is spent on a Dragon creature spell, it gains haste until end of turn."
fn parse_conditional_keyword_grant(lower: &str) -> Option<ManaSpellGrant> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("if that mana is spent on ")
        .parse(lower)
        .ok()?;
    let (rest, _) = opt(alt((tag::<_, _, OracleError<'_>>("a "), tag("an "))))
        .parse(rest)
        .ok()?;
    let (rest, subtype) = terminated(
        take_until::<_, _, OracleError<'_>>(" creature spell, it gains "),
        tag(" creature spell, it gains "),
    )
    .parse(rest)
    .ok()?;
    let (rest, keyword_text) = terminated(
        take_until::<_, _, OracleError<'_>>(" until end of turn"),
        tag(" until end of turn"),
    )
    .parse(rest)
    .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    let keyword = parse_keyword_from_oracle(keyword_text.trim())?;
    Some(ManaSpellGrant::AddKeywordUntilEndOfTurn {
        keyword,
        restriction: Some(ManaRestriction::OnlyForCreatureType(super::capitalize(
            subtype.trim(),
        ))),
    })
}

/// CR 106.6 + CR 603.3: Parse a "When you spend this mana to cast a [filter]
/// spell, [effect]" clause (the unparsed sub-ability of a mana ability — Lapis
/// Orb of Dragonkind, Scaled Nurturer, Gilanra) into a
/// `ManaSpellGrant::TriggerOnSpend`. `lower` is the lowercased clause text.
///
/// First pass recognizes two spell filters — "[a] [subtype] creature spell" and
/// "a spell with mana value N or greater/less" — and parses the effect via the
/// standard effect-chain parser. Returns `None` for unsupported filters or when
/// the effect is unparseable, so the clause stays a loud gap.
pub(crate) fn parse_mana_spend_trigger(lower: &str) -> Option<ManaSpellGrant> {
    // CR 106.6: Both the active ("when you spend this mana to cast …") and the
    // passive ("when that mana is spent to cast …") subject phrasings name the
    // same delayed-trigger-on-spend event. One `alt()` over the two equivalent
    // openings — parameterize, don't proliferate.
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("when you spend this mana to cast "),
        tag::<_, _, OracleError<'_>>("when that mana is spent to cast "),
    ))
    .parse(lower.trim())
    .ok()?;
    // Split "[filter], [effect]" on the first ", ".
    let (after, filter_part) = terminated(
        take_until::<_, _, OracleError<'_>>(", "),
        tag::<_, _, OracleError<'_>>(", "),
    )
    .parse(rest)
    .ok()?;
    let restriction = parse_spend_trigger_filter(filter_part.trim())?;
    let effect_text = after.trim().trim_end_matches('.').trim();
    if effect_text.is_empty() {
        return None;
    }
    // Parse the reflexive effect (scry N, gain N life, draw a card, …).
    let ability = super::parse_effect_chain(effect_text, AbilityKind::Activated);
    // First pass: accept only controller-scoped reflexive effects whose parse
    // fully consumes the clause. Anything else — notably spell-referencing
    // effects like Jade Orb's "it enters with an additional +1/+1 counter on it",
    // which `parse_effect_chain` parses *partially* (silently swallowing the
    // counter clause) — is rejected so the whole clause stays a loud gap rather
    // than flipping the card to "supported" with a swallowed clause (follow-ups).
    if !matches!(
        *ability.effect,
        Effect::Scry { .. } | Effect::GainLife { .. } | Effect::Draw { .. }
    ) {
        return None;
    }
    if ability.sub_ability.is_some() {
        return None;
    }
    Some(ManaSpellGrant::TriggerOnSpend {
        restriction: Some(restriction),
        ability: Box::new(ability),
    })
}

/// Parse the spell-filter portion of a "when you spend this mana to cast …"
/// clause into a `ManaRestriction`. First pass: mana-value thresholds and
/// "[subtype] creature spell". Returns `None` for unsupported filters.
fn parse_spend_trigger_filter(filter: &str) -> Option<ManaRestriction> {
    // "a spell with mana value N or greater/less" (keeps its article).
    if let Some((comparator, value)) = parse_mana_value_threshold(filter) {
        return Some(ManaRestriction::OnlyForSpellWithManaValue { comparator, value });
    }
    // CR 205.3m: "[a|an] creature spell that shares a creature type with your
    // commander" — a relational filter resolved against live commander state at
    // the spend site. `all_consuming` rejects any trailing text so the whole
    // clause stays a loud gap if the phrasing drifts.
    if all_consuming(value(
        ManaRestriction::SharesCreatureTypeWithCommander,
        (
            opt(alt((
                tag::<_, _, OracleError<'_>>("a "),
                tag::<_, _, OracleError<'_>>("an "),
            ))),
            tag::<_, _, OracleError<'_>>(
                "creature spell that shares a creature type with your commander",
            ),
        ),
    ))
    .parse(filter)
    .is_ok()
    {
        return Some(ManaRestriction::SharesCreatureTypeWithCommander);
    }
    // "[a|an] [subtype] creature spell".
    let (rest, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("a "),
        tag::<_, _, OracleError<'_>>("an "),
    )))
    .parse(filter)
    .ok()?;
    let (rest, subtype) = terminated(
        take_until::<_, _, OracleError<'_>>(" creature spell"),
        tag::<_, _, OracleError<'_>>(" creature spell"),
    )
    .parse(rest)
    .ok()?;
    if !rest.trim().is_empty() || subtype.trim().is_empty() {
        return None;
    }
    Some(ManaRestriction::OnlyForCreatureType(super::capitalize(
        subtype.trim(),
    )))
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

    if let Some((_, quantity_text)) = nom_on_lower(rest, &rest.to_lowercase(), |i| {
        value(
            (),
            alt((
                tag("mana of that color equal to "),
                tag("mana of the chosen color equal to "),
            )),
        )
        .parse(i)
    }) {
        let quantity_text = quantity_text.trim().trim_end_matches(['.', '"']);
        let count = parse_event_context_quantity(quantity_text)
            .or_else(|| parse_cda_quantity(quantity_text))?;
        return Some(Effect::Mana {
            produced: ManaProduction::ChosenColor {
                count,
                contribution,
                fixed_alternative: None,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        });
    }

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
        // triggering-spell spent-mana ref; fall back to `parse_cda_quantity` for
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
    use crate::types::ability::{ControllerRef, TypeFilter};

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
    /// `parse_event_context_quantity` to the triggering-spell spent-mana ref.
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
                        qty: QuantityRef::ManaSpentToCast {
                            scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
                            metric: crate::types::ability::CastManaSpentMetric::Total
                        }
                    },
                    "count must reference mana spent on the triggering spell"
                );
            }
            other => panic!("expected Colorless mana production, got {other:?}"),
        }
    }

    #[test]
    fn amount_of_that_color_equal_to_devotion_to_that_color() {
        let effect = try_parse_add_mana_effect(
            "Add an amount of mana of that color equal to your devotion to that color.",
        )
        .expect("chosen-color devotion mana must parse");
        let Effect::Mana { produced, .. } = effect else {
            panic!("expected Effect::Mana");
        };
        match produced {
            ManaProduction::ChosenColor { count, .. } => {
                assert_eq!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Devotion {
                            colors: crate::types::ability::DevotionColors::ChosenColor
                        }
                    }
                );
            }
            other => panic!("expected ChosenColor mana production, got {other:?}"),
        }
    }

    #[test]
    fn chosen_color_for_each_counter_on_self() {
        let effect =
            try_parse_add_mana_effect("Add one mana of that color for each charge counter on ~.")
                .expect("chosen-color for-each counter mana must parse");
        let Effect::Mana { produced, .. } = effect else {
            panic!("expected Effect::Mana");
        };
        match produced {
            ManaProduction::ChosenColor { count, .. } => {
                assert_eq!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::CountersOn {
                            scope: crate::types::ability::ObjectScope::Source,
                            counter_type: Some(crate::types::counter::CounterType::Generic(
                                "charge".to_string()
                            )),
                        }
                    }
                );
            }
            other => panic!("expected ChosenColor mana production, got {other:?}"),
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

    /// CR 106.1: Brigid, Doun's Mind — `"Add X {G} or X {W}, where X is the
    /// number of other creatures you control"`. The count-prefixed disjunctive
    /// color choice maps to `AnyOneColor` with the resolved `where X is …`
    /// quantity and `color_options = [Green, White]`. Previously this fell to
    /// `Effect::Unimplemented`.
    #[test]
    fn brigid_x_color_or_x_color_with_where_x() {
        let effect = try_parse_add_mana_effect(
            "Add X {G} or X {W}, where X is the number of other creatures you control",
        )
        .expect("Brigid clause must parse");
        let Effect::Mana { produced, .. } = effect else {
            panic!("expected Effect::Mana, got something else");
        };
        let ManaProduction::AnyOneColor {
            count,
            color_options,
            ..
        } = produced
        else {
            panic!("expected AnyOneColor, got {produced:?}");
        };
        assert_eq!(color_options, vec![ManaColor::Green, ManaColor::White]);
        // The `where X is …` clause must resolve X to an ObjectCount ref —
        // NOT a bare Variable("X") and NOT Fixed.
        match count {
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { .. },
            } => {}
            other => panic!("expected ObjectCount ref for X, got {other:?}"),
        }
    }

    /// CR 106.1 + CR 115.1: Carpet of Flowers class — the `where X is …`
    /// quantity can itself reference a target player. The mana effect must
    /// surface that player target so `ControllerRef::TargetPlayer` has a
    /// selected player at resolution time.
    #[test]
    fn any_one_color_where_x_target_opponent_controlled_land_count() {
        let effect = try_parse_add_mana_effect(
            "Add X mana of any one color, where X is the number of Islands target opponent controls.",
        )
        .expect("target-opponent where-X mana count must parse");
        let Effect::Mana {
            produced, target, ..
        } = effect
        else {
            panic!("expected Effect::Mana, got something else");
        };
        let ManaProduction::AnyOneColor {
            count,
            color_options,
            ..
        } = produced
        else {
            panic!("expected AnyOneColor, got {produced:?}");
        };
        assert_eq!(color_options, all_mana_colors());
        let QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } = count
        else {
            panic!("expected ObjectCount ref for X, got {count:?}");
        };
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected typed object-count filter, got {filter:?}");
        };
        assert_eq!(typed.controller, Some(ControllerRef::TargetPlayer));
        assert!(
            typed
                .type_filters
                .contains(&TypeFilter::Subtype("Island".to_string())),
            "expected Island subtype in object-count filter, got {:?}",
            typed.type_filters
        );

        let Some(TargetFilter::Typed(target_typed)) = target else {
            panic!("expected target opponent filter, got {target:?}");
        };
        assert_eq!(target_typed.controller, Some(ControllerRef::Opponent));
    }

    /// CR 106.1: Three-color count-prefixed choice — the combinator builds for
    /// the class (any number of disjuncts), not just Brigid's two colors.
    #[test]
    fn three_color_count_prefixed_choice() {
        let effect = try_parse_add_mana_effect("Add X {W}, X {U}, or X {B}")
            .expect("three-color X choice must parse");
        let Effect::Mana { produced, .. } = effect else {
            panic!("expected Effect::Mana");
        };
        let ManaProduction::AnyOneColor {
            count,
            color_options,
            ..
        } = produced
        else {
            panic!("expected AnyOneColor, got {produced:?}");
        };
        assert_eq!(
            color_options,
            vec![ManaColor::White, ManaColor::Blue, ManaColor::Black]
        );
        // No `where X is …` tail — X stays a Variable.
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string()
                }
            }
        );
    }

    /// CR 106.1: Fixed-count count-prefixed choice — `"Add 2 {G} or 2 {W}"`
    /// also routes through the combinator (the count prefix is a number).
    /// CR 106.1 + CR 106.3: "Add {B} or {G} for each permanent destroyed this
    /// way" (Culling Ritual). A >1-color disjunction scaled by a dynamic
    /// "for each" count lowers to AnyCombination (each unit chosen
    /// independently) with the count taken from the for-each clause -- not a
    /// fixed 1-mana AnyOneColor. Building-block test for the whole
    /// "<color set> for each <clause>" mana family.
    #[test]
    fn color_set_for_each_clause_scales_combination() {
        let effect =
            try_parse_add_mana_effect("Add {B} or {G} for each permanent destroyed this way")
                .expect("must parse");
        let Effect::Mana { produced, .. } = effect else {
            panic!("expected Effect::Mana, got {effect:?}");
        };
        let ManaProduction::AnyCombination {
            count,
            color_options,
        } = produced
        else {
            panic!("expected AnyCombination, got {produced:?}");
        };
        assert!(color_options.contains(&ManaColor::Black));
        assert!(color_options.contains(&ManaColor::Green));
        assert!(
            matches!(count, QuantityExpr::Ref { .. }),
            "count must be a dynamic for-each ref, got {count:?}"
        );
    }

    #[test]
    fn fixed_count_prefixed_color_choice() {
        let effect = try_parse_add_mana_effect("Add 2 {G} or 2 {W}")
            .expect("fixed-count color choice must parse");
        let Effect::Mana { produced, .. } = effect else {
            panic!("expected Effect::Mana");
        };
        let ManaProduction::AnyOneColor {
            count,
            color_options,
            ..
        } = produced
        else {
            panic!("expected AnyOneColor, got {produced:?}");
        };
        assert_eq!(color_options, vec![ManaColor::Green, ManaColor::White]);
        assert_eq!(count, QuantityExpr::Fixed { value: 2 });
    }

    /// Boundary: a count-free disjunctive form (`"{G}{G} or {W}{W}"`) has no
    /// leading count token, so `parse_repeated_count_color_choice` DECLINES and
    /// the clause routes to `parse_mana_combinations_clause` instead — yielding
    /// `ChoiceAmongCombinations`, not `AnyOneColor`.
    #[test]
    fn count_free_disjunction_routes_to_combinations() {
        let effect = try_parse_add_mana_effect("Add {G}{G} or {W}{W}")
            .expect("combinations form must parse");
        let Effect::Mana { produced, .. } = effect else {
            panic!("expected Effect::Mana");
        };
        match produced {
            ManaProduction::ChoiceAmongCombinations { options } => {
                assert_eq!(
                    options,
                    vec![
                        vec![ManaColor::Green, ManaColor::Green],
                        vec![ManaColor::White, ManaColor::White],
                    ]
                );
            }
            other => panic!("expected ChoiceAmongCombinations, got {other:?}"),
        }
    }

    /// CR 605.3b + CR 106.1a: "Add two mana of different colors" (Firemind
    /// Vessel, Component Pouch, Guild Globe, Interplanar Beacon) expands to the
    /// 10 distinct WUBRG color pairs as `ChoiceAmongCombinations`.
    #[test]
    fn add_two_mana_of_different_colors_yields_ten_pairs() {
        let options = extract_combinations("Add two mana of different colors")
            .expect("different-colors form must parse to combinations");
        assert_eq!(options.len(), 10, "C(5,2) == 10 pairs");
        for pair in &options {
            assert_eq!(pair.len(), 2, "each option is a 2-color pair");
            assert_ne!(pair[0], pair[1], "the two colors must differ");
            assert!(
                (pair[0] as usize) < (pair[1] as usize),
                "colors listed in canonical order"
            );
        }
        assert_eq!(
            options[0],
            vec![ManaColor::White, ManaColor::Blue],
            "first pair is canonical [White, Blue]"
        );
        // No duplicate pairs.
        let mut sorted = options.clone();
        sorted.dedup();
        assert_eq!(sorted.len(), 10, "no duplicate pairs");
    }

    /// CR 605.3b + CR 106.1a: the combination enumerator is general across N,
    /// not hardcoded for the only printed value (N=2).
    #[test]
    fn combinations_of_distinct_colors_is_general() {
        assert_eq!(combinations_of_distinct_colors(1).len(), 5);
        assert_eq!(combinations_of_distinct_colors(2).len(), 10);
        assert_eq!(combinations_of_distinct_colors(3).len(), 10);
        assert_eq!(combinations_of_distinct_colors(5).len(), 1);
        assert!(combinations_of_distinct_colors(0).is_empty());
        assert!(combinations_of_distinct_colors(6).is_empty());
        let pairs = combinations_of_distinct_colors(2);
        assert_eq!(pairs[0], vec![ManaColor::White, ManaColor::Blue]);
        // The single 5-combination is all five colors in canonical order.
        assert_eq!(
            combinations_of_distinct_colors(5)[0],
            ManaColor::ALL.to_vec()
        );
        // Every combination is strictly increasing (no repeats, canonical).
        for combo in combinations_of_distinct_colors(3) {
            for w in combo.windows(2) {
                assert!((w[0] as usize) < (w[1] as usize));
            }
        }
    }

    /// CR 106.1a: "different colors" must NOT collapse into the any-color
    /// variants — it is a distinct-combination choice, not "pick one color N
    /// times" (`AnyOneColor`) nor a free per-mana color choice
    /// (`AnyCombination`). Conversely, the genuine any-color phrasings still
    /// parse to their existing variants.
    #[test]
    fn different_colors_is_not_any_color() {
        let effect = try_parse_add_mana_effect("Add two mana of different colors")
            .expect("different-colors must parse");
        let Effect::Mana { produced, .. } = effect else {
            panic!("expected Effect::Mana");
        };
        assert!(
            matches!(produced, ManaProduction::ChoiceAmongCombinations { .. }),
            "different colors must be ChoiceAmongCombinations, got {produced:?}"
        );
        assert!(
            !matches!(produced, ManaProduction::AnyOneColor { .. }),
            "different colors must not be AnyOneColor"
        );
        assert!(
            !matches!(produced, ManaProduction::AnyCombination { .. }),
            "different colors must not be AnyCombination"
        );

        // "two mana of any color" → AnyOneColor across all five colors.
        let any = try_parse_add_mana_effect("Add two mana of any color")
            .expect("any-color must still parse");
        let Effect::Mana {
            produced: any_produced,
            ..
        } = any
        else {
            panic!("expected Effect::Mana");
        };
        let ManaProduction::AnyOneColor { color_options, .. } = any_produced else {
            panic!("expected AnyOneColor for any-color, got {any_produced:?}");
        };
        assert_eq!(color_options, all_mana_colors());

        // "mana of any one color" → AnyOneColor as well.
        let any_one = try_parse_add_mana_effect("Add mana of any one color")
            .expect("any-one-color must still parse");
        let Effect::Mana {
            produced: any_one_produced,
            ..
        } = any_one
        else {
            panic!("expected Effect::Mana");
        };
        assert!(
            matches!(any_one_produced, ManaProduction::AnyOneColor { .. }),
            "any one color must be AnyOneColor, got {any_one_produced:?}"
        );
    }

    /// CR 605.3b + CR 106.1a: Interplanar Beacon's bare effect clause (the
    /// activated-ability text fed to `try_parse_add_mana_effect`) parses to the
    /// 10-option different-colors choice.
    #[test]
    fn interplanar_beacon_effect_clause_parses() {
        let effect = try_parse_add_mana_effect("Add two mana of different colors.")
            .expect("Interplanar Beacon clause must parse");
        let Effect::Mana { produced, .. } = effect else {
            panic!("expected Effect::Mana");
        };
        let ManaProduction::ChoiceAmongCombinations { options } = produced else {
            panic!("expected ChoiceAmongCombinations, got {produced:?}");
        };
        assert_eq!(options.len(), 10);
    }

    /// Negative: mismatched repeated count (`"X {G} or {W}"` — second disjunct
    /// has no count) — the combinator declines, falling through to existing
    /// behavior rather than mis-parsing.
    #[test]
    fn mismatched_repeated_count_declines() {
        assert!(
            parse_repeated_count_color_choice("X {G} or {W}").is_none(),
            "missing count on a later disjunct must decline"
        );
        assert!(
            parse_repeated_count_color_choice("X {G} or 2 {W}").is_none(),
            "differing count on a later disjunct must decline"
        );
        assert!(
            parse_repeated_count_color_choice("{G} or {W}").is_none(),
            "no leading count token must decline"
        );
        assert!(
            parse_repeated_count_color_choice("X {G}").is_none(),
            "single color is not a choice"
        );
    }

    #[test]
    fn parses_conditional_mana_keyword_grant() {
        let grants = parse_mana_spell_grant(
            "if that mana is spent on a dragon creature spell, it gains haste until end of turn.",
        )
        .expect("conditional mana keyword grant must parse");
        assert_eq!(
            grants,
            vec![ManaSpellGrant::AddKeywordUntilEndOfTurn {
                keyword: crate::types::keywords::Keyword::Haste,
                restriction: Some(ManaRestriction::OnlyForCreatureType("Dragon".to_string())),
            }]
        );
    }

    /// CR 106.6 + CR 205.3m + CR 903.3: Path of Ancestry's passive-voice
    /// "when that mana is spent to cast a creature spell that shares a creature
    /// type with your commander, scry 1" must parse to a `TriggerOnSpend` with the
    /// relational `SharesCreatureTypeWithCommander` restriction.
    #[test]
    fn parses_path_of_ancestry_spend_trigger() {
        let grant = parse_mana_spend_trigger(
            "when that mana is spent to cast a creature spell that shares a creature type with your commander, scry 1",
        )
        .expect("Path of Ancestry spend trigger must parse");
        match grant {
            ManaSpellGrant::TriggerOnSpend {
                restriction,
                ability,
            } => {
                assert_eq!(
                    restriction,
                    Some(ManaRestriction::SharesCreatureTypeWithCommander)
                );
                assert!(matches!(*ability.effect, Effect::Scry { .. }));
            }
            other => panic!("expected TriggerOnSpend, got {other:?}"),
        }
    }

    /// The active-voice subject phrasing parses identically to the passive voice,
    /// confirming the `alt()` over both openings (parameterize-don't-proliferate).
    #[test]
    fn parses_active_voice_shares_commander_spend_trigger() {
        let grant = parse_mana_spend_trigger(
            "when you spend this mana to cast a creature spell that shares a creature type with your commander, scry 1",
        )
        .expect("active-voice equivalent must parse");
        assert!(matches!(
            grant,
            ManaSpellGrant::TriggerOnSpend {
                restriction: Some(ManaRestriction::SharesCreatureTypeWithCommander),
                ..
            }
        ));
    }

    /// A malformed relational filter must decline so the clause stays a loud gap
    /// rather than flipping to a wrong-but-supported parse.
    #[test]
    fn shares_commander_filter_rejects_trailing_text() {
        assert!(parse_mana_spend_trigger(
            "when that mana is spent to cast a creature spell that shares a creature type with your commander or an opponent's commander, scry 1",
        )
        .is_none());
    }

    /// CR 505.1 + CR 106.4: a subject-led mana clause ("the active player adds
    /// …") must reach the mana dispatcher rather than falling to Unimplemented.
    #[test]
    fn parse_add_mana_active_player_subject() {
        let effect = try_parse_add_mana_effect("the active player adds {C}{C}")
            .expect("subject-led mana clause must parse to Effect::Mana");
        match effect {
            Effect::Mana { target, .. } => {
                assert_eq!(target, Some(TargetFilter::ScopedPlayer));
            }
            other => panic!("expected Effect::Mana, got {other:?}"),
        }
    }

    /// Issue #2900: Blinkmoth Urn effect body after the intervening-if clause.
    #[test]
    fn parse_add_mana_that_player_for_each_artifact_they_control() {
        let effect =
            try_parse_add_mana_effect("that player adds {C} for each artifact they control.")
                .expect("Blinkmoth Urn mana body must parse");
        match effect {
            Effect::Mana {
                produced: ManaProduction::Colorless { count },
                target,
                ..
            } => {
                assert_eq!(
                    target,
                    Some(TargetFilter::ScopedPlayer),
                    "recipient must be the scoped phase player"
                );
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { .. }
                        }
                    ),
                    "count must be ObjectCount, got {count:?}"
                );
            }
            other => panic!("expected Effect::Mana, got {other:?}"),
        }
    }

    /// CR 106.1: `{C}{C} for each X` preserves the literal symbol count as a
    /// `Multiply` factor; `{C} for each X` emits a bare `Ref` (no `Multiply`).
    #[test]
    fn parse_colorless_mana_for_each_preserves_symbol_multiplier() {
        let two = try_parse_add_mana_effect(
            "add {C}{C} for each of your opponents who lost life this turn",
        )
        .expect("'{C}{C} for each' must parse");
        match two {
            Effect::Mana {
                produced: ManaProduction::Colorless { count },
                ..
            } => match count {
                QuantityExpr::Multiply { factor, inner } => {
                    assert_eq!(factor, 2);
                    assert!(matches!(*inner, QuantityExpr::Ref { .. }));
                }
                other => panic!("expected Multiply, got {other:?}"),
            },
            other => panic!("expected colorless Mana, got {other:?}"),
        }

        let one =
            try_parse_add_mana_effect("add {C} for each of your opponents who lost life this turn")
                .expect("'{C} for each' must parse");
        match one {
            Effect::Mana {
                produced: ManaProduction::Colorless { count },
                ..
            } => assert!(
                matches!(count, QuantityExpr::Ref { .. }),
                "single {{C}} must be a bare Ref, got {count:?}"
            ),
            other => panic!("expected colorless Mana, got {other:?}"),
        }
    }

    #[test]
    fn parse_black_dragon_gate_mana_ability() {
        // Issue #2933: "Add {B} or one mana of the chosen color" must retain
        // the fixed Black alternative through `try_parse_add_mana_effect`.
        let effect = try_parse_add_mana_effect("Add {B} or one mana of the chosen color.")
            .expect("Black Dragon Gate mana line must parse");
        assert!(
            matches!(
                effect,
                Effect::Mana {
                    produced: ManaProduction::ChosenColor {
                        fixed_alternative: Some(ManaColor::Black),
                        ..
                    },
                    ..
                }
            ),
            "expected ChosenColor with fixed_alternative Black, got {effect:?}"
        );
    }

    #[test]
    fn negated_nonartifact_spend_maps_to_artifact_spell_or_ability() {
        // CR 106.6: Hydraulic Helper — "This mana can't be spent to cast a
        // nonartifact spell" restricts spell-casting to artifact spells while
        // leaving ability activation unrestricted, so it lowers to the OR variant
        // with `ability: Any` (NOT a spells-only `SpellType`, which would wrongly
        // forbid paying for abilities). ASCII apostrophe form.
        let (restriction, grants) =
            parse_mana_spend_restriction("this mana can't be spent to cast a nonartifact spell")
                .expect("negated nonartifact restriction must parse");
        assert_eq!(
            restriction,
            ManaSpendRestriction::SpellTypeOrAbilityActivation {
                spell_type: "Artifact".to_string(),
                ability: AbilityActivationScope::Any,
            }
        );
        assert!(grants.is_empty());
    }

    #[test]
    fn negated_nonartifact_spend_curly_apostrophe_parses() {
        // CR 106.6: MTGJSON Oracle text is not apostrophe-normalized — the curly
        // (U+2019) apostrophe form of "can't" must parse identically to ASCII.
        let (restriction, _) = parse_mana_spend_restriction(
            "this mana can\u{2019}t be spent to cast a nonartifact spell",
        )
        .expect("curly-apostrophe negated restriction must parse");
        assert_eq!(
            restriction,
            ManaSpendRestriction::SpellTypeOrAbilityActivation {
                spell_type: "Artifact".to_string(),
                ability: AbilityActivationScope::Any,
            }
        );
    }

    #[test]
    fn negated_noncreature_spend_maps_to_creature_spell_or_ability() {
        // CR 106.6: Builds for the class — any non<TYPE> exclusion resolves to
        // the same OR variant over <TYPE>, not just artifact.
        let (restriction, _) =
            parse_mana_spend_restriction("this mana can't be spent to cast a noncreature spell")
                .expect("negated noncreature restriction must parse");
        assert_eq!(
            restriction,
            ManaSpendRestriction::SpellTypeOrAbilityActivation {
                spell_type: "Creature".to_string(),
                ability: AbilityActivationScope::Any,
            }
        );
    }

    // CR 106.6 + CR 107.3 + CR 202.3: Troyan, Gutsy Explorer — any-type
    // disjunctive MV/X spend restriction.
    #[test]
    fn mana_spend_restriction_troyan_mv_or_x_any_type() {
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast spells with mana value 5 or greater or spells with {x} in their mana costs",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellMatchingCostCriteria {
                spell_type: None,
                criteria: vec![
                    SpellCostCriterion::ManaValue {
                        comparator: Comparator::GE,
                        value: 5,
                    },
                    SpellCostCriterion::HasXInCost,
                ],
            })
        );
    }

    // CR 106.6 + CR 107.3 + CR 202.3: Helga, Skittish Seer — creature-narrowed
    // disjunctive MV/X spend restriction.
    #[test]
    fn mana_spend_restriction_helga_creature_mv_or_x() {
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast creature spells with mana value 4 or greater or creature spells with {x} in their mana costs",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellMatchingCostCriteria {
                spell_type: Some("Creature".to_string()),
                criteria: vec![
                    SpellCostCriterion::ManaValue {
                        comparator: Comparator::GE,
                        value: 4,
                    },
                    SpellCostCriterion::HasXInCost,
                ],
            })
        );
    }

    // A type-mismatched disjunct ("creature spells … or spells with {x}") is an
    // unsupported compound: the combinator returns None so the clause stays a
    // loud gap rather than silently dropping the narrowing on one disjunct.
    #[test]
    fn mana_spend_restriction_mv_or_x_type_mismatch_is_gap() {
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast creature spells with mana value 4 or greater or spells with {x} in their mana costs",
        );
        assert_eq!(result.map(|(r, _)| r), None);
    }

    // The single MV-threshold arm must still parse when there is no X disjunct,
    // confirming the disjunction arm doesn't shadow the plain reading.
    #[test]
    fn mana_spend_restriction_plain_mv_threshold_still_parses() {
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast spells with mana value 5 or greater",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellWithManaValue {
                comparator: Comparator::GE,
                value: 5,
            })
        );
    }

    // CR 106.6 + CR 400.7: Mm'menon, the Right Hand — "from anywhere other than
    // your hand" lowers to the `NotFrom` polarity over `Zone::Hand`, while the
    // existing positive "from your graveyard" reading stays `From`.
    #[test]
    fn mana_spend_restriction_not_from_hand() {
        assert_eq!(
            parse_mana_spend_restriction(
                "spend this mana only to cast a spell from anywhere other than your hand"
            )
            .map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellFromZone(ZoneSpend {
                zone: Zone::Hand,
                polarity: ZoneSpendPolarity::NotFrom,
            }))
        );
        // The exclusion marker must not bleed into the inclusion reading.
        assert_eq!(
            parse_mana_spend_restriction(
                "spend this mana only to cast a spell from your graveyard"
            )
            .map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellFromZone(ZoneSpend {
                zone: Zone::Graveyard,
                polarity: ZoneSpendPolarity::From,
            }))
        );
    }

    // CR 106.6 + CR 116.2m + CR 709.5e: Smoky Lounge — "cast Room spells and
    // unlock doors" is a heterogeneous disjunction joined by " and ". It lowers
    // to `Any([SpellType("Room"), UnlockDoor])`: the door-unlock leaf is a
    // non-cast special-action restriction sitting alongside a cast clause.
    #[test]
    fn mana_spend_restriction_smoky_lounge_room_or_unlock_doors() {
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast room spells and unlock doors",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::Any(vec![
                ManaSpendRestriction::SpellType("Room".to_string()),
                ManaSpendRestriction::UnlockDoor,
            ]))
        );
    }

    // The door-unlock clause parses with the singular article form too, and on
    // either side of the connective, so the leaf is order-independent.
    #[test]
    fn mana_spend_restriction_unlock_a_door_singular_clause() {
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast room spells or unlock a door",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::Any(vec![
                ManaSpendRestriction::SpellType("Room".to_string()),
                ManaSpendRestriction::UnlockDoor,
            ]))
        );
    }

    // GUARD: a same-clause type union ("instant and sorcery spells") must still
    // read as a single `SpellType`, never split by the new " and " delimiter.
    // The whole-remainder single-clause path consumes it before the splitter.
    #[test]
    fn mana_spend_restriction_instant_and_sorcery_stays_single_type() {
        let result =
            parse_mana_spend_restriction("spend this mana only to cast instant and sorcery spells");
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellType(
                "Instant and Sorcery".to_string()
            ))
        );
    }

    // HONEST-DEFER GUARD: the four batch-7 members whose disjunction contains a
    // non-cast leaf with no restriction-aware payment seam (turn-face-up,
    // activate-equip) must NOT produce a partial `Any` that silently drops the
    // unenforceable leaf — a partial disjunction would over-restrict the mana
    // (it could no longer pay the legal non-cast action). They stay an honest
    // gap (None) until the underlying special-action payment seams exist.
    #[test]
    fn mana_spend_restriction_unenforceable_noncast_leaves_stay_gap() {
        // Creeping Peeper: enchantment-spell, unlock-door, turn-face-up. The
        // turn-face-up leaf is unenforceable, so the whole thing is deferred.
        assert_eq!(
            parse_mana_spend_restriction(
                "spend this mana only to cast an enchantment spell, unlock a door, or turn a permanent face up",
            )
            .map(|(r, _)| r),
            None,
        );
        // Freya Crescent: Equipment-spell or activate-an-equip-ability.
        assert_eq!(
            parse_mana_spend_restriction(
                "spend this mana only to cast an equipment spell or activate an equip ability",
            )
            .map(|(r, _)| r),
            None,
        );
        // Tin Street Gossip: face-down spells or turn creatures face up.
        assert_eq!(
            parse_mana_spend_restriction(
                "spend this mana only to cast face-down spells or to turn creatures face up",
            )
            .map(|(r, _)| r),
            None,
        );
        // Overgrown Zealot: pure turn-permanents-face-up (no cast clause at all).
        assert_eq!(
            parse_mana_spend_restriction("spend this mana only to turn permanents face up")
                .map(|(r, _)| r),
            None,
        );
    }
}
