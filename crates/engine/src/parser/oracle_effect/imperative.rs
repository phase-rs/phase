use super::counter::{try_parse_double_effect, try_parse_put_counter, try_parse_remove_counter};
use super::mana::{try_parse_activate_only_condition, try_parse_add_mana_effect};
use super::token::try_parse_token;
use super::types::*;
use super::{resolve_it_pronoun, ParseContext};
use crate::parser::oracle_static::parse_continuous_modifications;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, Duration, Effect,
    GainLifePlayer, LibraryPosition, PaymentCost, PreventionAmount, PreventionScope, PtValue,
    QuantityExpr, QuantityRef, RoundingMode, StaticDefinition, TargetFilter, TypedFilter,
};
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::super::oracle_target::parse_target;
use super::super::oracle_util::{
    contains_object_pronoun, contains_possessive, parse_count_expr, parse_mana_symbols,
    parse_number, parse_ordinal, starts_with_possessive, strip_after, TextPair,
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
    let parsed_number = parse_number(lower_rest);
    let (pt, target_text) = parsed_number
        .map(|(n, rem)| (n as i32, rem.trim_start()))
        .unwrap_or((0, lower_rest));
    // Default to "target land you control" when no explicit target remains.
    // Handles: no text, punctuation-only, sequence connectors (", then ..."),
    // variable amounts ("X, where X is..." — parse_number fails), or non-target text.
    let has_explicit_target = if parsed_number.is_none() {
        false
    } else {
        let trimmed =
            target_text.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
        !trimmed.is_empty() && !trimmed.starts_with("then ") && !trimmed.starts_with("and ")
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
    if lower.starts_with("draw ") {
        let count = parse_count_expr(&text[5..])
            .map(|(q, _)| q)
            .unwrap_or(QuantityExpr::Fixed { value: 1 });
        return Some(NumericImperativeAst::Draw { count });
    }

    if lower.contains("gain") && lower.contains("life") {
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
        let after_gain = if lower.starts_with("you gain ") {
            &text[9..]
        } else if lower.starts_with("gain ") {
            &text[5..]
        } else {
            ""
        };
        if !after_gain.is_empty() {
            let amount = parse_count_expr(after_gain)
                .map(|(q, _)| q)
                .unwrap_or(QuantityExpr::Fixed { value: 1 });
            return Some(NumericImperativeAst::GainLife { amount });
        }
    }

    if lower.contains("lose") && lower.contains("life") {
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
        let amount = if let Some(life_pos) = lower.find("life") {
            let before_life = lower[..life_pos].trim();
            let last_word = before_life.split_whitespace().next_back().unwrap_or("");
            parse_count_expr(last_word)
                .map(|(q, _)| q)
                .unwrap_or(QuantityExpr::Fixed { value: 1 })
        } else {
            QuantityExpr::Fixed { value: 1 }
        };
        return Some(NumericImperativeAst::LoseLife { amount });
    }

    if lower.contains("gets +")
        || lower.contains("gets -")
        || lower.contains("get +")
        || lower.contains("get -")
    {
        if let Some(Effect::Pump {
            power,
            toughness,
            target: TargetFilter::Any,
        }) = super::try_parse_pump(lower, text)
        {
            return Some(NumericImperativeAst::Pump { power, toughness });
        }
    }

    if lower.starts_with("scry ") {
        let count = parse_count_expr(&text[5..])
            .map(|(q, _)| q)
            .unwrap_or(QuantityExpr::Fixed { value: 1 });
        return Some(NumericImperativeAst::Scry { count });
    }
    if lower.starts_with("surveil ") {
        let count = parse_count_expr(&text[8..])
            .map(|(q, _)| q)
            .unwrap_or(QuantityExpr::Fixed { value: 1 });
        return Some(NumericImperativeAst::Surveil { count });
    }
    if lower.starts_with("mill ") {
        let count = parse_count_expr(&text[5..])
            .map(|(q, _)| q)
            .unwrap_or(QuantityExpr::Fixed { value: 1 });
        return Some(NumericImperativeAst::Mill { count });
    }

    None
}

/// CR 107.2: Parse "half [possessive] life, rounded up/down" → `HalfRounded` expression.
/// General building block for halving life total expressions.
fn try_parse_half_life_amount(lower: &str) -> Option<QuantityExpr> {
    // Match "lose half their life, rounded up" / "lose half your life, rounded up"
    let after_lose = lower
        .strip_prefix("lose ")
        .or_else(|| lower.strip_prefix("loses "))?
        .trim();
    let after_half = after_lose.strip_prefix("half ")?;

    // Determine whose life total
    let qty =
        if after_half.starts_with("their life") || after_half.starts_with("that player's life") {
            QuantityRef::TargetLifeTotal
        } else if after_half.starts_with("your life") || after_half.starts_with("his or her life") {
            QuantityRef::LifeTotal
        } else {
            return None;
        };

    // Parse rounding direction
    let rounding = if lower.contains("rounded up") {
        RoundingMode::Up
    } else if lower.contains("rounded down") {
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
        NumericImperativeAst::LoseLife { amount } => Effect::LoseLife { amount },
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
        },
    }
}

/// Strip leading "a " / "an " article from target text before passing to `parse_target`.
/// Follows the same pattern used by `oracle_cost.rs` for sacrifice cost parsing.
fn strip_article(text: &str) -> &str {
    let lower = text.to_lowercase();
    if lower.starts_with("a ") {
        &text[2..]
    } else if lower.starts_with("an ") {
        &text[3..]
    } else {
        text
    }
}

/// NOTE: Shares verb prefixes with `try_parse_verb_and_target` in `mod.rs`.
/// When adding a new targeted verb here, check if it also needs to be added there
/// (for compound action splitting like "tap target creature and put a counter on it").
pub(super) fn parse_targeted_action_ast(text: &str, lower: &str) -> Option<TargetedImperativeAst> {
    if lower.starts_with("tap ") {
        let (target, _rem) = parse_target(strip_article(&text[4..]));
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::Tap { target });
    }
    if lower.starts_with("untap ") {
        let (target, _rem) = parse_target(strip_article(&text[6..]));
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::Untap { target });
    }
    if lower.starts_with("sacrifice ") {
        let (target, _rem) = parse_target(strip_article(&text[10..]));
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::Sacrifice { target });
    }
    if let Some(after_discard) = lower.strip_prefix("discard ") {
        // Detect whole-hand discard patterns before falling through to count parsing.
        // Uses starts_with (not contains) to avoid matching "discard a card from your hand".
        if after_discard.starts_with("your hand")
            || after_discard.starts_with("their hand")
            || after_discard.starts_with("his or her hand")
        {
            return Some(TargetedImperativeAst::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize,
                },
            });
        }
        let original_after = &text[text.len() - after_discard.len()..];
        let count = parse_count_expr(original_after)
            .map(|(q, _)| q)
            .unwrap_or(QuantityExpr::Fixed { value: 1 });
        return Some(TargetedImperativeAst::Discard { count });
    }
    if lower.starts_with("return ") {
        let rest = &text[7..];
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
    if lower.starts_with("fight ") {
        let (target, _rem) = parse_target(&text[6..]);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::Fight { target });
    }
    if lower.starts_with("gain control of ") {
        let (target, _rem) = parse_target(&text[16..]);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::GainControl { target });
    }
    // Earthbend: "earthbend [N] [target <type>]" → Animate with haste + is_earthbend
    if let Some(rest) = lower.strip_prefix("earthbend ") {
        let (target, power, toughness) = parse_earthbend_params(text, rest);
        return Some(TargetedImperativeAst::Earthbend {
            target,
            power,
            toughness,
        });
    }
    // Airbend: "airbend target <type> <mana_cost>" → GrantCastingPermission(ExileWithAltCost)
    if let Some(rest) = lower.strip_prefix("airbend ") {
        let original_rest = &text[text.len() - rest.len()..];
        let (target, after_target) = parse_target(original_rest);
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
        TargetedImperativeAst::Sacrifice { target } => Effect::Sacrifice { target },
        TargetedImperativeAst::Discard { count } => Effect::Discard {
            count,
            target: TargetFilter::Any,
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
    if lower.starts_with("seek ") {
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
    if lower.starts_with("look at the top ") {
        let count = parse_number(&text[16..]).map(|(n, _)| n).unwrap_or(1);
        return Some(SearchCreationImperativeAst::Dig { count });
    }
    if lower.starts_with("create ") {
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
        SearchCreationImperativeAst::Dig { count } => Effect::Dig {
            count,
            destination: None,
            keep_count: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: None,
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
            attach_to: token.attach_to,
            enters_attacking: false,
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
    if lower.starts_with("look at ") && lower.contains("hand") {
        if contains_possessive(lower, "look at", "hand") {
            // CR 603.7c: "that player's hand" resolves to the player from the triggering event.
            let target = if lower.contains("that player's hand") {
                TargetFilter::TriggeringPlayer
            } else {
                TargetFilter::Any
            };
            return Some(HandRevealImperativeAst::LookAtHand { target });
        }

        let after_look_at = &text[8..];
        let (target, _) = parse_target(after_look_at);
        return Some(HandRevealImperativeAst::LookAtHand { target });
    }

    if !lower.starts_with("reveal ") && !lower.starts_with("reveals ") {
        return None;
    }

    // CR 701.20a: "reveals a number of cards from their hand equal to X"
    if lower.contains("hand") && lower.contains("equal to ") {
        if let Some((_, qty_text)) = lower.split_once("equal to ") {
            let qty_text = qty_text.trim_end_matches('.');
            if let Some(qty) = super::super::oracle_quantity::parse_quantity_ref(qty_text) {
                return Some(HandRevealImperativeAst::RevealPartialHand {
                    count: crate::types::ability::QuantityExpr::Ref { qty },
                });
            }
        }
    }

    // Check for "the top [N] card(s) of [their/your] library" BEFORE the catch-all
    // "hand" check — text like "reveals the top card...then puts it into their hand"
    // contains "hand" as a destination, not as the reveal source.
    if lower.contains("the top ") && lower.contains("librar") {
        let count = if let Some(pos) = lower.find("the top ") {
            let after_top = &lower[pos + 8..];
            parse_number(after_top).map(|(n, _)| n).unwrap_or(1)
        } else {
            1
        };
        return Some(HandRevealImperativeAst::RevealTop { count });
    }

    if lower.contains("hand") {
        return Some(HandRevealImperativeAst::RevealHand);
    }

    // Fallback: reveal from top of library without explicit "library" mention
    let count = if let Some(pos) = lower.find("the top ") {
        let after_top = &lower[pos + 8..];
        parse_number(after_top).map(|(n, _)| n).unwrap_or(1)
    } else {
        1
    };
    Some(HandRevealImperativeAst::RevealTop { count })
}

pub(super) fn lower_hand_reveal_ast(ast: HandRevealImperativeAst) -> Effect {
    match ast {
        HandRevealImperativeAst::LookAtHand { target } => Effect::RevealHand {
            target,
            card_filter: TargetFilter::Any,
            count: None,
        },
        HandRevealImperativeAst::RevealHand => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter: TargetFilter::Any,
            count: None,
        },
        HandRevealImperativeAst::RevealPartialHand { count } => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter: TargetFilter::Any,
            count: Some(count),
        },
        HandRevealImperativeAst::RevealTop { count } => Effect::RevealTop {
            player: TargetFilter::Controller,
            count,
        },
    }
}

pub(super) fn parse_choose_ast(text: &str, lower: &str) -> Option<ChooseImperativeAst> {
    if let Some(rest) = lower.strip_prefix("choose ") {
        if super::is_choose_as_targeting(rest) {
            let stripped = &text["choose ".len()..];
            let inner = super::parse_effect(stripped);
            if !matches!(inner, Effect::Unimplemented { .. }) {
                return Some(ChooseImperativeAst::Reparse {
                    text: stripped.to_string(),
                });
            }
            let (target, _) = parse_target(stripped);
            return Some(ChooseImperativeAst::TargetOnly { target });
        }
    }

    if let Some(choice_type) = super::try_parse_named_choice(lower) {
        return Some(ChooseImperativeAst::NamedChoice { choice_type });
    }

    if lower.starts_with("choose ") && lower.contains("card from it") {
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
    if lower.starts_with("prevent ") {
        return Some(UtilityImperativeAst::Prevent {
            text: text.to_string(),
        });
    }
    if lower.starts_with("regenerate ") {
        return Some(UtilityImperativeAst::Regenerate {
            text: text.to_string(),
        });
    }
    if lower.starts_with("copy ") {
        let (target, _rem) = parse_target(&text[5..]);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(UtilityImperativeAst::Copy { target });
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
    if lower.starts_with("transform ") {
        let rest = &text["transform ".len()..];
        let (target, _) = parse_target(rest);
        if !matches!(target, TargetFilter::Any) {
            return Some(UtilityImperativeAst::Transform { target });
        }
    }
    if lower.starts_with("attach ") {
        let tp = TextPair::new(text, lower);
        let after_to = tp
            .strip_after(" to ")
            .map(|tp| tp.original)
            .unwrap_or(&text[7..]);
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
            let rest = lower.strip_prefix("regenerate ").unwrap_or(&lower);
            let (target, _) = parse_target(rest);
            Effect::Regenerate { target }
        }
        UtilityImperativeAst::Copy { target } => Effect::CopySpell { target },
        UtilityImperativeAst::Transform { target } => Effect::Transform { target },
        UtilityImperativeAst::Attach { target } => Effect::Attach { target },
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
    let rest = lower.strip_prefix("prevent ").unwrap_or(&lower);

    // Determine scope: combat damage only vs all damage
    let scope = if rest.contains("combat damage") {
        PreventionScope::CombatDamage
    } else {
        PreventionScope::AllDamage
    };

    // Determine amount: "all damage" vs "the next N damage"
    let amount = if rest.starts_with("all ") {
        PreventionAmount::All
    } else if let Some(after_next) = rest.strip_prefix("the next ") {
        let n = parse_number(after_next).map(|(n, _)| n).unwrap_or(1);
        PreventionAmount::Next(n)
    } else {
        // Fallback: try to extract a number
        let n = parse_number(rest).map(|(n, _)| n).unwrap_or(1);
        PreventionAmount::Next(n)
    };

    // Determine target
    let target = if rest.contains("any target") {
        TargetFilter::Any
    } else if rest.contains("target creature") || rest.contains("target permanent") {
        // Extract the target from the text
        let tp = TextPair::new(text, &lower);
        if let Some(from_target) = tp.find("target ").map(|pos| tp.split_at(pos).1) {
            let (t, _) = parse_target(from_target.original);
            t
        } else {
            TargetFilter::Any
        }
    } else if rest.contains("to you") || rest.contains("to its controller") {
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
    if !lower.starts_with("put ") {
        return None;
    }

    if lower.starts_with("put the top ") && lower.contains("graveyard") {
        let after = &lower[12..];
        let count = parse_number(after).map(|(n, _)| n).unwrap_or(1);
        return Some(PutImperativeAst::Mill { count });
    }

    // CR 701.24g: "put X on top of Y's library" — specific position, no auto-shuffle.
    // Must check before try_parse_put_zone_change which would emit ChangeZone (auto-shuffles).
    // Only matches forms WITHOUT an explicit origin zone ("from your hand") — those
    // specify a real zone transfer and should go through try_parse_put_zone_change.
    if lower.starts_with("put ") && lower.contains("on top of") && lower.contains("library") {
        let has_origin = lower.contains(" from ");
        if !has_origin {
            return Some(PutImperativeAst::TopOfLibrary);
        }
    }

    // CR 701.24g: "put that card on top" / "put it on top" / "put them on top" —
    // abbreviated form used after "shuffle" in search-and-put-on-top tutors (41 cards).
    if lower.starts_with("put ") && lower.ends_with("on top") {
        return Some(PutImperativeAst::TopOfLibrary);
    }

    // CR 701.24g: "put X on the bottom of Y's library" — specific position without
    // explicit origin zone. Forms with "from" (e.g. "from your hand") go through
    // try_parse_put_zone_change for proper ChangeZone handling.
    if lower.starts_with("put ") && lower.contains("on the bottom of") && lower.contains("library")
    {
        let has_origin = lower.contains(" from ");
        if !has_origin {
            return Some(PutImperativeAst::BottomOfLibrary);
        }
    }

    // CR 701.24g: "put that card on the bottom" / "put it on the bottom" —
    // abbreviated form without "of Y's library".
    if lower.starts_with("put ") && lower.ends_with("on the bottom") {
        return Some(PutImperativeAst::BottomOfLibrary);
    }

    // CR 701.24g: "put X into Y's library Nth from the top" —
    // specific positional placement (God-Eternals, Approach, Bury in Books).
    if lower.starts_with("put ") && lower.contains("from the top") {
        if let Some(pos) = lower.find("from the top") {
            // Look backwards from "from the top" to find the ordinal
            let before = lower[..pos].trim_end();
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
        ..
    }) = super::try_parse_put_zone_change(lower, text)
    {
        return Some(PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
            under_your_control,
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
        },
        PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
            under_your_control,
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
                    enter_tapped: false,
                    enters_attacking: false,
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
    let rest = lower.strip_prefix("put that many ")?;
    // Next word(s) are counter type: "+1/+1", "charge", "loyalty", etc.
    let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let raw_type = &rest[..type_end];
    let counter_type = super::counter::normalize_counter_type(raw_type);

    // Skip "counter" or "counters" keyword
    let after_type = rest[type_end..].trim_start();
    let after_counter = after_type
        .strip_prefix("counters")
        .or_else(|| after_type.strip_prefix("counter"))
        .unwrap_or(after_type)
        .trim_start();

    // Parse target after "on"
    let target = if let Some(on_rest) = after_counter.strip_prefix("on ") {
        if on_rest.starts_with("~") || on_rest.starts_with("this ") {
            TargetFilter::SelfRef
        } else if on_rest.starts_with("it") || on_rest.starts_with("itself") {
            // CR 608.2k: Bare pronoun — context-dependent
            resolve_it_pronoun(ctx)
        } else {
            let (t, _) = parse_target(on_rest);
            t
        }
    } else {
        TargetFilter::SelfRef
    };

    // count=0 signals "that many" — engine resolver reads from event context
    Some(Effect::PutCounter {
        counter_type,
        count: 0,
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
    if lower.contains("shuffle the rest") || lower.contains("shuffle them") {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Controller,
        });
    }
    if matches!(lower, "that player shuffles" | "target player shuffles") {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Player,
        });
    }
    if !lower.starts_with("shuffle") || !lower.contains("library") {
        return None;
    }

    // "shuffle {possessive} library" — extract the possessive word to determine the target.
    // Only matches the exact form "shuffle your library" / "shuffle their library" etc.;
    // compound forms like "shuffle your graveyard into your library" fall through.
    if let Some(possessive) = lower
        .strip_prefix("shuffle ")
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
    if lower.starts_with("shuffle ")
        && lower.contains(" into ")
        && lower.contains("library")
        && lower.contains(" from ")
    {
        let after_shuffle = &text["shuffle ".len()..];
        let (target, _) = parse_target(after_shuffle);
        let origin = if lower.contains("graveyard") {
            Some(Zone::Graveyard)
        } else if lower.contains("from your hand") {
            Some(Zone::Hand)
        } else if lower.contains("from exile") {
            Some(Zone::Exile)
        } else {
            None
        };
        return Some(ShuffleImperativeAst::TargetedChangeZoneToLibrary { target, origin });
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
    if lower.starts_with("destroy all ") || lower.starts_with("destroy each ") {
        let (target, _rem) = parse_target(&text[8..]);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(ZoneCounterImperativeAst::Destroy { target, all: true });
    }
    if lower.starts_with("destroy ") {
        let (target, _rem) = parse_target(&text[8..]);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        return Some(ZoneCounterImperativeAst::Destroy { target, all: false });
    }
    None
}

pub(super) fn parse_exile_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    if lower.starts_with("exile all ") || lower.starts_with("exile each ") {
        let rest_lower = &lower[6..]; // after "exile "
        let (target, _rem) = parse_target(&text[6..]);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        let origin = super::infer_origin_zone(rest_lower);
        return Some(ZoneCounterImperativeAst::Exile {
            origin,
            target,
            all: true,
        });
    }

    let rest_lower = lower.strip_prefix("exile ")?;
    let (target, _rem) = parse_target(&text[6..]);
    #[cfg(debug_assertions)]
    super::types::assert_no_compound_remainder(_rem, text);
    let origin = super::infer_origin_zone(rest_lower);
    Some(ZoneCounterImperativeAst::Exile {
        origin,
        target,
        all: false,
    })
}

pub(super) fn parse_counter_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    let rest = lower.strip_prefix("counter ")?;
    if rest.contains("activated or triggered ability") {
        // CR 118.12: Parse "unless pays" even for ability counters.
        let unless_payment = super::parse_unless_payment(rest);
        return Some(ZoneCounterImperativeAst::Counter {
            target: TargetFilter::StackAbility,
            source_static: None,
            unless_payment,
        });
    }

    let (target, _rem) = parse_target(&text[8..]);
    #[cfg(debug_assertions)]
    super::types::assert_no_compound_remainder(_rem, text);
    let target = if rest.contains("spell") {
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
    if let Some(rest) = lower.strip_prefix("pay ") {
        // "pay N life" → PaymentCost::Life (CR 118.2)
        if let Some(life_rest) = rest.strip_suffix(" life") {
            if let Some((n, _)) = parse_number(life_rest) {
                return Some(CostResourceImperativeAst::Pay {
                    cost: PaymentCost::Life { amount: n },
                });
            }
        }
        // "pay {2}{B}" → PaymentCost::Mana (CR 117.1)
        let offset = text.len() - rest.len();
        let rest_original = &text[offset..];
        if let Some((mana_cost, _)) = parse_mana_symbols(rest_original.trim()) {
            return Some(CostResourceImperativeAst::Pay {
                cost: PaymentCost::Mana { cost: mana_cost },
            });
        }
    }
    if lower.starts_with("add ") {
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
    if lower.contains("additional combat phase") {
        let with_main = lower.contains("followed by an additional main phase");
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
        "draw" if lower.contains("that many") => {
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

        // Utility verbs (CR 615, CR 701.19, CR 701.6)
        "prevent" | "regenerate" | "copy" | "attach" => parse_utility_imperative_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Utility(ast))),
        "transform" | "transforms" => parse_utility_imperative_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Utility(ast))),

        // Shuffle (CR 701.19)
        "shuffle" | "shuffles" => parse_shuffle_ast(text, lower).map(ImperativeFamilyAst::Shuffle),

        // Reveal / look at hand (CR 701.16)
        "reveal" => parse_hand_reveal_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::HandReveal(ast))),

        // Choose (CR 700.2)
        "choose" => parse_choose_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Choose(ast))),

        // ── Exact-match keyword actions ──
        "explore" => Some(ImperativeFamilyAst::Explore),
        // CR 702.162a: "connive" / "connives"
        "connive" | "connives" => Some(ImperativeFamilyAst::Connive),
        // CR 702.136: "investigate"
        "investigate" => Some(ImperativeFamilyAst::Investigate),
        // CR 701.62a: "manifest dread"
        "manifest" => {
            if lower == "manifest dread" {
                Some(ImperativeFamilyAst::ManifestDread)
            } else {
                None
            }
        }
        "proliferate" => Some(ImperativeFamilyAst::Proliferate),
        // CR 702.157a: "suspect it" / "suspect target creature"
        "suspect" | "suspects" => Some(ImperativeFamilyAst::GainKeyword(Effect::Suspect {
            target: crate::types::ability::TargetFilter::ParentTarget,
        })),
        // Blight N as an effect (e.g. trigger effect "blight 1")
        "blight" => {
            let rest = lower
                .strip_prefix("blight ")
                .unwrap_or(lower.strip_prefix("blight").unwrap_or(""));
            let count = super::super::oracle_util::parse_number(rest.trim())
                .map(|(n, _)| n)
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
            if let Some(rest) = lower.strip_prefix("collect evidence ") {
                let count = super::super::oracle_util::parse_number(rest.trim())
                    .map(|(n, _)| n)
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
            let rest = lower
                .strip_prefix("endure ")
                .or_else(|| lower.strip_prefix("endures "))
                .unwrap_or("");
            let count = super::super::oracle_util::parse_number(rest.trim())
                .map(|(n, _)| n)
                .unwrap_or(1);
            Some(ImperativeFamilyAst::GainKeyword(Effect::Endure {
                amount: count,
            }))
        }
        // CR 701.47a: "amass [Type] N"
        "amass" => try_parse_amass(text, lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.37a: "monstrosity N"
        "monstrosity" => try_parse_monstrosity(lower).map(ImperativeFamilyAst::GainKeyword),
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
        // CR 500.7: "take an extra turn after this one"
        "take" | "takes" => {
            if lower.contains("extra turn") {
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
        "goad" | "goads" => Some(ImperativeFamilyAst::Goad),

        // CR 701.12a: "exchange control of target [type] and target [type]"
        "exchange" => {
            if lower.starts_with("exchange control of ") {
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
            if lower.ends_with("this turn if able") || lower.ends_with("this combat if able") {
                Some(ImperativeFamilyAst::ForceBlock)
            } else {
                None
            }
        }
        // CR 509.1c: "must be blocked [this turn] [if able]"
        "must" => {
            if let Some(rest) = lower.strip_prefix("must be blocked") {
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
        "put" if lower.contains("that many") && lower.contains("counter") => {
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
            if lower.contains("control of") {
                parse_targeted_action_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Targeted(ast)))
            } else if lower.contains("life") {
                parse_numeric_imperative_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)))
            } else {
                // CR 702: keyword granting
                try_parse_gain_keyword(text).map(ImperativeFamilyAst::GainKeyword)
            }
        }

        // "lose" → "lose the game" (step 6) → "lose life" (step 3) → keyword (step 7)
        "lose" | "loses" => {
            if lower.contains("the game") {
                Some(ImperativeFamilyAst::LoseTheGame)
            } else if lower.contains("life") {
                parse_numeric_imperative_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)))
            } else if !lower.contains("mana") {
                try_parse_gain_keyword(text).map(ImperativeFamilyAst::LoseKeyword)
            } else {
                None
            }
        }

        // CR 104.3a: "win the game"
        "win" | "wins" => {
            if lower.contains("the game") {
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
        "you" => text
            .strip_prefix("you may ")
            .map(|stripped| ImperativeFamilyAst::YouMay {
                text: stripped.to_string(),
            }),

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
    let rest = lower
        .strip_prefix("gets ")
        .or_else(|| lower.strip_prefix("get "))?;

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
    let (count, counter_kind) = if let Some(kind) = before_counter.strip_prefix("a ") {
        (1u32, kind.trim())
    } else if let Some(kind) = before_counter.strip_prefix("an ") {
        (1u32, kind.trim())
    } else if let Some((n, rest)) = parse_number(before_counter) {
        (n, rest.trim())
    } else {
        return None;
    };

    // Validate: counter kind should be a single word (no spaces) to avoid false positives
    // like "gets +1/+1 counter" which is an object counter, not a player counter.
    if counter_kind.is_empty() || counter_kind.contains('+') || counter_kind.contains('-') {
        return None;
    }

    // Known player counter types — reject anything that's clearly an object counter.
    // Energy counters are NOT included — they use the dedicated GainEnergy effect.
    let known_player_counters = ["poison", "experience", "rad", "ticket"];

    // Only match known player counter types to avoid capturing object counter patterns
    if !known_player_counters.contains(&counter_kind) {
        return None;
    }

    let _ = plural; // plural is just grammatical, doesn't affect semantics
    Some(ImperativeFamilyAst::GivePlayerCounter {
        counter_kind: counter_kind.to_string(),
        count: QuantityExpr::Fixed {
            value: count as i32,
        },
    })
}

/// CR 706: Parse die side count from "roll a dN" / "roll a six-sided die" patterns.
fn try_parse_roll_die_sides(lower: &str) -> Option<u8> {
    // "roll a d20", "roll a d6", "roll a d4"
    let rest = lower
        .strip_prefix("roll a d")
        .or_else(|| lower.strip_prefix("rolls a d"))?;
    if let Ok(sides) = rest.parse::<u8>() {
        return Some(sides);
    }
    // Word-form: "roll a six-sided die", "roll a four-sided die"
    match rest {
        _ if rest.starts_with("four-sided") || rest.starts_with("4-sided") => Some(4),
        _ if rest.starts_with("six-sided") || rest.starts_with("6-sided") => Some(6),
        _ if rest.starts_with("twenty-sided") || rest.starts_with("20-sided") => Some(20),
        _ => None,
    }
}

/// CR 706.2: Try to parse a d20 result table line like "1—9 | Draw two cards"
/// or "20 | Search your library for a card". Returns `(min, max, effect_text)`.
pub(crate) fn try_parse_die_result_line(text: &str) -> Option<(u8, u8, &str)> {
    let trimmed = text.trim();

    // Find the pipe separator: "N—M | effect" or "N | effect"
    let pipe_idx = trimmed.find(" | ")?;
    let range_part = trimmed[..pipe_idx].trim();
    let effect_text = trimmed[pipe_idx + 3..].trim();

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
        ImperativeFamilyAst::ManifestDread => Effect::ManifestDread,
        ImperativeFamilyAst::BecomeMonarch => Effect::BecomeMonarch,
        ImperativeFamilyAst::Proliferate => Effect::Proliferate,
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
    if lower.starts_with("put ") && lower.contains("counter") {
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
        return match try_parse_put_counter(lower, text, ctx) {
            Some((
                Effect::PutCounter {
                    counter_type,
                    count,
                    target,
                },
                _remainder,
                _multi_target,
            )) => Some(ZoneCounterImperativeAst::PutCounter {
                counter_type,
                count,
                target,
            }),
            _ => None,
        };
    }
    if lower.starts_with("remove ") && lower.contains("counter") {
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
                }
            }
        }
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
    let rest = lower.strip_prefix("amass ")?.trim();
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
    let rest = lower
        .strip_prefix("monstrosity ")?
        .trim()
        .trim_end_matches('.');

    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Monstrosity { count })
}

#[cfg(test)]
mod tests {
    use super::*;

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
                assert_eq!(counter_kind, "poison");
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
                assert_eq!(counter_kind, "experience");
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
                assert_eq!(counter_kind, "rad");
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
                assert_eq!(counter_kind, "experience");
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
            Some(TargetedImperativeAst::Discard { count }) => {
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
            Some(TargetedImperativeAst::Discard { count }) => {
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
            Some(TargetedImperativeAst::Discard { count }) => {
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
            Some(TargetedImperativeAst::Discard { count }) => {
                assert!(
                    matches!(count, QuantityExpr::Fixed { value: 2 }),
                    "Expected Fixed(2), got {count:?}"
                );
            }
            other => panic!("Expected Discard with Fixed(2), got {other:?}"),
        }
    }
}
