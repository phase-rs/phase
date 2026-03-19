use super::types::*;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, StaticDefinition, TargetFilter,
};
use crate::types::zones::Zone;

pub(super) fn split_clause_sequence(text: &str) -> Vec<ClauseChunk> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    let mut paren_depth = 0usize;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while let Some(ch) = chars.next() {
        match ch {
            '(' if !in_single_quote && !in_double_quote => {
                paren_depth += 1;
                current.push(ch);
            }
            ')' if !in_single_quote && !in_double_quote => {
                paren_depth = paren_depth.saturating_sub(1);
                current.push(ch);
            }
            '\'' if !in_double_quote => {
                if is_possessive_apostrophe(&current, chars.peek().copied()) {
                    current.push(ch);
                } else {
                    in_single_quote = !in_single_quote;
                    current.push(ch);
                }
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                current.push(ch);
            }
            ',' if paren_depth == 0 && !in_single_quote && !in_double_quote => {
                let remainder = chars.clone().collect::<String>();
                if let Some((boundary, chars_to_skip)) =
                    split_comma_clause_boundary(&current, &remainder)
                {
                    push_clause_chunk(&mut chunks, &current, Some(boundary));
                    current.clear();
                    for _ in 0..chars_to_skip {
                        chars.next();
                    }
                } else {
                    current.push(ch);
                }
            }
            '.' if paren_depth == 0 && !in_single_quote && !in_double_quote => {
                push_clause_chunk(&mut chunks, &current, Some(ClauseBoundary::Sentence));
                current.clear();
                while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
                    chars.next();
                }
            }
            _ => current.push(ch),
        }
    }

    push_clause_chunk(&mut chunks, &current, None);
    chunks
}

fn split_comma_clause_boundary(current: &str, remainder: &str) -> Option<(ClauseBoundary, usize)> {
    let current_lower = current.trim().to_ascii_lowercase();
    let trimmed = remainder.trim_start();
    let whitespace_len = remainder.len() - trimmed.len();
    let trimmed_lower = trimmed.to_ascii_lowercase();

    if starts_prefix_clause(&current_lower) {
        return None;
    }

    if current_lower.starts_with("search ")
        && (trimmed_lower.starts_with("reveal ") || trimmed_lower.starts_with("put "))
    {
        return None;
    }

    if let Some(after_then) = trimmed.strip_prefix("then ") {
        if starts_clause_text(after_then) {
            return Some((ClauseBoundary::Then, whitespace_len + "then ".len()));
        }
    }

    if starts_clause_text(trimmed) {
        return Some((ClauseBoundary::Comma, whitespace_len));
    }

    None
}

fn starts_prefix_clause(current_lower: &str) -> bool {
    current_lower.starts_with("until ")
        || current_lower.starts_with("if ")
        || current_lower.starts_with("when ")
        || current_lower.starts_with("for each ")
}

pub(super) fn starts_clause_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let prefixes = [
        "add ",
        "all ",
        "attach ",
        "cast ",
        "counter ",
        "create ",
        "deal ",
        "destroy ",
        "discard ",
        "draw ",
        "each ",
        "each player ",
        "each opponent ",
        "exile ",
        "explore",
        "fight ",
        "flip ",
        "investigate",
        "gain control ",
        "gain ",
        "look at ",
        "lose ",
        "mill ",
        "proliferate",
        "put ",
        "return ",
        "reveal ",
        "roll ",
        "sacrifice ",
        "scry ",
        "search ",
        "shuffle ",
        "surveil ",
        "tap ",
        "that ",
        "this ",
        "those ",
        "they ",
        "target ",
        "untap ",
        "you may ",
        "you ",
        "it ",
    ];

    prefixes.iter().any(|prefix| lower.starts_with(prefix))
}

pub(super) fn is_possessive_apostrophe(current: &str, next: Option<char>) -> bool {
    let prev = current.chars().last();
    matches!(
        (prev, next),
        (Some(prev), Some(next)) if prev.is_alphanumeric() && next.is_alphanumeric()
    )
}

pub(super) fn push_clause_chunk(
    chunks: &mut Vec<ClauseChunk>,
    raw_text: &str,
    boundary_after: Option<ClauseBoundary>,
) {
    let text = raw_text.trim().trim_end_matches('.').trim();
    if text.is_empty() {
        return;
    }
    chunks.push(ClauseChunk {
        text: text.to_string(),
        boundary_after,
    });
}

pub(super) fn apply_clause_continuation(
    defs: &mut Vec<AbilityDefinition>,
    continuation: ContinuationAst,
    kind: AbilityKind,
) {
    match continuation {
        ContinuationAst::SearchDestination { destination } => {
            defs.push(AbilityDefinition::new(
                kind,
                Effect::ChangeZone {
                    origin: Some(Zone::Library),
                    destination,
                    target: TargetFilter::Any,
                    owner_library: false,
                },
            ));
        }
        ContinuationAst::RevealHandFilter { card_filter } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::RevealHand {
                card_filter: existing,
                ..
            } = &mut previous.effect
            {
                *existing = card_filter;
            }
        }
        ContinuationAst::ManaRestriction { restriction } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Mana { restrictions, .. } = &mut previous.effect {
                restrictions.push(restriction);
            }
        }
        ContinuationAst::CounterSourceStatic { source_static } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Counter {
                source_static: existing,
                ..
            } = &mut previous.effect
            {
                *existing = Some(source_static);
            }
        }
        ContinuationAst::SuspectLastCreated => {
            defs.push(AbilityDefinition::new(
                kind,
                Effect::Suspect {
                    target: TargetFilter::LastCreated,
                },
            ));
        }
        ContinuationAst::CantRegenerate => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            match &mut previous.effect {
                Effect::Destroy {
                    cant_regenerate, ..
                }
                | Effect::DestroyAll {
                    cant_regenerate, ..
                } => {
                    *cant_regenerate = true;
                }
                _ => {}
            }
        }
    }
}

pub(super) fn continuation_absorbs_current(
    continuation: &ContinuationAst,
    current_effect: &Effect,
) -> bool {
    match continuation {
        ContinuationAst::RevealHandFilter { .. } => {
            matches!(current_effect, Effect::RevealHand { .. })
        }
        ContinuationAst::ManaRestriction { .. } | ContinuationAst::CounterSourceStatic { .. } => {
            true
        }
        ContinuationAst::SearchDestination { .. } => false,
        ContinuationAst::SuspectLastCreated => matches!(current_effect, Effect::Suspect { .. }),
        ContinuationAst::CantRegenerate => true,
    }
}

pub(super) fn parse_intrinsic_continuation_ast(
    text: &str,
    effect: &Effect,
) -> Option<ContinuationAst> {
    match effect {
        Effect::SearchLibrary { .. } => Some(ContinuationAst::SearchDestination {
            destination: super::parse_search_destination(&text.to_lowercase()),
        }),
        _ => None,
    }
}

pub(super) fn parse_followup_continuation_ast(
    text: &str,
    previous_effect: &Effect,
) -> Option<ContinuationAst> {
    let lower = text.to_lowercase();

    match previous_effect {
        Effect::RevealHand { .. }
            if lower.contains("card from it") || lower.contains("card from among") =>
        {
            let card_filter = if lower.starts_with("you choose ") || lower.starts_with("choose ") {
                super::parse_choose_filter(&lower)
            } else {
                super::parse_choose_filter_from_sentence(&lower)
            };
            Some(ContinuationAst::RevealHandFilter { card_filter })
        }
        Effect::Mana { .. } => super::mana::parse_mana_spend_restriction(&lower)
            .map(|restriction| ContinuationAst::ManaRestriction { restriction }),
        Effect::Counter { .. }
            if lower.contains("countered this way") && lower.contains("loses all abilities") =>
        {
            Some(ContinuationAst::CounterSourceStatic {
                source_static: StaticDefinition::continuous().modifications(vec![
                    crate::types::ability::ContinuousModification::RemoveAllAbilities,
                ]),
            })
        }
        // "create a ... token and suspect it" → chain suspect on last created token
        Effect::Token { .. } if lower.starts_with("suspect ") => {
            Some(ContinuationAst::SuspectLastCreated)
        }
        // CR 701.15: "It can't be regenerated" / "They can't be regenerated" after Destroy/DestroyAll
        Effect::Destroy { .. } | Effect::DestroyAll { .. }
            if lower.contains("can't be regenerated")
                || lower.contains("cannot be regenerated") =>
        {
            Some(ContinuationAst::CantRegenerate)
        }
        _ => None,
    }
}
