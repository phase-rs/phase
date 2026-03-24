use super::super::oracle_target::parse_target;
use super::super::oracle_util::parse_number;
use super::types::*;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Chooser, Effect, StaticDefinition, TargetFilter,
};
use crate::types::zones::Zone;

/// Parse count from "choose one/two/three/N of them/those" text using `parse_number`.
/// Handles all chooser prefix forms: "choose ", "you choose ", "an opponent chooses ",
/// "target opponent chooses ".
fn parse_choose_count_from_text(lower: &str) -> u32 {
    let rest = lower
        .strip_prefix("an opponent chooses ")
        .or_else(|| lower.strip_prefix("target opponent chooses "))
        .unwrap_or_else(|| {
            let s = lower.strip_prefix("you ").unwrap_or(lower);
            s.strip_prefix("choose ")
                .or(s.strip_prefix("chooses "))
                .unwrap_or(s)
        });
    parse_number(rest).map(|(n, _)| n).unwrap_or(1)
}

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
            _ => {
                current.push(ch);
                // Detect bare " and " at word boundary followed by an imperative verb.
                // Handles patterns like "you lose 1 life and create a Treasure token".
                // Uses a restricted verb list to avoid false positives on noun phrases
                // like "target creature and all other creatures" or "it and each other".
                if paren_depth == 0
                    && !in_single_quote
                    && !in_double_quote
                    && current.ends_with(" and ")
                {
                    let remainder: String = chars.clone().collect();
                    let remainder_trimmed = remainder.trim_start();
                    // Suppress split when "and put" follows "from among" — the
                    // "put into hand / onto battlefield" is part of the same
                    // compound action, not a separate clause.
                    let before_and = &current[..current.len() - " and ".len()];
                    let before_lower = before_and.to_ascii_lowercase();
                    let suppress = before_lower.contains("from among");
                    if !suppress && starts_bare_and_clause(remainder_trimmed) {
                        push_clause_chunk(&mut chunks, before_and, Some(ClauseBoundary::Comma));
                        current.clear();
                    }
                }
            }
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

    // CR 701.18a: "search [library] for X, put/reveal Y" is a single compound action.
    // The search verb may follow a sequence connector like "Then" from a prior sentence.
    let search_start =
        current_lower.starts_with("search ") || current_lower.starts_with("then search ");
    if search_start && (trimmed_lower.starts_with("reveal ") || trimmed_lower.starts_with("put ")) {
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

    // Strip "and " connector before checking clause start
    // Handles patterns like ", and get {E}{E}" or ", and draw a card"
    if let Some(after_and) = trimmed_lower.strip_prefix("and ") {
        if starts_clause_text(after_and) {
            return Some((ClauseBoundary::Comma, whitespace_len));
        }
    }

    None
}

fn starts_prefix_clause(current_lower: &str) -> bool {
    current_lower.starts_with("until ")
        || current_lower.starts_with("if ")
        || current_lower.starts_with("when ")
        || current_lower.starts_with("whenever ")
        || current_lower.starts_with("for each ")
        || current_lower.starts_with("then if ")
        || current_lower.starts_with("otherwise")
        || current_lower.starts_with("if not")
        // CR 603.7a: Temporal prefix clauses must not be split on their internal comma.
        || current_lower.starts_with("at the beginning ")
        // CR 611.2b: "For as long as [condition], [effect]" — duration prefix clause.
        || current_lower.starts_with("for as long as ")
}

/// Check whether `text` begins with an imperative verb or pronoun that can start
/// an independent clause.  Used by the clause splitter to detect boundaries at
/// commas, "then", and bare "and".
///
/// **Convention — trailing space:**
/// - *Transitive* verbs (always require an object): include a trailing space
///   (e.g. `"draw "`, `"destroy "`).  This prevents false matches on noun phrases.
/// - *Intransitive* verbs (can appear bare at end-of-sentence, e.g. `", then shuffle."`):
///   omit the trailing space so the prefix matches even when followed by punctuation.
///   Current intransitive entries: `"explore"`, `"investigate"`, `"proliferate"`,
///   `"shuffle"`.
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
        "get ",
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
        "shuffle",
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

/// Restricted clause-start check for bare " and " splitting (not after comma).
/// Only includes imperative verbs that are unambiguously clause starters —
/// excludes pronouns/determiners like "all", "each", "it", "that", "those",
/// "they", "you" which commonly appear in noun phrases after "and"
/// (e.g. "target creature and all other creatures").
fn starts_bare_and_clause(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let prefixes = [
        "create ",
        "destroy ",
        "draw ",
        "discard ",
        "exile ",
        "gain ",
        "lose ",
        "mill ",
        "put ",
        "return ",
        "sacrifice ",
        "scry ",
        "search ",
        "shuffle",
        "surveil ",
        "tap ",
        "untap ",
    ];
    prefixes.iter().any(|prefix| lower.starts_with(prefix)) || starts_with_damage_clause(&lower)
}

/// Checks if text starts with a subject-prefixed damage verb.
/// Matches: "it deals N damage", "~ deals N damage", "this creature deals N damage",
/// "that creature deals N damage", bare "deals N damage", etc.
/// Used by `starts_bare_and_clause` to split patterns like
/// "sacrifice ~ and it deals 3 damage to target player".
fn starts_with_damage_clause(lower: &str) -> bool {
    if let Some(pos) = lower.find("deals ").or_else(|| lower.find("deal ")) {
        let subject = lower[..pos].trim();
        subject.is_empty() // bare "deals N damage"
            || subject == "it" // "it deals N damage"
            || subject == "~" // "~ deals N damage"
            || subject.starts_with("this ") // "this creature/enchantment/token deals"
            || subject.starts_with("that ") // "that creature deals"
    } else {
        false
    }
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
        ContinuationAst::SearchDestination {
            destination,
            enter_tapped,
            attach_to_source,
        } => {
            let mut change_zone = AbilityDefinition::new(
                kind,
                Effect::ChangeZone {
                    origin: Some(Zone::Library),
                    destination,
                    target: TargetFilter::Any,
                    owner_library: false,
                    enter_transformed: false,
                    under_your_control: false,
                    enter_tapped,
                    enters_attacking: false,
                },
            );
            // CR 303.4f: "attached to [source]" — forward the moved card to an Attach sub_ability
            if attach_to_source {
                change_zone.forward_result = true;
                change_zone.sub_ability = Some(Box::new(AbilityDefinition::new(
                    kind,
                    Effect::Attach {
                        target: TargetFilter::Any,
                    },
                )));
            }
            defs.push(change_zone);
        }
        ContinuationAst::RevealHandFilter { card_filter } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::RevealHand {
                card_filter: existing,
                ..
            } = &mut *previous.effect
            {
                *existing = card_filter;
            }
        }
        ContinuationAst::ManaRestriction { restriction } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Mana { restrictions, .. } = &mut *previous.effect {
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
            } = &mut *previous.effect
            {
                *existing = Some(*source_static);
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
            match &mut *previous.effect {
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
        ContinuationAst::PutBackInAnyOrder => {
            // CR 401.5: Absorbed into preceding Dig/RevealTop — the engine's DigChoice
            // handler places remaining cards. If the Dig already has rest_destination
            // set (e.g., by a preceding DigFromAmong), this is a no-op.
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Dig {
                rest_destination, ..
            } = &mut *previous.effect
            {
                if rest_destination.is_none() {
                    // Default: rest goes to bottom of library (not graveyard)
                    *rest_destination = Some(Zone::Library);
                }
            }
        }
        ContinuationAst::DigFromAmong {
            count,
            up_to: is_up_to,
            filter: card_filter,
            destination: kept_dest,
        } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Dig {
                keep_count,
                up_to,
                filter,
                destination,
                ..
            } = &mut *previous.effect
            {
                *keep_count = Some(count);
                *up_to = is_up_to;
                *filter = card_filter;
                *destination = Some(kept_dest);
            }
        }
        ContinuationAst::ChooseFromExile { count, chooser } => {
            defs.push(AbilityDefinition::new(
                kind,
                Effect::ChooseFromZone {
                    count,
                    zone: Zone::Exile,
                    chooser,
                },
            ));
        }
        ContinuationAst::EntersTappedAttacking => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            // CR 508.4 / CR 614.1: Patch the preceding effect to enter tapped and attacking.
            match &mut *previous.effect {
                Effect::CopyTokenOf {
                    enters_attacking,
                    tapped,
                    ..
                } => {
                    *enters_attacking = true;
                    *tapped = true;
                }
                Effect::Token {
                    enters_attacking,
                    tapped,
                    ..
                } => {
                    *enters_attacking = true;
                    *tapped = true;
                }
                Effect::ChangeZone {
                    enters_attacking,
                    enter_tapped,
                    ..
                } => {
                    *enters_attacking = true;
                    *enter_tapped = true;
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
        ContinuationAst::PutBackInAnyOrder => true,
        ContinuationAst::ChooseFromExile { .. } => true,
        ContinuationAst::EntersTappedAttacking => true,
        ContinuationAst::DigFromAmong { .. } => true,
    }
}

pub(super) fn parse_intrinsic_continuation_ast(
    text: &str,
    effect: &Effect,
    full_text: &str,
) -> Option<ContinuationAst> {
    match effect {
        Effect::SearchLibrary { .. } => {
            let full_lower = full_text.to_ascii_lowercase();
            // CR 701.24b: If later clauses contain "put on top", suppress the default
            // ChangeZone(→Hand) — the card stays in the library and a separate
            // PutAtLibraryPosition effect in the chain handles placement.
            // Also suppress for "Nth from the top" (Long-Term Plans, etc.)
            let has_positional_put = full_lower.contains("put that card on top")
                || full_lower.contains("put it on top")
                || full_lower.contains("put the card on top")
                || full_lower.contains("put them on top")
                || (full_lower.contains("put that card") && full_lower.contains("from the top"));
            if has_positional_put {
                return None;
            }
            let lower = text.to_lowercase();
            let attach_to_source = lower.contains("attached to");
            // CR 701.23a: "onto the battlefield tapped" — the searched card enters tapped.
            let enter_tapped = lower.contains("battlefield tapped");
            // Safety net: verify the clause splitter correctly separated all boundaries.
            // If this fires, a verb is missing from starts_clause_text() or the splitter's
            // search_start guard is incorrectly suppressing a split.
            // "shuffle" is excluded — it's part of the search compound action (CR 701.18a).
            #[cfg(debug_assertions)]
            if let Some(then_pos) = lower.rfind(", then ") {
                let after_then = lower[then_pos + ", then ".len()..].trim_end_matches('.');
                if !after_then.starts_with("shuffle") {
                    debug_assert!(
                        !starts_clause_text(after_then),
                        "Unsplit clause boundary in SearchLibrary continuation: \
                         ', then {}' — check starts_clause_text() for missing verb",
                        after_then,
                    );
                }
            }
            Some(ContinuationAst::SearchDestination {
                destination: super::parse_search_destination(&lower),
                enter_tapped,
                attach_to_source,
            })
        }
        _ => None,
    }
}

/// Parse "put up to N [filter] from among them/those cards onto the battlefield / into your hand"
/// into a DigFromAmong continuation that patches the preceding Dig effect.
///
/// Examples:
/// - "put up to two creature cards with mana value 3 or less from among them onto the battlefield"
/// - "put a creature card from among them into your hand"
/// - "you may reveal a creature card from among them and put it into your hand"
fn parse_dig_from_among(lower: &str, _original: &str) -> Option<ContinuationAst> {
    // Determine destination
    let destination = if lower.contains("onto the battlefield") {
        Zone::Battlefield
    } else {
        Zone::Hand
    };

    // Find "from among" to split the text into count+filter vs destination
    let from_among_pos = lower.find("from among")?;
    let before_from = &lower[..from_among_pos].trim();

    // Strip leading "put " or "you may reveal "
    let after_put = before_from
        .strip_prefix("you may put ")
        .or_else(|| before_from.strip_prefix("you may reveal "))
        .or_else(|| before_from.strip_prefix("put "))
        .or_else(|| before_from.strip_prefix("reveal "))
        .unwrap_or(before_from);

    // Parse "up to N" or "a/an" or just a number
    let (count, up_to, filter_text) = if let Some(rest) = after_put.strip_prefix("up to ") {
        if let Some((n, remainder)) = parse_number(rest) {
            (n, true, remainder.trim())
        } else {
            (1, true, rest)
        }
    } else if let Some(rest) = after_put.strip_prefix("any number of ") {
        // "any number of creatures" → up_to with a high cap
        (255, true, rest)
    } else if after_put.starts_with("a ") || after_put.starts_with("an ") {
        // "a creature card" / "an artifact card" — up_to 1 (player may choose none)
        let rest = after_put
            .strip_prefix("a ")
            .or_else(|| after_put.strip_prefix("an "))
            .unwrap_or(after_put);
        (1, true, rest)
    } else if let Some((n, remainder)) = parse_number(after_put) {
        // Explicit numeric count: "two creature cards" → exactly 2
        (n, false, remainder.trim())
    } else {
        (1, true, after_put)
    };

    // Parse the filter from the remaining text (e.g., "creature cards with mana value 3 or less")
    let filter = if filter_text.is_empty()
        || filter_text == "card"
        || filter_text == "cards"
        || filter_text == "of them"
    {
        TargetFilter::Any
    } else {
        let (parsed_filter, _) = parse_target(filter_text);
        parsed_filter
    };

    Some(ContinuationAst::DigFromAmong {
        count,
        up_to,
        filter,
        destination,
    })
}

pub(super) fn parse_followup_continuation_ast(
    text: &str,
    previous_effect: &Effect,
) -> Option<ContinuationAst> {
    let lower = text.to_lowercase();

    match previous_effect {
        Effect::RevealHand { .. }
            if lower.contains("card from it")
                || lower.contains("card from among")
                || lower.contains("one of them")
                || lower.contains("one of those") =>
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
                source_static: Box::new(StaticDefinition::continuous().modifications(vec![
                    crate::types::ability::ContinuousModification::RemoveAllAbilities,
                ])),
            })
        }
        // CR 401.5: "put them back in any order" / "put the rest on top in any order"
        // after Dig/RevealTop — signals player-chosen ordering of remaining cards.
        Effect::Dig { .. } | Effect::RevealTop { .. }
            if lower.contains("in any order")
                && (lower.contains("put them back")
                    || lower.contains("put the rest")
                    || lower.contains("put those cards")) =>
        {
            Some(ContinuationAst::PutBackInAnyOrder)
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
        // CR 700.2: "Choose/You choose/An opponent chooses/Target opponent chooses one/two/N
        // of them/those" after ChangeZone → ChooseFromZone building block
        Effect::ChangeZone { .. }
            if (lower.contains("of them") || lower.contains("of those"))
                && (lower.starts_with("choose ")
                    || lower.starts_with("you choose ")
                    || lower.starts_with("an opponent chooses ")
                    || lower.starts_with("target opponent chooses ")) =>
        {
            let count = parse_choose_count_from_text(&lower);
            let chooser = if lower.starts_with("an opponent chooses ")
                || lower.starts_with("target opponent chooses ")
            {
                Chooser::Opponent
            } else {
                Chooser::Controller
            };
            Some(ContinuationAst::ChooseFromExile { count, chooser })
        }
        // "Put up to N [filter] from among them/those cards onto the battlefield/into your hand"
        // after Dig — patches keep_count, filter, destination on the preceding Dig effect.
        Effect::Dig { .. }
            if (lower.contains("from among them") || lower.contains("from among those cards"))
                && (lower.contains("onto the battlefield")
                    || lower.contains("into your hand")
                    || lower.contains("into their hand")) =>
        {
            parse_dig_from_among(&lower, text)
        }
        // CR 508.4 / CR 614.1: "It/The token/He/She/Name enters tapped and attacking"
        // after CopyTokenOf, Token, or ChangeZone effects.
        Effect::CopyTokenOf { .. } | Effect::Token { .. } | Effect::ChangeZone { .. }
            if lower.contains("enters tapped and attacking") =>
        {
            Some(ContinuationAst::EntersTappedAttacking)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: extract just the text fields from split_clause_sequence output.
    fn clause_texts(input: &str) -> Vec<String> {
        split_clause_sequence(input)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    // --- Bare " and " splitting: positive cases (should split) ---

    #[test]
    fn bare_and_splits_lose_life_and_create_token() {
        // Lotho: "you lose 1 life and create a Treasure token"
        let chunks = clause_texts("you lose 1 life and create a Treasure token");
        assert_eq!(chunks, vec!["you lose 1 life", "create a Treasure token"]);
    }

    #[test]
    fn bare_and_splits_draw_and_lose() {
        let chunks = clause_texts("draw a card and lose 1 life");
        assert_eq!(chunks, vec!["draw a card", "lose 1 life"]);
    }

    #[test]
    fn bare_and_splits_destroy_and_gain() {
        let chunks = clause_texts("destroy target creature and gain 3 life");
        assert_eq!(chunks, vec!["destroy target creature", "gain 3 life"]);
    }

    // --- Bare " and " splitting: negative cases (must NOT split) ---

    #[test]
    fn bare_and_does_not_split_creature_and_all_other() {
        // Bile Blight: "target creature and all other creatures with the same name"
        let chunks = clause_texts("target creature and all other creatures with the same name");
        assert_eq!(
            chunks,
            vec!["target creature and all other creatures with the same name"]
        );
    }

    #[test]
    fn bare_and_does_not_split_each_opponent_and_each_creature() {
        // Goblin Chainwhirler: "each opponent and each creature and planeswalker they control"
        let chunks = clause_texts("each opponent and each creature and planeswalker they control");
        assert_eq!(
            chunks,
            vec!["each opponent and each creature and planeswalker they control"]
        );
    }

    #[test]
    fn bare_and_does_not_split_it_and_each_other() {
        let chunks = clause_texts("exile it and each other creature");
        assert_eq!(chunks, vec!["exile it and each other creature"]);
    }

    #[test]
    fn bare_and_does_not_split_power_and_toughness() {
        let chunks = clause_texts("power and toughness each equal to the number of cards");
        assert_eq!(
            chunks,
            vec!["power and toughness each equal to the number of cards"]
        );
    }

    #[test]
    fn bare_and_does_not_split_you_and_target_opponent() {
        let chunks = clause_texts("you and target opponent each draw a card");
        assert_eq!(chunks, vec!["you and target opponent each draw a card"]);
    }

    // --- Comma-based splitting still works ---

    #[test]
    fn comma_then_clause_still_splits() {
        let chunks = clause_texts("draw a card, then discard a card");
        assert_eq!(chunks, vec!["draw a card", "discard a card"]);
    }

    #[test]
    fn sentence_boundary_still_splits() {
        let chunks = clause_texts("draw a card. Create a token");
        assert_eq!(chunks, vec!["draw a card", "Create a token"]);
    }

    #[test]
    fn earthbender_search_stays_together() {
        // The full effect text after stripping the trigger condition.
        // Period after "earthbend 2" should split into two sentences,
        // and the search clause must stay with "put it onto the battlefield tapped".
        // "then shuffle" correctly splits into its own clause.
        let chunks = clause_texts(
            "earthbend 2. Then search your library for a basic land card, put it onto the battlefield tapped, then shuffle",
        );
        assert_eq!(
            chunks,
            vec![
                "earthbend 2",
                "Then search your library for a basic land card, put it onto the battlefield tapped",
                "shuffle",
            ]
        );
    }

    #[test]
    fn bare_shuffle_at_end_of_sentence_splits() {
        let chunks = clause_texts("draw a card, then shuffle.");
        assert_eq!(chunks, vec!["draw a card", "shuffle"]);
    }

    #[test]
    fn intransitive_verbs_match_without_trailing_space() {
        // Intransitive verbs can appear bare at end-of-sentence (", then shuffle.")
        // They MUST match in starts_clause_text without a trailing space.
        let intransitive = ["shuffle", "explore", "investigate", "proliferate"];
        for verb in intransitive {
            assert!(
                starts_clause_text(verb),
                "Intransitive verb '{}' must match in starts_clause_text \
                 without trailing space — otherwise ', then {}.' fails to split",
                verb,
                verb,
            );
        }
    }

    #[test]
    fn for_as_long_as_prefix_does_not_split_on_comma() {
        // CR 611.2b: "For as long as [condition], [effect]" must not split
        // at the internal comma separating the condition from the effect body.
        let chunks = split_clause_sequence(
            "For as long as this creature remains tapped, gain control of target creature",
        );
        assert_eq!(
            chunks.len(),
            1,
            "expected 1 chunk (unsplit), got {}: {:?}",
            chunks.len(),
            chunks.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }

    // --- Bare " and " splitting: damage clause patterns ---

    #[test]
    fn bare_and_splits_sacrifice_and_it_deals_damage() {
        // Mogg Bombers: "sacrifice ~ and it deals 3 damage to target player"
        let chunks =
            clause_texts("sacrifice ~ and it deals 3 damage to target player or planeswalker");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "sacrifice ~");
        assert!(chunks[1].starts_with("it deals 3 damage"));
    }

    #[test]
    fn bare_and_splits_gain_life_and_card_deals_damage() {
        // Axelrod Gunnarson: "you gain 1 life and ~ deals 1 damage to target player"
        let chunks =
            clause_texts("you gain 1 life and ~ deals 1 damage to target player or planeswalker");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "you gain 1 life");
        assert!(chunks[1].starts_with("~ deals 1 damage"));
    }

    #[test]
    fn bare_and_splits_that_creature_deals_damage() {
        // Form of the Dinosaur: "and that creature deals damage equal to its power to you"
        let chunks = clause_texts("~ deals 15 damage to target creature and that creature deals damage equal to its power to you");
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn starts_with_damage_clause_positive() {
        assert!(starts_with_damage_clause("it deals 3 damage"));
        assert!(starts_with_damage_clause("this creature deals 1 damage"));
        assert!(starts_with_damage_clause("that creature deals damage"));
        assert!(starts_with_damage_clause("deals 5 damage"));
        assert!(starts_with_damage_clause("~ deals 2 damage"));
        assert!(starts_with_damage_clause("this enchantment deals 4 damage"));
    }

    #[test]
    fn starts_with_damage_clause_negative() {
        assert!(!starts_with_damage_clause("it and each other creature"));
        assert!(!starts_with_damage_clause("all creatures deal"));
        assert!(!starts_with_damage_clause("each player deals"));
        assert!(!starts_with_damage_clause("you lose 3 life"));
    }
}
