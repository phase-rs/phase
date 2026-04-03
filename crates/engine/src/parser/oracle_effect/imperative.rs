use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::value;
use nom::Parser;
use nom_language::error::VerboseError;

use super::counter::{try_parse_double_effect, try_parse_put_counter, try_parse_remove_counter};
use super::mana::{try_parse_activate_only_condition, try_parse_add_mana_effect};
use super::token::try_parse_token;
use super::types::*;
use super::{resolve_it_pronoun, ParseContext};
use crate::parser::oracle_nom::bridge::nom_on_lower;
use crate::parser::oracle_nom::primitives as nom_primitives;
use crate::parser::oracle_static::parse_continuous_modifications;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, Duration, Effect,
    GainLifePlayer, LibraryPosition, PaymentCost, PreventionAmount, PreventionScope, PtValue,
    QuantityExpr, QuantityRef, RoundingMode, StaticDefinition, TargetFilter, TypedFilter,
};
use crate::types::player::PlayerCounterKind;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::super::oracle_target::parse_target;
use super::super::oracle_util::{
    contains_object_pronoun, contains_possessive, parse_count_expr, parse_mana_symbols,
    parse_ordinal, split_around, starts_with_possessive, strip_after, TextPair,
};

/// Earthbend keyword action default target: "target land you control".
/// Used when the Earthbend verb appears without an explicit target (e.g., after
/// reminder text stripping removes the parenthetical that contains the target).
pub(super) fn default_earthbend_target() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You))
}

/// Parse "earthbend [N] [target <type>]" from the text after "earthbend ".
/// Returns `(target, power, toughness)`. Defaults to "target land you control"
/// when no explicit target remains (reminder text stripped, sequence connectors,
/// or variable amounts like "X, where X is...").
///
/// Shared by both the single-imperative parser (`parse_targeted_action_ast`)
/// and the sequence-level parser (`try_parse_verb_and_target` in `mod.rs`).
pub(super) fn parse_earthbend_params(text: &str, lower_rest: &str) -> (TargetFilter, i32, i32) {
    // Delegate to nom combinator (input already lowercase from lower_rest parameter).
    let parsed_number = nom_primitives::parse_number.parse(lower_rest).ok();
    let (pt, target_text) = parsed_number
        .map(|(rem, n)| (n as i32, rem.trim_start()))
        .unwrap_or((0, lower_rest));
    // Default to "target land you control" when no explicit target remains.
    // Handles: no text, punctuation-only, sequence connectors (", then ..."),
    // variable amounts ("X, where X is..." — parse_number fails), or non-target text.
    let has_explicit_target = if parsed_number.is_none() {
        false
    } else {
        let trimmed =
            target_text.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
        !trimmed.is_empty()
            && tag::<_, _, VerboseError<&str>>("then ")
                .parse(trimmed)
                .is_err()
            && tag::<_, _, VerboseError<&str>>("and ")
                .parse(trimmed)
                .is_err()
    };
    let target = if has_explicit_target {
        let (t, _) = parse_target(&text[text.len() - target_text.len()..]);
        t
    } else {
        default_earthbend_target()
    };
    (target, pt, pt)
}

pub(super) fn parse_numeric_imperative_ast(
    text: &str,
    lower: &str,
) -> Option<NumericImperativeAst> {
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| value((), tag("draw ")).parse(input))
    {
        let count = parse_count_expr(rest)
            .map(|(q, _)| q)
            .unwrap_or(QuantityExpr::Fixed { value: 1 });
        return Some(NumericImperativeAst::Draw { count });
    }

    if nom_primitives::scan_contains(lower, "gain") && nom_primitives::scan_contains(lower, "life")
    {
        // CR 119.1: Handle "life equal to {quantity}" — dynamic amount from game state.
        if let Some(qty_text) =
            strip_after(lower, "life equal to ").map(|s| s.trim_end_matches('.'))
        {
            if let Some(qty) =
                crate::parser::oracle_quantity::parse_event_context_quantity(qty_text)
            {
                return Some(NumericImperativeAst::GainLife { amount: qty });
            }
        }
        let after_gain = nom_on_lower(text, lower, |input| {
            value((), alt((tag("you gain "), tag("gain ")))).parse(input)
        })
        .map(|(_, rest)| rest)
        .unwrap_or("");
        if !after_gain.is_empty() {
            let amount = parse_count_expr(after_gain)
                .map(|(q, _)| q)
                .unwrap_or(QuantityExpr::Fixed { value: 1 });
            return Some(NumericImperativeAst::GainLife { amount });
        }
    }

    if nom_primitives::scan_contains(lower, "lose") && nom_primitives::scan_contains(lower, "life")
    {
        if let Some(expr) = try_parse_half_life_amount(lower) {
            return Some(NumericImperativeAst::LoseLife { amount: expr });
        }
        // CR 119.3: Handle "life equal to {quantity}" — dynamic amount from game state.
        if let Some(qty_text) =
            strip_after(lower, "life equal to ").map(|s| s.trim_end_matches('.'))
        {
            if let Some(qty) =
                crate::parser::oracle_quantity::parse_event_context_quantity(qty_text)
            {
                return Some(NumericImperativeAst::LoseLife { amount: qty });
            }
        }
        // Extract count before "life": "lose 3 life", "you lose X life", etc.
        let amount = if let Ok((_, before_life)) =
            take_until::<_, _, VerboseError<&str>>("life").parse(lower)
        {
            let before_life = before_life.trim();
            let last_word = before_life.split_whitespace().next_back().unwrap_or("");
            parse_count_expr(last_word)
                .map(|(q, _)| q)
                .unwrap_or(QuantityExpr::Fixed { value: 1 })
        } else {
            QuantityExpr::Fixed { value: 1 }
        };
        return Some(NumericImperativeAst::LoseLife { amount });
    }

    if nom_primitives::scan_contains(lower, "gets +")
        || nom_primitives::scan_contains(lower, "gets -")
        || nom_primitives::scan_contains(lower, "get +")
        || nom_primitives::scan_contains(lower, "get -")
    {
        // Accept any pump — discard the target. Callers that need subject threading
        // (e.g., try_parse_for_each_effect) extract the subject separately via
        // thread_for_each_subject after lowering the AST.
        if let Some(Effect::Pump {
            power, toughness, ..
        }) = super::try_parse_pump(lower, text)
        {
            return Some(NumericImperativeAst::Pump { power, toughness });
        }
    }

    // Keyword action verbs with numeric count: scry N, surveil N, mill N
    if let Some((verb, rest)) = nom_on_lower(text, lower, |input| {
        alt((
            value("scry", tag("scry ")),
            value("surveil", tag("surveil ")),
            value("mill", tag("mill ")),
        ))
        .parse(input)
    }) {
        let count = parse_count_expr(rest)
            .map(|(q, _)| q)
            .unwrap_or(QuantityExpr::Fixed { value: 1 });
        return match verb {
            "scry" => Some(NumericImperativeAst::Scry { count }),
            "surveil" => Some(NumericImperativeAst::Surveil { count }),
            "mill" => Some(NumericImperativeAst::Mill { count }),
            _ => unreachable!(),
        };
    }

    None
}

/// CR 107.2: Parse "half [possessive] life, rounded up/down" → `HalfRounded` expression.
/// General building block for halving life total expressions.
fn try_parse_half_life_amount(lower: &str) -> Option<QuantityExpr> {
    // Match "lose half their life, rounded up" / "lose half your life, rounded up"
    let (_, after_lose) = alt((tag::<_, _, VerboseError<&str>>("lose "), tag("loses ")))
        .parse(lower)
        .ok()?;
    let (_, after_half) = tag::<_, _, VerboseError<&str>>("half ")
        .parse(after_lose.trim())
        .ok()?;

    // Determine whose life total
    let qty = if alt((
        tag::<_, _, VerboseError<&str>>("their life"),
        tag("that player's life"),
    ))
    .parse(after_half)
    .is_ok()
    {
        QuantityRef::TargetLifeTotal
    } else if alt((
        tag::<_, _, VerboseError<&str>>("your life"),
        tag("his or her life"),
    ))
    .parse(after_half)
    .is_ok()
    {
        QuantityRef::LifeTotal
    } else {
        return None;
    };

    // Parse rounding direction
    let rounding = if nom_primitives::scan_contains(lower, "rounded up") {
        RoundingMode::Up
    } else if nom_primitives::scan_contains(lower, "rounded down") {
        RoundingMode::Down
    } else {
        // Default to up per most MTG cards using "half life"
        RoundingMode::Up
    };

    Some(QuantityExpr::HalfRounded {
        inner: Box::new(QuantityExpr::Ref { qty }),
        rounding,
    })
}

pub(super) fn lower_numeric_imperative_ast(ast: NumericImperativeAst) -> Effect {
    match ast {
        NumericImperativeAst::Draw { count } => Effect::Draw { count },
        NumericImperativeAst::GainLife { amount } => Effect::GainLife {
            amount,
            player: GainLifePlayer::Controller,
        },
        NumericImperativeAst::LoseLife { amount } => Effect::LoseLife {
            amount,
            target: None,
        },
        NumericImperativeAst::Pump { power, toughness } => Effect::Pump {
            power,
            toughness,
            target: TargetFilter::Any,
        },
        NumericImperativeAst::Scry { count } => Effect::Scry { count },
        NumericImperativeAst::Surveil { count } => Effect::Surveil { count },
        NumericImperativeAst::Mill { count } => Effect::Mill {
            count,
            // CR 701.17a: "Mill" with no subject defaults to the controller.
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        },
    }
}

/// Strip leading "a " / "an " article from target text before passing to `parse_target`.
/// Follows the same pattern used by `oracle_cost.rs` for sacrifice cost parsing.
fn strip_article(text: &str) -> &str {
    let lower = text.to_lowercase();
    nom_on_lower(text, &lower, |input| {
        value((), alt((tag("a "), tag("an ")))).parse(input)
    })
    .map(|(_, rest)| rest)
    .unwrap_or(text)
}

/// CR 608.2c: Extract "unless you discard a [type] card" suffix from discard text.
/// Returns the text with the suffix stripped and the parsed filter, or the original text
/// with None if no "unless you discard" clause is present.
///
/// Handles: creature, artifact, instant or sorcery, basic land, enchantment, subtype cards.
fn parse_discard_unless_filter<'a>(
    lower: &'a str,
    _original: &'a str,
) -> (&'a str, Option<TargetFilter>) {
    let Some((before, after_unless)) = split_around(lower, " unless you discard ") else {
        return (lower, None);
    };

    // Strip leading article "a " / "an "
    let type_text = strip_article(after_unless);
    // Strip trailing " card" / " card." — parse_target expects type phrase without "card"
    let type_text = type_text
        .strip_suffix(" card.")
        .or_else(|| type_text.strip_suffix(" card"))
        .unwrap_or(type_text)
        .trim_end_matches('.');

    let (filter, _) = parse_target(type_text);
    if matches!(filter, TargetFilter::Any) {
        // parse_target couldn't parse the type — don't strip
        return (lower, None);
    }
    (before, Some(filter))
}

/// NOTE: Shares verb prefixes with `try_parse_verb_and_target` in `mod.rs`.
/// When adding a new targeted verb here, check if it also needs to be added there
/// (for compound action splitting like "tap target creature and put a counter on it").
pub(super) fn parse_targeted_action_ast(text: &str, lower: &str) -> Option<TargetedImperativeAst> {
    // CR 701.26a/b: Tap/untap all — mass variants must be checked before single-target
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("tap all "), tag("tap each ")))).parse(input)
    }) {
        let (target, _rem) = parse_target(rest);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::TapAll { target });
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("untap all "), tag("untap each ")))).parse(input)
    }) {
        let (target, _rem) = parse_target(rest);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::UntapAll { target });
    }
    // Simple targeted verbs: tap, untap, sacrifice — parse target after verb prefix
    if let Some((verb, rest)) = nom_on_lower(text, lower, |input| {
        alt((
            value("tap", tag("tap ")),
            value("untap", tag("untap ")),
            value("sacrifice", tag("sacrifice ")),
        ))
        .parse(input)
    }) {
        let (target_text, _) = super::strip_optional_target_prefix(strip_article(rest));
        let (target, _rem) = parse_target(target_text);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return match verb {
            "tap" => Some(TargetedImperativeAst::Tap { target }),
            "untap" => Some(TargetedImperativeAst::Untap { target }),
            "sacrifice" => Some(TargetedImperativeAst::Sacrifice { target }),
            _ => unreachable!(),
        };
    }
    if let Some((_, after_discard_orig)) =
        nom_on_lower(text, lower, |input| value((), tag("discard ")).parse(input))
    {
        let after_discard = &lower[lower.len() - after_discard_orig.len()..];
        // CR 701.9a: Detect "at random" suffix for random discard effects.
        let random = nom_primitives::scan_contains(after_discard, "at random");
        // CR 701.9b: Detect "up to" prefix for optional partial discard.
        let (after_discard, up_to) =
            match tag::<_, _, VerboseError<&str>>("up to ").parse(after_discard) {
                Ok((rest, _)) => (rest, true),
                Err(_) => (after_discard, false),
            };
        // Strip "all the cards in " / "all cards in " prefix compositionally for
        // patterns like "discard all the cards in your hand" / "discards all cards in their hand".
        let after_discard = alt((
            tag::<_, _, VerboseError<&str>>("all the cards in "),
            tag("all cards in "),
        ))
        .parse(after_discard)
        .map(|(rest, _)| rest)
        .unwrap_or(after_discard);
        // Detect whole-hand discard patterns before falling through to count parsing.
        // Uses tag prefix (not contains) to avoid matching "discard a card from your hand".
        if alt((
            tag::<_, _, VerboseError<&str>>("your hand"),
            tag("their hand"),
            tag("his or her hand"),
        ))
        .parse(after_discard)
        .is_ok()
        {
            return Some(TargetedImperativeAst::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize,
                },
                random,
                up_to,
                unless_filter: None,
            });
        }
        // CR 608.2c: Strip "unless you discard a [type] card" suffix before count parsing.
        // Compute original-case offset before the unless strip narrows the slice.
        let original_after = &text[text.len() - after_discard.len()..];
        let (after_discard, unless_filter) =
            parse_discard_unless_filter(after_discard, original_after);
        // Re-derive original_after for the narrowed (unless-stripped) text.
        let original_after = &original_after[..after_discard.len()];
        let count = parse_count_expr(original_after)
            .map(|(q, _)| q)
            .unwrap_or(QuantityExpr::Fixed { value: 1 });
        return Some(TargetedImperativeAst::Discard {
            count,
            random,
            up_to,
            unless_filter,
        });
    }
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("return ")).parse(input))
    {
        let (target_text, dest) = super::strip_return_destination_ext(rest);
        let (target, _rem) = parse_target(target_text);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return match dest {
            Some(d) if d.zone == Zone::Battlefield => {
                Some(TargetedImperativeAst::ReturnToBattlefield {
                    target,
                    enter_transformed: d.transformed,
                    under_your_control: d.under_your_control,
                    enter_tapped: d.enter_tapped,
                })
            }
            Some(d) if d.zone == Zone::Hand => Some(TargetedImperativeAst::Return { target }),
            Some(d) => Some(TargetedImperativeAst::ReturnToZone {
                target,
                destination: d.zone,
            }),
            None => Some(TargetedImperativeAst::Return { target }),
        };
    }
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("fight ")).parse(input))
    {
        let (target_text, _) = super::strip_optional_target_prefix(rest);
        let (target, _rem) = parse_target(target_text);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::Fight { target });
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("gain control of ")).parse(input)
    }) {
        let (target_text, _) = super::strip_optional_target_prefix(rest);
        let (target, _rem) = parse_target(target_text);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::GainControl { target });
    }
    // Earthbend: "earthbend [N] [target <type>]" → Animate with haste + is_earthbend
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("earthbend ")).parse(input)
    }) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        let (target, power, toughness) = parse_earthbend_params(text, rest_lower);
        return Some(TargetedImperativeAst::Earthbend {
            target,
            power,
            toughness,
        });
    }
    // Airbend: "airbend target <type> <mana_cost>" → GrantCastingPermission(ExileWithAltCost)
    if let Some((_, original_rest)) =
        nom_on_lower(text, lower, |input| value((), tag("airbend ")).parse(input))
    {
        let (target_text, _) = super::strip_optional_target_prefix(original_rest);
        let (target, after_target) = parse_target(target_text);
        let cost = parse_mana_symbols(after_target.trim_start())
            .map(|(c, _)| c)
            .unwrap_or(crate::types::mana::ManaCost::Cost {
                generic: 2,
                shards: vec![],
            });
        return Some(TargetedImperativeAst::Airbend { target, cost });
    }
    None
}

pub(super) fn lower_targeted_action_ast(ast: TargetedImperativeAst) -> Effect {
    match ast {
        TargetedImperativeAst::Tap { target } => Effect::Tap { target },
        TargetedImperativeAst::Untap { target } => Effect::Untap { target },
        TargetedImperativeAst::TapAll { target } => Effect::TapAll { target },
        TargetedImperativeAst::UntapAll { target } => Effect::UntapAll { target },
        TargetedImperativeAst::Sacrifice { target } => Effect::Sacrifice {
            target,
            up_to: false,
        },
        TargetedImperativeAst::Discard {
            count,
            random,
            up_to,
            unless_filter,
        } => Effect::Discard {
            count,
            // CR 701.9a: "Discard" with no subject defaults to the controller.
            // Subject injection overrides this for "target player discards" patterns.
            target: TargetFilter::Controller,
            random,
            up_to,
            unless_filter,
        },
        TargetedImperativeAst::Return { target } => Effect::Bounce {
            target,
            destination: None,
        },
        // CR 400.7: Return to battlefield is a zone change, not a bounce.
        TargetedImperativeAst::ReturnToBattlefield {
            target,
            enter_transformed,
            under_your_control,
            enter_tapped,
        } => Effect::ChangeZone {
            origin: None,
            destination: Zone::Battlefield,
            target,
            owner_library: false,
            enter_transformed,
            under_your_control,
            enter_tapped,
            enters_attacking: false,
            up_to: false,
        },
        // CR 400.6: Return to a non-hand, non-battlefield zone (graveyard, library).
        TargetedImperativeAst::ReturnToZone {
            target,
            destination,
        } => Effect::ChangeZone {
            origin: None,
            destination,
            target,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
        },
        TargetedImperativeAst::Fight { target } => Effect::Fight {
            target,
            subject: TargetFilter::SelfRef,
        },
        TargetedImperativeAst::GainControl { target } => Effect::GainControl { target },
        TargetedImperativeAst::Earthbend {
            target,
            power,
            toughness,
        } => Effect::Animate {
            power: Some(power),
            toughness: Some(toughness),
            types: vec!["Creature".to_string()],
            remove_types: vec![],
            target,
            keywords: vec![crate::types::keywords::Keyword::Haste],
            is_earthbend: true,
        },
        TargetedImperativeAst::Airbend { target, cost } => Effect::GrantCastingPermission {
            permission: crate::types::ability::CastingPermission::ExileWithAltCost { cost },
            target,
        },
        TargetedImperativeAst::ZoneCounterProxy(ast) => lower_zone_counter_ast(*ast),
    }
}

pub(super) fn parse_search_and_creation_ast(
    text: &str,
    lower: &str,
) -> Option<SearchCreationImperativeAst> {
    if let Some((_, _)) = nom_on_lower(text, lower, |input| value((), tag("seek ")).parse(input)) {
        let details = super::parse_seek_details(lower);
        return Some(SearchCreationImperativeAst::Seek {
            filter: details.filter,
            count: details.count,
            destination: details.destination,
            enter_tapped: details.enter_tapped,
        });
    }
    if starts_with_possessive(lower, "search", "library") {
        let details = super::parse_search_library_details(lower);
        return Some(SearchCreationImperativeAst::SearchLibrary {
            filter: details.filter,
            count: details.count,
            reveal: details.reveal,
        });
    }
    // CR 701.16a + CR 701.20a: "look at the top N" (private) and "reveal the top N" (public)
    // both produce Dig — the reveal flag distinguishes visibility semantics.
    if let Some((reveal, rest)) = nom_on_lower(text, lower, |input| {
        alt((
            value(false, tag("look at the top ")),
            value(true, tag("reveal the top ")),
            value(true, tag("reveals the top ")),
        ))
        .parse(input)
    }) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        // Try numeric count first ("three cards"), then "x" as a variable
        // resolved later by apply_where_x_effect_expression.
        let count = if let Ok((_, n)) = nom_primitives::parse_number.parse(rest_lower) {
            QuantityExpr::Fixed { value: n as i32 }
        } else if tag::<_, _, VerboseError<&str>>("x")
            .parse(rest_lower)
            .is_ok()
        {
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            }
        } else {
            QuantityExpr::Fixed { value: 1 }
        };
        return Some(SearchCreationImperativeAst::Dig { count, reveal });
    }
    if let Some((_, _)) = nom_on_lower(text, lower, |input| value((), tag("create ")).parse(input))
    {
        return match try_parse_token(lower, text) {
            Some(Effect::CopyTokenOf { target, .. }) => {
                Some(SearchCreationImperativeAst::CopyTokenOf { target })
            }
            Some(Effect::Token {
                name,
                power,
                toughness,
                types,
                colors,
                keywords,
                tapped,
                count,
                attach_to,
                ..
            }) => Some(SearchCreationImperativeAst::Token {
                token: Box::new(TokenDescription {
                    name,
                    power: Some(power),
                    toughness: Some(toughness),
                    types,
                    colors,
                    keywords,
                    tapped,
                    count,
                    attach_to,
                }),
            }),
            _ => None,
        };
    }
    None
}

pub(super) fn lower_search_and_creation_ast(ast: SearchCreationImperativeAst) -> Effect {
    match ast {
        SearchCreationImperativeAst::SearchLibrary {
            filter,
            count,
            reveal,
        } => Effect::SearchLibrary {
            filter,
            count,
            reveal,
        },
        SearchCreationImperativeAst::Dig { count, reveal } => Effect::Dig {
            count,
            destination: None,
            keep_count: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: None,
            reveal,
        },
        SearchCreationImperativeAst::CopyTokenOf { target } => Effect::CopyTokenOf {
            target,
            enters_attacking: false,
            tapped: false,
        },
        SearchCreationImperativeAst::Token { token } => Effect::Token {
            name: token.name,
            power: token.power.unwrap_or(PtValue::Fixed(0)),
            toughness: token.toughness.unwrap_or(PtValue::Fixed(0)),
            types: token.types,
            colors: token.colors,
            keywords: token.keywords,
            tapped: token.tapped,
            count: token.count,
            owner: TargetFilter::Controller,
            attach_to: token.attach_to,
            enters_attacking: false,
            supertypes: vec![],
        },
        SearchCreationImperativeAst::Seek {
            filter,
            count,
            destination,
            enter_tapped,
        } => Effect::Seek {
            filter,
            count,
            destination,
            enter_tapped,
        },
    }
}

pub(super) fn parse_hand_reveal_ast(text: &str, lower: &str) -> Option<HandRevealImperativeAst> {
    if nom_on_lower(text, lower, |input| value((), tag("look at ")).parse(input)).is_some()
        && nom_primitives::scan_contains(lower, "hand")
    {
        if contains_possessive(lower, "look at", "hand") {
            // CR 603.7c: "that player's hand" resolves to the player from the triggering event.
            let target = if nom_primitives::scan_contains(lower, "that player's hand") {
                TargetFilter::TriggeringPlayer
            } else {
                TargetFilter::Any
            };
            return Some(HandRevealImperativeAst::LookAt { target });
        }

        let (_, after_look_at) =
            nom_on_lower(text, lower, |input| value((), tag("look at ")).parse(input))?;
        let (target, _) = parse_target(after_look_at);
        return Some(HandRevealImperativeAst::LookAt { target });
    }

    nom_on_lower(text, lower, |input| {
        value((), alt((tag("reveal "), tag("reveals ")))).parse(input)
    })?;

    // CR 701.20a: "reveals a number of cards from their hand equal to X"
    if nom_primitives::scan_contains(lower, "hand")
        && nom_primitives::scan_contains(lower, "equal to ")
    {
        if let Some((_, qty_text)) = lower.split_once("equal to ") {
            let qty_text = qty_text.trim_end_matches('.');
            if let Some(qty) = super::super::oracle_quantity::parse_quantity_ref(qty_text) {
                return Some(HandRevealImperativeAst::RevealPartial {
                    count: crate::types::ability::QuantityExpr::Ref { qty },
                });
            }
        }
    }

    // "reveal the top N" is now handled by parse_search_and_creation_ast → Dig path.
    // This function only handles hand-related reveals.

    if nom_primitives::scan_contains(lower, "hand") {
        return Some(HandRevealImperativeAst::RevealAll);
    }

    None
}

pub(super) fn lower_hand_reveal_ast(ast: HandRevealImperativeAst) -> Effect {
    match ast {
        HandRevealImperativeAst::LookAt { target } => Effect::RevealHand {
            target,
            card_filter: TargetFilter::Any,
            count: None,
        },
        HandRevealImperativeAst::RevealAll => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter: TargetFilter::Any,
            count: None,
        },
        HandRevealImperativeAst::RevealPartial { count } => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter: TargetFilter::Any,
            count: Some(count),
        },
    }
}

pub(super) fn parse_choose_ast(text: &str, lower: &str) -> Option<ChooseImperativeAst> {
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("choose ")).parse(input))
    {
        let rest_lower = &lower[lower.len() - rest.len()..];
        if super::is_choose_as_targeting(rest_lower) {
            let inner = super::parse_effect(rest);
            if !matches!(inner, Effect::Unimplemented { .. }) {
                return Some(ChooseImperativeAst::Reparse {
                    text: rest.to_string(),
                });
            }
            let (target, _) = parse_target(rest);
            return Some(ChooseImperativeAst::TargetOnly { target });
        }
    }

    if let Some(choice_type) = super::try_parse_named_choice(lower) {
        return Some(ChooseImperativeAst::NamedChoice { choice_type });
    }

    if nom_on_lower(text, lower, |input| value((), tag("choose ")).parse(input)).is_some()
        && nom_primitives::scan_contains(lower, "card from it")
    {
        return Some(ChooseImperativeAst::RevealHandFilter {
            card_filter: super::parse_choose_filter(lower),
        });
    }

    None
}

pub(super) fn lower_choose_ast(ast: ChooseImperativeAst) -> Effect {
    match ast {
        ChooseImperativeAst::TargetOnly { target } => Effect::TargetOnly { target },
        ChooseImperativeAst::Reparse { text } => super::parse_effect(&text),
        ChooseImperativeAst::NamedChoice { choice_type } => Effect::Choose {
            choice_type,
            persist: false,
        },
        ChooseImperativeAst::RevealHandFilter { card_filter } => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter,
            count: None,
        },
    }
}

pub(super) fn parse_utility_imperative_ast(
    text: &str,
    lower: &str,
) -> Option<UtilityImperativeAst> {
    // Simple verb dispatch: prevent, regenerate, copy
    if let Some((verb, rest)) = nom_on_lower(text, lower, |input| {
        alt((
            value("prevent", tag("prevent ")),
            value("regenerate", tag("regenerate ")),
            value("copy", tag("copy ")),
        ))
        .parse(input)
    }) {
        return match verb {
            "prevent" => Some(UtilityImperativeAst::Prevent {
                text: text.to_string(),
            }),
            "regenerate" => Some(UtilityImperativeAst::Regenerate {
                text: text.to_string(),
            }),
            "copy" => {
                let (target, _rem) = parse_target(rest);
                #[cfg(debug_assertions)]
                super::types::assert_no_compound_remainder(_rem, text);
                Some(UtilityImperativeAst::Copy { target })
            }
            _ => unreachable!(),
        };
    }
    if matches!(
        lower,
        "transform"
            | "transform ~"
            | "transform this"
            | "transform this creature"
            | "transform this permanent"
            | "transform this artifact"
            | "transform this land"
    ) {
        return Some(UtilityImperativeAst::Transform {
            target: TargetFilter::SelfRef,
        });
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("transform ")).parse(input)
    }) {
        let (target, _) = parse_target(rest);
        if !matches!(target, TargetFilter::Any) {
            return Some(UtilityImperativeAst::Transform { target });
        }
    }
    // CR 613.4d: "switch [target]'s power and toughness"
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("switch ")).parse(input))
    {
        let (target, rem) = parse_target(rest);
        // Consume "'s power and toughness" or " power and toughness" suffix
        let rem_lower = rem.to_lowercase();
        if tag::<_, _, VerboseError<&str>>("'s power and toughness")
            .parse(rem_lower.as_str())
            .is_ok()
            || tag::<_, _, VerboseError<&str>>(" power and toughness")
                .parse(rem_lower.as_str())
                .is_ok()
        {
            return Some(UtilityImperativeAst::SwitchPT { target });
        }
    }
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("attach ")).parse(input))
    {
        let tp = TextPair::new(text, lower);
        let after_to = tp.strip_after(" to ").map(|tp| tp.original).unwrap_or(rest);
        let (target, _rem) = parse_target(after_to);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(UtilityImperativeAst::Attach { target });
    }
    None
}

pub(super) fn lower_utility_imperative_ast(ast: UtilityImperativeAst) -> Effect {
    match ast {
        UtilityImperativeAst::Prevent { text } => parse_prevent_effect(&text),
        UtilityImperativeAst::Regenerate { text } => {
            let lower = text.to_lowercase();
            let rest = tag::<_, _, VerboseError<&str>>("regenerate ")
                .parse(&*lower)
                .map(|(r, _)| r)
                .unwrap_or(&lower);
            let (target, _) = parse_target(rest);
            Effect::Regenerate { target }
        }
        UtilityImperativeAst::Copy { target } => Effect::CopySpell { target },
        UtilityImperativeAst::Transform { target } => Effect::Transform { target },
        UtilityImperativeAst::Attach { target } => Effect::Attach { target },
        // CR 613.4d: Switch power and toughness.
        UtilityImperativeAst::SwitchPT { target } => Effect::SwitchPT { target },
    }
}

/// CR 615: Parse "prevent" damage effects into `Effect::PreventDamage`.
///
/// Handles patterns like:
/// - "prevent the next N damage that would be dealt to any target this turn"
/// - "prevent all damage that would be dealt this turn"
/// - "prevent all combat damage that would be dealt this turn"
/// - "prevent the next N damage that would be dealt to target creature"
fn parse_prevent_effect(text: &str) -> Effect {
    let lower = text.to_lowercase();
    let rest = tag::<_, _, VerboseError<&str>>("prevent ")
        .parse(&*lower)
        .map(|(r, _)| r)
        .unwrap_or(&lower);

    // Determine scope: combat damage only vs all damage
    let scope = if nom_primitives::scan_contains(rest, "combat damage") {
        PreventionScope::CombatDamage
    } else {
        PreventionScope::AllDamage
    };

    // Determine amount: "all damage" vs "the next N damage"
    let amount = if tag::<_, _, VerboseError<&str>>("all ").parse(rest).is_ok() {
        PreventionAmount::All
    } else if let Ok((after_next, _)) = tag::<_, _, VerboseError<&str>>("the next ").parse(rest) {
        let n = nom_primitives::parse_number
            .parse(after_next)
            .map(|(_, n)| n)
            .unwrap_or(1);
        PreventionAmount::Next(n)
    } else {
        // Fallback: try to extract a number
        let n = nom_primitives::parse_number
            .parse(rest)
            .map(|(_, n)| n)
            .unwrap_or(1);
        PreventionAmount::Next(n)
    };

    // Determine target
    let target = if nom_primitives::scan_contains(rest, "any target") {
        TargetFilter::Any
    } else if nom_primitives::scan_contains(rest, "target creature")
        || nom_primitives::scan_contains(rest, "target permanent")
    {
        // Extract the target from the text
        let tp = TextPair::new(text, &lower);
        if let Ok((_, before)) = take_until::<_, _, VerboseError<&str>>("target ").parse(tp.lower) {
            let (_, from_target) = tp.split_at(before.len());
            let (t, _) = parse_target(from_target.original);
            t
        } else {
            TargetFilter::Any
        }
    } else if nom_primitives::scan_contains(rest, "to you")
        || nom_primitives::scan_contains(rest, "to its controller")
    {
        TargetFilter::Controller
    } else {
        // Default: "that would be dealt" with no specific target → Any
        TargetFilter::Any
    };

    Effect::PreventDamage {
        amount,
        target,
        scope,
    }
}

/// CR 702: Parse bare "gain [keyword]" / "gain [keyword] until end of turn"
/// in the imperative path. Handles "gain haste", "gain trample and haste",
/// "gain flying until end of turn", etc.
///
/// Reuses `parse_continuous_modifications` which already handles
/// "gain/gains [keyword]" via `extract_keyword_clause`.
fn try_parse_gain_keyword(text: &str) -> Option<Effect> {
    let (text_without_duration, duration) = super::strip_trailing_duration(text);
    let modifications = parse_continuous_modifications(text_without_duration);

    // Only accept if we got at least one AddKeyword or RemoveKeyword modification
    let has_keyword = modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::AddKeyword { .. }
                | ContinuousModification::RemoveKeyword { .. }
        )
    });
    if !has_keyword {
        return None;
    }

    // Default duration: UntilEndOfTurn for keyword granting sub-abilities
    let duration = duration.or(Some(Duration::UntilEndOfTurn));

    Some(Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(modifications)
            .description(text.to_string())],
        duration,
        target: None,
    })
}

pub(super) fn lower_imperative_ast(ast: ImperativeAst) -> Effect {
    match ast {
        ImperativeAst::Numeric(ast) => lower_numeric_imperative_ast(ast),
        ImperativeAst::Targeted(ast) => lower_targeted_action_ast(ast),
        ImperativeAst::SearchCreation(ast) => lower_search_and_creation_ast(ast),
        ImperativeAst::HandReveal(ast) => lower_hand_reveal_ast(ast),
        ImperativeAst::Choose(ast) => lower_choose_ast(ast),
        ImperativeAst::Utility(ast) => lower_utility_imperative_ast(ast),
    }
}

pub(super) fn parse_put_ast(text: &str, lower: &str) -> Option<PutImperativeAst> {
    tag::<_, _, VerboseError<&str>>("put ").parse(lower).ok()?;

    if let Ok((after, _)) = tag::<_, _, VerboseError<&str>>("put the top ").parse(lower) {
        if nom_primitives::scan_contains(lower, "graveyard") {
            let count = nom_primitives::parse_number
                .parse(after)
                .map(|(_, n)| n)
                .unwrap_or(1);
            return Some(PutImperativeAst::Mill { count });
        }
    }

    // CR 701.24g: "put X on top of Y's library" — specific position, no auto-shuffle.
    // Must check before try_parse_put_zone_change which would emit ChangeZone (auto-shuffles).
    // Only matches forms WITHOUT an explicit origin zone ("from your hand") — those
    // specify a real zone transfer and should go through try_parse_put_zone_change.
    if nom_primitives::scan_contains(lower, "on top of")
        && nom_primitives::scan_contains(lower, "library")
    {
        let has_origin = nom_primitives::scan_contains(lower, " from ");
        if !has_origin {
            return Some(PutImperativeAst::TopOfLibrary);
        }
    }

    // CR 701.24g: "put that card on top" / "put it on top" / "put them on top" —
    // abbreviated form used after "shuffle" in search-and-put-on-top tutors (41 cards).
    if lower.ends_with("on top") {
        return Some(PutImperativeAst::TopOfLibrary);
    }

    // CR 701.24g: "put X on the bottom of Y's library" — specific position without
    // explicit origin zone. Forms with "from" (e.g. "from your hand") go through
    // try_parse_put_zone_change for proper ChangeZone handling.
    if nom_primitives::scan_contains(lower, "on the bottom of")
        && nom_primitives::scan_contains(lower, "library")
    {
        let has_origin = nom_primitives::scan_contains(lower, " from ");
        if !has_origin {
            return Some(PutImperativeAst::BottomOfLibrary);
        }
    }

    // CR 701.24g: "put that card on the bottom" / "put it on the bottom" —
    // abbreviated form without "of Y's library".
    if lower.ends_with("on the bottom") {
        return Some(PutImperativeAst::BottomOfLibrary);
    }

    // CR 701.24g: "put X into Y's library Nth from the top" —
    // specific positional placement (God-Eternals, Approach, Bury in Books).
    if let Ok((_, before_from)) =
        take_until::<_, _, VerboseError<&str>>("from the top").parse(lower)
    {
        {
            // Look backwards from "from the top" to find the ordinal
            let before = before_from.trim_end();
            if let Some(last_space) = before.rfind(' ') {
                let ordinal_word = &before[last_space + 1..];
                if let Some((n, _)) = parse_ordinal(ordinal_word) {
                    return Some(PutImperativeAst::NthFromTop { n });
                }
            }
        }
    }

    if let Some(Effect::ChangeZone {
        origin,
        destination,
        target,
        under_your_control,
        enter_tapped,
        ..
    }) = super::try_parse_put_zone_change(lower, text)
    {
        return Some(PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
            under_your_control,
            enter_tapped,
        });
    }

    None
}

pub(super) fn lower_put_ast(ast: PutImperativeAst) -> Effect {
    match ast {
        PutImperativeAst::Mill { count } => Effect::Mill {
            count: QuantityExpr::Fixed {
                value: count as i32,
            },
            // CR 701.17a: "Put top N into graveyard" is self-mill.
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        },
        PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
            under_your_control,
            enter_tapped,
        } => {
            // CR 610.3: Mass filters (ExiledBySource, TrackedSet) act on all matching
            // objects without individual targeting — use ChangeZoneAll.
            // ExiledBySource always originates from Exile regardless of inferred zone.
            if matches!(
                target,
                TargetFilter::ExiledBySource | TargetFilter::TrackedSet { .. }
            ) {
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Exile),
                    destination,
                    target,
                }
            } else {
                Effect::ChangeZone {
                    origin,
                    destination,
                    target,
                    owner_library: false,
                    enter_transformed: false,
                    under_your_control,
                    enter_tapped,
                    enters_attacking: false,
                    up_to: false,
                }
            }
        }
        // CR 701.24g: Place at a specific position — uses move_to_library_position,
        // not ChangeZone which auto-shuffles per CR 401.3.
        PutImperativeAst::TopOfLibrary => Effect::PutAtLibraryPosition {
            target: TargetFilter::Any,
            position: LibraryPosition::Top,
        },
        PutImperativeAst::BottomOfLibrary => Effect::PutAtLibraryPosition {
            target: TargetFilter::Any,
            position: LibraryPosition::Bottom,
        },
        PutImperativeAst::NthFromTop { n } => Effect::PutAtLibraryPosition {
            target: TargetFilter::Any,
            position: LibraryPosition::NthFromTop { n },
        },
    }
}

/// Parse "put that many {type} counter(s) on {target}" — dynamic counter count from event context.
/// CR 120.1: "that many" references the amount from the triggering event (e.g., damage dealt).
/// Produces PutCounter with count=0 as a sentinel for event-context resolution.
fn try_parse_that_many_counters(lower: &str, ctx: &ParseContext) -> Option<Effect> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("put that many ")
        .parse(lower)
        .ok()?;
    // Next word(s) are counter type: "+1/+1", "charge", "loyalty", etc.
    let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let raw_type = &rest[..type_end];
    let counter_type = super::counter::normalize_counter_type(raw_type);

    // Skip "counter" or "counters" keyword
    let after_type = rest[type_end..].trim_start();
    let after_counter = alt((tag::<_, _, VerboseError<&str>>("counters"), tag("counter")))
        .parse(after_type)
        .map(|(r, _)| r)
        .unwrap_or(after_type)
        .trim_start();

    // Parse target after "on"
    let target =
        if let Ok((on_rest, _)) = tag::<_, _, VerboseError<&str>>("on ").parse(after_counter) {
            if alt((tag::<_, _, VerboseError<&str>>("~"), tag("this ")))
                .parse(on_rest)
                .is_ok()
            {
                TargetFilter::SelfRef
            } else if alt((tag::<_, _, VerboseError<&str>>("it"), tag("itself")))
                .parse(on_rest)
                .is_ok()
            {
                // CR 608.2k: Bare pronoun — context-dependent
                resolve_it_pronoun(ctx)
            } else {
                let (t, _) = parse_target(on_rest);
                t
            }
        } else {
            TargetFilter::SelfRef
        };

    // CR 603.7c: "that many" — resolve from trigger event context at runtime.
    Some(Effect::PutCounter {
        counter_type,
        count: QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },
        target,
    })
}

pub(super) fn parse_shuffle_ast(text: &str, lower: &str) -> Option<ShuffleImperativeAst> {
    if matches!(lower, "shuffle" | "then shuffle") {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Controller,
        });
    }
    // "shuffle the rest into your library" — the "rest" are already in the library
    // from a preceding dig/reveal effect; this is just a shuffle.
    if nom_primitives::scan_contains(lower, "shuffle the rest")
        || nom_primitives::scan_contains(lower, "shuffle them")
    {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Controller,
        });
    }
    if matches!(lower, "that player shuffles" | "target player shuffles") {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Player,
        });
    }
    if tag::<_, _, VerboseError<&str>>("shuffle")
        .parse(lower)
        .is_err()
        || !nom_primitives::scan_contains(lower, "library")
    {
        return None;
    }

    // "shuffle {possessive} library" — extract the possessive word to determine the target.
    // Only matches the exact form "shuffle your library" / "shuffle their library" etc.;
    // compound forms like "shuffle your graveyard into your library" fall through.
    if let Some(possessive) = tag::<_, _, VerboseError<&str>>("shuffle ")
        .parse(lower)
        .ok()
        .map(|(rest, _)| rest)
        .and_then(|s| s.strip_suffix(" library"))
    {
        let target = match possessive {
            "your" => Some(TargetFilter::Controller),
            "their" | "its owner's" | "that player's" => Some(TargetFilter::Player),
            _ => None,
        };
        if let Some(target) = target {
            return Some(ShuffleImperativeAst::ShuffleLibrary { target });
        }
    }
    if contains_object_pronoun(lower, "shuffle", "into")
        || contains_object_pronoun(lower, "shuffles", "into")
    {
        return Some(ShuffleImperativeAst::ChangeZoneToLibrary);
    }
    if contains_possessive(lower, "shuffle", "graveyard") {
        return Some(ShuffleImperativeAst::ChangeZoneAllToLibrary {
            origin: Zone::Graveyard,
        });
    }
    if contains_possessive(lower, "shuffle", "hand") {
        return Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origin: Zone::Hand });
    }
    // CR 701.24a: "shuffle target card from your graveyard into your library" —
    // targeted zone change (origin → library) + implicit shuffle.
    // Placed after possessive checks to avoid matching "shuffle your graveyard into library".
    if let Some((_, after_shuffle)) =
        nom_on_lower(text, lower, |input| value((), tag("shuffle ")).parse(input))
    {
        if nom_primitives::scan_contains(lower, "into")
            && nom_primitives::scan_contains(lower, "library")
            && nom_primitives::scan_contains(lower, "from")
        {
            let (target, _) = parse_target(after_shuffle);
            let origin = if nom_primitives::scan_contains(lower, "graveyard") {
                Some(Zone::Graveyard)
            } else if nom_primitives::scan_contains(lower, "from your hand") {
                Some(Zone::Hand)
            } else if nom_primitives::scan_contains(lower, "from exile") {
                Some(Zone::Exile)
            } else {
                None
            };
            return Some(ShuffleImperativeAst::TargetedChangeZoneToLibrary { target, origin });
        }
    }

    Some(ShuffleImperativeAst::Unimplemented {
        text: text.to_string(),
    })
}

/// CR 701.24a: Lower a shuffle AST into a `ParsedEffectClause`.
/// Compound forms ("shuffle X into library") produce a `ChangeZone` + `Shuffle` sub_ability
/// chain so the library is actually randomized after the zone move.
pub(super) fn lower_shuffle_ast(ast: ShuffleImperativeAst) -> ParsedEffectClause {
    match ast {
        ShuffleImperativeAst::ShuffleLibrary { target } => {
            parsed_clause(Effect::Shuffle { target })
        }
        ShuffleImperativeAst::ChangeZoneToLibrary => {
            let effect = Effect::ChangeZone {
                origin: None,
                destination: Zone::Library,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
            };
            with_shuffle_sub_ability(effect)
        }
        ShuffleImperativeAst::ChangeZoneAllToLibrary { origin } => {
            let effect = Effect::ChangeZoneAll {
                origin: Some(origin),
                destination: Zone::Library,
                target: TargetFilter::Controller,
            };
            with_shuffle_sub_ability(effect)
        }
        // CR 701.24a: Targeted zone change to library with implicit shuffle sub_ability.
        ShuffleImperativeAst::TargetedChangeZoneToLibrary { target, origin } => {
            let effect = Effect::ChangeZone {
                origin,
                destination: Zone::Library,
                target,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
            };
            with_shuffle_sub_ability(effect)
        }
        ShuffleImperativeAst::Unimplemented { text } => parsed_clause(Effect::Unimplemented {
            name: "shuffle".to_string(),
            description: Some(text),
        }),
    }
}

/// Wrap an effect with a `Shuffle` sub_ability for compound "X into library" operations.
fn with_shuffle_sub_ability(effect: Effect) -> ParsedEffectClause {
    let shuffle = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Shuffle {
            target: TargetFilter::Controller,
        },
    );
    ParsedEffectClause {
        effect,
        duration: None,
        sub_ability: Some(Box::new(shuffle)),
        distribute: None,
        multi_target: None,
    }
}

pub(super) fn parse_destroy_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    if nom_on_lower(text, lower, |input| {
        value((), alt((tag("destroy all "), tag("destroy each ")))).parse(input)
    })
    .is_some()
    {
        let (_, rest) = nom_on_lower(text, lower, |input| value((), tag("destroy ")).parse(input))?;
        let (target, _rem) = parse_target(rest);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(ZoneCounterImperativeAst::Destroy { target, all: true });
    }
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("destroy ")).parse(input))
    {
        let (target, _rem) = parse_target(rest);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(ZoneCounterImperativeAst::Destroy { target, all: false });
    }
    None
}

pub(super) fn parse_exile_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("exile the top ").parse(lower) {
        let (count, remainder) = nom_primitives::parse_number
            .parse(rest)
            .map(|(rem, n)| (n, rem.trim_start()))
            .unwrap_or((1, rest));
        // Only handles "your library" (TargetFilter::Controller). Opponent/any-player
        // targeting ("target player's library") falls through to ChangeZone handling.
        if alt((
            tag::<_, _, VerboseError<&str>>("card of your library"),
            tag("cards of your library"),
        ))
        .parse(remainder)
        .is_ok()
        {
            return Some(ZoneCounterImperativeAst::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed {
                    value: count as i32,
                },
            });
        }
    }

    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("exile all "), tag("exile each ")))).parse(input)
    }) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        let (parsed_target, _rem) = parse_target(rest);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        // CR 701.5a: "exile all spells" must constrain to the stack.
        let target = if nom_primitives::scan_contains(rest_lower, "spell") {
            super::constrain_filter_to_stack(parsed_target)
        } else {
            parsed_target
        };
        let origin = super::infer_origin_zone(rest_lower);
        return Some(ZoneCounterImperativeAst::Exile {
            origin,
            target,
            all: true,
        });
    }

    let (_, rest_text) = nom_on_lower(text, lower, |input| value((), tag("exile ")).parse(input))?;
    let rest_lower = &lower[lower.len() - rest_text.len()..];

    // CR 400.12: "exile their graveyard" acts on all cards in that zone.
    // Bare possessive zone references have same semantics as "exile all/each".
    if starts_with_possessive(rest_lower, "", "graveyard")
        || starts_with_possessive(rest_lower, "", "library")
        || starts_with_possessive(rest_lower, "", "hand")
    {
        let (target, _rem) = parse_target(rest_text);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        let origin = super::infer_origin_zone(rest_lower);
        return Some(ZoneCounterImperativeAst::Exile {
            origin,
            target,
            all: true,
        });
    }

    let (parsed_target, _rem) = parse_target(rest_text);
    #[cfg(debug_assertions)]
    super::types::assert_no_compound_remainder(_rem, text);
    // CR 701.5a: "exile target spell" must constrain targeting to the stack,
    // mirroring parse_counter_ast at line 1218-1219.
    let target = if nom_primitives::scan_contains(rest_lower, "spell") {
        super::constrain_filter_to_stack(parsed_target)
    } else {
        parsed_target
    };
    let origin = super::infer_origin_zone(rest_lower);
    Some(ZoneCounterImperativeAst::Exile {
        origin,
        target,
        all: false,
    })
}

pub(super) fn parse_counter_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    let (rest_orig, rest) = {
        let (_, rest_orig) =
            nom_on_lower(text, lower, |input| value((), tag("counter ")).parse(input))?;
        let rest_lower = &lower[lower.len() - rest_orig.len()..];
        (rest_orig, rest_lower)
    };
    if nom_primitives::scan_contains(rest, "activated or triggered ability") {
        // CR 118.12: Parse "unless pays" even for ability counters.
        let unless_payment = super::parse_unless_payment(rest);
        return Some(ZoneCounterImperativeAst::Counter {
            target: TargetFilter::StackAbility,
            source_static: None,
            unless_payment,
        });
    }

    let (target, _rem) = parse_target(rest_orig);
    #[cfg(debug_assertions)]
    super::types::assert_no_compound_remainder(_rem, text);
    let target = if nom_primitives::scan_contains(rest, "spell") {
        super::constrain_filter_to_stack(target)
    } else {
        target
    };
    // CR 118.12: Parse "unless its controller pays {X}" for conditional counters
    let unless_payment = super::parse_unless_payment(rest);
    Some(ZoneCounterImperativeAst::Counter {
        target,
        source_static: None,
        unless_payment,
    })
}

pub(super) fn parse_cost_resource_ast(
    text: &str,
    lower: &str,
) -> Option<CostResourceImperativeAst> {
    if let Some(Effect::Unimplemented {
        name,
        description: Some(description),
    }) = try_parse_activate_only_condition(text)
    {
        if name == "activate_only_if_controls_land_subtype_any" {
            return Some(
                CostResourceImperativeAst::ActivateOnlyIfControlsLandSubtypeAny {
                    subtypes: description.split('|').map(ToString::to_string).collect(),
                },
            );
        }
    }
    if let Some((_, rest_orig)) =
        nom_on_lower(text, lower, |input| value((), tag("pay ")).parse(input))
    {
        let rest = &lower[lower.len() - rest_orig.len()..];
        // "pay N life" → PaymentCost::Life (CR 118.2)
        if let Some(life_rest) = rest.strip_suffix(" life") {
            if let Ok((_, n)) = nom_primitives::parse_number.parse(life_rest) {
                return Some(CostResourceImperativeAst::Pay {
                    cost: PaymentCost::Life { amount: n },
                });
            }
        }
        // CR 107.14: "pay {E}", "pay {E}{E}", "pay N {E}" → PaymentCost::Energy
        if rest.contains("{e}") {
            let energy_count = rest.matches("{e}").count() as u32;
            let cleaned = rest.replace("{e}", "").replace(' ', "");
            if cleaned.is_empty() {
                // Pure {E} symbols: "pay {e}{e}"
                return Some(CostResourceImperativeAst::Pay {
                    cost: PaymentCost::Energy {
                        amount: QuantityExpr::Fixed {
                            value: energy_count as i32,
                        },
                    },
                });
            }
            // "pay N {e}" / "pay eight {e}" — number prefix + {e} suffix
            if rest.ends_with("{e}") {
                let prefix = rest.trim_end_matches("{e}").trim();
                if let Ok((_, n)) = nom_primitives::parse_number.parse(prefix) {
                    return Some(CostResourceImperativeAst::Pay {
                        cost: PaymentCost::Energy {
                            amount: QuantityExpr::Fixed { value: n as i32 },
                        },
                    });
                }
            }
        }
        // "pay {2}{B}" → PaymentCost::Mana (CR 117.1)
        if let Some((mana_cost, _)) = parse_mana_symbols(rest_orig.trim()) {
            return Some(CostResourceImperativeAst::Pay {
                cost: PaymentCost::Mana { cost: mana_cost },
            });
        }
    }
    if nom_on_lower(text, lower, |input| value((), tag("add ")).parse(input)).is_some() {
        return match try_parse_add_mana_effect(text) {
            Some(Effect::Mana {
                produced,
                restrictions,
                ..
            }) => Some(CostResourceImperativeAst::Mana {
                produced,
                restrictions,
            }),
            _ => None,
        };
    }
    if let Some(effect) = super::try_parse_damage(lower, text) {
        return match effect {
            Effect::DealDamage {
                amount,
                target,
                damage_source: None,
            } => Some(CostResourceImperativeAst::Damage {
                amount,
                target,
                all: false,
            }),
            Effect::DamageAll { amount, target } => Some(CostResourceImperativeAst::Damage {
                amount,
                target,
                all: true,
            }),
            // DealDamage with damage_source, DamageEachPlayer, etc. — pass through directly
            other => Some(CostResourceImperativeAst::DamageEffect(Box::new(other))),
        };
    }
    None
}

pub(super) fn lower_cost_resource_ast(ast: CostResourceImperativeAst) -> Effect {
    match ast {
        CostResourceImperativeAst::ActivateOnlyIfControlsLandSubtypeAny { subtypes } => {
            Effect::Unimplemented {
                name: "activate_only_if_controls_land_subtype_any".to_string(),
                description: Some(subtypes.join("|")),
            }
        }
        CostResourceImperativeAst::Mana {
            produced,
            restrictions,
        } => Effect::Mana {
            produced,
            restrictions,
            expiry: None,
        },
        CostResourceImperativeAst::Damage {
            amount,
            target,
            all,
        } => {
            if all {
                Effect::DamageAll { amount, target }
            } else {
                Effect::DealDamage {
                    amount,
                    target,
                    damage_source: None,
                }
            }
        }
        CostResourceImperativeAst::Pay { cost } => Effect::PayCost { cost },
        CostResourceImperativeAst::DamageEffect(effect) => *effect,
    }
}

pub(super) fn parse_imperative_family_ast(
    text: &str,
    lower: &str,
    ctx: &ParseContext,
) -> Option<ImperativeFamilyAst> {
    let first_word = lower.split_whitespace().next().unwrap_or("");

    // CR 500.8: "additional combat phase" can appear in various sentence structures
    // ("there is an additional combat phase", "after this phase, there is an additional...").
    // Intercept early regardless of first_word.
    if nom_primitives::scan_contains(lower, "additional combat phase") {
        let with_main =
            nom_primitives::scan_contains(lower, "followed by an additional main phase");
        return Some(ImperativeFamilyAst::GainKeyword(
            Effect::AdditionalCombatPhase {
                target: TargetFilter::Controller,
                with_main_phase: with_main,
            },
        ));
    }

    // NOTE: when adding verbs here, also add them to IMPERATIVE_EXTRA_VERBS
    // in game/gap_analysis.rs so the parser gap analyzer can classify them.
    match first_word {
        // ── Unambiguous single-category verbs ──

        // Cost/resource verbs (CR 117-118)
        "pay" | "spend" => {
            parse_cost_resource_ast(text, lower).map(ImperativeFamilyAst::CostResource)
        }

        // CR 701.10: "double the power/toughness" or "double the number of counters"
        "double" => try_parse_double_effect(lower, ctx).map(ImperativeFamilyAst::GainKeyword),

        // Zone-change/counter verbs (CR 701)
        "destroy" => parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter),
        "exile" => parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter),
        "counter" => parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter),

        // Numeric verbs (CR 121)
        "draw" if nom_primitives::scan_contains(lower, "that many") => {
            // "draw that many cards" → EventContextAmount, bypass numeric AST
            // which can only represent fixed u32 counts.
            Some(ImperativeFamilyAst::GainKeyword(Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
            }))
        }
        "draw" => parse_numeric_imperative_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast))),
        "scry" | "surveil" | "mill" => parse_numeric_imperative_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast))),

        // Targeted action verbs (CR 701)
        "tap" | "untap" | "sacrifice" | "discard" | "return" | "fight" => {
            parse_targeted_action_ast(text, lower)
                .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Targeted(ast)))
        }
        "earthbend" | "airbend" => parse_targeted_action_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Targeted(ast))),

        // Search/creation verbs (CR 701.18, CR 111.2)
        "search" | "seek" => parse_search_and_creation_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(ast))),
        "create" => parse_search_and_creation_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(ast))),

        // Utility verbs (CR 615, CR 701.19, CR 701.6, CR 613.4d)
        "prevent" | "regenerate" | "copy" | "attach" | "switch" => {
            parse_utility_imperative_ast(text, lower)
                .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Utility(ast)))
        }
        "transform" | "transforms" => parse_utility_imperative_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Utility(ast))),

        // Shuffle (CR 701.19)
        "shuffle" | "shuffles" => parse_shuffle_ast(text, lower).map(ImperativeFamilyAst::Shuffle),

        // Reveal: "reveal the top N" → Dig (via search path), else hand reveal (CR 701.16, CR 701.20)
        "reveal" | "reveals" => parse_search_and_creation_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(ast)))
            .or_else(|| {
                parse_hand_reveal_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::HandReveal(ast)))
            }),

        // Choose (CR 700.2)
        "choose" => parse_choose_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Choose(ast))),

        // ── Exact-match keyword actions ──
        "explore" if lower == "explore" || lower == "explore again" => {
            Some(ImperativeFamilyAst::Explore)
        }
        // CR 702.162a: "connive" / "connives" — extract target from remainder
        "connive" | "connives" => {
            let rest = lower[first_word.len()..].trim();
            if !rest.is_empty() {
                let (target, _) = parse_target(rest);
                Some(ImperativeFamilyAst::GainKeyword(Effect::Connive {
                    target,
                    count: 1,
                }))
            } else {
                Some(ImperativeFamilyAst::Connive)
            }
        }
        // CR 702.136: "investigate"
        "investigate" => Some(ImperativeFamilyAst::Investigate),
        // CR 701.48a: "learn"
        "learn" => Some(ImperativeFamilyAst::Learn),
        // CR 701.62a: "manifest dread"
        "manifest" => {
            if lower == "manifest dread" {
                Some(ImperativeFamilyAst::ManifestDread)
            } else {
                None
            }
        }
        "proliferate" => Some(ImperativeFamilyAst::Proliferate),
        // CR 701.36a: "populate"
        "populate" => Some(ImperativeFamilyAst::Populate),
        // CR 701.30: "clash with an opponent"
        "clash" => {
            if tag::<_, _, VerboseError<&str>>("clash with an opponent")
                .parse(lower)
                .is_ok()
            {
                Some(ImperativeFamilyAst::Clash)
            } else {
                None
            }
        }
        // CR 702.157a: "suspect it" / "suspect target creature"
        "suspect" | "suspects" => {
            let rest = lower[first_word.len()..].trim();
            let target = if !rest.is_empty() {
                let (t, _) = parse_target(rest);
                t
            } else {
                crate::types::ability::TargetFilter::ParentTarget
            };
            Some(ImperativeFamilyAst::GainKeyword(Effect::Suspect { target }))
        }
        // Blight N as an effect (e.g. trigger effect "blight 1")
        "blight" => {
            let rest = alt((tag::<_, _, VerboseError<&str>>("blight "), tag("blight")))
                .parse(lower)
                .map(|(r, _)| r)
                .unwrap_or("");
            let count = nom_primitives::parse_number
                .parse(rest.trim())
                .map(|(_, n)| n)
                .unwrap_or(1);
            Some(ImperativeFamilyAst::GainKeyword(Effect::BlightEffect {
                count,
                target: crate::types::ability::TargetFilter::ParentTarget,
            }))
        }
        // Forage keyword action (CR 702.166a)
        "forage" => Some(ImperativeFamilyAst::GainKeyword(Effect::Forage)),
        // Collect evidence N keyword action (CR 702.163a)
        "collect" => {
            if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("collect evidence ").parse(lower)
            {
                let count = nom_primitives::parse_number
                    .parse(rest.trim())
                    .map(|(_, n)| n)
                    .unwrap_or(1);
                Some(ImperativeFamilyAst::GainKeyword(Effect::CollectEvidence {
                    amount: count,
                }))
            } else {
                None
            }
        }
        // Endure N keyword action
        "endure" | "endures" => {
            let rest = alt((tag::<_, _, VerboseError<&str>>("endure "), tag("endures ")))
                .parse(lower)
                .map(|(r, _)| r)
                .unwrap_or("");
            let count = nom_primitives::parse_number
                .parse(rest.trim())
                .map(|(_, n)| n)
                .unwrap_or(1);
            Some(ImperativeFamilyAst::GainKeyword(Effect::Endure {
                amount: count,
            }))
        }
        // CR 701.47a: "amass [Type] N"
        "amass" => try_parse_amass(text, lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.37a: "monstrosity N"
        "monstrosity" => try_parse_monstrosity(lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.46a: "adapt N"
        "adapt" => try_parse_adapt(lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.39a: "bolster N"
        "bolster" => try_parse_bolster(lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 509.1b / CR 508.1d: "can't be blocked [this turn]", "can't attack", etc.
        // These appear as subjectless clauses in compound effects (e.g., "gets +2/+0 and can't be blocked this turn").
        "can't" | "cannot" => try_parse_subjectless_cant(lower),
        // CR 705: "flip a coin"
        "flip" | "flips" => {
            if lower == "flip a coin" || lower == "flips a coin" {
                Some(ImperativeFamilyAst::FlipCoin)
            } else {
                None
            }
        }
        // CR 706: "roll a d20"
        "roll" | "rolls" => {
            try_parse_roll_die_sides(lower).map(|sides| ImperativeFamilyAst::RollDie { sides })
        }
        // CR 722: "become the monarch"
        "become" | "becomes" => {
            if lower == "become the monarch" || lower == "becomes the monarch" {
                Some(ImperativeFamilyAst::BecomeMonarch)
            } else {
                None
            }
        }
        // CR 701.49: "venture into the dungeon" / "venture into the Undercity"
        "venture" => alt((
            value(
                ImperativeFamilyAst::VentureIntoUndercity,
                tag::<_, _, VerboseError<&str>>("venture into the undercity"),
            ),
            value(
                ImperativeFamilyAst::VentureIntoDungeon,
                tag("venture into the dungeon"),
            ),
        ))
        .parse(lower)
        .ok()
        .map(|(_, ast)| ast),
        // CR 500.7: "take an extra turn after this one"
        // CR 725: "take the initiative"
        "take" | "takes" => {
            if alt((
                value((), tag::<_, _, VerboseError<&str>>("take the initiative")),
                value((), tag("takes the initiative")),
            ))
            .parse(lower)
            .is_ok()
            {
                Some(ImperativeFamilyAst::TakeTheInitiative)
            } else if nom_primitives::scan_contains(lower, "extra turn") {
                Some(ImperativeFamilyAst::GainKeyword(Effect::ExtraTurn {
                    target: TargetFilter::Controller,
                }))
            } else {
                None
            }
        }
        // CR 702.26a: "phase out"
        "phase" | "phases" => {
            if lower == "phase out" || lower == "phases out" {
                Some(ImperativeFamilyAst::PhaseOut)
            } else {
                None
            }
        }

        // CR 701.15a: "goad target creature" / "goads target creature" / "goad it"
        "goad" | "goads" => {
            let rest = lower[first_word.len()..].trim();
            if !rest.is_empty() {
                let (target, _) = parse_target(rest);
                Some(ImperativeFamilyAst::GainKeyword(Effect::Goad { target }))
            } else {
                Some(ImperativeFamilyAst::Goad)
            }
        }

        // CR 701.12a: "exchange control of target [type] and target [type]"
        "exchange" => {
            if tag::<_, _, VerboseError<&str>>("exchange control of ")
                .parse(lower)
                .is_ok()
            {
                Some(ImperativeFamilyAst::ExchangeControl)
            } else {
                None
            }
        }

        // ── Combat-related ──

        // CR 509.1g: "block [object] this turn/combat if able"
        // Handles: "block this turn if able", "blocks ~ this turn if able",
        // "blocks it this combat if able", "blocks this creature this turn if able"
        "block" | "blocks" => {
            if nom_primitives::scan_contains(lower, "this turn if able")
                || nom_primitives::scan_contains(lower, "this combat if able")
            {
                Some(ImperativeFamilyAst::ForceBlock)
            } else {
                None
            }
        }
        // CR 509.1c: "must be blocked [this turn] [if able]"
        "must" => {
            if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("must be blocked").parse(lower) {
                let rest = rest.trim();
                if rest.is_empty()
                    || rest == "this turn if able"
                    || rest == "if able"
                    || rest == "this turn"
                {
                    return Some(ImperativeFamilyAst::MustBeBlocked);
                }
            }
            None
        }

        // ── Multi-category verbs (priority sub-dispatch) ──

        // "put that many +1/+1 counters on ~" — dynamic counter count from event context.
        // Intercepted before standard dispatch because parse_number can't handle "that many".
        // Produces a PutCounter with the counter type and target, using EventContextAmount
        // for the count. The engine resolver reads the count from the resolved ability's
        // event_context_amount field.
        "put"
            if nom_primitives::scan_contains(lower, "that many")
                && nom_primitives::scan_contains(lower, "counter") =>
        {
            try_parse_that_many_counters(lower, ctx)
                .map(ImperativeFamilyAst::GainKeyword)
                .or_else(|| {
                    parse_zone_counter_ast(text, lower, ctx)
                        .map(ImperativeFamilyAst::ZoneCounter)
                        .or_else(|| parse_put_ast(text, lower).map(ImperativeFamilyAst::Put))
                })
        }
        // "put" → counter (step 2) first, then zone-change (step 12)
        "put" => parse_zone_counter_ast(text, lower, ctx)
            .map(ImperativeFamilyAst::ZoneCounter)
            .or_else(|| parse_put_ast(text, lower).map(ImperativeFamilyAst::Put)),

        // "remove" → counter removal (step 2)
        "remove" => parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter),

        // "add" → mana/cost-resource (step 1)
        "add" => parse_cost_resource_ast(text, lower).map(ImperativeFamilyAst::CostResource),

        // "gain" → "gain control of" (step 4) → "gain life" (step 3) → keyword (step 8)
        // The current if/else chain checks numeric first (step 3), but numeric guards with
        // `contains("gain") && contains("life")`, so "gain control of" never matches numeric.
        // This reordering makes the disambiguation explicit.
        "gain" | "gains" => {
            if nom_primitives::scan_contains(lower, "control of") {
                parse_targeted_action_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Targeted(ast)))
            } else if nom_primitives::scan_contains(lower, "life") {
                parse_numeric_imperative_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)))
            } else {
                // CR 702: keyword granting
                try_parse_gain_keyword(text).map(ImperativeFamilyAst::GainKeyword)
            }
        }

        // "lose" → "lose the game" (step 6) → "lose life" (step 3) → keyword (step 7)
        "lose" | "loses" => {
            if nom_primitives::scan_contains(lower, "the game") {
                Some(ImperativeFamilyAst::LoseTheGame)
            } else if nom_primitives::scan_contains(lower, "life") {
                parse_numeric_imperative_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)))
            } else if !nom_primitives::scan_contains(lower, "mana") {
                try_parse_gain_keyword(text).map(ImperativeFamilyAst::LoseKeyword)
            } else {
                None
            }
        }

        // CR 104.3a: "win the game"
        "win" | "wins" => {
            if nom_primitives::scan_contains(lower, "the game") {
                Some(ImperativeFamilyAst::WinTheGame)
            } else {
                None
            }
        }

        // "look" → "look at the top" (step 5) → "look at hand" (step 10)
        "look" => parse_search_and_creation_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(ast)))
            .or_else(|| {
                parse_hand_reveal_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::HandReveal(ast)))
            }),

        // "gets"/"get" → try player counter first, then pump (step 3)
        "gets" | "get" => try_parse_player_counter(lower).or_else(|| {
            parse_numeric_imperative_ast(text, lower)
                .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)))
        }),

        // "deals"/"deal" → damage (via cost_resource, which contains try_parse_damage)
        "deals" | "deal" => {
            parse_cost_resource_ast(text, lower).map(ImperativeFamilyAst::CostResource)
        }

        // "you may" → optional wrapper
        "you" => nom_on_lower(text, lower, |input| value((), tag("you may ")).parse(input)).map(
            |(_, stripped)| ImperativeFamilyAst::YouMay {
                text: stripped.to_string(),
            },
        ),

        // Unknown first word — try position-agnostic parsers that use `contains`/`find`
        // rather than `starts_with`. This handles cases where the verb isn't the first
        // word (e.g., "Lightning Bolt deals 3 damage" after failed subject stripping,
        // "each player gains 2 life" where "each" isn't a verb, or
        // "that player shuffles" where "that" precedes the verb).
        _ => {
            // Damage: try_parse_damage uses lower.find("deals ") — matches anywhere
            if let Some(ast) = parse_cost_resource_ast(text, lower) {
                return Some(ImperativeFamilyAst::CostResource(ast));
            }
            // Numeric: contains("gain")+contains("life"), contains("gets +"), etc.
            if let Some(ast) = parse_numeric_imperative_ast(text, lower) {
                return Some(ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)));
            }
            // Shuffle: "that player shuffles" / "target player shuffles" have
            // non-verb first words but are exact-match shuffle patterns
            if let Some(ast) = parse_shuffle_ast(text, lower) {
                return Some(ImperativeFamilyAst::Shuffle(ast));
            }
            None
        }
    }
}

/// CR 122.1: Parse "get/gets a/an/N [type] counter(s)" into a GivePlayerCounter AST.
/// Handles patterns like:
/// - "get a poison counter"
/// - "gets two experience counters"
/// - "get ten rad counters"
fn try_parse_player_counter(lower: &str) -> Option<ImperativeFamilyAst> {
    // Strip "get/gets " prefix
    let (rest, _) = alt((tag::<_, _, VerboseError<&str>>("gets "), tag("get ")))
        .parse(lower)
        .ok()?;

    // Must end with "counter" or "counters"
    let (before_counter, plural) = if let Some(s) = rest.strip_suffix(" counters") {
        (s, true)
    } else if let Some(s) = rest.strip_suffix(" counter") {
        (s, false)
    } else {
        return None;
    };

    // Parse quantity + counter kind from the remaining text.
    // Patterns: "a poison" / "an experience" / "two rad" / "10 poison"
    let (count, counter_kind) = if let Ok((kind, _)) =
        alt((tag::<_, _, VerboseError<&str>>("a "), tag("an "))).parse(before_counter)
    {
        (1u32, kind.trim())
    } else if let Ok((rest, n)) = nom_primitives::parse_number.parse(before_counter) {
        (n, rest.trim())
    } else {
        return None;
    };

    // Validate: counter kind should be a single word (no spaces) to avoid false positives
    // like "gets +1/+1 counter" which is an object counter, not a player counter.
    if counter_kind.is_empty() || counter_kind.contains('+') || counter_kind.contains('-') {
        return None;
    }

    // CR 122.1b: Map to typed PlayerCounterKind — reject anything that's an object counter.
    // Energy counters are NOT included — they use the dedicated GainEnergy effect.
    let kind = match counter_kind {
        "poison" => PlayerCounterKind::Poison,
        "experience" => PlayerCounterKind::Experience,
        "rad" => PlayerCounterKind::Rad,
        "ticket" => PlayerCounterKind::Ticket,
        _ => return None,
    };

    let _ = plural; // plural is just grammatical, doesn't affect semantics
    Some(ImperativeFamilyAst::GivePlayerCounter {
        counter_kind: kind,
        count: QuantityExpr::Fixed {
            value: count as i32,
        },
    })
}

/// CR 706: Parse die side count from "roll a dN" / "roll a six-sided die" patterns.
fn try_parse_roll_die_sides(lower: &str) -> Option<u8> {
    // "roll a d20", "roll a d6", "roll a d4"
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("roll a d"),
        tag("rolls a d"),
    ))
    .parse(lower)
    .ok()?;
    if let Ok(sides) = rest.parse::<u8>() {
        return Some(sides);
    }
    // Word-form: "roll a six-sided die", "roll a four-sided die"
    if alt((
        tag::<_, _, VerboseError<&str>>("four-sided"),
        tag("4-sided"),
    ))
    .parse(rest)
    .is_ok()
    {
        return Some(4);
    }
    if alt((tag::<_, _, VerboseError<&str>>("six-sided"), tag("6-sided")))
        .parse(rest)
        .is_ok()
    {
        return Some(6);
    }
    if alt((
        tag::<_, _, VerboseError<&str>>("twenty-sided"),
        tag("20-sided"),
    ))
    .parse(rest)
    .is_ok()
    {
        return Some(20);
    }
    None
}

/// CR 706.2: Try to parse a d20 result table line like "1—9 | Draw two cards"
/// or "20 | Search your library for a card". Returns `(min, max, effect_text)`.
pub(crate) fn try_parse_die_result_line(text: &str) -> Option<(u8, u8, &str)> {
    let trimmed = text.trim();

    // Find the pipe separator: "N—M | effect" or "N | effect"
    let (_, (range_part, effect_text)) = nom_primitives::split_once_on(trimmed, " | ").ok()?;
    let range_part = range_part.trim();
    let effect_text = effect_text.trim();

    // Parse range: "1—9" (em dash U+2014), "10—19", "20" (single value)
    let (min, max) = if let Some(dash_idx) = range_part.find('\u{2014}') {
        let min_str = &range_part[..dash_idx];
        let max_str = &range_part[dash_idx + '\u{2014}'.len_utf8()..];
        (min_str.parse::<u8>().ok()?, max_str.parse::<u8>().ok()?)
    } else {
        // Single value like "20"
        let val = range_part.parse::<u8>().ok()?;
        (val, val)
    };

    Some((min, max, effect_text))
}

/// CR 705: Try to parse "if you win the flip, [effect]" / "if you lose the flip, [effect]"
/// from Oracle text. Returns `(is_win, effect_text)`.
pub(crate) fn try_parse_coin_flip_branch(text: &str) -> Option<(bool, &str)> {
    const WIN: &str = "if you win the flip, ";
    const LOSE: &str = "if you lose the flip, ";
    if let Some(prefix) = text.get(..WIN.len()) {
        if prefix.eq_ignore_ascii_case(WIN) {
            return Some((true, &text[WIN.len()..]));
        }
    }
    if let Some(prefix) = text.get(..LOSE.len()) {
        if prefix.eq_ignore_ascii_case(LOSE) {
            return Some((false, &text[LOSE.len()..]));
        }
    }
    None
}

pub(super) fn lower_imperative_family_ast(ast: ImperativeFamilyAst) -> ParsedEffectClause {
    match ast {
        ImperativeFamilyAst::Shuffle(ast) => lower_shuffle_ast(ast),
        // All other arms produce a bare Effect with no sub_ability chain.
        other => parsed_clause(lower_imperative_family_effect(other)),
    }
}

fn lower_imperative_family_effect(ast: ImperativeFamilyAst) -> Effect {
    match ast {
        ImperativeFamilyAst::Structured(ast) => lower_imperative_ast(ast),
        ImperativeFamilyAst::CostResource(ast) => lower_cost_resource_ast(ast),
        ImperativeFamilyAst::ZoneCounter(ast) => lower_zone_counter_ast(ast),
        ImperativeFamilyAst::Explore => Effect::Explore,
        ImperativeFamilyAst::Connive => Effect::Connive {
            target: TargetFilter::Any,
            count: 1,
        },
        ImperativeFamilyAst::PhaseOut => Effect::PhaseOut {
            target: TargetFilter::Any,
        },
        ImperativeFamilyAst::ForceBlock => Effect::ForceBlock {
            target: TargetFilter::Any,
        },
        // CR 701.15a: Goad target creature. Subject injection fills target from parsed text.
        ImperativeFamilyAst::Goad => Effect::Goad {
            target: TargetFilter::Any,
        },
        // CR 701.12a: Exchange control of two permanents. Targets come from ability.targets.
        ImperativeFamilyAst::ExchangeControl => Effect::ExchangeControl,
        // CR 509.1c: Must be blocked — grant transient MustBeBlocked static via GenericEffect.
        // Uses AddStaticMode so the mode propagates through the layer system to
        // static_definitions, where combat.rs checks it.
        ImperativeFamilyAst::MustBeBlocked => {
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::MustBeBlocked)
                    .modifications(vec![ContinuousModification::AddStaticMode {
                        mode: StaticMode::MustBeBlocked,
                    }])],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            }
        }
        ImperativeFamilyAst::Investigate => Effect::Investigate,
        ImperativeFamilyAst::Learn => Effect::Learn,
        ImperativeFamilyAst::ManifestDread => Effect::ManifestDread,
        ImperativeFamilyAst::BecomeMonarch => Effect::BecomeMonarch,
        ImperativeFamilyAst::VentureIntoDungeon => Effect::VentureIntoDungeon,
        ImperativeFamilyAst::VentureIntoUndercity => Effect::VentureInto {
            dungeon: crate::game::dungeon::DungeonId::Undercity,
        },
        ImperativeFamilyAst::TakeTheInitiative => Effect::TakeTheInitiative,
        ImperativeFamilyAst::Proliferate => Effect::Proliferate,
        // CR 701.36a: Populate.
        ImperativeFamilyAst::Populate => Effect::Populate,
        // CR 701.30: Clash with an opponent.
        ImperativeFamilyAst::Clash => Effect::Clash,
        ImperativeFamilyAst::GainKeyword(effect) => effect,
        ImperativeFamilyAst::LoseKeyword(effect) => effect,
        ImperativeFamilyAst::LoseTheGame => Effect::LoseTheGame,
        ImperativeFamilyAst::WinTheGame => Effect::WinTheGame,
        ImperativeFamilyAst::RollDie { sides } => Effect::RollDie {
            sides,
            results: vec![],
        },
        ImperativeFamilyAst::FlipCoin => Effect::FlipCoin {
            win_effect: None,
            lose_effect: None,
        },
        ImperativeFamilyAst::Put(ast) => lower_put_ast(ast),
        ImperativeFamilyAst::YouMay { text } => super::parse_effect(&text),
        // CR 122.1: Player counter manipulation. Target is set by subject injection.
        ImperativeFamilyAst::GivePlayerCounter {
            counter_kind,
            count,
        } => Effect::GivePlayerCounter {
            counter_kind,
            count,
            target: TargetFilter::Controller,
        },
        // Shuffle is handled in `lower_imperative_family_ast` directly.
        ImperativeFamilyAst::Shuffle(_) => unreachable!(),
    }
}

pub(super) fn parse_zone_counter_ast(
    text: &str,
    lower: &str,
    ctx: &ParseContext,
) -> Option<ZoneCounterImperativeAst> {
    if let Some(ast) = parse_destroy_ast(text, lower) {
        return Some(ast);
    }
    if let Some(ast) = parse_exile_ast(text, lower) {
        return Some(ast);
    }
    if let Some(ast) = parse_counter_ast(text, lower) {
        return Some(ast);
    }
    if tag::<_, _, VerboseError<&str>>("put ").parse(lower).is_ok()
        && nom_primitives::scan_contains(lower, "counter")
    {
        // Try move-counters first ("put its counters on ...")
        if let Some((
            Effect::MoveCounters {
                source,
                counter_type,
                target,
            },
            _rem,
        )) = super::counter::try_parse_move_counters(lower, text)
        {
            return Some(ZoneCounterImperativeAst::MoveCounters {
                source,
                counter_type,
                target,
            });
        }
        // Then fixed-count put ("put N counter(s) on ...")
        // Detect "each"/"all" to route to PutCounterAll (mass placement without targeting).
        let is_all = nom_primitives::scan_contains(lower, "counter on each")
            || nom_primitives::scan_contains(lower, "counters on each")
            || nom_primitives::scan_contains(lower, "counter on all")
            || nom_primitives::scan_contains(lower, "counters on all");
        return match try_parse_put_counter(lower, text, ctx) {
            Some((
                Effect::PutCounter {
                    counter_type,
                    count,
                    target,
                },
                _remainder,
                multi_target,
            )) => {
                if is_all && multi_target.is_none() {
                    Some(ZoneCounterImperativeAst::PutCounterAll {
                        counter_type,
                        count,
                        target,
                    })
                } else {
                    Some(ZoneCounterImperativeAst::PutCounter {
                        counter_type,
                        count,
                        target,
                    })
                }
            }
            _ => None,
        };
    }
    if tag::<_, _, VerboseError<&str>>("remove ")
        .parse(lower)
        .is_ok()
        && nom_primitives::scan_contains(lower, "counter")
    {
        return match try_parse_remove_counter(lower, ctx) {
            Some(Effect::RemoveCounter {
                counter_type,
                count,
                target,
            }) => Some(ZoneCounterImperativeAst::RemoveCounter {
                counter_type,
                count,
                target,
            }),
            _ => None,
        };
    }
    None
}

pub(super) fn lower_zone_counter_ast(ast: ZoneCounterImperativeAst) -> Effect {
    match ast {
        ZoneCounterImperativeAst::Destroy { target, all } => {
            if all {
                Effect::DestroyAll {
                    target,
                    cant_regenerate: false,
                }
            } else {
                Effect::Destroy {
                    target,
                    cant_regenerate: false,
                }
            }
        }
        ZoneCounterImperativeAst::Exile {
            origin,
            target,
            all,
        } => {
            if all {
                Effect::ChangeZoneAll {
                    origin,
                    destination: Zone::Exile,
                    target,
                }
            } else {
                Effect::ChangeZone {
                    origin,
                    destination: Zone::Exile,
                    target,
                    owner_library: false,
                    enter_transformed: false,
                    under_your_control: false,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                }
            }
        }
        ZoneCounterImperativeAst::ExileTop { player, count } => Effect::ExileTop { player, count },
        ZoneCounterImperativeAst::Counter {
            target,
            source_static,
            unless_payment,
        } => Effect::Counter {
            target,
            source_static: source_static.map(|s| *s),
            unless_payment,
        },
        ZoneCounterImperativeAst::PutCounter {
            counter_type,
            count,
            target,
        } => Effect::PutCounter {
            counter_type,
            count,
            target,
        },
        ZoneCounterImperativeAst::PutCounterAll {
            counter_type,
            count,
            target,
        } => Effect::PutCounterAll {
            counter_type,
            count,
            target,
        },
        ZoneCounterImperativeAst::RemoveCounter {
            counter_type,
            count,
            target,
        } => Effect::RemoveCounter {
            counter_type,
            count,
            target,
        },
        ZoneCounterImperativeAst::MoveCounters {
            source,
            counter_type,
            target,
        } => Effect::MoveCounters {
            source,
            counter_type,
            target,
        },
    }
}

/// CR 701.47a: Parse "amass {Type} {N}" from Oracle text.
///
/// Handles all subtypes generically. The subtype is canonicalized from plural
/// to singular form (e.g., "Zombies" -> "Zombie") via `parse_subtype`.
fn try_parse_amass(text: &str, lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("amass ")
        .parse(lower)
        .ok()?;
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }

    // Parse subtype from original text (preserving case for parse_subtype)
    let original_rest = text[text.len() - rest.len()..].trim();
    let (subtype, consumed) = crate::parser::oracle_util::parse_subtype(original_rest)?;
    let remainder = rest[consumed..].trim();

    // Parse count: numeric or "X" via shared helper.
    let count = parse_count_expr(remainder)
        .map(|(q, _)| q)
        .unwrap_or(QuantityExpr::Fixed { value: 1 });

    Some(Effect::Amass { subtype, count })
}

/// CR 701.37a: Parse "monstrosity {N}" from Oracle text.
///
/// Used inside activated ability effect text (after the colon).
fn try_parse_monstrosity(lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("monstrosity ")
        .parse(lower)
        .ok()?;
    let rest = rest.trim().trim_end_matches('.');

    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Monstrosity { count })
}

/// CR 701.46a: Parse "adapt N" from Oracle text.
///
/// Used inside activated ability effect text (after the colon).
fn try_parse_adapt(lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("adapt ")
        .parse(lower)
        .ok()?;
    let rest = rest.trim().trim_end_matches('.');

    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Adapt { count })
}

/// CR 509.1b / CR 508.1d: Parse subjectless "can't" clauses that appear in compound effects.
///
/// Handles "can't be blocked [this turn]", "can't attack [this turn]", "can't block [this turn]",
/// and compound forms like "can't attack or block". These delegate to the subject.rs
/// static-granting machinery, wrapping the result in a `GenericEffect`.
fn try_parse_subjectless_cant(lower: &str) -> Option<ImperativeFamilyAst> {
    use crate::parser::oracle_effect::subject::parse_restriction_modes;

    let trimmed = lower.trim_end_matches('.');

    // Determine duration from the suffix: "this combat" → UntilEndOfCombat,
    // "this turn" (or bare) → UntilEndOfTurn.
    let (clean, duration) = if let Some(c) = trimmed.strip_suffix(" this combat") {
        (c, Duration::UntilEndOfCombat)
    } else if let Some(c) = trimmed.strip_suffix(" this turn") {
        (c, Duration::UntilEndOfTurn)
    } else {
        (trimmed, Duration::UntilEndOfTurn)
    };

    let modes = parse_restriction_modes(clean)?;
    let statics: Vec<StaticDefinition> = modes.into_iter().map(StaticDefinition::new).collect();
    Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
        static_abilities: statics,
        duration: Some(duration),
        target: None,
    }))
}

/// CR 701.39a: Parse "bolster N" from Oracle text.
fn try_parse_bolster(lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("bolster ")
        .parse(lower)
        .ok()?;
    let rest = rest.trim().trim_end_matches('.');

    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Bolster { count })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gain_life_equal_to_life_lost() {
        let text = "gain life equal to the life you've lost this turn";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        match result {
            Some(NumericImperativeAst::GainLife { amount }) => {
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::LifeLostThisTurn
                        }
                    ),
                    "Expected LifeLostThisTurn, got {amount:?}"
                );
            }
            other => panic!("Expected GainLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_earthbend_verb() {
        let text = "Earthbend 3 target land";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower);
        assert!(result.is_some(), "Should parse 'earthbend' verb");
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::Animate {
                power,
                toughness,
                is_earthbend,
                keywords,
                ..
            } => {
                assert_eq!(power, Some(3));
                assert_eq!(toughness, Some(3));
                assert!(is_earthbend);
                assert!(keywords.contains(&crate::types::keywords::Keyword::Haste));
            }
            other => panic!("Expected Effect::Animate, got {other:?}"),
        }
    }

    #[test]
    fn parse_airbend_verb() {
        let text = "Airbend target creature {2}";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower);
        assert!(result.is_some(), "Should parse 'airbend' verb");
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::GrantCastingPermission { permission, .. } => {
                assert!(
                    matches!(
                        permission,
                        crate::types::ability::CastingPermission::ExileWithAltCost { ref cost }
                            if matches!(cost, crate::types::mana::ManaCost::Cost { generic: 2, .. })
                    ),
                    "Expected ExileWithAltCost with {{2}}, got {permission:?}"
                );
            }
            other => panic!("Expected Effect::GrantCastingPermission, got {other:?}"),
        }
    }

    #[test]
    fn parse_airbend_up_to_one_other_target_creature_or_spell() {
        let text = "Airbend up to one other target creature or spell {2}";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower).expect("Should parse airbend");
        let effect = lower_targeted_action_ast(result);
        match effect {
            Effect::GrantCastingPermission {
                permission,
                target: crate::types::ability::TargetFilter::Or { filters },
            } => {
                assert!(matches!(
                    permission,
                    crate::types::ability::CastingPermission::ExileWithAltCost { ref cost }
                        if matches!(cost, crate::types::mana::ManaCost::Cost { generic: 2, .. })
                ));
                assert!(
                    filters.iter().any(|filter| matches!(
                        filter,
                        crate::types::ability::TargetFilter::Typed(tf)
                            if tf.type_filters.contains(&crate::types::ability::TypeFilter::Creature)
                                && tf.properties.contains(&crate::types::ability::FilterProp::Another)
                    )),
                    "expected creature branch with Another, got {filters:?}"
                );
                assert!(
                    filters.iter().any(|filter| matches!(
                        filter,
                        crate::types::ability::TargetFilter::Typed(tf)
                            if tf.type_filters.contains(&crate::types::ability::TypeFilter::Card)
                                && tf.properties.contains(&crate::types::ability::FilterProp::Another)
                    )),
                    "expected spell branch with Another, got {filters:?}"
                );
            }
            other => panic!(
                "Expected GrantCastingPermission with creature-or-spell target, got {other:?}"
            ),
        }
    }

    #[test]
    fn parse_earthbend_default_pt() {
        let text = "Earthbend target land";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower);
        assert!(result.is_some());
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::Animate {
                power, toughness, ..
            } => {
                assert_eq!(power, Some(0));
                assert_eq!(toughness, Some(0));
            }
            other => panic!("Expected Effect::Animate, got {other:?}"),
        }
    }

    #[test]
    fn parse_earthbend_no_explicit_target() {
        // After reminder text stripping, "Earthbend 2." has no explicit target.
        // Should default to "target land you control" per keyword definition.
        let text = "Earthbend 2.";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower);
        assert!(
            result.is_some(),
            "Should parse 'earthbend' without explicit target"
        );
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::Animate {
                power,
                toughness,
                is_earthbend,
                target,
                ..
            } => {
                assert_eq!(power, Some(2));
                assert_eq!(toughness, Some(2));
                assert!(is_earthbend);
                assert_eq!(
                    target,
                    default_earthbend_target(),
                    "Should default to land you control"
                );
            }
            other => panic!("Expected Effect::Animate, got {other:?}"),
        }
    }

    #[test]
    fn parse_lose_life_equal_to_mana_value() {
        let text = "loses life equal to that card's mana value";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(result.is_some(), "Should parse 'loses life equal to'");
        match result.unwrap() {
            NumericImperativeAst::LoseLife { amount } => {
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::Ref {
                            qty: QuantityRef::EventContextSourceManaValue
                        }
                    ),
                    "Expected EventContextSourceManaValue, got {amount:?}"
                );
            }
            other => panic!("Expected LoseLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_gain_life_equal_to_power() {
        let text = "gain life equal to its power";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(result.is_some(), "Should parse 'gain life equal to'");
        match result.unwrap() {
            NumericImperativeAst::GainLife { amount } => {
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::Ref {
                            qty: QuantityRef::EventContextSourcePower
                        }
                    ),
                    "Expected EventContextSourcePower, got {amount:?}"
                );
            }
            other => panic!("Expected GainLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_amass_zombies_2() {
        let result = try_parse_amass("amass Zombies 2", "amass zombies 2");
        assert!(result.is_some(), "Should parse 'amass Zombies 2'");
        match result.unwrap() {
            Effect::Amass { subtype, count } => {
                assert_eq!(subtype, "Zombie");
                assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
            }
            other => panic!("Expected Amass, got {other:?}"),
        }
    }

    #[test]
    fn parse_amass_orcs_3() {
        let result = try_parse_amass("amass Orcs 3", "amass orcs 3");
        assert!(result.is_some(), "Should parse 'amass Orcs 3'");
        match result.unwrap() {
            Effect::Amass { subtype, count } => {
                assert_eq!(subtype, "Orc");
                assert!(matches!(count, QuantityExpr::Fixed { value: 3 }));
            }
            other => panic!("Expected Amass, got {other:?}"),
        }
    }

    #[test]
    fn parse_amass_zombies_x() {
        let result = try_parse_amass("amass Zombies X", "amass zombies x");
        assert!(result.is_some(), "Should parse 'amass Zombies X'");
        match result.unwrap() {
            Effect::Amass { subtype, count } => {
                assert_eq!(subtype, "Zombie");
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Variable { .. }
                    }
                ));
            }
            other => panic!("Expected Amass, got {other:?}"),
        }
    }

    #[test]
    fn parse_monstrosity_4() {
        let result = try_parse_monstrosity("monstrosity 4.");
        assert!(result.is_some(), "Should parse 'monstrosity 4.'");
        match result.unwrap() {
            Effect::Monstrosity { count } => {
                assert!(matches!(count, QuantityExpr::Fixed { value: 4 }));
            }
            other => panic!("Expected Monstrosity, got {other:?}"),
        }
    }

    #[test]
    fn parse_monstrosity_x() {
        let result = try_parse_monstrosity("monstrosity x.");
        assert!(result.is_some(), "Should parse 'monstrosity X.'");
        match result.unwrap() {
            Effect::Monstrosity { count } => {
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Variable { .. }
                    }
                ));
            }
            other => panic!("Expected Monstrosity, got {other:?}"),
        }
    }

    #[test]
    fn parse_get_a_poison_counter() {
        let result = try_parse_player_counter("get a poison counter");
        assert!(result.is_some(), "Should parse 'get a poison counter'");
        match result.unwrap() {
            ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            } => {
                assert_eq!(counter_kind, PlayerCounterKind::Poison);
                assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    #[test]
    fn parse_gets_two_experience_counters() {
        let result = try_parse_player_counter("gets two experience counters");
        assert!(
            result.is_some(),
            "Should parse 'gets two experience counters'"
        );
        match result.unwrap() {
            ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            } => {
                assert_eq!(counter_kind, PlayerCounterKind::Experience);
                assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    #[test]
    fn parse_get_three_rad_counters() {
        let result = try_parse_player_counter("get three rad counters");
        assert!(result.is_some(), "Should parse 'get three rad counters'");
        match result.unwrap() {
            ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            } => {
                assert_eq!(counter_kind, PlayerCounterKind::Rad);
                assert!(matches!(count, QuantityExpr::Fixed { value: 3 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    #[test]
    fn parse_get_an_experience_counter() {
        let result = try_parse_player_counter("get an experience counter");
        assert!(result.is_some(), "Should parse 'get an experience counter'");
        match result.unwrap() {
            ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            } => {
                assert_eq!(counter_kind, PlayerCounterKind::Experience);
                assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    #[test]
    fn parse_player_counter_rejects_plus1_counter() {
        // "+1/+1 counter" is an object counter, not a player counter
        let result = try_parse_player_counter("gets a +1/+1 counter");
        assert!(
            result.is_none(),
            "Should NOT parse '+1/+1 counter' as player counter"
        );
    }

    #[test]
    fn parse_player_counter_rejects_unknown_type() {
        // "charge counter" is an object counter, not a known player counter
        let result = try_parse_player_counter("get a charge counter");
        assert!(
            result.is_none(),
            "Should NOT parse unknown counter type as player counter"
        );
    }

    #[test]
    fn parse_additional_combat_phase() {
        let text = "there is an additional combat phase after this phase";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &ParseContext::default());
        assert!(result.is_some(), "Should parse additional combat phase");
        let effect = lower_imperative_family_effect(result.unwrap());
        assert!(
            matches!(
                effect,
                Effect::AdditionalCombatPhase {
                    with_main_phase: false,
                    ..
                }
            ),
            "Expected AdditionalCombatPhase without main phase, got {effect:?}"
        );
    }

    #[test]
    fn parse_additional_combat_with_main_phase() {
        let text = "there is an additional combat phase followed by an additional main phase";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &ParseContext::default());
        assert!(result.is_some(), "Should parse additional combat + main");
        let effect = lower_imperative_family_effect(result.unwrap());
        assert!(
            matches!(
                effect,
                Effect::AdditionalCombatPhase {
                    with_main_phase: true,
                    ..
                }
            ),
            "Expected AdditionalCombatPhase with main phase, got {effect:?}"
        );
    }

    #[test]
    fn parse_after_this_phase_additional_combat() {
        let text = "after this phase, there is an additional combat phase";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &ParseContext::default());
        assert!(result.is_some(), "Should parse 'after this phase' variant");
    }

    #[test]
    fn parse_discard_your_hand() {
        let text = "discard your hand";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower);
        match result {
            Some(TargetedImperativeAst::Discard { count, .. }) => {
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::HandSize
                        }
                    ),
                    "Expected HandSize ref, got {count:?}"
                );
            }
            other => panic!("Expected Discard with HandSize, got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_their_hand() {
        let text = "discard their hand";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower);
        match result {
            Some(TargetedImperativeAst::Discard { count, .. }) => {
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::HandSize
                        }
                    ),
                    "Expected HandSize ref, got {count:?}"
                );
            }
            other => panic!("Expected Discard with HandSize, got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_a_card_regression() {
        let text = "discard a card";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower);
        match result {
            Some(TargetedImperativeAst::Discard { count, .. }) => {
                assert!(
                    matches!(count, QuantityExpr::Fixed { value: 1 }),
                    "Expected Fixed(1), got {count:?}"
                );
            }
            other => panic!("Expected Discard with Fixed(1), got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_two_cards_regression() {
        let text = "discard two cards";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower);
        match result {
            Some(TargetedImperativeAst::Discard { count, .. }) => {
                assert!(
                    matches!(count, QuantityExpr::Fixed { value: 2 }),
                    "Expected Fixed(2), got {count:?}"
                );
            }
            other => panic!("Expected Discard with Fixed(2), got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_unless_creature() {
        let text = "discard two cards unless you discard a creature card";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower);
        match result {
            Some(TargetedImperativeAst::Discard {
                count,
                unless_filter,
                ..
            }) => {
                assert!(
                    matches!(count, QuantityExpr::Fixed { value: 2 }),
                    "Expected Fixed(2), got {count:?}"
                );
                assert!(
                    unless_filter.is_some(),
                    "Expected unless_filter for creature"
                );
            }
            other => panic!("Expected Discard with unless_filter, got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_unless_pirate() {
        let text = "discard two cards unless you discard a Pirate card";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower);
        match result {
            Some(TargetedImperativeAst::Discard {
                count,
                unless_filter,
                ..
            }) => {
                assert!(
                    matches!(count, QuantityExpr::Fixed { value: 2 }),
                    "Expected Fixed(2), got {count:?}"
                );
                assert!(
                    unless_filter.is_some(),
                    "Expected unless_filter for Pirate subtype"
                );
            }
            other => panic!("Expected Discard with unless_filter, got {other:?}"),
        }
    }
}
