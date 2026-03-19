use super::counter::{try_parse_put_counter, try_parse_remove_counter};
use super::mana::{try_parse_activate_only_condition, try_parse_add_mana_effect};
use super::token::try_parse_token;
use super::types::*;
use crate::parser::oracle_static::parse_continuous_modifications;
use crate::types::ability::{
    ContinuousModification, Duration, Effect, GainLifePlayer, PaymentCost, PreventionAmount,
    PreventionScope, PtValue, QuantityExpr, StaticDefinition, TargetFilter,
};
use crate::types::zones::Zone;

use super::super::oracle_target::parse_target;
use super::super::oracle_util::{
    contains_object_pronoun, contains_possessive, parse_mana_symbols, parse_number,
    starts_with_possessive,
};

pub(super) fn parse_numeric_imperative_ast(
    text: &str,
    lower: &str,
) -> Option<NumericImperativeAst> {
    if lower.starts_with("draw ") {
        let count = parse_number(&text[5..]).map(|(n, _)| n).unwrap_or(1);
        return Some(NumericImperativeAst::Draw { count });
    }

    if lower.contains("gain") && lower.contains("life") {
        let after_gain = if lower.starts_with("you gain ") {
            &text[9..]
        } else if lower.starts_with("gain ") {
            &text[5..]
        } else {
            ""
        };
        if !after_gain.is_empty() {
            let amount = parse_number(after_gain).map(|(n, _)| n as i32).unwrap_or(1);
            return Some(NumericImperativeAst::GainLife { amount });
        }
    }

    if lower.contains("lose") && lower.contains("life") {
        let amount = super::extract_number_before(lower, "life").unwrap_or(1) as i32;
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
        let count = parse_number(&text[5..]).map(|(n, _)| n).unwrap_or(1);
        return Some(NumericImperativeAst::Scry { count });
    }
    if lower.starts_with("surveil ") {
        let count = parse_number(&text[8..]).map(|(n, _)| n).unwrap_or(1);
        return Some(NumericImperativeAst::Surveil { count });
    }
    if lower.starts_with("mill ") {
        let count = parse_number(&text[5..]).map(|(n, _)| n).unwrap_or(1);
        return Some(NumericImperativeAst::Mill { count });
    }

    None
}

pub(super) fn lower_numeric_imperative_ast(ast: NumericImperativeAst) -> Effect {
    match ast {
        NumericImperativeAst::Draw { count } => Effect::Draw {
            count: QuantityExpr::Fixed {
                value: count as i32,
            },
        },
        NumericImperativeAst::GainLife { amount } => Effect::GainLife {
            amount: QuantityExpr::Fixed { value: amount },
            player: GainLifePlayer::Controller,
        },
        NumericImperativeAst::LoseLife { amount } => Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: amount },
        },
        NumericImperativeAst::Pump { power, toughness } => Effect::Pump {
            power,
            toughness,
            target: TargetFilter::Any,
        },
        NumericImperativeAst::Scry { count } => Effect::Scry { count },
        NumericImperativeAst::Surveil { count } => Effect::Surveil { count },
        NumericImperativeAst::Mill { count } => Effect::Mill {
            count: QuantityExpr::Fixed {
                value: count as i32,
            },
            target: TargetFilter::Any,
        },
    }
}

pub(super) fn parse_targeted_action_ast(text: &str, lower: &str) -> Option<TargetedImperativeAst> {
    if lower.starts_with("tap ") {
        let (target, _) = parse_target(&text[4..]);
        return Some(TargetedImperativeAst::Tap { target });
    }
    if lower.starts_with("untap ") {
        let (target, _) = parse_target(&text[6..]);
        return Some(TargetedImperativeAst::Untap { target });
    }
    if lower.starts_with("sacrifice ") {
        let (target, _) = parse_target(&text[10..]);
        return Some(TargetedImperativeAst::Sacrifice { target });
    }
    if lower.starts_with("discard ") {
        let count = parse_number(&text[8..]).map(|(n, _)| n).unwrap_or(1);
        return Some(TargetedImperativeAst::Discard { count });
    }
    if lower.starts_with("return ") {
        let rest = &text[7..];
        let (target_text, destination) = super::strip_return_destination(rest);
        let (target, _) = parse_target(target_text);
        return match destination {
            Some(Zone::Battlefield) => Some(TargetedImperativeAst::ReturnToBattlefield { target }),
            _ => Some(TargetedImperativeAst::Return { target }),
        };
    }
    if lower.starts_with("fight ") {
        let (target, _) = parse_target(&text[6..]);
        return Some(TargetedImperativeAst::Fight { target });
    }
    if lower.starts_with("gain control of ") {
        let (target, _) = parse_target(&text[16..]);
        return Some(TargetedImperativeAst::GainControl { target });
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
        TargetedImperativeAst::ReturnToBattlefield { target } => Effect::ChangeZone {
            origin: None,
            destination: Zone::Battlefield,
            target,
            owner_library: false,
        },
        TargetedImperativeAst::Fight { target } => Effect::Fight { target },
        TargetedImperativeAst::GainControl { target } => Effect::GainControl { target },
        TargetedImperativeAst::ZoneCounterProxy(ast) => lower_zone_counter_ast(*ast),
    }
}

pub(super) fn parse_search_and_creation_ast(
    text: &str,
    lower: &str,
) -> Option<SearchCreationImperativeAst> {
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
            Some(Effect::CopySpell { target }) => {
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
            }) => Some(SearchCreationImperativeAst::Token {
                token: TokenDescription {
                    name,
                    power: Some(power),
                    toughness: Some(toughness),
                    types,
                    colors,
                    keywords,
                    tapped,
                    count,
                    attach_to,
                },
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
        },
        SearchCreationImperativeAst::CopyTokenOf { target } => Effect::CopySpell { target },
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
        },
    }
}

pub(super) fn parse_hand_reveal_ast(text: &str, lower: &str) -> Option<HandRevealImperativeAst> {
    if lower.starts_with("look at ") && lower.contains("hand") {
        if contains_possessive(lower, "look at", "hand") {
            return Some(HandRevealImperativeAst::LookAtHand {
                target: TargetFilter::Any,
            });
        }

        let after_look_at = &text[8..];
        let (target, _) = parse_target(after_look_at);
        return Some(HandRevealImperativeAst::LookAtHand { target });
    }

    if !lower.starts_with("reveal ") {
        return None;
    }

    if lower.contains("hand") {
        return Some(HandRevealImperativeAst::RevealHand);
    }

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
        },
        HandRevealImperativeAst::RevealHand => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter: TargetFilter::Any,
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
        let (target, _) = parse_target(&text[5..]);
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
        let to_pos = lower.find(" to ").map(|p| p + 4).unwrap_or(7);
        let (target, _) = parse_target(&text[to_pos..]);
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
        if let Some(pos) = lower.find("target ") {
            let (t, _) = parse_target(&text[pos..]);
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

    if let Some(Effect::ChangeZone {
        origin,
        destination,
        target,
        ..
    }) = super::try_parse_put_zone_change(lower, text)
    {
        return Some(PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
        });
    }

    if lower.starts_with("put ") && lower.contains("on top of") && lower.contains("library") {
        return Some(PutImperativeAst::TopOfLibrary);
    }

    None
}

pub(super) fn lower_put_ast(ast: PutImperativeAst) -> Effect {
    match ast {
        PutImperativeAst::Mill { count } => Effect::Mill {
            count: QuantityExpr::Fixed {
                value: count as i32,
            },
            target: TargetFilter::Any,
        },
        PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
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
                }
            }
        }
        PutImperativeAst::TopOfLibrary => Effect::ChangeZone {
            origin: None,
            destination: Zone::Library,
            target: TargetFilter::Any,
            owner_library: false,
        },
    }
}

pub(super) fn parse_shuffle_ast(text: &str, lower: &str) -> Option<ShuffleImperativeAst> {
    if matches!(lower, "shuffle" | "then shuffle") {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Controller,
        });
    }
    // "shuffle ... and put that card on top" / "shuffle ... and put it on top"
    // Compound pattern: shuffle library, then place the searched card on top.
    if lower.starts_with("shuffle")
        && (lower.contains("put that card on top")
            || lower.contains("put it on top")
            || lower.contains("put the card on top"))
    {
        return Some(ShuffleImperativeAst::ShuffleAndPutOnTop);
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

    Some(ShuffleImperativeAst::Unimplemented {
        text: text.to_string(),
    })
}

pub(super) fn lower_shuffle_ast(ast: ShuffleImperativeAst) -> Effect {
    match ast {
        ShuffleImperativeAst::ShuffleLibrary { target } => Effect::Shuffle { target },
        // "shuffle and put that card on top" — the shuffle itself. The "put on top"
        // is handled via sub_ability chaining at the SearchLibrary destination level,
        // so here we just emit a Shuffle. The parent SearchLibrary chain will place
        // the found card via ChangeZone { destination: Library } in the sub_ability.
        ShuffleImperativeAst::ShuffleAndPutOnTop => Effect::Shuffle {
            target: TargetFilter::Controller,
        },
        ShuffleImperativeAst::ChangeZoneToLibrary => Effect::ChangeZone {
            origin: None,
            destination: Zone::Library,
            target: TargetFilter::Any,
            owner_library: false,
        },
        ShuffleImperativeAst::ChangeZoneAllToLibrary { origin } => Effect::ChangeZoneAll {
            origin: Some(origin),
            destination: Zone::Library,
            target: TargetFilter::Controller,
        },
        ShuffleImperativeAst::Unimplemented { text } => Effect::Unimplemented {
            name: "shuffle".to_string(),
            description: Some(text),
        },
    }
}

pub(super) fn parse_destroy_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    if lower.starts_with("destroy all ") || lower.starts_with("destroy each ") {
        let (target, _) = parse_target(&text[8..]);
        return Some(ZoneCounterImperativeAst::Destroy { target, all: true });
    }
    if lower.starts_with("destroy ") {
        let (target, _) = parse_target(&text[8..]);
        return Some(ZoneCounterImperativeAst::Destroy { target, all: false });
    }
    None
}

pub(super) fn parse_exile_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    if lower.starts_with("exile all ") || lower.starts_with("exile each ") {
        let rest_lower = &lower[6..]; // after "exile "
        let (target, _) = parse_target(&text[6..]);
        let origin = super::infer_origin_zone(rest_lower);
        return Some(ZoneCounterImperativeAst::Exile {
            origin,
            target,
            all: true,
        });
    }

    let rest_lower = lower.strip_prefix("exile ")?;
    let (target, _) = parse_target(&text[6..]);
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

    let (target, _) = parse_target(&text[8..]);
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
            }) => Some(CostResourceImperativeAst::Mana {
                produced,
                restrictions,
            }),
            _ => None,
        };
    }
    if let Some(effect) = super::try_parse_damage(lower, text) {
        return match effect {
            Effect::DealDamage { amount, target } => Some(CostResourceImperativeAst::Damage {
                amount: quantity_to_damage_amount(&amount),
                target,
                all: false,
            }),
            Effect::DamageAll { amount, target } => Some(CostResourceImperativeAst::Damage {
                amount,
                target,
                all: true,
            }),
            _ => None,
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
                    amount: damage_amount_to_quantity(&amount),
                    target,
                }
            }
        }
        CostResourceImperativeAst::Pay { cost } => Effect::PayCost { cost },
    }
}

pub(super) fn parse_imperative_family_ast(text: &str, lower: &str) -> Option<ImperativeFamilyAst> {
    if let Some(ast) = parse_cost_resource_ast(text, lower) {
        return Some(ImperativeFamilyAst::CostResource(ast));
    }
    if let Some(ast) = parse_zone_counter_ast(text, lower) {
        return Some(ImperativeFamilyAst::ZoneCounter(ast));
    }
    if let Some(ast) = parse_numeric_imperative_ast(text, lower) {
        return Some(ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)));
    }
    if let Some(ast) = parse_targeted_action_ast(text, lower) {
        return Some(ImperativeFamilyAst::Structured(ImperativeAst::Targeted(
            ast,
        )));
    }
    if let Some(ast) = parse_search_and_creation_ast(text, lower) {
        return Some(ImperativeFamilyAst::Structured(
            ImperativeAst::SearchCreation(ast),
        ));
    }
    if lower == "explore" || lower.starts_with("explore.") {
        return Some(ImperativeFamilyAst::Explore);
    }
    // CR 702.162a: "connive" / "connives"
    if lower == "connive" || lower == "connives" {
        return Some(ImperativeFamilyAst::Connive);
    }
    // CR 702.26a: "phase out" / "phases out"
    if lower == "phase out" || lower == "phases out" {
        return Some(ImperativeFamilyAst::PhaseOut);
    }
    // CR 509.1g: "block this turn if able" / "blocks this turn if able"
    if lower == "block this turn if able" || lower == "blocks this turn if able" {
        return Some(ImperativeFamilyAst::ForceBlock);
    }
    // CR 702.136: "investigate" — create a Clue token.
    if lower == "investigate" || lower.starts_with("investigate.") {
        return Some(ImperativeFamilyAst::Investigate);
    }
    // CR 722: "become the monarch" / "becomes the monarch"
    if lower == "become the monarch" || lower == "becomes the monarch" {
        return Some(ImperativeFamilyAst::BecomeMonarch);
    }
    if lower == "proliferate" || lower.starts_with("proliferate.") {
        return Some(ImperativeFamilyAst::Proliferate);
    }
    // CR 706: "roll a d20" / "roll a d6" / "roll a d4" / word-form variants
    if let Some(sides) = try_parse_roll_die_sides(lower) {
        return Some(ImperativeFamilyAst::RollDie { sides });
    }

    // CR 705: "flip a coin"
    if lower == "flip a coin" || lower == "flips a coin" {
        return Some(ImperativeFamilyAst::FlipCoin);
    }

    // CR 104.3a: "lose the game" / "win the game" — game-ending effects.
    // Must come before keyword granting to intercept "lose" before it falls through.
    if lower == "lose the game" || lower == "loses the game" {
        return Some(ImperativeFamilyAst::LoseTheGame);
    }
    if lower == "win the game" || lower == "wins the game" {
        return Some(ImperativeFamilyAst::WinTheGame);
    }
    // CR 702: "lose [keyword]" / "lose [keyword] until end of turn" — keyword removal
    // in bare imperative form. Mirrors "gain [keyword]" using RemoveKeyword.
    // Also handles compound "lose defender and gains flying" via parse_continuous_modifications.
    if lower.starts_with("lose ")
        && !lower.contains("life")
        && !lower.contains("the game")
        && !lower.contains("mana")
    {
        if let Some(effect) = try_parse_gain_keyword(text) {
            return Some(ImperativeFamilyAst::LoseKeyword(effect));
        }
    }
    // CR 702: "gain [keyword]" / "gain [keyword] until end of turn" — keyword granting
    // in bare imperative form (subject-stripped sub-abilities like "it gains haste").
    // Must come before shuffle/utility to intercept "gain" before it falls through.
    if lower.starts_with("gain ") && !lower.contains("life") && !lower.contains("control") {
        if let Some(effect) = try_parse_gain_keyword(text) {
            return Some(ImperativeFamilyAst::GainKeyword(effect));
        }
    }
    if let Some(ast) = parse_shuffle_ast(text, lower) {
        return Some(ImperativeFamilyAst::Shuffle(ast));
    }
    if let Some(ast) = parse_hand_reveal_ast(text, lower) {
        return Some(ImperativeFamilyAst::Structured(ImperativeAst::HandReveal(
            ast,
        )));
    }
    if let Some(ast) = parse_utility_imperative_ast(text, lower) {
        return Some(ImperativeFamilyAst::Structured(ImperativeAst::Utility(ast)));
    }
    if let Some(ast) = parse_put_ast(text, lower) {
        return Some(ImperativeFamilyAst::Put(ast));
    }
    if let Some(stripped) = text.strip_prefix("you may ") {
        return Some(ImperativeFamilyAst::YouMay {
            text: stripped.to_string(),
        });
    }
    if let Some(ast) = parse_choose_ast(text, lower) {
        return Some(ImperativeFamilyAst::Structured(ImperativeAst::Choose(ast)));
    }
    None
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
    let lower = text.to_lowercase();
    if let Some(rest) = lower.strip_prefix("if you win the flip, ") {
        let _ = rest; // Only used for prefix detection
        let effect_text = &text["if you win the flip, ".len()..];
        Some((true, effect_text))
    } else if let Some(rest) = lower.strip_prefix("if you lose the flip, ") {
        let _ = rest;
        let effect_text = &text["if you lose the flip, ".len()..];
        Some((false, effect_text))
    } else {
        None
    }
}

pub(super) fn lower_imperative_family_ast(ast: ImperativeFamilyAst) -> Effect {
    match ast {
        ImperativeFamilyAst::Structured(ast) => lower_imperative_ast(ast),
        ImperativeFamilyAst::CostResource(ast) => lower_cost_resource_ast(ast),
        ImperativeFamilyAst::ZoneCounter(ast) => lower_zone_counter_ast(ast),
        ImperativeFamilyAst::Explore => Effect::Explore,
        ImperativeFamilyAst::Connive => Effect::Connive {
            target: TargetFilter::Any,
        },
        ImperativeFamilyAst::PhaseOut => Effect::PhaseOut {
            target: TargetFilter::Any,
        },
        ImperativeFamilyAst::ForceBlock => Effect::ForceBlock {
            target: TargetFilter::Any,
        },
        ImperativeFamilyAst::Investigate => Effect::Investigate,
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
        ImperativeFamilyAst::Shuffle(ast) => lower_shuffle_ast(ast),
        ImperativeFamilyAst::Put(ast) => lower_put_ast(ast),
        ImperativeFamilyAst::YouMay { text } => super::parse_effect(&text),
    }
}

pub(super) fn parse_zone_counter_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
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
        return match try_parse_put_counter(lower, text) {
            Some((
                Effect::PutCounter {
                    counter_type,
                    count,
                    target,
                },
                _remainder,
            )) => Some(ZoneCounterImperativeAst::PutCounter {
                counter_type,
                count,
                target,
            }),
            _ => None,
        };
    }
    if lower.starts_with("remove ") && lower.contains("counter") {
        return match try_parse_remove_counter(lower) {
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
    }
}
