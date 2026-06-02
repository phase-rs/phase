use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, tag_no_case, take_until};
use nom::character::complete::multispace1;
use nom::combinator::{all_consuming, eof, opt, value};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::super::oracle_nom::bridge::nom_on_lower;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::primitives::parse_keyword_name;
use super::super::oracle_target::parse_target;
use super::super::oracle_util::{contains_possessive, TextPair};
use super::{apply_where_x_to_filter, strip_trailing_where_x};
use crate::parser::oracle_ir::ast::*;
use crate::parser::oracle_ir::context::ParseContext;
use crate::parser::oracle_quantity::{parse_cda_quantity, parse_quantity_ref};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, CastingPermission, Chooser, CopyRetargetPermission,
    CounterSourceRider, Effect, LibraryPosition, PermissionGrantee, QuantityExpr, QuantityRef,
    StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::counter::CounterType;
use crate::types::keywords::Keyword;
use crate::types::zones::Zone;

/// CR 608.2c + CR 701.23i: Strip a leading player-subject from a search-result
/// continuation chunk so the absorption matcher sees the bare verb form. Used
/// by the SearchDestination follow-up absorber to handle iterated-search
/// variants (Winds of Abandon: "those players put those cards onto the
/// battlefield tapped") whose subject was demoted from a top-level subject
/// because the put-step has already been folded into the search continuation.
///
/// Single nom `alt()` over the player-subject prefixes — extend by adding new
/// arms here, never by adding more enumerated `matches!` arms downstream.
///
/// Intentionally does NOT delegate to `subject::parse_subject_application`:
/// that function is a full subject parser that returns a `SubjectApplication`
/// (filter + targeting + multi-target spec) for use at clause boundaries.
/// Here we only need to peel a known set of player-pronoun prefixes from a
/// continuation chunk before re-tokenizing — there is no filter to derive,
/// no target to attach, and no multi-target structure. The simpler local form
/// keeps the search-continuation absorber decoupled from the subject parser's
/// richer return type and avoids constructing/then-discarding a
/// `SubjectApplication` on the hot continuation path.
fn strip_search_result_subject(lower: &str) -> &str {
    alt((
        tag::<_, _, OracleError<'_>>("those players "),
        tag("that player "),
        tag("each player "),
    ))
    .parse(lower)
    .map(|(rest, _)| rest)
    .unwrap_or(lower)
}

fn is_search_result_reveal_clause(lower: &str) -> bool {
    matches!(
        lower.trim().trim_end_matches('.'),
        "reveal that card" | "reveal those cards" | "reveal the card" | "reveal them" | "reveal it"
    )
}

fn has_conditional_search_result_destination(lower: &str) -> bool {
    fn parse_clause(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
        let (input, _) = alt((
            tag::<_, _, OracleError<'_>>("put that card onto the battlefield"),
            tag("put it onto the battlefield"),
            tag("put them onto the battlefield"),
            tag("put those cards onto the battlefield"),
        ))
        .parse(input)?;
        let (input, _) = opt(tag(" tapped")).parse(input)?;
        let (input, _) = alt((tag(" if it's "), tag(" if it is "))).parse(input)?;
        let (input, _) = take_until(" card").parse(input)?;
        let (input, _) = tag(" card").parse(input)?;
        let (input, _) = opt(tag(".")).parse(input)?;
        Ok((input, ()))
    }

    let bare = strip_search_result_subject(lower.trim().trim_end_matches('.'));
    parse_clause(bare).is_ok()
        || nom_primitives::scan_at_word_boundaries(lower, |input| {
            parse_clause(strip_search_result_subject(input))
        })
        .is_some()
}

/// Parse count from "choose one/two/three/N of them/those" text using nom combinator.
/// Handles all chooser prefix forms: "choose ", "you choose ", "an opponent chooses ",
/// "target opponent chooses ".
fn parse_choose_count_from_text(lower: &str) -> u32 {
    // Strip chooser prefix using nom combinators (input already lowercase).
    let rest = alt((tag("an opponent chooses "), tag("target opponent chooses ")))
        .parse(lower)
        .map(|(rest, _)| rest)
        .unwrap_or_else(|_: nom::Err<OracleError<'_>>| {
            let s = tag::<_, _, OracleError<'_>>("you ")
                .parse(lower)
                .map(|(rest, _)| rest)
                .unwrap_or(lower);
            alt((tag::<_, _, OracleError<'_>>("choose "), tag("chooses ")))
                .parse(s)
                .map(|(rest, _)| rest)
                .unwrap_or(s)
        });
    // Delegate to nom combinator for the number.
    nom_primitives::parse_number
        .parse(rest)
        .map(|(_, n)| n)
        .unwrap_or(1)
}

fn parse_choice_partition_destinations(lower: &str) -> Option<(Zone, Zone)> {
    parse_put_choice_partition_destinations(lower)
        .or_else(|| parse_shuffle_choice_partition_destinations(lower))
}

fn starts_have_base_power_toughness(input: &str) -> bool {
    value(
        (),
        (
            tag_no_case::<_, _, OracleError<'_>>("have"),
            multispace1,
            tag_no_case("base"),
            multispace1,
            tag_no_case("power"),
            multispace1,
            tag_no_case("and"),
            multispace1,
            tag_no_case("toughness"),
            multispace1,
        ),
    )
    .parse(input)
    .is_ok()
}

fn parse_put_chosen_cards_at_library_position(lower: &str) -> Option<LibraryPosition> {
    value(
        LibraryPosition::Top,
        all_consuming((
            tag::<_, _, OracleError<'_>>("put those cards on top"),
            opt(alt((
                tag(" of your library"),
                tag(" of their owner's library"),
            ))),
            tag(" in any order"),
            opt(tag(".")),
        )),
    )
    .parse(lower.trim())
    .map(|(_, position)| position)
    .ok()
}

fn parse_put_choice_partition_destinations(lower: &str) -> Option<(Zone, Zone)> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("put ").parse(lower).ok()?;
    let (rest, _) = parse_chosen_cards_reference(rest).ok()?;
    let (rest, chosen_destination) = parse_choice_partition_destination(rest).ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" and ").parse(rest).ok()?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>("put ")).parse(rest).ok()?;
    let (rest, _) = parse_rest_cards_reference(rest).ok()?;
    let (_, rest_destination) = parse_choice_partition_destination(rest).ok()?;
    Some((chosen_destination, rest_destination))
}

fn parse_shuffle_choice_partition_destinations(lower: &str) -> Option<(Zone, Zone)> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("shuffle ").parse(lower).ok()?;
    let (rest, _) = parse_chosen_cards_reference(rest).ok()?;
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>(" into your library"),
        tag(" into their library"),
        tag(" into its owner's library"),
    ))
    .parse(rest)
    .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" and put ").parse(rest).ok()?;
    let (rest, _) = parse_rest_cards_reference(rest).ok()?;
    let (_, rest_destination) = parse_choice_partition_destination(rest).ok()?;
    Some((Zone::Library, rest_destination))
}

fn parse_chosen_cards_reference(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>("the chosen cards"),
            tag("the chosen card"),
        )),
    )
    .parse(input)
}

pub(super) fn parse_rest_cards_reference(
    input: &str,
) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>("the rest"),
            tag("the other cards"),
            tag("the other card"),
            // CR 608.2c: Bare "the other" ("...and the other into your hand")
            // appears in cultivate-class splits. Listed LAST so the longer
            // "the other card(s)" forms above are matched first (no shadowing).
            tag("the other"),
        )),
    )
    .parse(input)
}

/// CR 701.20a: Detect the rest-pile zone in a `RevealUntil` continuation
/// chunk. The "rest" subject may be phrased as "the rest" / "all other cards
/// revealed this way" / "the other cards" — and may be governed by an
/// imperative verb that itself encodes the zone ("exile all other cards
/// revealed this way" → Exile).
///
/// Returns `None` only when no rest-subject phrase is present in `lower`.
/// When a rest subject is detected but no explicit destination phrase is
/// found, defaults to `Zone::Library` (covers "on the bottom", "in any
/// order", "shuffles ... into their library", and the bare "and the rest"
/// variant). This matches the prior behavior of the kept-card and
/// standalone-rest arms before consolidation.
fn parse_reveal_until_rest_zone(lower: &str) -> Option<Zone> {
    // CR 701.20a: Recognize all rest-subject phrasings used across the
    // RevealUntil family. "the rest" is the canonical form; "all other cards"
    // and "the other cards" appear in Hermit Druid, Avenging Druid, Demonic
    // Consultation, Sacred Guide, Spoils of the Vault, Reviving Vapors, etc.
    let has_rest_subject = nom_primitives::scan_contains(lower, "the rest")
        || nom_primitives::scan_contains(lower, "all other cards")
        || nom_primitives::scan_contains(lower, "other cards revealed this way");
    if !has_rest_subject {
        return None;
    }

    // CR 701.20a: Imperative verb "exile" preceding the rest subject routes
    // the rest pile to exile (Aesthetic Consultation, Demonic Consultation,
    // Divining Witch, Sacred Guide, Spoils of the Vault).
    if nom_primitives::scan_contains(lower, "exile all other cards")
        || nom_primitives::scan_contains(lower, "exile the rest")
        || nom_primitives::scan_contains(lower, "exile the other cards")
    {
        return Some(Zone::Exile);
    }

    // Possessive variants for graveyard cover both single-controller
    // ("your", "their") and multi-controller ("their owners'") forms. The
    // multi-owner form is used by Dance, Pathetic Marionette where each
    // opponent's revealed cards return to their respective graveyards.
    if nom_primitives::scan_contains(lower, "into your graveyard")
        || nom_primitives::scan_contains(lower, "into their graveyard")
        || nom_primitives::scan_contains(lower, "into their owners' graveyards")
    {
        return Some(Zone::Graveyard);
    }

    // Default: bottom of library — covers "on the bottom of your library",
    // "in any order", "shuffles ... into their library", and the bare
    // "and the rest" with no zone phrase.
    Some(Zone::Library)
}

pub(super) fn parse_choice_partition_destination(
    input: &str,
) -> Result<(&str, Zone), nom::Err<OracleError<'_>>> {
    alt((
        value(
            Zone::Graveyard,
            alt((
                tag::<_, _, OracleError<'_>>(" into your graveyard"),
                tag(" into their graveyard"),
                tag(" into its owner's graveyard"),
            )),
        ),
        value(
            Zone::Hand,
            alt((
                tag::<_, _, OracleError<'_>>(" into your hand"),
                tag(" into their hand"),
            )),
        ),
        value(
            Zone::Library,
            alt((
                tag::<_, _, OracleError<'_>>(" into your library"),
                tag(" into their library"),
                tag(" into its owner's library"),
                tag(" on the bottom of your library"),
                tag(" on the bottom of their library"),
            )),
        ),
        value(
            Zone::Exile,
            alt((
                tag::<_, _, OracleError<'_>>(" into exile"),
                tag(" in exile"),
            )),
        ),
    ))
    .parse(input)
}

fn append_definition_to_sub_chain(ability: &mut AbilityDefinition, next: AbilityDefinition) {
    let mut cursor = ability;
    loop {
        if cursor.sub_ability.is_none() {
            if cursor.optional
                && matches!(*cursor.effect, Effect::CastFromZone { .. })
                && matches!(
                    *next.effect,
                    Effect::PutAtLibraryPosition {
                        target: TargetFilter::ExiledBySource,
                        ..
                    }
                )
            {
                cursor.else_ability = Some(Box::new(next.clone()));
            }
            cursor.sub_ability = Some(Box::new(next));
            break;
        }
        cursor = cursor
            .sub_ability
            .as_mut()
            .expect("sub_ability checked above")
            .as_mut();
    }
}

fn deepest_effect(ability: &AbilityDefinition) -> &Effect {
    let mut cursor = ability;
    while let Some(sub) = cursor.sub_ability.as_deref() {
        cursor = sub;
    }
    &cursor.effect
}

fn plotted_grant_target(previous: &AbilityDefinition) -> TargetFilter {
    match deepest_effect(previous) {
        Effect::ChangeZone {
            destination: Zone::Exile,
            target: TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. },
            ..
        } => TargetFilter::TrackedSet {
            id: crate::types::identifiers::TrackedSetId(0),
        },
        Effect::ChangeZone {
            destination: Zone::Exile,
            ..
        } => TargetFilter::ParentTarget,
        _ => TargetFilter::ParentTarget,
    }
}

fn parse_becomes_plotted_continuation(lower: &str) -> bool {
    let text = lower.trim().trim_end_matches('.').trim(); // allow-noncombinator: punctuation cleanup before all_consuming
    all_consuming(alt((
        value((), tag::<_, _, OracleError<'_>>("it becomes plotted")),
        value((), tag("that card becomes plotted")),
        value((), tag("they become plotted")),
    )))
    .parse(text)
    .is_ok()
}

fn parse_put_all_back_in_any_order(lower: &str) -> bool {
    (
        tag::<_, _, OracleError<'_>>("put "),
        alt((tag("them"), tag("those cards"), tag("the cards"))),
        tag(" back"),
        alt((
            tag(" in any order"),
            tag(" on top of your library in any order"),
            tag(" on top in any order"),
        )),
        opt(tag(".")),
        eof,
    )
        .parse(lower.trim())
        .is_ok()
}

fn parse_put_one_dig_card_on_top(lower: &str) -> bool {
    (
        alt((
            tag::<_, _, OracleError<'_>>("you may put "),
            tag("may put "),
            tag("put "),
        )),
        alt((tag("one of those cards"), tag("one of them"))),
        tag(" back "),
        alt((tag("on top of your library"), tag("on top"))),
        opt(tag(".")),
        eof,
    )
        .parse(lower.trim())
        .is_ok()
}

fn parse_exile_rest_after_dig(lower: &str) -> bool {
    (
        tag::<_, _, OracleError<'_>>("exile the rest"),
        opt(tag(".")),
        eof,
    )
        .parse(lower.trim())
        .is_ok()
}

/// CR 406.3 + CR 701.16a: Recognize the "[then] exile it/them/that card/those
/// cards/the card [face down]" clause that follows a private `Dig` look step —
/// the Gonti, Canny Acquisitor impulse idiom. Returns `Some(face_down)` when the
/// whole clause matches (`face_down = true` only for the explicit hidden-
/// information suffix). Composes the (pronoun × optional "face down") axes with
/// nom combinators rather than enumerating the permutations as match-arm
/// literals; the clause-boundary splitter has already stripped the leading
/// "then" connector.
fn parse_exile_looked_at_card(lower: &str) -> Option<bool> {
    let trimmed = lower.trim().trim_end_matches('.').trim_end();
    let (rest, _) = tag::<_, _, OracleError<'_>>("exile ").parse(trimmed).ok()?;
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("it"),
        tag("them"),
        tag("that card"),
        tag("those cards"),
        tag("the card"),
    ))
    .parse(rest)
    .ok()?;
    let (rest, face_down) = alt((
        value(
            true,
            preceded(
                multispace1::<_, OracleError<'_>>,
                tag::<_, _, OracleError<'_>>("face down"),
            ),
        ),
        value(false, eof::<_, OracleError<'_>>),
    ))
    .parse(rest)
    .ok()?;
    eof::<_, OracleError<'_>>(rest).ok()?;
    Some(face_down)
}

pub(super) fn split_clause_sequence(text: &str) -> Vec<ClauseChunk> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    let mut paren_depth = 0usize;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    // CR 109.5 + CR 115.1: once a compound-subject-each head ("you and" / "~ and"
    // followed by "<noun> each") is detected, keep the WHOLE distributed body
    // intact for the rest of the current chunk — its internal " and "s
    // ("get +2/+0 and gain haste ... and attack this turn if able") are body
    // delimiters owned by `try_parse_compound_subject_each`, not clause splits.
    let mut compound_subject_each_sticky = false;

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
                if !in_double_quote {
                    let remainder = chars.clone().collect::<String>();
                    if quote_closes_sentence_before_sequence(&current, &remainder) {
                        push_clause_chunk(&mut chunks, &current, Some(ClauseBoundary::Sentence));
                        current.clear();
                        while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
                            chars.next();
                        }
                    }
                }
            }
            ',' if paren_depth == 0 && !in_single_quote && !in_double_quote => {
                let remainder = chars.clone().collect::<String>();
                if let Some((boundary, chars_to_skip)) =
                    split_comma_clause_boundary(&current, &remainder)
                {
                    push_clause_chunk(&mut chunks, &current, Some(boundary));
                    current.clear();
                    compound_subject_each_sticky = false;
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
                compound_subject_each_sticky = false;
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
                    // CR 603.7a: Suppress bare-and splitting inside temporal prefix
                    // clauses (e.g., "at the beginning of your next upkeep, draw a
                    // card and gain 3 life"). The entire compound inner effect must
                    // stay as one clause so CreateDelayedTrigger wraps all effects.
                    // CR 608.2c: Preserve targeted compound actions so the effect
                    // parser can retarget continuation clauses like
                    // "tap target creature ... and put a stun counter on it".
                    let targeted_compound_continuation =
                        nom_primitives::scan_contains(&before_lower, "target")
                            && tag::<_, _, OracleError<'_>>("put ")
                                .parse(remainder_trimmed)
                                .is_ok();
                    // CR 615 + CR 615.5: "[If damage would be dealt to <target>
                    // this turn,] prevent that damage and put that many <kind>
                    // counter(s) on <target>" — the rider is the prevention
                    // follow-up, not a separate clause. The full sentence is
                    // owned by `try_parse_conditional_damage_prevention_with_followup`
                    // and bisecting here would strip the rider into a fresh
                    // chunk whose "on it" pronoun re-binds to the trigger source
                    // via `resolve_pronoun_target` instead of the parent
                    // target. Same suppression shape as the "tap target
                    // creature ... and put a stun counter on it" continuation.
                    let prevent_then_put_continuation =
                        nom_primitives::scan_contains(&before_lower, "prevent that damage")
                            && tag::<_, _, OracleError<'_>>("put ")
                                .parse(remainder_trimmed)
                                .is_ok();
                    // CR 701.18a + CR 701.23: "search [zones] for [filter] and exile them"
                    // is a single compound search-and-exile action — keep it together so
                    // the imperative dispatcher can recognize the multi-zone pattern.
                    // Accepts "search ..." and "then search ..." prefixes, and either
                    // "with that name" or "with the same name as that card" suffixes.
                    let has_search_prefix = nom_primitives::scan_contains(&before_lower, "search ");
                    let search_with_that_name = has_search_prefix
                        && parse_search_exile_name_suffix(&before_lower).is_ok()
                        && tag::<_, _, OracleError<'_>>("exile them")
                            .parse(remainder_trimmed)
                            .is_ok();
                    // CR 707.9: ", except <body> and <body> [and …]" — inside
                    // a copy-effect except clause, " and " is an internal
                    // delimiter between recognised body shapes (SetName, P/T,
                    // type additions, "has this ability", etc.) handled by
                    // the shared `become_copy_except` parser. The chain
                    // splitter must NOT bisect the body at this " and ", or
                    // the second body fragment ("and she has this ability")
                    // becomes a stray sub_ability and never reaches the
                    // except parser.
                    //
                    // `scan_contains` matches phrases starting at word
                    // boundaries (post-space), so we probe for the bare word
                    // "except " rather than ", except " — a leading comma
                    // never sits at a word start.
                    let inside_except_clause =
                        nom_primitives::scan_contains(&before_lower, "except ");
                    let choice_partition_remainder =
                        nom_primitives::scan_contains(&before_lower, "the chosen card")
                            && (opt(tag::<_, _, OracleError<'_>>("put ")), tag("the rest"))
                                .parse(remainder_trimmed)
                                .is_ok();
                    let sacrifice_rest_remainder = preceded(
                        opt(tag::<_, _, OracleError<'_>>("then ")),
                        alt((
                            tag::<_, _, OracleError<'_>>("sacrifices the rest"),
                            tag("sacrifice the rest"),
                        )),
                    )
                    .parse(remainder_trimmed)
                    .is_ok();
                    // CR 109.5 + CR 608.2c + CR 800.4g: "you and that player each <body>"
                    // (and analogous "you and <player-noun> each <body>" compound
                    // subjects) is a SINGLE compound subject distributing the body
                    // across two recipients — not two separate clauses.
                    // `try_parse_compound_subject_each` in the effect parser owns the
                    // distribution logic; here we must keep the text as one chunk so
                    // the combinator sees the full prefix.
                    //
                    // The detection is tight: the chunk-so-far must be exactly the
                    // first-subject token — "you" (player axis) or "~" (the
                    // self-reference, object axis; e.g. Gogo's "~ and that creature
                    // each ...") — so we do not suppress mid-sentence "you draw a
                    // card and that player draws a card" (two clauses). The remainder
                    // must start with a compound-subject noun phrase followed by
                    // " each " — distinguishing it from the standard clause-starter
                    // "that player <verb>" (which is a separate clause).
                    // CR 603.12 + CR 109.5 + CR 115.1: strip leading reflexive
                    // connector ("if you do, ", "when you do, ", ...) so the
                    // compound-subject body stays intact even when introduced by
                    // an "If you do," reflexive frame (Gogo, Mysterious Mime).
                    let trimmed = before_lower.trim_end();
                    let first_subject_token =
                        crate::parser::oracle_nom::condition::parse_reflexive_conditional_connector(
                            trimmed,
                        )
                        .map(|(rest, _)| rest.trim())
                        .unwrap_or(trimmed);
                    let compound_subject_each = (first_subject_token == "you"
                        || first_subject_token == "~")
                        && remainder_trimmed_starts_with_compound_subject_each(remainder_trimmed);
                    if compound_subject_each {
                        compound_subject_each_sticky = true;
                    }
                    // CR 608.2c: "Otherwise, X and Y" — the body following an
                    // "otherwise" prefix is a single Otherwise branch even when
                    // it contains an internal " and ". Without this guard the
                    // splitter peels "Y" off as a sibling clause that then
                    // attaches as a sub_ability of the conditional's PARENT
                    // effect instead of the else_ability body — the exemplar
                    // is Approach of the Second Sun's "Otherwise, put ~ into
                    // its owner's library seventh from the top and you gain
                    // 7 life" where "you gain 7 life" must stay inside the
                    // otherwise branch.
                    //
                    // Match only the printed Oracle-text shapes ("otherwise,
                    // " and "otherwise "), mirroring the otherwise-prefix
                    // table in `starts_prefix_clause`. This rejects accidental
                    // prefix overlap from any future text whose first word
                    // shares those letters but is not the conditional fallback
                    // keyword.
                    let inside_otherwise_body = alt((
                        tag::<_, _, OracleError<'_>>("otherwise, "),
                        tag("otherwise "),
                    ))
                    .parse(before_lower.trim_start())
                    .is_ok();
                    // CR 613.1d + CR 613.4b: "have base power and toughness N/N"
                    // is a layer-7b continuous modification, never an imperative
                    // clause starter. Suppress the split so
                    // `parse_continuous_modifications` can handle the compound
                    // (e.g. "lose all abilities and have base power and toughness
                    // 1/1 until end of turn") as a single GenericEffect with the
                    // correct `affected` filter inherited from the subject.
                    let have_base_pt_continuation =
                        starts_have_base_power_toughness(remainder_trimmed);
                    let continuous_modifier_conjunct =
                        starts_you_control_subject_predicate(&before_lower)
                            && alt((
                                tag::<_, _, OracleError<'_>>("gain "),
                                tag("gains "),
                                tag("have "),
                                tag("has "),
                            ))
                            .parse(remainder_trimmed)
                            .is_ok();
                    // CR 706.2: "roll a d{N} and (add|subtract) {quantity}" —
                    // the modifier clause is part of the same RollDie effect
                    // (it shifts the natural result) and must NOT be peeled
                    // off as a sibling clause. Without this suppression
                    // "Roll a d20 and add the number of cards in your hand"
                    // would split into ["Roll a d20", "add ..."] and the
                    // modifier silently becomes an Unimplemented sub_ability
                    // — bypassing the typed modifier path on every D&D-set
                    // d20 card.
                    let roll_die_modifier_continuation = ends_with_roll_die_phrase(&before_lower)
                        && alt((tag::<_, _, OracleError<'_>>("add "), tag("subtract ")))
                            .parse(remainder_trimmed)
                            .is_ok();
                    let suppress = (nom_primitives::scan_contains(&before_lower, "from among")
                        && !sacrifice_rest_remainder)
                        || is_inside_temporal_prefix(&before_lower)
                        || targeted_compound_continuation
                        || prevent_then_put_continuation
                        || search_with_that_name
                        || inside_except_clause
                        || choice_partition_remainder
                        || compound_subject_each
                        || compound_subject_each_sticky // CR 109.5 + CR 115.1: keep the whole compound-subject body intact
                        || inside_otherwise_body
                        || have_base_pt_continuation
                        || continuous_modifier_conjunct
                        || roll_die_modifier_continuation;
                    if !suppress && starts_bare_and_clause(remainder_trimmed) {
                        push_clause_chunk(&mut chunks, before_and, Some(ClauseBoundary::Comma));
                        current.clear();
                        compound_subject_each_sticky = false;
                    } else if !suppress {
                        // CR 508.1d / CR 509.1c: "<subj> gains <keyword> until end
                        // of turn and <attack|must-be-blocked> ... if able" — the
                        // trailing conjunct is a recognized standalone combat
                        // requirement that is NOT verb-headed by any entry in
                        // `starts_bare_and_clause`'s list, so the bare-and logic
                        // above never splits it. Split it here and prepend
                        // conjunct 1's subject so each half reaches its existing
                        // parser with the correct `affected`. The combat-requirement
                        // gate keeps multi-keyword lists ("gains flying and haste")
                        // — which do NOT match the recognizer — on the untouched
                        // single-clause path.
                        if let Some(prepend) =
                            combat_requirement_conjunct_prepend(before_and, remainder_trimmed)
                        {
                            push_clause_chunk(&mut chunks, before_and, Some(ClauseBoundary::Comma));
                            current.clear();
                            compound_subject_each_sticky = false;
                            current.push_str(&prepend);
                        }
                    }
                }
            }
        }
    }

    push_clause_chunk(&mut chunks, &current, None);
    chunks
}

fn quote_closes_sentence_before_sequence(current: &str, remainder: &str) -> bool {
    let quoted_text_ends_sentence = current
        .chars()
        .rev()
        .nth(1)
        .is_some_and(|ch| matches!(ch, '.' | '!' | '?'));
    if !quoted_text_ends_sentence {
        return false;
    }

    let trimmed = remainder.trim_start();
    let trimmed_lower = trimmed.to_ascii_lowercase();
    let sequence_starts = alt((
        tag::<_, _, OracleError<'_>>("then, if "),
        tag("then if "),
        tag("then "),
        tag("if "),
        tag("otherwise"),
    ))
    .parse(trimmed_lower.as_str())
    .is_ok();
    sequence_starts
}

fn parse_search_exile_name_suffix(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    let (rest, _) = take_until::<_, _, OracleError<'_>>("with ").parse(input)?;
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("with that name"),
        tag("with the chosen name"),
        tag("with the same name as that card"),
    ))
    .parse(rest)?;
    let (rest, _) = eof.parse(rest)?;
    Ok((rest, ()))
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
    // CR 701.18a: Enumerated "search" prefixes — do NOT use contains(" search ").
    let search_start = alt((
        tag::<_, _, OracleError<'_>>("search "),
        tag("then search "),
        tag("you may search "),
        tag("you search "),
        tag("then you may search "),
        tag("then you search "),
    ))
    .parse(current_lower.as_str())
    .is_ok();
    if search_start
        && alt((tag::<_, _, OracleError<'_>>("reveal "), tag("put ")))
            .parse(trimmed_lower.as_str())
            .is_ok()
    {
        return None;
    }

    if tag::<_, _, OracleError<'_>>("then ")
        .parse(trimmed_lower.as_str())
        .is_ok()
    {
        let after_then = &trimmed["then ".len()..];
        let after_then_lower = &trimmed_lower["then ".len()..];
        if starts_clause_text_or_conjugated(after_then)
            || starts_you_control_subject_predicate(after_then_lower)
            || starts_with_damage_clause(after_then_lower)
        {
            return Some((ClauseBoundary::Then, whitespace_len + "then ".len()));
        }
    }

    // CR 120.2b + CR 608.2c: Multi-target damage split — "deals A damage to
    // T1, B damage to T2[, and C damage to T3]" (Cone of Flame, Banshee,
    // Serpentine Spike). When the closing chunk already established a
    // damage event AND the next segment is a bare "<amount> damage" tail,
    // the comma is a within-effect delimiter — not a clause boundary. Keep
    // the line as one chunk so `try_parse_multi_target_damage_chain` can
    // build the chained DealDamage sub_abilities.
    if current_ends_with_damage_recipient(&current_lower)
        && starts_with_damage_amount_continuation(&trimmed_lower)
    {
        return None;
    }

    if starts_clause_text_or_conjugated(trimmed) || starts_with_damage_clause(&trimmed_lower) {
        return Some((ClauseBoundary::Comma, whitespace_len));
    }

    // Strip "and " connector before checking clause start
    // Handles patterns like ", and get {E}{E}" or ", and draw a card"
    if let Ok((after_and, _)) = tag::<_, _, OracleError<'_>>("and ").parse(trimmed_lower.as_str()) {
        // Multi-target damage chain final segment — same gate as the leading
        // "and" form but for ", and B damage to T2".
        if current_ends_with_damage_recipient(&current_lower)
            && starts_with_damage_amount_continuation(after_and)
        {
            return None;
        }
        if starts_clause_text_or_conjugated(after_and) || starts_with_damage_clause(after_and) {
            return Some((ClauseBoundary::Comma, whitespace_len));
        }
    }

    None
}

/// CR 120.2b: True when the closing chunk text contains a `damage to `
/// boundary at a word position (i.e., the chunk has already established a
/// damage event with a recipient). Used by the multi-target damage chain
/// detector to recognize a continuation comma instead of a clause boundary.
fn current_ends_with_damage_recipient(current_lower: &str) -> bool {
    nom_primitives::scan_contains(current_lower, "damage to ")
}

/// CR 120.2b: True when `trimmed_lower` (post-comma, post-optional-"and ")
/// begins with a bare amount + "damage" tail — i.e. a damage continuation
/// segment that should be re-attached to the preceding damage clause.
///
/// Recognised amount shapes mirror [`parse_bare_damage_continuation`]:
/// fixed numbers, `half X`/`half <ref>`, `twice that much`, `that much`,
/// `X`. Each must be immediately followed by ` damage`.
fn starts_with_damage_amount_continuation(trimmed_lower: &str) -> bool {
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("twice that much damage"),
        tag("that much damage"),
    ))
    .parse(trimmed_lower)
    {
        return rest.is_empty() || rest.starts_with([' ', ',', '.']);
    }
    let Some((_amount, rest)) = crate::parser::oracle_util::parse_count_expr(trimmed_lower) else {
        return false;
    };
    tag::<_, _, OracleError<'_>>("damage").parse(rest).is_ok()
}

fn starts_prefix_clause(current_lower: &str) -> bool {
    // CR 603.7a: Temporal prefix clauses must not be split on their internal comma.
    // CR 611.2b: "For as long as [condition], [effect]" — duration prefix clause.
    alt((
        tag::<_, _, OracleError<'_>>("until "),
        tag("after "),
        tag("if "),
        tag("when "),
        tag("whenever "),
        tag("for each "),
        tag("then if "),
        // "then, if ..." (with comma after "then") — same scoping as "then if".
        // Regression: A Good Thing ("Then, if you have 1,000 or more life, you
        // lose the game") — without this, the splitter bisects the conditional
        // at the comma between life and "you lose", orphaning the body.
        tag("then, if "),
        tag("otherwise"),
        tag("if not"),
        tag("the next time "),
        tag("at the beginning "),
        tag("for as long as "),
    ))
    .parse(current_lower)
    .is_ok()
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
    starts_clause_text_lower(&lower)
}

/// Check whether `text` begins with a conjugated (third-person) verb form that,
/// after deconjugation, would match a recognized imperative verb.
///
/// This handles patterns like "draws seven cards" or "sacrifices a creature"
/// where the subject carries over from the prior clause (e.g.,
/// "Each player discards their hand, then draws seven cards.").
///
/// Uses `normalize_verb_token` for irregular forms (does→do, has→have, copies→copy)
/// and the standard -s stripping for regular verbs.
pub(super) fn starts_clause_text_or_conjugated(text: &str) -> bool {
    if starts_clause_text(text) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    let first_word = lower.split_whitespace().next().unwrap_or("");
    // Only attempt deconjugation on words ending in 's' that aren't already
    // recognized — avoids false positives on noun phrases.
    if !first_word.ends_with('s') || first_word.ends_with("ss") {
        return false;
    }
    // Exclude possessive pronouns and determiners that happen to end in 's'
    // but are not conjugated verbs (e.g., "its", "this", "those").
    if matches!(
        first_word,
        "its" | "this" | "those" | "his" | "less" | "plus" | "as"
    ) {
        return false;
    }
    let base = super::normalize_verb_token(first_word);
    if base == first_word {
        return false; // normalize_verb_token didn't change it — not a conjugated verb
    }
    // Reconstruct with the base form and check again.
    let rest = &lower[first_word.len()..];
    let deconjugated = format!("{base}{rest}");
    starts_clause_text_lower(&deconjugated)
}

/// CR 706.2: True iff `before_lower` ends with a "roll a d{N}" phrase (with
/// the standard set of polyhedral side counts) or the word-form variants
/// "six-sided die", "twenty-sided die", etc. Used by the bare-and splitter to
/// keep "roll a d20 and add/subtract X" intact so the typed modifier path
/// fires instead of producing a stray Unimplemented sub_ability.
fn ends_with_roll_die_phrase(before_lower: &str) -> bool {
    let trimmed = before_lower.trim_end();
    // Numeric form: any "roll a d<digits>" tail. allow-noncombinator:
    // structural rsplit on a runtime ' ' separator (not a literal dispatch
    // token) followed by a char-class digit scan — no string-method dispatch
    // against any literal phrase.
    if let Some(last_word) = trimmed.rsplit(' ').next() {
        // allow-noncombinator: CR 706.1a "d{digits}" structural shape — single-char prefix `d` followed by an ASCII-digit run on a pre-tokenized last-word slice; this is a CR-spec character-class scan, not dispatch against any literal phrase.
        if let Some(digits) = last_word.strip_prefix('d') {
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                return true;
            }
        }
    }
    // Word-form: "...-sided die". The tail must match one of the polyhedral
    // phrases. Take the last three space-separated tokens (`<N>-sided die`)
    // and parse them with nom alternatives.
    let tail = trimmed.rsplitn(3, ' ').collect::<Vec<_>>();
    if tail.len() != 3 {
        return false;
    }
    // tail is reversed; reconstruct "<N>-sided die" by re-joining.
    let candidate = format!("{} {}", tail[1], tail[0]);
    let parsed: Result<((), ()), nom::Err<OracleError<'_>>> = alt((
        value((), tag::<_, _, OracleError<'_>>("four-sided die")),
        value((), tag("six-sided die")),
        value((), tag("eight-sided die")),
        value((), tag("ten-sided die")),
        value((), tag("twelve-sided die")),
        value((), tag("twenty-sided die")),
    ))
    .parse(candidate.as_str())
    .map(|(_, v)| ((), v));
    parsed.is_ok()
}

fn starts_you_control_subject_predicate(s: &str) -> bool {
    let Ok((after_subject, subject)) =
        take_until::<_, _, OracleError<'_>>(" you control ").parse(s)
    else {
        return false;
    };
    if subject.trim().is_empty() {
        return false;
    }
    let Ok((predicate, _)) = tag::<_, _, OracleError<'_>>(" you control ").parse(after_subject)
    else {
        return false;
    };
    alt((
        tag::<_, _, OracleError<'_>>("get "),
        tag("gets "),
        tag("gain "),
        tag("gains "),
        tag("have "),
        tag("has "),
    ))
    .parse(predicate)
    .is_ok()
}

/// Inner implementation operating on pre-lowercased input.
fn starts_clause_text_lower(s: &str) -> bool {
    if starts_multiword_keyword_continuation(s) {
        return false;
    }

    // Table-driven prefix check via nom tag() — try all imperative verbs and
    // pronoun/determiner clause starters.  Split into multiple alt() groups
    // chained with .or() to stay within nom's 21-tuple limit.
    alt((
        value((), tag::<_, _, OracleError<'_>>("add ")),
        value((), tag("all ")),
        value((), tag("attach ")),
        value((), tag("airbend ")),
        value((), tag("cast ")),
        value((), tag("counter ")),
        value((), tag("create ")),
        value((), tag("deal ")),
        value((), tag("destroy ")),
        value((), tag("discard ")),
        value((), tag("draw ")),
        value((), tag("earthbend ")),
        value((), tag("each player ")),
        value((), tag("each opponent ")),
        value((), tag("each ")),
        value((), tag("exile ")),
        value((), tag("explore")),
        value((), tag("fight ")),
        value((), tag("flip ")),
        value((), tag("investigate")),
        value((), tag("gain control ")),
    ))
    .or(alt((
        value((), tag("gain ")),
        value((), tag("get ")),
        value((), tag("have ")),
        value((), tag("look at ")),
        value((), tag("lose ")),
        value((), tag("mill ")),
        value((), tag("proliferate")),
        value((), tag("put ")),
        value((), tag("return ")),
        value((), tag("reveal ")),
        value((), tag("roll ")),
        value((), tag("sacrifice ")),
        value((), tag("scry ")),
        value((), tag("search ")),
        value((), tag("shuffle")),
        value((), tag("surveil ")),
        value((), tag("tap ")),
        value((), tag("that ")),
        value((), tag("this ")),
        value((), tag("those ")),
    )))
    .or(value((), tag("open ")))
    .or(alt((
        value((), tag("conjure ")),
        value((), tag("target ")),
        value((), tag("transform ")),
        value((), tag("unattach ")),
        value((), tag("untap ")),
        value((), tag("you may ")),
        value((), tag("you ")),
        value((), tag("incubate ")),
        value((), tag("it ")),
        value((), tag("its controller ")),
        value((), tag("copy ")),
        value((), tag("double ")),
        value((), tag("goad ")),
        value((), tag("manifest ")),
        value((), tag("populate")),
        // CR 608.2c (issue #534): "choose " as a clause starter so chains
        // like "..., then choose an opponent" are split at the comma.
        // Without this, "choose an opponent" stays glued to the preceding
        // clause and `try_parse_choose_player_to_verb` is never invoked.
        value((), tag("choose ")),
        value((), tag("remove ")),
        value((), tag("seek ")),
        value((), tag("connive")),
        value((), tag("they ")),
    )))
    .parse(s)
    .is_ok()
}

fn starts_multiword_keyword_continuation(s: &str) -> bool {
    let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("double strike"),
        tag("double team"),
    ))
    .parse(s) else {
        return false;
    };
    rest.chars()
        .next()
        .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
}

/// CR 603.7a: Check if accumulated clause text begins with a temporal prefix
/// (delayed trigger condition), indicating the clause body should not be split.
/// These prefixes create CreateDelayedTrigger wrappers in parse_effect_chain_ir,
/// and splitting the inner compound effect would leave only the first sub-effect
/// wrapped while the remainder becomes a separate top-level clause.
fn is_inside_temporal_prefix(lower: &str) -> bool {
    // Check the raw accumulated text (which may include a leading comma+space
    // from a prior clause boundary). The temporal prefix starts the clause.
    let trimmed = lower.trim_start_matches(|c: char| c == ',' || c.is_whitespace());
    alt((
        tag::<_, _, OracleError<'_>>("at the beginning of the next "),
        tag("at the beginning of your next "),
        tag("at the end of "),
    ))
    .parse(trimmed)
    .is_ok()
}

/// CR 109.5 + CR 115.1 + CR 608.2c: Detect that the remainder after the first
/// subject ("you and" / "~ and") starts a compound-subject distribution clause:
/// "<noun> each <body>".
///
/// Used by the chunk splitter to suppress " and " splitting when the entire
/// phrase is a single compound subject ("you and that player each Y", "~ and
/// that creature each Y") rather than two clauses joined by "and". The
/// recognized noun phrases mirror the expansion axis in
/// `try_parse_compound_subject_each`; new compound forms are added by extending
/// both sites in lockstep.
///
/// Recognized second-subject axes (mirror `try_parse_compound_subject_each`):
/// - "that player each" — the player-axis form (Council's-dilemma "for each
///   player who chose <choice>" body).
/// - "target opponent each" / "target player each" — targeted player-axis forms.
/// - "that creature each" — the object-axis form (CR 115.1 parent-target
///   binding; e.g. Gogo, Mysterious Mime's "~ and that creature each get
///   +2/+0 and gain haste ... and attack this turn if able").
fn remainder_trimmed_starts_with_compound_subject_each(remainder: &str) -> bool {
    let lower = remainder.to_ascii_lowercase();
    let result: nom::IResult<&str, (), OracleError<'_>> = alt((
        value((), tag("that player each ")),
        value((), tag("target opponent each ")),
        value((), tag("target player each ")),
        value((), tag("that creature each ")),
    ))
    .parse(lower.as_str());
    result.is_ok()
}

/// Restricted clause-start check for bare " and " splitting (not after comma).
/// Only includes imperative verbs that are unambiguously clause starters —
/// excludes bare pronouns/determiners like "all", "each", "it", "that", "those"
/// which commonly appear in noun phrases after "and"
/// (e.g. "target creature and all other creatures").
///
/// Subject-prefixed verb patterns ("you gain", "you lose", etc.) are safe because
/// "you" + verb is never a noun phrase — it always starts an independent clause.
pub(crate) fn starts_bare_and_clause(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    starts_bare_and_clause_lower(&lower)
}

/// Inner implementation operating on pre-lowercased input.
fn starts_bare_and_clause_lower(s: &str) -> bool {
    // Split into multiple alt() groups chained with .or() for nom's tuple limit.
    let has_verb_prefix = alt((
        value((), tag::<_, _, OracleError<'_>>("add ")),
        value((), tag::<_, _, OracleError<'_>>("create ")),
        value((), tag("destroy ")),
        value((), tag("draw ")),
        value((), tag("discard ")),
        value((), tag("exile ")),
        value((), tag("gain control ")),
        value((), tag("have ")),
        value((), tag("manifest ")),
        value((), tag("mill ")),
        value((), tag("open ")),
        value((), tag("put ")),
        value((), tag("return ")),
        value((), tag("sacrifice ")),
        value((), tag("scry ")),
        value((), tag("search ")),
        value((), tag("shuffle")),
        value((), tag("surveil ")),
        value((), tag("tap ")),
        value((), tag("untap ")),
        // CR 701.27 + CR 701.28: "transform"/"convert" are imperative game actions.
        // Primal Amulet: "remove those counters and transform it" must split here so
        // each clause reaches the effect dispatcher independently.
        value((), tag("transform ")),
    ))
    .or(value((), tag("cast ")))
    .or(value((), tag("cloak ")))
    .or(value((), tag("convert ")))
    // CR 701.20a (issue #516): "reveal " as an imperative clause starter so
    // chains like "choose land or nonland and reveal cards from the top of
    // your library ..." split at the bare " and " and each half reaches its
    // dispatcher (`try_parse_named_choice` for "choose ...", and
    // `try_parse_reveal_until` for "reveal cards ..."). Without this the
    // chunk stays as one clause and `try_parse_named_choice` rejects the
    // "land or nonland and reveal ..." remainder because the second label
    // exceeds the 2-word cap.
    .or(value((), tag("reveal ")))
    .or(value((), tag("returns ")))
    // CR 122.1 + CR 608.2c: third-person "puts" conjugation. Oversimplify
    // class: "Each player creates a … token and puts a number of +1/+1
    // counters on it equal to …" — the subject ("Each player") iterates and
    // the followup must split as its own clause so the Token effect's
    // continuation absorber (`try_parse_put_counters_on_token_followup`)
    // sees the counter-placement on its own and lifts it onto
    // `Token.enter_with_counters`. Mirrors the imperative `put ` axis above
    // — same verb, different conjugation. Sits in the `.or()` chain rather
    // than the first `alt()` tuple because the tuple is already at nom's
    // 21-arm limit; adding it inline would push the cluster over and trip
    // the `Choice<...>` trait-bound check at compile time.
    .or(value((), tag("puts ")))
    .or(alt((
        // CR 608.2c: Subject-prefixed verb patterns — "you [verb]" is always a clause start.
        value((), tag("you gain ")),
        value((), tag("you lose ")),
        value((), tag("you draw ")),
        value((), tag("you create ")),
        value((), tag("you mill ")),
        value((), tag("you scry ")),
        value((), tag("you put ")),
        value((), tag("you exile ")),
        value((), tag("you return ")),
        value((), tag("you sacrifice ")),
        value((), tag("you search ")),
        value((), tag("you surveil ")),
        value((), tag("you get ")),
        value((), tag("you may ")),
        // CR 707.10c: "[subject] may copy this spell and may choose a new
        // target for that copy" — the Chain cycle joins the optional copy and
        // its retarget grant with "and". "may choose" begins a verb phrase,
        // never a noun-phrase continuation, so the split is safe; it lets the
        // retarget clause reach `parse_followup_continuation_ast` rather than
        // being silently dropped as a `copy <target>` remainder.
        value((), tag("may choose ")),
        value((), tag("its controller ")),
        value((), tag("their controller ")),
        // Sword trigger patterns
        value((), tag("you untap ")),
        value((), tag("that player ")),
    )))
    .or(alt((
        // CR 608.2k: "it [conjugated-verb]" is always subject + predicate, never a
        // noun phrase. "doesn't"/"can't"/"cannot" are restriction predicates; "gains"/
        // "gets"/"has" are continuous modification predicates. Safe to split because
        // a bare pronoun followed by a conjugated verb cannot be part of a noun phrase.
        value((), tag::<_, _, OracleError<'_>>("it doesn't ")),
        value((), tag("it can't ")),
        value((), tag("it cannot ")),
        value((), tag("it gains ")),
        value((), tag("it gets ")),
        value((), tag("it has ")),
        value((), tag("it loses ")),
        value((), tag("this creature gets ")),
        value((), tag("~ gets ")),
        // CR 104.3 + CR 119.7 + CR 119.8: Bare-plural-player subject + restriction
        // predicate. Everybody Lives! prints "Players can't lose life this turn
        // and players can't lose the game or win the game this turn." — the
        // conjunction must split so each half parses as its own
        // subject + predicate clause. Safe to split: "players can't" /
        // "players cannot" can only begin a subject-predicate clause, never a
        // noun-phrase continuation.
        value((), tag("players can't ")),
        value((), tag("players cannot ")),
    )))
    // CR 701.63: "<self-ref subject> endures N" as a conjunct ("you lose 1
    // life and this creature endures 1" — Sinkhole Surveyor). The self-ref
    // subject axis (it / this creature / ~) is composed with the "endures "
    // verb as a single unit, not enumerated per permutation. A self-ref
    // pronoun/phrase followed by the conjugated keyword-action verb is always
    // a subject-predicate clause start, never a noun-phrase continuation.
    .or(preceded(
        alt((
            tag::<_, _, OracleError<'_>>("it "),
            tag("this creature "),
            tag("~ "),
        )),
        value((), tag("endures ")),
    ))
    .or(alt((
        // CR 608.2c: Anaphoric back-reference subject + continuous-modification
        // predicate. The subject phrases ("those creatures" / "that creature" /
        // "those permanents" / "that permanent") reference the targets or
        // affected objects established by a prior conjunct in the same chain —
        // e.g. Nalia de'Arnise's "put a +1/+1 counter on each creature you
        // control and those creatures gain deathtouch until end of turn".
        // Without this split, the compound stays as one chunk;
        // `try_split_targeted_compound` bisects it and feeds the conjunct to
        // the imperative-only `parse_imperative_effect`, which has no
        // subject-predicate path and emits `Effect::Unimplemented { name:
        // "those", ... }`. Splitting here routes the conjunct through
        // `parse_clause_ast` → `try_parse_subject_continuous_clause` so the
        // keyword grant and its duration land on the sub-clause. Verb agreement
        // pairs each subject number with its matching continuous predicate:
        // "gain"/"have"/"lose" are the plural-subject stems, "gains"/"has"/
        // "loses" are the singular conjugations. Safe to split: an anaphoric
        // noun phrase followed by a conjugated continuous-modification verb
        // cannot be a continuation noun phrase.
        // Plural anaphoric subjects: "those {creatures,permanents,tokens}" +
        // plural-stem continuous verb. Nested-prefix form (CLAUDE.md "Nest
        // nom combinators by prefix dispatch") so subject ∈ {3 phrases} and
        // verb ∈ {gain,get,have,lose} compose without enumerating all 12
        // tuples, and the overall `alt(...)` arity stays under nom's
        // 21-tuple limit. The first inner `tag` binds the error type for
        // the rest of the tree.
        value(
            (),
            (
                alt((
                    tag::<_, _, OracleError<'_>>("those creatures "),
                    tag("those permanents "),
                    tag("those tokens "),
                )),
                alt((tag("gain "), tag("get "), tag("have "), tag("lose "))),
            ),
        ),
        // Singular anaphoric subjects: "that {creature,permanent,token}" +
        // singular-conjugation continuous verb (gains/gets/has/loses).
        // Single-token grants ("create one X token, that token gains haste")
        // are rarer than the plural form but real, so all three subject
        // nouns are paired with the singular verb set.
        value(
            (),
            (
                alt((
                    tag("that creature "),
                    tag("that permanent "),
                    tag("that token "),
                )),
                alt((tag("gains "), tag("gets "), tag("has "), tag("loses "))),
            ),
        ),
        // CR 602.5 + CR 603.2a + CR 608.2c: Possessive-anaphor activation
        // prohibition conjunct — "[X gets -3/-0] and its activated abilities
        // can't be activated" (Dovin Baan). The possessive subject axis ("its"/
        // "their"/"that creature's"/"that permanent's") is composed with the
        // fixed predicate so the split routes the conjunct through
        // `parse_clause_ast` → `try_parse_subject_restriction_clause`. Safe to
        // split: a possessive subject followed by "activated abilities can't be
        // activated" is always a subject-predicate restriction clause, never a
        // noun-phrase continuation of the prior conjunct.
        value(
            (),
            (
                alt((
                    tag("its "),
                    tag("their "),
                    tag("that creature's "),
                    tag("that creature\u{2019}s "),
                    tag("that permanent's "),
                    tag("that permanent\u{2019}s "),
                )),
                tag("activated abilities can't be activated"),
            ),
        ),
    )))
    .parse(s)
    .is_ok();
    if has_verb_prefix {
        return true;
    }
    // "gain N <noun>" / "lose N <noun>" — imperative with numeric/X argument
    // (e.g., "gain 3 life", "lose 2 life") is a clause start. Bare "gain
    // <keyword>" / "gain a <keyword>" is a continuous-modification rider on
    // the previous pump clause and must NOT split (Heron's Grace, Sorin
    // Solemn Visitor, Soul of Theros, Jeskai Charm, ~14 cards). Discriminator:
    // the token after the verb must be a count expression (digits or "X"
    // followed by a word boundary), not a keyword name.
    if let Ok((rest, _)) = alt((tag::<_, _, OracleError<'_>>("gain "), tag("lose "))).parse(s) {
        // Reject conjugated "gains"/"loses" (handled separately above).
        let conjugated = tag::<_, _, OracleError<'_>>("gains ").parse(s).is_ok()
            || tag::<_, _, OracleError<'_>>("loses ").parse(s).is_ok();
        if !conjugated && next_token_is_count(rest) {
            return true;
        }
    }
    // CR 119.3 + CR 121.1 + CR 608.2c: Conjugated third-person-singular player
    // action verb + count argument. When the prior conjunct established a
    // targeted player subject ("Target player draws X cards and loses X life"
    // — Pact of the Serpent; "Target player draws a card and loses 1 life" —
    // Shadrix Silverquill mode), the second conjunct's verb appears in the
    // singular conjugation with the player subject elided. The verb axis is
    // restricted to player-only actions: "draws N cards" (CR 121.1), "loses
    // N life" (CR 119.3), "gains N life" (CR 119.3). These verbs never apply
    // to non-player subjects in Magic — life is a player-only attribute (CR
    // 119) and drawing is a player-only action (CR 121) — so the split is
    // safe regardless of prior subject. The count+noun discriminator keeps
    // conjugated continuous-modifier forms ("gains flying", "loses all
    // abilities") on the un-split path: those are never followed by a player
    // action count such as "a card" or "1 life". Sibling-clause X-binding
    // (`compute_sentence_where_x`) and player-subject inheritance
    // (`carried_targeted_player_subject`) handle the rest once both chunks
    // reach the chain loop.
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("draws "),
        tag("loses "),
        tag("gains "),
    ))
    .parse(s)
    {
        if next_token_is_player_action_count(rest) {
            return true;
        }
    }
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("get ").parse(s) {
        let rest = rest.trim_start();
        if alt((
            value((), tag::<_, _, OracleError<'_>>("{e}")),
            value((), (nom_primitives::parse_number, multispace1, tag("{e}"))),
            value((), (tag("x"), multispace1, tag("{e}"))),
        ))
        .parse(rest)
        .is_ok()
        {
            return true;
        }
    }
    starts_with_damage_clause(s)
}

/// CR 508.1d / CR 509.1c: For a bare-`" and "` boundary, decide whether the
/// conjunction is "<subj> gain(s) <keyword> until end of turn **and** <combat
/// requirement> ... if able". Returns `Some(prepend)` — the subject text to
/// seed conjunct 2 with — only when BOTH halves match:
///
/// - `before_and` is a continuous clause (contains a gain/get predicate), and
/// - `remainder_trimmed` is a recognized standalone combat requirement
///   ("attack(s) ... if able" / "must be blocked ..." / "can block ...").
///
/// The prepend keeps conjunct 2's `affected` correct: for a *targeted* subject
/// ("target creature ...") it returns the anaphor `"it "` so the chunk loop's
/// unconditional-anaphor rewrite collapses conjunct 2's outer target to
/// `ParentTarget` (one shared target, not a second slot). For a non-targeted
/// set-scoped subject ("all Revelers ...") it returns the literal subject text
/// so `inject_subject_target` threads the typed filter onto conjunct 2.
///
/// Anchor-start case: when `before_and` itself BEGINS with the gain/get verb at
/// offset 0 (the subject was already lifted by an enclosing compound-subject
/// distribution, e.g. Gogo's "~ and that creature each get +2/+0 and gain haste
/// ... and attack this turn if able"), there is no subject to thread — return
/// `Some("")` so the conjunct splits with no prepend.
fn combat_requirement_conjunct_prepend(
    before_and: &str,
    remainder_trimmed: &str,
) -> Option<String> {
    let remainder_lower = remainder_trimmed.to_ascii_lowercase();
    if !super::imperative::is_standalone_combat_requirement(&remainder_lower)
        && !super::subject::is_can_block_extra_predicate(&remainder_lower)
        && !super::subject::is_can_attack_despite_defender_predicate(&remainder_lower)
    {
        return None;
    }
    let before_lower = before_and.to_ascii_lowercase();
    // CR 508.1d / CR 509.1c: chunk begins with the gain/get verb at offset 0
    // (subject already lifted by the enclosing compound-subject distribution);
    // emit an empty subject so the trailing combat-requirement conjunct splits.
    // This anchor-start check has PRIORITY over the interior `take_until` arms
    // below — those scan for the FIRST " gain"/" get" in the chunk, which would
    // spuriously bind an interior verb (e.g. "get +2/+0 and gain haste ..." has
    // " gain" mid-string) and return a bogus non-empty subject. A verb at
    // offset 0 unambiguously means the subject was already lifted.
    let anchor_start = {
        let r: nom::IResult<&str, &str, OracleError<'_>> =
            alt((tag("gain "), tag("gains "), tag("get "), tag("gets ")))
                .parse(before_lower.as_str());
        r.is_ok()
    };
    if anchor_start {
        return Some(String::new());
    }
    // Conjunct 1 must be a continuous predicate: locate the gain/get verb.
    let subject_text = take_until::<_, _, OracleError<'_>>(" gain")
        .parse(before_lower.as_str())
        .ok()
        .and_then(|(after, before_verb)| {
            // Confirm a real " gain " / " gains " verb boundary.
            alt((tag::<_, _, OracleError<'_>>(" gains "), tag(" gain ")))
                .parse(after)
                .ok()?;
            // Map the verb position back onto the original-case slice.
            let subject = before_and[..before_verb.len()].trim();
            (!subject.is_empty()).then_some(subject)
        })
        .or_else(|| {
            take_until::<_, _, OracleError<'_>>(" get")
                .parse(before_lower.as_str())
                .ok()
                .and_then(|(after, before_verb)| {
                    // Confirm a real " get " / " gets " verb boundary.
                    alt((tag::<_, _, OracleError<'_>>(" gets "), tag(" get ")))
                        .parse(after)
                        .ok()?;
                    // Map the verb position back onto the original-case slice.
                    let subject = before_and[..before_verb.len()].trim();
                    (!subject.is_empty()).then_some(subject)
                })
        })?;
    // Targeted subject → anaphor; non-targeted set subject → literal subject.
    let subject_lower = subject_text.to_ascii_lowercase();
    let targeted = alt((
        value((), tag::<_, _, OracleError<'_>>("another target ")),
        value((), tag("target ")),
    ))
    .parse(subject_lower.as_str())
    .is_ok();
    if targeted {
        Some("it ".to_string())
    } else {
        Some(format!("{subject_text} "))
    }
}

/// CR 121.1 / CR 119.1: Returns true when the token immediately following a
/// `gain `/`lose ` prefix is a count expression — i.e. digits, or `X`/`x`
/// terminated by a non-alphanumeric boundary so we don't false-match "x" inside
/// "x-cost" (only `X ` / `X,` / `X.` / end-of-string). Distinguishes imperative
/// "gain 3 life" / "lose X life" from continuous-modification "gain lifelink".
fn next_token_is_count(s: &str) -> bool {
    let trimmed = s.trim_start();
    let first_char = match trimmed.chars().next() {
        Some(c) => c,
        None => return false,
    };
    if first_char.is_ascii_digit() {
        return true;
    }
    if first_char == 'x' || first_char == 'X' {
        let after = &trimmed[first_char.len_utf8()..];
        let next = after.chars().next();
        return next.map(|c| !c.is_alphanumeric()).unwrap_or(true);
    }
    false
}

/// CR 121.1 / CR 119.3: Returns true when a conjugated player-action verb is
/// followed by a count plus the matching player-action noun (`card(s)` or
/// `life`). Unlike the imperative `gain`/`lose` heuristic above, this accepts
/// article counts ("draws a card") without false-splitting continuous keyword
/// grants such as "gains flying" or "loses all abilities".
fn next_token_is_player_action_count(s: &str) -> bool {
    let count = alt((
        value((), nom_primitives::parse_number),
        value((), tag::<_, _, OracleError<'_>>("x")),
    ));
    let noun = alt((tag("cards"), tag("card"), tag("life")));
    (count, multispace1, noun).parse(s.trim_start()).is_ok()
}

/// Checks if text starts with a subject-prefixed damage verb.
/// Matches: "it deals N damage", "~ deals N damage", "this creature deals N damage",
/// "that creature deals N damage", bare "deals N damage", etc.
/// Used by `starts_bare_and_clause` to split patterns like
/// "sacrifice ~ and it deals 3 damage to target player".
fn starts_with_damage_clause(lower: &str) -> bool {
    if let Ok((_, before)) = take_until::<_, _, OracleError<'_>>("deals ")
        .parse(lower)
        .or_else(|_| take_until::<_, _, OracleError<'_>>("deal ").parse(lower))
    {
        let subject = before.trim();
        subject.is_empty() // bare "deals N damage"
            || subject == "it" // "it deals N damage"
            || subject == "~" // "~ deals N damage"
            || alt((
                tag::<_, _, OracleError<'_>>("this "),
                tag("that "),
            ))
            .parse(subject)
            .is_ok()
    } else {
        false
    }
}

pub(super) fn is_possessive_apostrophe(current: &str, next: Option<char>) -> bool {
    let prev = current.chars().last();
    matches!(
        (prev, next),
        (Some(prev), Some(next)) if prev.is_alphanumeric() && next.is_alphanumeric()
            || prev == 's' && next.is_whitespace()
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

/// CR 707.10c: A `CopySpell` may be the chain's effect directly (activated /
/// spell / triggered contexts) or nested inside a `CreateDelayedTrigger`
/// wrapper ("When you next cast ..., copy that spell"). Mirrors
/// `def_tree_has_optional`'s descent through `CreateDelayedTrigger`.
fn effect_wraps_copy_spell(effect: &Effect) -> bool {
    match effect {
        Effect::CopySpell { .. } => true,
        Effect::CreateDelayedTrigger { effect: inner, .. } => {
            effect_wraps_copy_spell(&inner.effect)
        }
        _ => false,
    }
}

/// CR 701.8 + CR 608.2c: nom recognizer for the "if a permanent's ability is
/// countered this way, destroy that permanent" continuation clause (Teferi's
/// Response, Green Slime). Operates on lowercased text; tolerates a trailing
/// period/whitespace.
///
/// Composed from independent axes rather than enumerated as full strings:
///   - condition subject ("a permanent's ability" / "an ability") — the gate
///     that scopes the destroy to *abilities* whose source is a permanent.
///   - destroy object ("that permanent" / "that source") — the determiner that
///     refers back to the countered ability's source permanent.
fn recognize_counter_destroy_rider(lower: &str) -> bool {
    let clause = lower.trim().trim_end_matches('.').trim_end();
    value(
        (),
        (
            tag::<_, _, OracleError<'_>>("if "),
            alt((tag("a permanent's ability"), tag("an ability"))),
            tag(" is countered this way, destroy "),
            alt((tag("that permanent"), tag("that source"))),
            eof,
        ),
    )
    .parse(clause)
    .is_ok()
}

/// CR 707.10c: nom recognizer for the "[you] may choose [a] new target[s] for
/// {the,that} copy/copies" continuation clause that grants copy retargeting.
/// Operates on lowercased text; tolerates a trailing period/whitespace.
///
/// The clause is composed from independent axes rather than enumerated as full
/// strings:
///   - optional `"you "` subject ("You may choose ..." vs the bare "may choose
///     ..." form produced by clause-splitting Chain of Smog's "... and may
///     choose a new target for that copy").
///   - singular/plural target ("a new target" / "new targets").
///   - determiner ("the copy/copies" — Fork/Twincast; "that copy" — the Chain
///     cycle's "a new target for that copy").
fn recognize_copy_retarget_clause(lower: &str) -> bool {
    let clause = lower.trim().trim_end_matches('.').trim_end();
    value(
        (),
        (
            opt(tag::<_, _, OracleError<'_>>("you ")),
            tag("may choose "),
            alt((tag("a new target "), tag("new targets "))),
            tag("for "),
            alt((tag("the copies"), tag("the copy"), tag("that copy"))),
            eof,
        ),
    )
    .parse(clause)
    .is_ok()
}

/// CR 707.10c: Set `retarget` on the (possibly delayed-trigger-wrapped)
/// `CopySpell`. Returns true if a `CopySpell` was found and patched.
fn set_copy_retarget(effect: &mut Effect, perm: CopyRetargetPermission) -> bool {
    match effect {
        Effect::CopySpell { retarget, .. } => {
            *retarget = perm;
            true
        }
        Effect::CreateDelayedTrigger { effect: inner, .. } => {
            set_copy_retarget(&mut inner.effect, perm)
        }
        _ => false,
    }
}

/// CR 707.10c: Patch the `retarget` permission on the `CopySpell` reachable
/// from this ability definition — its own effect, or a `CopySpell` carried as
/// a (transitive) `sub_ability`. The Chain cycle (Chain of Acid / Plasma /
/// Smog / Vapor) nests the optional copy under the parent effect ("Target
/// player discards two cards. That player may copy this spell ..."), so the
/// "and may choose a new target for that copy" continuation must descend the
/// sub-ability chain rather than only inspecting the top-level effect.
fn set_copy_retarget_in_ability(
    def: &mut AbilityDefinition,
    perm: &CopyRetargetPermission,
) -> bool {
    if set_copy_retarget(&mut def.effect, perm.clone()) {
        return true;
    }
    def.sub_ability
        .as_deref_mut()
        .is_some_and(|sub| set_copy_retarget_in_ability(sub, perm))
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
            reveal,
            attach_to_source,
        } => {
            // CR 701.23a: A multi-zone tutor ("graveyard, hand, and/or library")
            // finds the card in any searched zone, so the put-step must move it
            // from wherever it actually is (`origin: None`). A library-only
            // search keeps `origin: Some(Library)` — that origin doubles as the
            // CR 701.23b fail-to-find signal for the change-zone resolver.
            let mut multi_zone_search = false;
            if let Some(previous) = defs.last_mut() {
                if let Effect::SearchLibrary {
                    reveal: existing_reveal,
                    source_zones,
                    ..
                } = &mut *previous.effect
                {
                    *existing_reveal |= reveal;
                    multi_zone_search = source_zones.iter().any(|zone| *zone != Zone::Library);
                }
                apply_search_destination_to_ability_chain(previous, destination, enter_tapped);
            }
            let put_origin = if multi_zone_search {
                None
            } else {
                Some(Zone::Library)
            };
            let mut change_zone = AbilityDefinition::new(
                kind,
                Effect::ChangeZone {
                    origin: put_origin,
                    destination,
                    target: TargetFilter::Any,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
            );
            // CR 303.4f: "attached to [source]" — forward the moved card to an Attach sub_ability
            if attach_to_source {
                change_zone.forward_result = true;
                change_zone.sub_ability = Some(Box::new(AbilityDefinition::new(
                    kind,
                    Effect::Attach {
                        attachment: TargetFilter::SelfRef,
                        target: TargetFilter::Any,
                    },
                )));
            }
            defs.push(change_zone);
        }
        ContinuationAst::RevealHandFilter {
            card_filter,
            choice_optional,
        } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::RevealHand {
                card_filter: existing,
                choice_optional: existing_choice_optional,
                ..
            } = &mut *previous.effect
            {
                match card_filter {
                    Some(filter) => *existing = filter,
                    None if matches!(existing, TargetFilter::None) => {
                        *existing = TargetFilter::Any;
                    }
                    None => {}
                }
                *existing_choice_optional = choice_optional;
            }
        }
        ContinuationAst::ManaRestriction {
            restriction,
            grants: new_grants,
        } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Mana {
                restrictions,
                grants,
                ..
            } = &mut *previous.effect
            {
                restrictions.push(restriction);
                grants.extend(new_grants);
            }
        }
        ContinuationAst::ManaGrant { grants: new_grants } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Mana { grants, .. } = &mut *previous.effect {
                grants.extend(new_grants);
            }
        }
        ContinuationAst::CounterSourceStatic { source_static } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Counter {
                source_rider: existing,
                ..
            } = &mut *previous.effect
            {
                // CR 611.2: "that permanent loses all abilities ..." rider.
                *existing = Some(CounterSourceRider::LosesAbilities {
                    static_def: source_static,
                });
            }
        }
        ContinuationAst::CounterSourceRiderDestroy => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Counter {
                source_rider: existing,
                ..
            } = &mut *previous.effect
            {
                // CR 701.8: "If a permanent's ability is countered this way,
                // destroy that permanent." rider (Teferi's Response, Green Slime).
                *existing = Some(CounterSourceRider::Destroy);
            }
        }
        ContinuationAst::CopyMayRetarget => {
            // CR 707.10c: patch the preceding CopySpell — descending through a
            // CreateDelayedTrigger wrapper ("When you next cast ..., copy that
            // spell" — Galvanic Iteration) and through the sub-ability chain
            // ("That player may copy this spell ..." — the Chain cycle, where
            // the optional CopySpell is nested under the parent discard).
            if let Some(previous) = defs.last_mut() {
                set_copy_retarget_in_ability(
                    previous,
                    &CopyRetargetPermission::MayChooseNewTargets,
                );
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
        ContinuationAst::FlashbackCostEqualsManaCost => {}
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
        ContinuationAst::PutRest {
            destination,
            reorder_all,
        } => {
            // Absorbed into preceding Dig or RevealUntil — sets rest_destination
            // for unchosen/non-matching cards. CR 608.2c: When the preceding def is
            // a conditional "instead" alternative (new def with `else_ability =
            // base_def`), patch BOTH branches so the rest-placement applies whether
            // the condition was true or false.
            //
            // CR 608.2c: the "put the rest" clause patches the earlier "look at
            // the top N" instruction. When a transparent clause (e.g.
            // `Sacrifice` — Birthing Ritual) sits between the `Dig` and this
            // clause, `defs.last()` is the intervening clause. Search back for
            // the nearest rest-patchable def (`Dig`/`RevealUntil` — what
            // `patch_rest_destination_recursively` handles).
            let Some(previous) = defs
                .iter_mut()
                .rev()
                .find(|d| matches!(&*d.effect, Effect::Dig { .. } | Effect::RevealUntil { .. }))
            else {
                return;
            };
            patch_rest_destination_recursively(previous, destination, reorder_all);
        }
        ContinuationAst::DigFromAmong {
            count,
            up_to: is_up_to,
            filter: card_filter,
            destination: kept_dest,
            rest_destination: rest_dest,
        } => {
            // CR 608.2c: the "from among those cards" continuation patches the
            // earlier "look at the top N" instruction. When a transparent
            // clause (e.g. `Sacrifice` — Birthing Ritual) sits between the
            // `Dig` and this continuation, `defs.last()` is that intervening
            // clause, not the `Dig`. Search back for the nearest `Dig`/`Mill`.
            let Some(previous) = defs
                .iter_mut()
                .rev()
                .find(|d| matches!(&*d.effect, Effect::Dig { .. } | Effect::Mill { .. }))
            else {
                return;
            };
            if let Effect::Dig {
                keep_count,
                up_to,
                filter,
                destination,
                rest_destination,
                reveal,
                ..
            } = &mut *previous.effect
            {
                *keep_count = Some(count);
                *up_to = is_up_to;
                *filter = card_filter;
                // CR 701.33: When `destination` is None the kept cards are NOT
                // auto-routed by the Dig resolver; downstream sub_abilities
                // read the tracked set and route by type. Also promote the
                // Dig to reveal:true — "from among them" is a reveal-form.
                *destination = kept_dest;
                if kept_dest.is_none() {
                    *reveal = true;
                }
                if let Some(rd) = rest_dest {
                    *rest_destination = Some(rd);
                }
            } else if matches!(&*previous.effect, Effect::Mill { .. }) {
                // CR 701.17c + CR 608.2c: "...from among the milled cards" after
                // a `Mill`. The `Mill` already mills its `count` cards to its
                // `destination` (CR 701.17a); the continuation reads that milled
                // set. Unlike the `Dig` form (which patches the source effect's
                // keep/filter), `Mill` keeps its own `count`/`destination`
                // intact and we PUSH a fresh `ChangeZone` sub-ability that
                // selects from the milled cards. `TrackedSetFiltered` with the
                // sentinel `TrackedSetId(0)` resolves to the most recent tracked
                // set at resolution — the engine auto-publishes the `Mill`'s
                // affected objects (the milled cards) into that set. Phase-2
                // sub-chain assembly folds this pushed def into `Mill.sub_ability`.
                defs.push(AbilityDefinition::new(
                    kind,
                    Effect::ChangeZone {
                        origin: None,
                        destination: kept_dest.unwrap_or(Zone::Hand),
                        target: TargetFilter::TrackedSetFiltered {
                            id: crate::types::identifiers::TrackedSetId(0),
                            filter: Box::new(card_filter),
                        },
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: false,
                        enters_attacking: false,
                        up_to: is_up_to,
                        enter_with_counters: vec![],
                    },
                ));
            }
        }
        ContinuationAst::ChooseFromExile { count, chooser } => {
            defs.push(AbilityDefinition::new(
                kind,
                Effect::ChooseFromZone {
                    count,
                    zone: Zone::Exile,
                    additional_zones: Vec::new(),
                    zone_owner: crate::types::ability::ZoneOwner::Controller,
                    filter: None,
                    chooser,
                    up_to: false,
                    constraint: None,
                },
            ));
        }
        ContinuationAst::SearchRevealResult => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::SearchLibrary { reveal, .. } = &mut *previous.effect {
                *reveal = true;
            }
        }
        ContinuationAst::SearchResultClauseHandled => {}
        ContinuationAst::PutChoiceRemainderOnBottom => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            let bottom_def = AbilityDefinition::new(
                kind,
                Effect::PutAtLibraryPosition {
                    target: TargetFilter::Any,
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    position: crate::types::ability::LibraryPosition::Bottom,
                },
            );
            // Walk into the sub_ability chain to find the right attachment point.
            // For ChooseFromZone, the sub_ability is ChangeZone(Library→Hand) and we
            // attach the bottom-placement as *its* sub_ability (unchosen targets flow there).
            // For a bare ChangeZone(Library→Hand), attach directly.
            let target_def = if matches!(&*previous.effect, Effect::ChooseFromZone { .. }) {
                previous.sub_ability.as_deref_mut()
            } else {
                Some(previous)
            };
            if let Some(def) = target_def {
                if matches!(
                    &*def.effect,
                    Effect::ChangeZone {
                        origin: Some(Zone::Library),
                        destination: Zone::Hand,
                        ..
                    }
                ) && def.sub_ability.is_none()
                {
                    def.sub_ability = Some(Box::new(bottom_def));
                }
            }
        }
        ContinuationAst::ChoicePartitionDestinations {
            chosen_destination,
            rest_destination,
        } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if matches!(&*previous.effect, Effect::ChooseFromZone { .. }) {
                let existing_tail = previous.sub_ability.take();
                let mut chosen_def = AbilityDefinition::new(
                    kind,
                    Effect::ChangeZone {
                        origin: None,
                        destination: chosen_destination,
                        target: TargetFilter::Any,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: false,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                    },
                );
                let mut rest_def = AbilityDefinition::new(
                    kind,
                    Effect::ChangeZone {
                        origin: None,
                        destination: rest_destination,
                        target: TargetFilter::Any,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: false,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                    },
                );
                if (chosen_destination == Zone::Library || rest_destination == Zone::Library)
                    && existing_tail.is_none()
                {
                    rest_def.sub_ability = Some(Box::new(AbilityDefinition::new(
                        kind,
                        Effect::Shuffle {
                            target: TargetFilter::Controller,
                        },
                    )));
                }
                if let Some(tail) = existing_tail {
                    append_definition_to_sub_chain(&mut rest_def, *tail);
                }
                chosen_def.sub_ability = Some(Box::new(rest_def));
                previous.sub_ability = Some(Box::new(chosen_def));
            }
        }
        ContinuationAst::PutChosenCardsAtLibraryPosition { position } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Dig {
                destination,
                rest_destination,
                ..
            } = &mut *previous.effect
            {
                *destination = Some(Zone::Library);
                *rest_destination = Some(Zone::Library);
            }
            let put_def = AbilityDefinition::new(
                kind,
                Effect::PutAtLibraryPosition {
                    target: TargetFilter::Any,
                    count: QuantityExpr::Fixed { value: 0 },
                    position,
                },
            );
            append_definition_to_sub_chain(previous, put_def);
        }
        ContinuationAst::BecomesPlotted => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            let grant_def = AbilityDefinition::new(
                kind,
                Effect::GrantCastingPermission {
                    permission: CastingPermission::Plotted { turn_plotted: 0 },
                    target: plotted_grant_target(previous),
                    grantee: PermissionGrantee::ObjectOwner,
                },
            );
            append_definition_to_sub_chain(previous, grant_def);
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
        ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            // CR 122.6a: Patch the preceding Token effect to enter with counters.
            if let Effect::Token {
                enter_with_counters,
                ..
            } = &mut *previous.effect
            {
                enter_with_counters.push((counter_type, count));
            }
        }
        ContinuationAst::RevealUntilKept {
            destination,
            enter_tapped: tapped,
            enters_attacking: attacking,
            rest_destination: rest_dest,
            optional_decline,
        } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::RevealUntil {
                kept_destination,
                enter_tapped,
                enters_attacking,
                rest_destination,
                kept_optional_to,
                ..
            } = &mut *previous.effect
            {
                match optional_decline {
                    // CR 701.20a + CR 608.2c: optional kept clause ("you may put
                    // that card onto the battlefield"). `destination` is the
                    // accept zone, `decline` the decline zone. `enter_tapped`
                    // applies to the accept zone (always Battlefield).
                    Some(decline) => {
                        *kept_optional_to = Some(destination);
                        *kept_destination = decline;
                        *enter_tapped = tapped;
                        // CR 508.4: accept zone is the battlefield here.
                        *enters_attacking = attacking;
                    }
                    // Mandatory kept clause. Refine `kept_destination` without
                    // clobbering a `kept_optional_to` set by a prior chunk (the
                    // GAP-1 fix: Songbirds' Blessing's "If you don't, put it into
                    // your hand" sentence refines the decline zone to Hand).
                    None => {
                        *kept_destination = destination;
                        if destination == Zone::Battlefield {
                            *enter_tapped = tapped;
                            // CR 508.4: "put that card onto the battlefield
                            // tapped and attacking" (Raph & Mikey, Fireflux Squad).
                            *enters_attacking = attacking;
                        }
                    }
                }
                if let Some(rest) = rest_dest {
                    *rest_destination = rest;
                }
            }
        }
        ContinuationAst::GrantExtraTurnAfterControlledTurn => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::ControlNextTurn {
                grant_extra_turn_after,
                ..
            } = &mut *previous.effect
            {
                *grant_extra_turn_after = true;
            }
        }
        // CR 701.20a: "puts those cards into [zone]" — both the matching card and
        // the non-matching cards go to the same zone.
        ContinuationAst::RevealUntilAllToZone { destination } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::RevealUntil {
                kept_destination,
                rest_destination,
                ..
            } = &mut *previous.effect
            {
                *kept_destination = destination;
                *rest_destination = destination;
            }
        }
        // CR 406.3 + CR 701.16a: Rewrite the preceding private `Dig` (the
        // "look at the top N cards of <player>'s library" look step) into an
        // `Effect::ExileTop` so the looked-at card(s) actually leave the
        // library — the Gonti, Canny Acquisitor impulse idiom. `player`/`count`
        // were lifted from the `Dig` (with `ParentTarget` re-bound to the
        // triggering player) during recognition; `face_down` carries the
        // hidden-information suffix. ExileTop publishes a tracked set the
        // following `GrantCastingPermission(PlayFromExile)` binds to, so the
        // exiled card becomes playable.
        ContinuationAst::ExileLookedAtCard {
            player,
            count,
            face_down,
        } => {
            let Some(previous) = defs
                .iter_mut()
                .rev()
                .find(|d| matches!(&*d.effect, Effect::Dig { .. }))
            else {
                return;
            };
            *previous.effect = Effect::ExileTop {
                player,
                count,
                face_down,
            };
        }
        ContinuationAst::ChooseAndSacrificeRestFilter { sacrifice_filter } => {
            let Some(filter) = sacrifice_filter else {
                return;
            };
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::ChooseAndSacrificeRest {
                sacrifice_filter: existing,
                ..
            } = &mut *previous.effect
            {
                *existing = filter;
            }
        }
    }
}

fn apply_search_destination_to_ability_chain(
    ability: &mut AbilityDefinition,
    destination: Zone,
    enter_tapped: bool,
) {
    let mut cursor = Some(ability);
    while let Some(sub_ability) = cursor {
        if let Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: existing_destination,
            enter_tapped: existing_enter_tapped,
            ..
        } = &mut *sub_ability.effect
        {
            *existing_destination = destination;
            *existing_enter_tapped = enter_tapped;
        }
        cursor = sub_ability.sub_ability.as_deref_mut();
    }
}

/// Recursively patch `rest_destination` on Dig/RevealUntil effects reachable from
/// `def` via `else_ability`. CR 608.2c: When a preceding def is a conditional
/// "instead" wrapper (new_def with `else_ability = base_def`), a trailing
/// "Put the rest on the bottom..." clause applies to both the alternative and
/// base branches — neither branch is selected until resolution.
fn patch_rest_destination_recursively(
    def: &mut AbilityDefinition,
    destination: Zone,
    reorder_all: bool,
) {
    match &mut *def.effect {
        Effect::Dig {
            destination: kept_destination,
            rest_destination,
            ..
        } => {
            if reorder_all {
                *kept_destination = Some(Zone::Library);
                *rest_destination = Some(Zone::Library);
            } else if rest_destination.is_none() {
                *rest_destination = Some(destination);
            }
        }
        Effect::RevealUntil {
            rest_destination, ..
        } => {
            *rest_destination = destination;
        }
        _ => {}
    }
    if let Some(else_def) = def.else_ability.as_deref_mut() {
        patch_rest_destination_recursively(else_def, destination, reorder_all);
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
        ContinuationAst::ManaRestriction { .. }
        | ContinuationAst::ManaGrant { .. }
        | ContinuationAst::CounterSourceStatic { .. }
        | ContinuationAst::CounterSourceRiderDestroy => true,
        // CR 707.10c: recognition was already gated on a preceding CopySpell in
        // parse_followup_continuation_ast, so absorption is unconditional —
        // identical to the CounterSourceStatic precedent.
        ContinuationAst::CopyMayRetarget => true,
        ContinuationAst::FlashbackCostEqualsManaCost => true,
        ContinuationAst::SearchDestination { .. } => false,
        ContinuationAst::SuspectLastCreated => matches!(current_effect, Effect::Suspect { .. }),
        ContinuationAst::CantRegenerate => true,
        ContinuationAst::PutRest { .. } => true,
        ContinuationAst::ChooseFromExile { .. } => true,
        ContinuationAst::SearchRevealResult => true,
        ContinuationAst::SearchResultClauseHandled => true,
        ContinuationAst::PutChoiceRemainderOnBottom => true,
        ContinuationAst::ChoicePartitionDestinations { .. } => true,
        ContinuationAst::PutChosenCardsAtLibraryPosition { .. } => true,
        ContinuationAst::BecomesPlotted => true,
        ContinuationAst::EntersTappedAttacking => true,
        ContinuationAst::TokenEntersWithCounters { .. } => true,
        ContinuationAst::DigFromAmong { .. } => true,
        ContinuationAst::GrantExtraTurnAfterControlledTurn => true,
        ContinuationAst::RevealUntilKept { .. } => true,
        ContinuationAst::RevealUntilAllToZone { .. } => true,
        // Recognition was already gated on a preceding `Dig` in
        // parse_followup_continuation_ast; the "exile it [face down]" clause is
        // folded into that Dig (rewritten to ExileTop) and emits no sibling def.
        ContinuationAst::ExileLookedAtCard { .. } => true,
        ContinuationAst::ChooseAndSacrificeRestFilter { .. } => true,
    }
}

pub(super) fn parse_intrinsic_continuation_ast(
    text: &str,
    effect: &Effect,
    full_text: &str,
) -> Option<ContinuationAst> {
    match effect {
        Effect::SearchLibrary { split, .. } => {
            // CR 701.23a + CR 608.2c: When the search carries a split destination
            // (cultivate-class "put one onto the battlefield tapped and the other
            // into your hand"), the partition is handled at resolution by the
            // `SearchPartitionChoice` flow. Suppress the flat default ChangeZone
            // here so the found set is not collapsed to a single battlefield move
            // (mirrors the `has_positional_put` suppression below).
            if split.is_some() {
                return None;
            }
            let full_lower = full_text.to_ascii_lowercase();
            // CR 608.2c: Conditional result destinations ("put it onto the
            // battlefield tapped if it's a land card. Otherwise, put it into
            // your hand" — Archdruid's Charm) are represented by the parsed
            // conditional ChangeZone/else branch. Do not synthesize the
            // unconditional SearchDestination continuation ahead of that branch.
            if has_conditional_search_result_destination(&full_lower) {
                return None;
            }
            // CR 701.24b: If later clauses contain "put on top", suppress the default
            // ChangeZone(→Hand) — the card stays in the library and a separate
            // PutAtLibraryPosition effect in the chain handles placement.
            // Also suppress for "Nth from the top" (Long-Term Plans, etc.)
            let has_positional_put =
                nom_primitives::scan_contains(&full_lower, "put that card on top")
                    || nom_primitives::scan_contains(&full_lower, "put it on top")
                    || nom_primitives::scan_contains(&full_lower, "put the card on top")
                    || nom_primitives::scan_contains(&full_lower, "put them on top")
                    || nom_primitives::scan_contains(&full_lower, "put those cards on top")
                    || (nom_primitives::scan_contains(&full_lower, "put that card")
                        && nom_primitives::scan_contains(&full_lower, "from the top"));
            if has_positional_put {
                return None;
            }
            let lower = text.to_lowercase();
            let attach_to_source = nom_primitives::scan_contains(&full_lower, "attached to")
                || nom_primitives::scan_contains(&lower, "attached to");
            // CR 701.23a + CR 701.18a: Scan "onto the battlefield tapped" across
            // the whole sentence (full_lower) so the destination compound's
            // "enters tapped" modifier is detected even when the put-step is
            // in a sibling clause (Assassin's Trophy-style split).
            let enter_tapped = nom_primitives::scan_contains(&full_lower, "battlefield tapped");
            let reveal = nom_primitives::scan_contains(&lower, "reveal")
                || nom_primitives::scan_contains(&full_lower, "reveal that card")
                || nom_primitives::scan_contains(&full_lower, "reveal it");
            // Safety net: verify the clause splitter correctly separated all boundaries.
            // If this fires, a verb is missing from starts_clause_text() or the splitter's
            // search_start guard is incorrectly suppressing a split.
            // CR 701.18a: Shuffle clauses are part of the search compound action —
            // both "shuffle" and "that player shuffles" are valid terminators.
            #[cfg(debug_assertions)]
            if let Some(then_pos) = lower.rfind(", then ") {
                let after_then = lower[then_pos + ", then ".len()..].trim_end_matches('.');
                let is_shuffle_clause = alt((
                    value((), tag::<_, _, OracleError<'_>>("shuffle")),
                    value((), tag("that player shuffles")),
                ))
                .parse(after_then)
                .is_ok();
                if !is_shuffle_clause {
                    debug_assert!(
                        !starts_clause_text(after_then),
                        "Unsplit clause boundary in SearchLibrary continuation: \
                         ', then {}' — check starts_clause_text() for missing verb",
                        after_then,
                    );
                }
            }
            // CR 701.23a + CR 701.18a: The "put [it] onto the battlefield" /
            // "put [it] into your hand" destination clause for a library search
            // compound lives in the same sentence as the search, but may have
            // been split into a subsequent chunk by the comma-splitter (e.g.,
            // "search their library for a basic land card, put it onto the
            // battlefield, then shuffle"). Use full_lower so we scan across the
            // whole sentence rather than only the chunk containing "search".
            Some(ContinuationAst::SearchDestination {
                destination: super::parse_search_destination(&full_lower),
                enter_tapped,
                reveal,
                attach_to_source,
            })
        }
        _ => None,
    }
}

/// CR 701.20e + CR 608.2c: Parse "put/return up to N [filter] from among
/// them/those cards onto the battlefield / into your hand / to your hand" into
/// a DigFromAmong continuation that patches the preceding Dig effect. The
/// player follows the Oracle text instructions in written order (CR 608.2c).
///
/// Also handles "put N of them into your hand [and the rest on the bottom]" — the simpler
/// form used by Impulse, Stock Up, Dig Through Time, etc. where no filter is specified.
///
/// CR 202.3 + CR 107.3i: A trailing "where X is <expression>" defining clause
/// (Birthing Ritual: "put a creature card with mana value X or less from among
/// those cards onto the battlefield, where X is 1 plus the sacrificed
/// creature's mana value") binds the literal `X` in the filter's `Cmc` bound.
/// The where-clause is stripped up front and applied to the parsed filter via
/// the shared `apply_where_x_to_filter` building block, so the `Cmc` bound
/// resolves against the sacrificed creature's mana value rather than staying an
/// unbounded `QuantityRef::Variable("X")`.
///
/// Examples:
/// - "put up to two creature cards with mana value 3 or less from among them onto the battlefield"
/// - "put a creature card from among them into your hand"
/// - "return a permanent card from among them to your hand"
/// - "you may reveal a creature card from among them and put it into your hand"
/// - "put two of them into your hand and the rest on the bottom of your library in any order"
/// - "put two of those cards into your hand"
pub(super) fn parse_dig_from_among(lower: &str, original: &str) -> Option<ContinuationAst> {
    // CR 202.3 + CR 107.3i: Strip a trailing "where X is <expression>" defining
    // clause before destination/count/filter parsing. `where_x_expression`
    // (when present) is applied to the parsed filter at the end.
    let (lower, where_x_expression) = if original.len() == lower.len() {
        let (stripped, where_x) = strip_trailing_where_x(TextPair::new(original, lower));
        (stripped.lower, where_x)
    } else {
        (lower, None)
    };
    // CR 608.2c: A reflexive "if you do, " conditional prefix (Birthing Ritual:
    // "...sacrifice a creature. If you do, you may put a creature card ...")
    // rides on the followup clause text — the `IfYouDo` condition is lifted
    // onto the lowered def elsewhere. Strip the leading `if <cond>, ` so the
    // count/filter combinators see the bare imperative they expect.
    let lower = match (
        tag::<_, _, OracleError<'_>>("if "),
        take_until::<_, _, OracleError<'_>>(", "),
        tag::<_, _, OracleError<'_>>(", "),
    )
        .parse(lower)
    {
        Ok((rest, _)) => rest,
        Err(_) => lower,
    };
    // Determine kept-cards destination. `None` is the reveal-only form (Zimone's
    // Experiment): "reveal up to N <filter> cards from among them, then put the
    // rest on the bottom" — the kept cards are NOT auto-routed; subsequent
    // sub_abilities route them by type via `TargetFilter::TrackedSetFiltered`.
    let destination = if nom_primitives::scan_contains(lower, "onto the battlefield") {
        Some(Zone::Battlefield)
    } else if nom_primitives::scan_contains(lower, "into your hand")
        || nom_primitives::scan_contains(lower, "into their hand")
        || nom_primitives::scan_contains(lower, "to your hand")
        || nom_primitives::scan_contains(lower, "to their hand")
    {
        Some(Zone::Hand)
    } else {
        None
    };

    // "put N of them into your hand [and the rest on the bottom]" — no filter, count explicit.
    // Must be checked BEFORE the "from among" path since "of them" appears in both forms.
    if let Ok((_, before_of)) = alt((
        take_until::<_, _, OracleError<'_>>(" of those cards"),
        take_until(" of those"),
        take_until(" of them"),
    ))
    .parse(lower)
    {
        let before_of = before_of.trim();
        let after_put = alt((tag::<_, _, OracleError<'_>>("you may put "), tag("put ")))
            .parse(before_of)
            .map(|(rest, _)| rest)
            .unwrap_or(before_of);

        // Delegate to nom combinator (input already lowercase from lower).
        let (count, up_to) =
            if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("up to ").parse(after_put) {
                nom_primitives::parse_number
                    .parse(rest)
                    .map_or((1, true), |(_, n)| (n, true))
            } else if let Ok((_, n)) = nom_primitives::parse_number.parse(after_put) {
                (n, false)
            } else {
                // "a/an" or unrecognized → treat as up_to 1
                (1, true)
            };

        // Detect rest destination from "and the rest on the bottom/into graveyard" suffix.
        let rest_destination = parse_of_them_rest_destination(lower);

        return Some(ContinuationAst::DigFromAmong {
            count,
            up_to,
            filter: TargetFilter::Any,
            destination,
            rest_destination,
        });
    }

    // CR 701.17c + CR 608.2c: "return a card milled this way to your hand"
    // is the same tracked-set continuation as "from among the milled cards",
    // but its filter appears before the "milled this way" marker rather than
    // before "from among".
    if let Ok((_, before_milled)) = alt((
        take_until::<_, _, OracleError<'_>>("that was milled this way"),
        take_until("milled this way"),
    ))
    .parse(lower)
    {
        let before_milled = before_milled.trim();
        let (after_put, prefix_optional) = if let Ok((rest, _)) = alt((
            tag::<_, _, OracleError<'_>>("you may put "),
            tag("you may reveal "),
            tag("you may return "),
        ))
        .parse(before_milled)
        {
            (rest, true)
        } else if let Ok((rest, _)) = alt((
            tag::<_, _, OracleError<'_>>("put "),
            tag("reveal "),
            tag("return "),
        ))
        .parse(before_milled)
        {
            (rest, false)
        } else {
            (before_milled, false)
        };

        let (count, up_to, filter_text) =
            if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("up to ").parse(after_put) {
                if let Ok((remainder, n)) = nom_primitives::parse_number.parse(rest) {
                    (n, true, remainder.trim())
                } else {
                    (1, true, rest)
                }
            } else if let Ok((rest, _)) =
                tag::<_, _, OracleError<'_>>("any number of ").parse(after_put)
            {
                (255, true, rest)
            } else if let Ok((rest, _)) = nom_primitives::parse_article.parse(after_put) {
                (1, prefix_optional, rest)
            } else if let Ok((remainder, n)) = nom_primitives::parse_number.parse(after_put) {
                (n, prefix_optional, remainder.trim())
            } else {
                (1, prefix_optional, after_put)
            };

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
        let filter = apply_where_x_to_filter(filter, where_x_expression.as_deref());

        return Some(ContinuationAst::DigFromAmong {
            count,
            up_to,
            filter,
            destination,
            rest_destination: None,
        });
    }

    // Find "from among" to split the text into count+filter vs destination
    let (_, before_from) = take_until::<_, _, OracleError<'_>>("from among")
        .parse(lower)
        .ok()?;
    let before_from = &before_from.trim();

    // Strip leading "put " or "you may reveal " using nom combinators.
    let after_put = alt((
        tag::<_, _, OracleError<'_>>("you may put "),
        tag("you may reveal "),
        tag("you may return "),
        tag("put "),
        tag("reveal "),
        tag("return "),
    ))
    .parse(*before_from)
    .map(|(rest, _)| rest)
    .unwrap_or(before_from);

    // Parse "up to N" or "a/an" or just a number
    // Delegate to nom combinator (input already lowercase from lower).
    let (count, up_to, filter_text) = if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("up to ").parse(after_put)
    {
        if let Ok((remainder, n)) = nom_primitives::parse_number.parse(rest) {
            (n, true, remainder.trim())
        } else {
            (1, true, rest)
        }
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("any number of ").parse(after_put) {
        // "any number of creatures" → up_to with a high cap
        (255, true, rest)
    } else if let Ok((rest, _)) = nom_primitives::parse_article.parse(after_put) {
        // "a creature card" / "an artifact card" — up_to 1 (player may choose none)
        (1, true, rest)
    } else if let Ok((remainder, n)) = nom_primitives::parse_number.parse(after_put) {
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
    // CR 202.3 + CR 107.3i: Bind the literal `X` in the filter's `Cmc` bound
    // with the stripped "where X is <expression>" defining clause.
    let filter = apply_where_x_to_filter(filter, where_x_expression.as_deref());

    Some(ContinuationAst::DigFromAmong {
        count,
        up_to,
        filter,
        destination,
        rest_destination: None, // rest_destination handled by subsequent PutRest continuation
    })
}

/// Extract rest_destination from "put N of them into your hand and the rest/the other on the bottom/graveyard".
/// Returns None if neither "and the rest" nor "and the other" anaphor is present.
///
/// CR 401.1 + CR 401.4: "the rest" / "the other" both refer to the un-chosen
/// remainder of the looked-at pile. The grammatical difference is purely a
/// count distinction — "the other" is used when exactly one card remains
/// (the count=2-keep=1 form, e.g. Sleight of Hand, Sea Gate Oracle); "the
/// rest" generalizes to any remainder count. Both anaphors point to the same
/// rest_destination semantics, so they share the same downstream zone
/// classification.
fn parse_of_them_rest_destination(lower: &str) -> Option<Zone> {
    let (_, (_, after_rest)) = nom_primitives::split_once_on(lower, " and the rest")
        .or_else(|_| nom_primitives::split_once_on(lower, " and the other"))
        .ok()?;
    if contains_possessive(after_rest, "into", "graveyard") {
        Some(Zone::Graveyard)
    } else if contains_possessive(after_rest, "into", "hand") {
        Some(Zone::Hand)
    } else {
        // Default: bottom of library ("on the bottom", "in any order", etc.)
        Some(Zone::Library)
    }
}

/// CR 608.2c: The controller follows a card's instructions in written order;
/// later text may modify or refer to an earlier instruction. Some intervening
/// clauses sit BETWEEN the earlier instruction and the later modifying clause
/// without being the antecedent the later clause patches. Birthing Ritual is
/// the canonical case: "look at the top seven cards ... Then you may sacrifice
/// a creature. If you do, you may put a creature card ... onto the
/// battlefield" — the third clause's `DigFromAmong` continuation patches the
/// first clause's `Dig`, not the intervening `Sacrifice`.
///
/// This predicate marks clause kinds an antecedent search may legitimately
/// skip over when looking back for a `DigFromAmong` target. It is an
/// exhaustive `match` with no wildcard arm: adding a new `Effect` variant
/// forces a deliberate decision about whether it is lookback-transparent.
pub(super) fn clause_is_dig_lookback_transparent(effect: &Effect) -> bool {
    match effect {
        // CR 701.21 + CR 608.2c: A `Sacrifice` clause between a "look at the
        // top N" instruction and a later "if you do, put ... from among those
        // cards" continuation is transparent — the continuation patches the
        // `Dig`, and the sacrificed creature feeds the continuation's filter
        // via `ObjectScope::CostPaidObject`.
        Effect::Sacrifice { .. } | Effect::PayCost { .. } => true,
        Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::DealDamage { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::Counter { .. }
        | Effect::CounterAll { .. }
        | Effect::Token { .. }
        | Effect::GainLife { .. }
        | Effect::LoseLife { .. }
        | Effect::Tap { .. }
        | Effect::Untap { .. }
        | Effect::TapAll { .. }
        | Effect::UntapAll { .. }
        | Effect::AddCounter { .. }
        | Effect::RemoveCounter { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::PumpAll { .. }
        | Effect::DamageAll { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::DestroyAll { .. }
        | Effect::ChangeZone { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::Dig { .. }
        | Effect::GainControl { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::UnattachAll { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::Bounce { .. }
        | Effect::BounceAll { .. }
        | Effect::Explore
        | Effect::ExploreAll { .. }
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch
        | Effect::Proliferate
        | Effect::Populate
        | Effect::Clash
        | Effect::Vote { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::Myriad
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounter { .. }
        | Effect::PutCounterAll { .. }
        | Effect::MultiplyCounter { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::MoveCounters { .. }
        | Effect::Animate { .. }
        | Effect::RegisterBending { .. }
        | Effect::GenericEffect { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::Suspect { .. }
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::SetClassLevel { .. }
        | Effect::CreateDelayedTrigger { .. }
        | Effect::AddTargetReplacement { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::CreateEmblem { .. }
        | Effect::CastFromZone { .. }
        | Effect::PreventDamage { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::LoseTheGame
        | Effect::WinTheGame
        | Effect::RollDie { .. }
        | Effect::FlipCoin { .. }
        | Effect::FlipCoins { .. }
        | Effect::FlipCoinUntilLose { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::Cascade
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::Double { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::Monstrosity { .. }
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::Conjure { .. }
        | Effect::ChooseOneOf { .. }
        // CR 614.12 + CR 303.4: Return-as-Aura is its own emitted sub-effect
        // following a `ChangeZone`; it is not a lookback-transparent clause
        // for the Dig-from-among continuation search.
        | Effect::ReturnAsAura { .. }
        | Effect::Unimplemented { .. } => false,
    }
}

pub(super) fn parse_followup_continuation_ast(
    text: &str,
    previous_effect: &Effect,
    ctx: &mut ParseContext,
) -> Option<ContinuationAst> {
    let lower = text.to_lowercase();

    match previous_effect {
        Effect::ChooseAndSacrificeRest { .. } => parse_choose_and_sacrifice_rest_followup(&lower),
        Effect::SearchLibrary { .. } if is_search_result_reveal_clause(&lower) => {
            Some(ContinuationAst::SearchRevealResult)
        }
        Effect::RevealHand { .. }
            if nom_primitives::scan_contains(&lower, "card from it")
                || nom_primitives::scan_contains(&lower, "card from among")
                || nom_primitives::scan_contains(&lower, "one of them")
                || nom_primitives::scan_contains(&lower, "one of those") =>
        {
            let card_filter = if nom_primitives::scan_at_word_boundaries(&lower, |input| {
                alt((
                    tag::<_, _, OracleError<'_>>("one of them"),
                    tag("one of those"),
                ))
                .parse(input)
            })
            .is_some()
            {
                None
            } else if alt((
                tag::<_, _, OracleError<'_>>("you may choose "),
                tag("you choose "),
                tag("may choose "),
                tag("choose "),
            ))
            .parse(lower.as_str())
            .is_ok()
            {
                Some(super::parse_choose_filter(&lower, ctx))
            } else {
                Some(super::parse_choose_filter_from_sentence(&lower, ctx))
            };
            let choice_optional = alt((
                tag::<_, _, OracleError<'_>>("you may choose "),
                tag("may choose "),
            ))
            .parse(lower.as_str())
            .is_ok();
            Some(ContinuationAst::RevealHandFilter {
                card_filter,
                choice_optional,
            })
        }
        Effect::Mana { .. } => {
            if let Some((restriction, grants)) = super::mana::parse_mana_spend_restriction(&lower) {
                return Some(ContinuationAst::ManaRestriction {
                    restriction,
                    grants,
                });
            }
            // CR 106.6: "that spell can't be countered" as a standalone clause
            // after comma-splitting from the restriction text.
            if let Some(grants) = super::mana::parse_mana_spell_grant(&lower) {
                return Some(ContinuationAst::ManaGrant { grants });
            }
            None
        }
        Effect::GenericEffect {
            static_abilities, ..
        } if lower == "the flashback cost is equal to its mana cost"
            && static_abilities.iter().any(|def| {
                def.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        crate::types::ability::ContinuousModification::AddKeyword {
                            keyword: crate::types::keywords::Keyword::Flashback(_)
                        }
                    )
                })
            }) =>
        {
            Some(ContinuationAst::FlashbackCostEqualsManaCost)
        }
        Effect::Counter { .. }
            if nom_primitives::scan_contains(&lower, "countered this way")
                && nom_primitives::scan_contains(&lower, "loses all abilities") =>
        {
            Some(ContinuationAst::CounterSourceStatic {
                source_static: Box::new(StaticDefinition::continuous().modifications(vec![
                    crate::types::ability::ContinuousModification::RemoveAllAbilities,
                ])),
            })
        }
        // CR 701.8 + CR 608.2c: "If a permanent's ability is countered this way,
        // destroy that permanent." (Teferi's Response, Green Slime). Claiming
        // this clause as a continuation prevents the generic sequence parser
        // from emitting a stray chained `Destroy { ParentTarget }`.
        Effect::Counter { .. } if recognize_counter_destroy_rider(&lower) => {
            Some(ContinuationAst::CounterSourceRiderDestroy)
        }
        // CR 707.10c: "You may choose new targets for the copy/copies." after a
        // CopySpell — directly, or wrapped in a CreateDelayedTrigger ("When you
        // next cast ..., copy that spell"). The guard re-confirms the wrapper
        // actually contains a CopySpell.
        Effect::CopySpell { .. } | Effect::CreateDelayedTrigger { .. }
            if effect_wraps_copy_spell(previous_effect)
                && recognize_copy_retarget_clause(&lower) =>
        {
            Some(ContinuationAst::CopyMayRetarget)
        }
        // CR 201.2 + CR 608.2c: "[You may] put one of those cards onto the
        // battlefield if it has the same name as a permanent" after Dig —
        // Mitotic-Manipulation-style name-match selection. Patches the
        // preceding Dig with destination=Battlefield, keep_count=1, up_to=true
        // (always optional — "may" or implicit "if"), and a filter that
        // restricts selectable cards to those sharing a name with any
        // permanent currently on the battlefield.
        Effect::Dig { .. }
            if (nom_primitives::scan_contains(&lower, "one of those cards")
                || nom_primitives::scan_contains(&lower, "one of them"))
                && nom_primitives::scan_contains(&lower, "onto the battlefield")
                && (nom_primitives::scan_contains(&lower, "the same name as a permanent")
                    || nom_primitives::scan_contains(&lower, "shares a name with a permanent")) =>
        {
            use crate::types::ability::{FilterProp, TypedFilter};
            Some(ContinuationAst::DigFromAmong {
                count: 1,
                up_to: true,
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::NameMatchesAnyPermanent { controller: None },
                ])),
                destination: Some(Zone::Battlefield),
                rest_destination: None,
            })
        }
        // "You may put one of those cards back on top of your library" after
        // Dig — keep up to one looked-at card on top, leaving the remainder
        // for a following rest-placement clause.
        Effect::Dig { .. } if parse_put_one_dig_card_on_top(&lower) => {
            Some(ContinuationAst::DigFromAmong {
                count: 1,
                up_to: true,
                filter: TargetFilter::Any,
                destination: Some(Zone::Library),
                rest_destination: None,
            })
        }
        // "put them back in any order" after Dig means all looked-at cards
        // stay in the library and the player's submitted order becomes the
        // new top order. Leave keep_count unset so runtime resolves dynamic N.
        Effect::Dig { .. } if parse_put_all_back_in_any_order(&lower) => {
            Some(ContinuationAst::PutRest {
                destination: Zone::Library,
                reorder_all: true,
            })
        }
        Effect::SearchLibrary { .. } | Effect::Shuffle { .. } | Effect::Dig { .. }
            if parse_put_chosen_cards_at_library_position(&lower).is_some() =>
        {
            Some(ContinuationAst::PutChosenCardsAtLibraryPosition {
                position: parse_put_chosen_cards_at_library_position(&lower)
                    .expect("guard parsed position"),
            })
        }
        Effect::ChangeZone { .. } | Effect::ChooseFromZone { .. }
            if parse_becomes_plotted_continuation(&lower) =>
        {
            Some(ContinuationAst::BecomesPlotted)
        }
        // "Exile the rest" after Dig — sets rest_destination on the preceding
        // looked-at pile while preserving any prior kept-card destination.
        Effect::Dig { .. } if parse_exile_rest_after_dig(&lower) => {
            Some(ContinuationAst::PutRest {
                destination: Zone::Exile,
                reorder_all: false,
            })
        }
        // CR 406.3 + CR 701.16a: "[then] exile it/them [face down]" after a
        // private `Dig` (the "look at the top N cards of <player>'s library"
        // look step). This is the Gonti, Canny Acquisitor impulse idiom —
        // "look at the top card of that player's library, then exile it face
        // down. You may play that card ...". Plain `Dig` only inspects the top
        // cards (CR 701.16a); without a destination they stay in the library,
        // so the exile clause must rewrite the `Dig` into an `Effect::ExileTop`
        // (the face-down impulse-exile primitive shared with Cunning Rhetoric /
        // Bomat Courier) for the looked-at card to actually leave the library.
        //
        // `reveal: false` scopes this to the private "look at" form — a public
        // "reveal the top card ... then exile it" is a different visibility
        // class and is not the impulse idiom. `parse_exile_looked_at_card`
        // composes the pronoun and optional "face down" axes with combinators.
        Effect::Dig {
            player: dig_player,
            count,
            reveal: false,
            ..
        } if parse_exile_looked_at_card(&lower).is_some() => {
            // CR 406.3: hidden-information suffix → the card is exiled face down.
            let face_down = parse_exile_looked_at_card(&lower).unwrap_or(false);
            // CR 608.2c: "that player's library" parsed to `ParentTarget` at the
            // Dig site; re-resolve it through the shared library-owner combinator
            // so a damage/attack trigger binds to `TriggeringPlayer` (the proven
            // Cunning Rhetoric path) rather than the blocked-attacker object that
            // `ParentTarget` resolves to in a combat-damage context.
            let player = match dig_player {
                TargetFilter::ParentTarget => super::imperative::that_player_library_filter(ctx),
                other => other.clone(),
            };
            Some(ContinuationAst::ExileLookedAtCard {
                player,
                count: count.clone(),
                face_down,
            })
        }
        // "put the rest on the bottom" / "put those cards into your graveyard"
        // after Dig — sets rest_destination on the preceding Dig effect.
        Effect::Dig { .. }
            if nom_primitives::scan_contains(&lower, "put them back")
                || nom_primitives::scan_contains(&lower, "put the rest")
                || nom_primitives::scan_contains(&lower, "put those cards") =>
        {
            let destination = if nom_primitives::scan_contains(&lower, "into your graveyard")
                || nom_primitives::scan_contains(&lower, "into their graveyard")
            {
                Zone::Graveyard
            } else if nom_primitives::scan_contains(&lower, "into your hand")
                || nom_primitives::scan_contains(&lower, "into their hand")
            {
                Zone::Hand
            } else {
                // Default: bottom of library (covers "on the bottom", "back in any order", etc.)
                Zone::Library
            };
            Some(ContinuationAst::PutRest {
                destination,
                reorder_all: false,
            })
        }
        // CR 701.20a: "put that card into your hand / onto the battlefield" after RevealUntil
        // — overrides kept_destination. Also extracts rest_destination from a compound
        // rest clause merged on "and" (suppressed split because the rest-subject — "the
        // rest", "all other cards", "the other cards" — does not start with a recognized
        // imperative verb). Both bare imperative ("put that card", second-person
        // reveal-until) and third-person ("the player puts that card",
        // Polymorph / Proteus Staff / Transmogrify) forms are accepted.
        Effect::RevealUntil { .. }
            if nom_primitives::scan_contains(&lower, "put that card")
                || nom_primitives::scan_contains(&lower, "puts that card")
                || nom_primitives::scan_contains(&lower, "put it")
                || nom_primitives::scan_contains(&lower, "puts it") =>
        {
            let (destination, enter_tapped, enters_attacking) =
                if nom_primitives::scan_contains(&lower, "onto the battlefield") {
                    let tapped = nom_primitives::scan_contains(&lower, "tapped");
                    // CR 508.4: "put that card onto the battlefield tapped and
                    // attacking" (Raph & Mikey, Fireflux Squad).
                    let attacking = nom_primitives::scan_contains(&lower, "attacking");
                    (Zone::Battlefield, tapped, attacking)
                } else {
                    // Default "into your hand"
                    (Zone::Hand, false, false)
                };
            let rest = parse_reveal_until_rest_zone(&lower);
            // CR 701.20a + CR 608.2c: "you may put that card onto the battlefield"
            // makes the kept destination a controller choice. The decline zone is
            // the explicit "if you don't, put it into your hand" (→ Hand) or the
            // bottom-of-library rest pile by default (→ Library).
            let optional = nom_primitives::scan_contains(&lower, "you may put");
            let optional_decline = if optional {
                Some(if nom_primitives::scan_contains(&lower, "into your hand") {
                    Zone::Hand
                } else {
                    Zone::Library
                })
            } else {
                None
            };
            Some(ContinuationAst::RevealUntilKept {
                destination,
                enter_tapped,
                enters_attacking,
                rest_destination: rest,
                optional_decline,
            })
        }
        // CR 701.20a: "put the rest" / "the rest on the bottom" / "put the revealed cards"
        // after RevealUntil — overrides rest_destination. The "the rest" without "put"
        // occurs when split_clause_sequence splits "put X and the rest" on "and".
        // Also recognizes:
        //   • "shuffles ... revealed this way into <possessive> library" (Polymorph,
        //     Transmogrify) — the engine's existing rest=Library destination already
        //     random-orders, satisfying the shuffle semantics.
        //   • Third-person "puts" verb form (Polymorph chain).
        // CR 701.20a: "puts those cards into [zone]" / "put those cards into [zone]"
        // after RevealUntil — the entire revealed pile (matching card + everything
        // revealed before it) goes to the same zone. Checked before the PutRest arm
        // because "those cards" is a distinct semantic from "the rest" and must
        // override both kept_destination and rest_destination. Used by Balustrade
        // Spy, Consuming Aberration, Destroy the Evidence, Undercity Informer.
        Effect::RevealUntil { .. }
            if nom_primitives::scan_contains(&lower, "puts those cards")
                || nom_primitives::scan_contains(&lower, "put those cards") =>
        {
            let destination = if nom_primitives::scan_contains(&lower, "into your graveyard")
                || nom_primitives::scan_contains(&lower, "into their graveyard")
            {
                Zone::Graveyard
            } else if nom_primitives::scan_contains(&lower, "into exile")
                || nom_primitives::scan_contains(&lower, "on the bottom")
            {
                Zone::Library
            } else {
                Zone::Graveyard
            };
            Some(ContinuationAst::RevealUntilAllToZone { destination })
        }
        //   • "put the revealed cards" / "put them back" after RevealUntil — the
        //     revealed pile's destination override for the non-matching cards only.
        //   • "all other cards revealed this way" / "the other cards" / "exile all
        //     other cards revealed this way" — second-sentence rest clauses for
        //     Spoils of the Vault, Sacred Guide, Reviving Vapors and the broader
        //     "all other cards" family.
        Effect::RevealUntil { .. }
            if nom_primitives::scan_contains(&lower, "put the rest")
                || nom_primitives::scan_contains(&lower, "puts the rest")
                || nom_primitives::scan_contains(&lower, "the rest on the bottom")
                || nom_primitives::scan_contains(&lower, "the rest into your graveyard")
                || nom_primitives::scan_contains(&lower, "put the revealed cards")
                || nom_primitives::scan_contains(&lower, "put them back")
                || nom_primitives::scan_contains(&lower, "all other cards revealed this way")
                || nom_primitives::scan_contains(&lower, "other cards revealed this way")
                || (nom_primitives::scan_contains(&lower, "shuffle")
                    && nom_primitives::scan_contains(&lower, "library")) =>
        {
            // Delegate to the shared rest-zone matcher so the kept-card and
            // standalone-rest arms recognize the same destination phrases.
            let destination = parse_reveal_until_rest_zone(&lower).unwrap_or(Zone::Library);
            Some(ContinuationAst::PutRest {
                destination,
                reorder_all: false,
            })
        }
        // "create a ... token and suspect it" → chain suspect on last created token
        Effect::Token { .. }
            if tag::<_, _, OracleError<'_>>("suspect ")
                .parse(lower.as_str())
                .is_ok() =>
        {
            Some(ContinuationAst::SuspectLastCreated)
        }
        // CR 701.19c + CR 608.2c: "It can't be regenerated" prevents regeneration shields;
        // later text modifies the preceding Destroy instruction per CR 608.2c.
        Effect::Destroy { .. } | Effect::DestroyAll { .. }
            if nom_primitives::scan_contains(&lower, "can't be regenerated")
                || nom_primitives::scan_contains(&lower, "cannot be regenerated") =>
        {
            Some(ContinuationAst::CantRegenerate)
        }
        Effect::ChooseFromZone { .. }
            if lower == "put the rest on the bottom of your library in a random order"
                || lower == "put the rest on the bottom of your library in any order"
                || lower == "put the rest on the bottom of your library" =>
        {
            Some(ContinuationAst::PutChoiceRemainderOnBottom)
        }
        Effect::ChooseFromZone { .. } => parse_choice_partition_destinations(&lower)
            .map(|(chosen_destination, rest_destination)| {
                ContinuationAst::ChoicePartitionDestinations {
                    chosen_destination,
                    rest_destination,
                }
            })
            .or_else(|| {
                parse_put_chosen_cards_at_library_position(&lower)
                    .map(|position| ContinuationAst::PutChosenCardsAtLibraryPosition { position })
            }),
        // CR 700.2: "Choose/You choose/An opponent chooses/Target opponent chooses one/two/N
        // of them/those" after ChangeZone, ExileTop, RevealTop, or RevealHand →
        // ChooseFromZone building block
        Effect::ChangeZone { .. }
        | Effect::ExileTop { .. }
        | Effect::RevealTop { .. }
        | Effect::RevealHand { .. }
            if (nom_primitives::scan_contains(&lower, "of them")
                || nom_primitives::scan_contains(&lower, "of those"))
                && alt((
                    tag::<_, _, OracleError<'_>>("choose "),
                    tag("you choose "),
                    tag("an opponent chooses "),
                    tag("target opponent chooses "),
                ))
                .parse(lower.as_str())
                .is_ok() =>
        {
            let count = parse_choose_count_from_text(&lower);
            let chooser = if alt((
                tag::<_, _, OracleError<'_>>("an opponent chooses "),
                tag("target opponent chooses "),
            ))
            .parse(lower.as_str())
            .is_ok()
            {
                Chooser::Opponent
            } else {
                Chooser::Controller
            };
            Some(ContinuationAst::ChooseFromExile { count, chooser })
        }
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Hand,
            ..
        } if matches!(
            lower.trim(),
            "reveal that card"
                | "reveal those cards"
                | "reveal the card"
                | "reveal them"
                | "reveal it"
                | "put that card into your hand"
                | "put it into your hand"
        ) =>
        {
            Some(ContinuationAst::SearchResultClauseHandled)
        }
        Effect::SearchOutsideGame {
            destination: Zone::Hand,
            ..
        } if matches!(
            lower.trim(),
            "put that card into your hand" | "put it into your hand"
        ) =>
        {
            Some(ContinuationAst::SearchResultClauseHandled)
        }
        // CR 701.23a + CR 701.18a: When the preceding SearchDestination
        // continuation already moved the found card onto the battlefield
        // (e.g., Assassin's Trophy / Ranging Raptors / Harrow compound), the
        // explicit "put it onto the battlefield" chunk in the same sentence is
        // a paraphrase and must be absorbed to avoid a duplicate ChangeZone.
        //
        // CR 701.23i + CR 609.3: Iterated-search variants (Winds of Abandon class)
        // surface a plural subject ("those players put those cards onto the
        // battlefield tapped") because the search step has `repeat_for:
        // TrackedSetSize`. The compound has already been folded by the
        // SearchDestination intrinsic continuation; the standalone restatement
        // here would duplicate the ChangeZone if not absorbed. Use a structural
        // prefix-strip on the player-subject so all (subject × pronoun × tapped)
        // permutations match without N! enumerated arms.
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Battlefield,
            ..
        } if {
            let bare = strip_search_result_subject(lower.trim().trim_end_matches('.'));
            matches!(
                bare,
                "put that card onto the battlefield"
                    | "put it onto the battlefield"
                    | "put them onto the battlefield"
                    | "put those cards onto the battlefield"
                    | "put that card onto the battlefield tapped"
                    | "put it onto the battlefield tapped"
                    | "put them onto the battlefield tapped"
                    | "put those cards onto the battlefield tapped"
            )
        } =>
        {
            Some(ContinuationAst::SearchResultClauseHandled)
        }
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Exile,
            ..
        } if matches!(
            lower.trim(),
            "exile it"
                | "exile it face down"
                | "exile that card"
                | "exile that card face down"
                | "exile the card"
                | "exile the card face down"
        ) =>
        {
            Some(ContinuationAst::SearchResultClauseHandled)
        }
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Hand,
            ..
        } if lower == "put the rest on the bottom of your library in a random order"
            || lower == "put the rest on the bottom of your library in any order"
            || lower == "put the rest on the bottom of your library" =>
        {
            Some(ContinuationAst::PutChoiceRemainderOnBottom)
        }
        // "Put/return up to N [filter] from among them/those cards onto the
        // battlefield/into your hand/to your hand"
        // and "put N of them into your hand [and the rest on the bottom]"
        // after Dig — patches keep_count, filter, destination on the preceding Dig effect.
        //
        // CR 701.17c: An effect that refers to a milled card finds it in the
        // zone it moved to. "...from among the milled cards" after a `Mill`
        // is the same "from among [a prior selection set]" continuation as the
        // Dig form — `parse_dig_from_among` returns a `DigFromAmong`
        // continuation which, in `apply_clause_continuation`, pushes a
        // `TrackedSetFiltered`-targeted sub-ability when the source is a `Mill`.
        Effect::Dig { .. } | Effect::Mill { .. }
            if (nom_primitives::scan_contains(&lower, "from among them")
                || nom_primitives::scan_contains(&lower, "from among those cards")
                || nom_primitives::scan_contains(&lower, "from among the milled cards")
                || nom_primitives::scan_contains(&lower, "milled this way")
                || nom_primitives::scan_contains(&lower, "of them")
                || nom_primitives::scan_contains(&lower, "of those cards")
                || nom_primitives::scan_contains(&lower, "of those"))
                && (nom_primitives::scan_contains(&lower, "onto the battlefield")
                    || nom_primitives::scan_contains(&lower, "into your hand")
                    || nom_primitives::scan_contains(&lower, "into their hand")
                    || nom_primitives::scan_contains(&lower, "to your hand")
                    || nom_primitives::scan_contains(&lower, "to their hand")) =>
        {
            parse_dig_from_among(&lower, text)
        }
        // CR 701.33: "[You may] reveal [up to] N <filter> cards from among
        // them" after Dig — the reveal-only form where the kept cards are NOT
        // immediately routed to a fixed destination. Used by cards like
        // Zimone's Experiment where subsequent sub_abilities route the
        // revealed cards by type via `TargetFilter::TrackedSetFiltered`. The
        // Dig resolver populates a tracked set with the kept cards;
        // downstream effects consume that set.
        //
        // The guard is `from among` + `reveal` without any inline destination
        // phrase — if the clause carried its own destination, the previous
        // arm (with inline-destination requirement) would have matched first.
        Effect::Dig { .. }
            if nom_primitives::scan_contains(&lower, "reveal")
                && (nom_primitives::scan_contains(&lower, "from among them")
                    || nom_primitives::scan_contains(&lower, "from among those cards"))
                && !nom_primitives::scan_contains(&lower, "onto the battlefield")
                && !nom_primitives::scan_contains(&lower, "into your hand")
                && !nom_primitives::scan_contains(&lower, "into their hand") =>
        {
            parse_dig_from_among(&lower, text)
        }
        // CR 508.4 / CR 614.1: "It/The token enters tapped and attacking" (singular)
        // or "They/Those tokens enter tapped and attacking" (plural)
        // after CopyTokenOf, Token, or ChangeZone effects.
        Effect::CopyTokenOf { .. } | Effect::Token { .. } | Effect::ChangeZone { .. }
            if nom_primitives::scan_contains(&lower, "enters tapped and attacking")
                || nom_primitives::scan_contains(&lower, "enter tapped and attacking") =>
        {
            Some(ContinuationAst::EntersTappedAttacking)
        }
        Effect::ControlNextTurn { .. }
            if nom_primitives::scan_contains(&lower, "after that turn")
                && nom_primitives::scan_contains(&lower, "takes an extra turn") =>
        {
            Some(ContinuationAst::GrantExtraTurnAfterControlledTurn)
        }
        // CR 122.6a + CR 614.1c: Token enters-with-counters continuation. Two forms:
        //   * Declarative: "The token enters with X +1/+1 counters on it[, where X is ...]"
        //     or "It enters with X +1/+1 counters on it[, where X is ...]"
        //   * Imperative followup: "and put N [type] counter(s) on it"
        //     after a `create a [token]` clause (G'raha Tia, Fractal Anomaly,
        //     Fractal Tender, Berta — class of "create token ... and put
        //     counter on it" where "it" is the just-created token).
        // Both lift onto the preceding Token effect's `enter_with_counters`
        // so counters apply as the token enters (CR 614.1c replacement)
        // rather than as a post-ETB PutCounter effect that would mistakenly
        // target the source ability via `SelfRef`/`ParentTarget`.
        Effect::Token { .. } => try_parse_token_enters_with_counters(&lower)
            .or_else(|| try_parse_put_counters_on_token_followup(&lower)),
        _ => None,
    }
}

fn parse_choose_and_sacrifice_rest_followup(lower: &str) -> Option<ContinuationAst> {
    type E<'a> = OracleError<'a>;
    let lower = lower.trim();

    all_consuming(terminated(
        preceded(
            opt(tag::<_, _, E>("then ")),
            alt((
                parse_bare_choose_and_sacrifice_rest_filter,
                parse_explicit_choose_and_sacrifice_rest_filter,
            )),
        ),
        opt(tag(".")),
    ))
    .parse(lower)
    .ok()
    .map(|(_, sacrifice_filter)| ContinuationAst::ChooseAndSacrificeRestFilter { sacrifice_filter })
}

fn parse_bare_choose_and_sacrifice_rest_filter(
    input: &str,
) -> Result<(&str, Option<TargetFilter>), nom::Err<OracleError<'_>>> {
    let (input, _) =
        alt((tag::<_, _, OracleError<'_>>("sacrifices"), tag("sacrifice"))).parse(input)?;
    let (input, _) = tag(" the rest").parse(input)?;
    Ok((input, None))
}

fn parse_explicit_choose_and_sacrifice_rest_filter(
    input: &str,
) -> Result<(&str, Option<TargetFilter>), nom::Err<OracleError<'_>>> {
    let (input, _) = opt(tag::<_, _, OracleError<'_>>("each player ")).parse(input)?;
    let (input, _) = alt((
        tag::<_, _, OracleError<'_>>("sacrifices "),
        tag("sacrifice "),
    ))
    .parse(input)?;
    let (input, _) = tag("all other ").parse(input)?;
    let (input, filter) = parse_nonland_permanent_domain(input)?;
    Ok((input, Some(filter)))
}

fn parse_nonland_permanent_domain(
    input: &str,
) -> Result<(&str, TargetFilter), nom::Err<OracleError<'_>>> {
    let (input, _) = tag::<_, _, OracleError<'_>>("nonland permanents ").parse(input)?;
    let (input, _) = alt((
        tag::<_, _, OracleError<'_>>("they control"),
        tag("you control"),
        tag("that player controls"),
    ))
    .parse(input)?;
    Ok((
        input,
        TargetFilter::Typed(
            TypedFilter::permanent().with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
        ),
    ))
}

/// CR 122.6a: Parse "the token/it enters with X [counter type] counter(s) on it[, where X is ...]".
/// Returns `TokenEntersWithCounters` continuation on success.
fn try_parse_token_enters_with_counters(lower: &str) -> Option<ContinuationAst> {
    // Match subject prefix: "the token enters with " / "it enters with "
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("the token enters with "),
        tag("it enters with "),
    ))
    .parse(lower)
    .ok()?;

    // Parse count: could be "x", a number, or "a number of"
    let (rest, count_prefix) = alt((
        // "x " — variable resolved later via "where X is"
        value(None, tag::<_, _, OracleError<'_>>("x ")),
        // "a number of " — dynamic count resolved via suffix
        value(None, tag("a number of ")),
    ))
    .parse(rest)
    .unwrap_or_else(|_: nom::Err<OracleError<'_>>| {
        // Try parsing a fixed number
        if let Ok((r, n)) = nom_primitives::parse_number(rest) {
            let r = r.trim_start();
            (r, Some(n))
        } else {
            (rest, None)
        }
    });

    // Parse counter type: "+1/+1 " is the most common
    let (rest, counter_type) = alt((
        value(
            CounterType::Plus1Plus1,
            tag::<_, _, OracleError<'_>>("+1/+1 "),
        ),
        value(CounterType::Minus1Minus1, tag("-1/-1 ")),
    ))
    .parse(rest)
    .ok()?;

    // Consume "counter(s) on it"
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("counters on it"),
        tag("counter on it"),
    ))
    .parse(rest)
    .ok()?;

    // Parse optional ", where x is [quantity]"
    let quantity = if let Ok((rest_where, _)) =
        tag::<_, _, OracleError<'_>>(", where x is ").parse(rest.trim_start_matches(['.', ' ']))
    {
        let qty_text = rest_where.trim().trim_end_matches('.');
        parse_cda_quantity(qty_text)
            .or_else(|| parse_quantity_ref(qty_text).map(|q| QuantityExpr::Ref { qty: q }))
    } else if let Ok((rest_equal, _)) =
        tag::<_, _, OracleError<'_>>("equal to ").parse(rest.trim_start_matches(['.', ' ']))
    {
        let qty_text = rest_equal.trim().trim_end_matches('.');
        parse_cda_quantity(qty_text)
            .or_else(|| parse_quantity_ref(qty_text).map(|q| QuantityExpr::Ref { qty: q }))
    } else {
        None
    };

    let count = if let Some(qty) = quantity {
        qty
    } else if let Some(n) = count_prefix {
        QuantityExpr::Fixed { value: n as i32 }
    } else {
        // X without "where X is" — variable resolved from spell payment at runtime
        QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        }
    };

    Some(ContinuationAst::TokenEntersWithCounters {
        counter_type,
        count,
    })
}

/// CR 122.6a + CR 614.1c: Parse the imperative followup form
/// "put N [counter type] counter(s) on it[, where X is ...]" /
/// "put[s] a number of [counter type] counter(s) on it equal to <quantity>"
/// that follows a `create a [token]` clause. "It" refers to the just-created
/// token; the counters must be lifted onto `Token.enter_with_counters` so they
/// apply as the token enters the battlefield (CR 614.1c) rather than as a
/// post-ETB PutCounter effect targeting the ability source.
///
/// Verb axis: `put ` (imperative, "create a token and put …") or `puts `
/// (third-person, "Each player creates a token and puts …" — Oversimplify
/// class). The verb is a single `alt()` over the two conjugations — adding
/// a third form means extending the `alt()`, not duplicating the function.
///
/// Quantity axis: three forms in priority order —
///   1. `"a number of <type> counter(s) on it equal to <quantity>"` →
///      delegated to the shared `parse_dynamic_counter_suffix_body` building
///      block (single source of truth for "enters with N counters equal to
///      X"; also used by `Effect::ChangeZone.enter_with_counters`).
///   2. `"N <type> counter(s) on it, where x is <quantity>"` → fixed N with
///      a deferred X-binding (Fractal Anomaly class).
///   3. `"N <type> counter(s) on it"` → fixed N.
///
/// Returns `TokenEntersWithCounters` so it shares the continuation absorption
/// path with `try_parse_token_enters_with_counters` (declarative form).
fn try_parse_put_counters_on_token_followup(lower: &str) -> Option<ContinuationAst> {
    // Optional leading "and " (rare — usually consumed by the splitter),
    // then the verb. Both `put ` (imperative) and `puts ` (third-person,
    // "Each player … puts a number of counters on it …") feed the same
    // counter-suffix grammar — the verb is a single `alt()` axis.
    let (rest, _) = nom::combinator::opt(tag::<_, _, OracleError<'_>>("and "))
        .parse(lower)
        .ok()?;
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("puts "),
        tag::<_, _, OracleError<'_>>("put "),
    ))
    .parse(rest)
    .ok()?;

    // Form 1 (priority): dynamic "a number of <type> counters on it equal to
    // <quantity>". Delegates to the shared building block in `oracle_effect/
    // mod.rs`. The body consumes the full clause (including trailing period),
    // so on success we're done — emit the continuation directly.
    if let Ok((_, (counter_type, count))) =
        super::parse_dynamic_counter_suffix_body(rest.trim_end_matches('.').trim_end())
    {
        return Some(ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        });
    }

    // Forms 2 + 3 (fixed N with optional "where x is" rebind): the original
    // grammar, retained for the Fractal Anomaly / G'raha Tia class. Counter
    // count is `x` (deferred), an article (1), or a literal number.
    let (rest, count_prefix) = alt((
        // "x " — variable resolved later via "where X is" or by caller payment
        value(None, tag::<_, _, OracleError<'_>>("x ")),
        value(Some(1u32), tag("a ")),
        value(Some(1u32), tag("an ")),
    ))
    .parse(rest)
    .unwrap_or_else(|_: nom::Err<OracleError<'_>>| {
        if let Ok((r, n)) = nom_primitives::parse_number(rest) {
            (r.trim_start(), Some(n))
        } else {
            (rest, None)
        }
    });

    // Parse counter type: only +1/+1 and -1/-1 are common in token contexts
    // (matches the AST scope of the existing enters-with-counters helper).
    let (rest, counter_type) = alt((
        value(
            CounterType::Plus1Plus1,
            tag::<_, _, OracleError<'_>>("+1/+1 "),
        ),
        value(CounterType::Minus1Minus1, tag("-1/-1 ")),
    ))
    .parse(rest)
    .ok()?;

    // Consume "counter(s) on it" — the "on it" anaphor pinning the counters
    // to the just-created token.
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("counters on it"),
        tag("counter on it"),
    ))
    .parse(rest)
    .ok()?;

    // Optional ", where x is [quantity]" suffix (Fractal Anomaly). The
    // followup clause is already trimmed by the splitter, so no leading
    // punctuation cleanup is needed before the comma.
    let quantity =
        if let Ok((rest_where, _)) = tag::<_, _, OracleError<'_>>(", where x is ").parse(rest) {
            // allow-noncombinator: trailing-period cleanup on a pre-tokenized
            // suffix; not parsing dispatch.
            let qty_text = rest_where.trim().trim_end_matches('.');
            parse_cda_quantity(qty_text)
                .or_else(|| parse_quantity_ref(qty_text).map(|q| QuantityExpr::Ref { qty: q }))
        } else if let Ok((rest_equal, _)) =
            tag::<_, _, OracleError<'_>>("equal to ").parse(rest.trim_start_matches(['.', ' ']))
        {
            // CR 122.6a: "put a number of counters on it equal to [qty]" — dynamic
            // counter count in the imperative followup form (Primo, the Unbounded).
            // allow-noncombinator: trailing-period cleanup on a pre-tokenized suffix.
            let qty_text = rest_equal.trim().trim_end_matches('.');
            parse_cda_quantity(qty_text)
                .or_else(|| parse_quantity_ref(qty_text).map(|q| QuantityExpr::Ref { qty: q }))
        } else {
            None
        };

    let count = if let Some(qty) = quantity {
        qty
    } else if let Some(n) = count_prefix {
        QuantityExpr::Fixed { value: n as i32 }
    } else {
        // Bare X with no "where X is" — variable resolved from the enclosing
        // ability's payment (e.g., G'raha Tia: X is the spell's mana value
        // paid as life via the parent PayCost).
        QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        }
    };

    Some(ContinuationAst::TokenEntersWithCounters {
        counter_type,
        count,
    })
}

/// Parse a keyword-list tail: one or more keyword names joined by `", "`,
/// `" and "`, or `", and "`, optionally terminated by `.`.
///
/// Composed from `parse_keyword_name` (the single keyword authority) +
/// `alt`-of-separators — one `alt()` call per axis of variation, never an
/// enumeration of full phrases. Unrecognized words abort the list, so the
/// combinator only accepts a clean, fully-keyword sequence.
fn parse_keyword_list(input: &str) -> nom::IResult<&str, Vec<Keyword>, OracleError<'_>> {
    let separator = |i| -> nom::IResult<&str, (), OracleError<'_>> {
        alt((
            value((), tag::<_, _, OracleError<'_>>(", and ")),
            value((), tag(", ")),
            value((), tag(" and ")),
        ))
        .parse(i)
    };
    // A single keyword: name → `Keyword`. `parse_keyword_name` only matches
    // evergreen-vocabulary words, so the `FromStr` parse below cannot fail.
    let one_keyword = |i| -> nom::IResult<&str, Keyword, OracleError<'_>> {
        let (rest, name) = parse_keyword_name(i)?;
        let keyword: Keyword = name
            .parse()
            .map_err(|_| nom::Err::Error(nom::error::Error::new(i, nom::error::ErrorKind::Fail)))?;
        Ok((rest, keyword))
    };
    let (mut rest, first) = one_keyword(input)?;
    let mut keywords = vec![first];
    while let Ok((after_sep, ())) = separator(rest) {
        let Ok((after_kw, keyword)) = one_keyword(after_sep) else {
            break;
        };
        keywords.push(keyword);
        rest = after_kw;
    }
    Ok((rest, keywords))
}

/// CR 702: Parse "The same is true for <keyword list>." — Odric, Lunarch
/// Marshal's follow-up sentence that extends the antecedent keyword grant to
/// each additional listed keyword.
///
/// Returns the parsed keyword list. The chunk loop wraps this in
/// `SpecialClause::SameIsTrueFor`; lowering reads the antecedent
/// `GenericEffect` clause and clones its grant template once per keyword.
/// Generalized over the whole evergreen-keyword vocabulary — covers every card
/// of this "the same is true for …" class, not Odric alone.
pub(super) fn try_parse_same_is_true_continuation(text: &str) -> Option<Vec<Keyword>> {
    let lower = text.to_lowercase();
    let (keywords, rest) = nom_on_lower(text, &lower, |i| {
        let (i, _) = tag("the same is true for ").parse(i)?;
        parse_keyword_list(i)
    })?;
    // The sentence must be fully consumed by the keyword list (modulo a
    // trailing period) — a leftover tail means this is not a pure
    // same-is-true-for clause.
    if rest.trim().trim_end_matches('.').is_empty() {
        Some(keywords)
    } else {
        None
    }
}

/// CR 608.2c: Parse "Repeat this process for <keyword list>." — Kathril, Aspect
/// Warper. Returns the keyword list; the chunk loop wraps it in
/// `SpecialClause::RepeatProcessForKeywords` and lowering replicates the
/// antecedent conditional keyword-counter clause once per keyword. Mirrors
/// `try_parse_same_is_true_continuation`; covers every "repeat this process for
/// <list>" card, not Kathril alone.
pub(super) fn try_parse_repeat_process_for_keywords(text: &str) -> Option<Vec<Keyword>> {
    let lower = text.to_lowercase();
    let (keywords, rest) = nom_on_lower(text, &lower, |i| {
        let (i, _) = tag("repeat this process for ").parse(i)?;
        parse_keyword_list(i)
    })?;
    // The sentence must be fully consumed by the keyword list (modulo a trailing
    // period) — a leftover tail means this is some other "repeat this process …"
    // form (e.g. "repeat this process any number of times") and must not match.
    if rest.trim().trim_end_matches('.').is_empty() {
        Some(keywords)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::QuantityExpr;

    #[test]
    fn rest_cards_reference_matches_bare_the_other() {
        // 5a: bare "the other" (cultivate) must parse.
        let (rest, ()) =
            parse_rest_cards_reference("the other into your hand").expect("bare 'the other'");
        assert_eq!(rest, " into your hand");
        // Ordering guard: "the other cards" must still consume the full phrase
        // (longer form precedes so it is not shadowed by the bare "the other").
        let (rest, ()) =
            parse_rest_cards_reference("the other cards on the bottom").expect("'the other cards'");
        assert_eq!(rest, " on the bottom");
        // And "the rest" remains matched.
        let (rest, ()) = parse_rest_cards_reference("the rest into your hand").expect("'the rest'");
        assert_eq!(rest, " into your hand");
    }

    /// Helper: extract just the text fields from split_clause_sequence output.
    fn clause_texts(input: &str) -> Vec<String> {
        split_clause_sequence(input)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    #[test]
    fn quoted_token_ability_boundary_splits_before_then_if() {
        let chunks = clause_texts(
            "create a tapped 0/1 black Wizard creature token with \"Whenever you cast a noncreature spell, this token deals 1 damage to each opponent.\" Then if you control four or more Wizards, transform ~.",
        );
        assert_eq!(
            chunks,
            vec![
                "create a tapped 0/1 black Wizard creature token with \"Whenever you cast a noncreature spell, this token deals 1 damage to each opponent.\"",
                "Then if you control four or more Wizards, transform ~",
            ]
        );
    }

    // --- Bare " and " splitting: positive cases (should split) ---

    #[test]
    fn bare_and_splits_lose_life_and_create_token() {
        // Lotho: "you lose 1 life and create a Treasure token"
        let chunks = clause_texts("you lose 1 life and create a Treasure token");
        assert_eq!(chunks, vec!["you lose 1 life", "create a Treasure token"]);
    }

    #[test]
    fn bare_and_keeps_chosen_name_search_exile_compound() {
        // CR 701.23a + CR 701.18a: "search ... with the chosen name and exile
        // them" is one search compound, not a SearchLibrary followed by a second
        // standalone ChangeZone.
        let chunks = clause_texts(
            "search target opponent's graveyard, hand, and library for any number of cards with the chosen name and exile them",
        );
        assert_eq!(chunks.len(), 1, "unexpected split: {chunks:?}");
    }

    #[test]
    fn bare_and_splits_draw_and_lose() {
        let chunks = clause_texts("draw a card and lose 1 life");
        assert_eq!(chunks, vec!["draw a card", "lose 1 life"]);
    }

    #[test]
    fn bare_and_splits_draw_and_add_mana() {
        let chunks = clause_texts("draw that many cards and add that much {R}");
        assert_eq!(chunks, vec!["draw that many cards", "add that much {R}"]);
    }

    #[test]
    fn bare_and_splits_destroy_and_gain() {
        let chunks = clause_texts("destroy target creature and gain 3 life");
        assert_eq!(chunks, vec!["destroy target creature", "gain 3 life"]);
    }

    #[test]
    fn bare_and_splits_create_token_and_manifest() {
        let chunks = clause_texts(
            "create a Treasure token and manifest the top card of that player's library",
        );
        assert_eq!(
            chunks,
            vec![
                "create a Treasure token",
                "manifest the top card of that player's library"
            ]
        );
    }

    #[test]
    fn sentence_split_handles_plural_possessive_apostrophe() {
        let chunks = clause_texts(
            "return target artifacts to their owners' hands. you may cast a spell from your hand",
        );
        assert_eq!(
            chunks,
            vec![
                "return target artifacts to their owners' hands",
                "you may cast a spell from your hand"
            ]
        );
    }

    /// CR 701.27 + CR 701.28: "transform"/"convert" must split as clause-starts.
    /// Primal Amulet class: "remove those counters and transform it" reaches
    /// the dispatcher as two independent clauses so each parses cleanly.
    #[test]
    fn bare_and_splits_remove_and_transform() {
        let chunks = clause_texts("remove those counters and transform it");
        assert_eq!(chunks, vec!["remove those counters", "transform it"]);
    }

    #[test]
    fn bare_and_splits_remove_and_convert() {
        let chunks = clause_texts("remove all of them and convert this creature");
        assert_eq!(chunks, vec!["remove all of them", "convert this creature"]);
    }

    // --- Bare " and " splitting: negative cases (must NOT split) ---

    #[test]
    fn bare_and_preserves_chosen_rest_choice_partition() {
        let chunks =
            clause_texts("Put the chosen cards into your graveyard and the rest into your hand.");
        assert_eq!(
            chunks,
            vec!["Put the chosen cards into your graveyard and the rest into your hand"]
        );
    }

    #[test]
    fn bare_and_preserves_shuffle_chosen_rest_choice_partition() {
        let chunks = clause_texts(
            "Shuffle the chosen cards into your library and put the rest into your hand.",
        );
        assert_eq!(
            chunks,
            vec!["Shuffle the chosen cards into your library and put the rest into your hand"]
        );
    }

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
    fn bare_and_does_not_split_targeted_put_counter_continuation() {
        let chunks =
            clause_texts("tap target creature an opponent controls and put a stun counter on it");
        assert_eq!(
            chunks,
            vec!["tap target creature an opponent controls and put a stun counter on it"]
        );
    }

    #[test]
    fn bare_and_does_not_split_power_and_toughness() {
        let chunks = clause_texts("power and toughness each equal to the number of cards");
        assert_eq!(
            chunks,
            vec!["power and toughness each equal to the number of cards"]
        );
    }

    /// CR 613.1d + CR 613.4b: Vedalken Humiliator — "lose all abilities and
    /// have base power and toughness 1/1 until end of turn" must stay as one
    /// chunk so `parse_continuous_modifications` produces a single GenericEffect
    /// with both RemoveAllAbilities and SetPower/SetToughness modifications on
    /// the same affected filter (opponents' creatures).
    #[test]
    fn bare_and_does_not_split_lose_abilities_and_have_base_pt() {
        let chunks = clause_texts(
            "creatures your opponents control lose all abilities and have base power and toughness 1/1 until end of turn",
        );
        assert_eq!(
            chunks,
            vec![
                "creatures your opponents control lose all abilities and have base power and toughness 1/1 until end of turn"
            ]
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
    fn comma_then_you_control_subject_predicate_splits() {
        let chunks = clause_texts(
            "create a 2/2 colorless Robot artifact creature token, then creatures you control get +1/+0 and gain haste until end of turn",
        );
        assert_eq!(
            chunks,
            vec![
                "create a 2/2 colorless Robot artifact creature token",
                "creatures you control get +1/+0 and gain haste until end of turn",
            ]
        );
    }

    #[test]
    fn static_modifier_conjunct_does_not_split() {
        let chunks =
            clause_texts("creatures you control get +1/+0 and gain haste until end of turn");
        assert_eq!(
            chunks,
            vec!["creatures you control get +1/+0 and gain haste until end of turn"]
        );
    }

    #[test]
    fn comma_then_its_controller_clause_splits() {
        let chunks = clause_texts(
            "exile the chosen creature, then its controller gains life equal to its mana value",
        );
        assert_eq!(
            chunks,
            vec![
                "exile the chosen creature",
                "its controller gains life equal to its mana value"
            ]
        );
    }

    #[test]
    fn comma_keyword_list_does_not_split_double_strike() {
        let chunks = clause_texts(
            "creatures you control gain flying, vigilance, and double strike until end of turn",
        );
        assert_eq!(
            chunks,
            vec![
                "creatures you control gain flying, vigilance, and double strike until end of turn"
            ]
        );
    }

    #[test]
    fn comma_keyword_list_does_not_split_double_team() {
        let chunks = clause_texts("creatures you control gain flying, and double team");
        assert_eq!(
            chunks,
            vec!["creatures you control gain flying, and double team"]
        );
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
    fn conjugated_verb_splits_after_then() {
        // CR 608.2c: Third-person verb forms after ", then" must split.
        // "Each player discards their hand, then draws seven cards."
        let chunks = clause_texts("discards their hand, then draws seven cards");
        assert_eq!(chunks, vec!["discards their hand", "draws seven cards"]);
    }

    #[test]
    fn conjugated_verb_puts_splits_after_then() {
        // "then puts that card on the bottom" should split
        let chunks = clause_texts("reveals the top card, then puts that card on the bottom");
        assert_eq!(
            chunks,
            vec!["reveals the top card", "puts that card on the bottom"]
        );
    }

    #[test]
    fn conjugated_verb_sacrifices_splits_after_then() {
        let chunks = clause_texts("creates a token, then sacrifices a creature");
        assert_eq!(chunks, vec!["creates a token", "sacrifices a creature"]);
    }

    #[test]
    fn comma_conjugated_player_predicates_split() {
        let chunks = clause_texts(
            "target opponent sacrifices a creature, discards a card, and loses 3 life",
        );
        assert_eq!(
            chunks,
            vec![
                "target opponent sacrifices a creature",
                "discards a card",
                "and loses 3 life"
            ]
        );
    }

    #[test]
    fn possessive_its_does_not_trigger_deconjugation() {
        // Bare "its" must NOT be deconjugated — it is a possessive pronoun.
        assert!(!starts_clause_text_or_conjugated("its power increases"));
        assert!(starts_clause_text_or_conjugated(
            "its controller gains life"
        ));
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
    fn bare_and_splits_sacrifice_and_open_attraction() {
        let chunks = clause_texts("sacrifice this Attraction and open an Attraction");
        assert_eq!(
            chunks,
            vec!["sacrifice this Attraction", "open an Attraction"]
        );
    }

    #[test]
    fn bare_and_splits_sacrifice_and_returns() {
        let chunks =
            clause_texts("that player simultaneously sacrifices the artifact and returns it");
        assert_eq!(
            chunks,
            vec![
                "that player simultaneously sacrifices the artifact",
                "returns it"
            ]
        );
    }

    #[test]
    fn bare_and_splits_search_and_cast() {
        let chunks = clause_texts(
            "search your library for an instant card with mana value 4 or less and cast that card without paying its mana cost",
        );
        assert_eq!(
            chunks,
            vec![
                "search your library for an instant card with mana value 4 or less",
                "cast that card without paying its mana cost"
            ]
        );
    }

    #[test]
    fn bare_and_splits_search_and_cloak() {
        let chunks = clause_texts("search your library for a nonland card and cloak it");
        assert_eq!(
            chunks,
            vec!["search your library for a nonland card", "cloak it"]
        );
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
    fn from_among_sacrifice_rest_splits_for_absorption() {
        let chunks = clause_texts(
            "each player chooses an artifact, a creature, an enchantment, and a planeswalker from among the nonland permanents they control, then sacrifices the rest.",
        );
        assert_eq!(chunks.len(), 2);
        assert_eq!(
            chunks[1], "sacrifices the rest",
            "sacrifice-rest continuation must be a separate chunk"
        );
    }

    #[test]
    fn choose_and_sacrifice_rest_followup_accepts_then_and_period() {
        let effect = Effect::ChooseAndSacrificeRest {
            categories: vec![crate::types::card_type::CoreType::Artifact],
            chooser_scope: crate::types::ability::CategoryChooserScope::EachPlayerSelf,
            choose_filter: TargetFilter::Typed(TypedFilter::permanent()),
            sacrifice_filter: TargetFilter::Typed(TypedFilter::permanent()),
        };
        assert_eq!(
            parse_followup_continuation_ast(
                "then sacrifices the rest.",
                &effect,
                &mut ParseContext::default(),
            ),
            Some(ContinuationAst::ChooseAndSacrificeRestFilter {
                sacrifice_filter: None,
            })
        );
    }

    #[test]
    fn bare_and_splits_gain_life_and_get_energy() {
        let chunks = clause_texts("you gain 1 life and get {E} (an energy counter)");
        assert_eq!(
            chunks,
            vec!["you gain 1 life", "get {E} (an energy counter)"]
        );
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

    // --- CR 707.10c: copy-retarget clause recognition ---

    #[test]
    fn recognize_copy_retarget_clause_variants() {
        // Fork / Twincast — "You may choose new targets for the copy/copies."
        assert!(recognize_copy_retarget_clause(
            "you may choose new targets for the copy."
        ));
        assert!(recognize_copy_retarget_clause(
            "you may choose new targets for the copies"
        ));
        // The Chain cycle — "[and] may choose a new target for that copy"
        // (no "you" subject after clause-splitting; singular; "that copy").
        assert!(recognize_copy_retarget_clause(
            "may choose a new target for that copy"
        ));
        assert!(recognize_copy_retarget_clause(
            "you may choose a new target for that copy."
        ));
        // Negatives.
        assert!(!recognize_copy_retarget_clause("copy that spell"));
        assert!(!recognize_copy_retarget_clause(
            "may choose a new target for the creature"
        ));
    }

    #[test]
    fn copy_retarget_followup_recognized_after_copy_spell() {
        let copy = Effect::CopySpell {
            target: TargetFilter::SelfRef,
            retarget: CopyRetargetPermission::KeepOriginalTargets,
        };
        let result = parse_followup_continuation_ast(
            "may choose a new target for that copy",
            &copy,
            &mut ParseContext::default(),
        );
        assert_eq!(result, Some(ContinuationAst::CopyMayRetarget));
    }

    /// CR 707.10c: `set_copy_retarget_in_ability` must descend the sub-ability
    /// chain — the Chain cycle nests the optional `CopySpell` under its parent.
    #[test]
    fn set_copy_retarget_descends_into_sub_ability() {
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Player,
                random: false,
                unless_filter: None,
                filter: None,
            },
        );
        def.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
            },
        )));

        assert!(set_copy_retarget_in_ability(
            &mut def,
            &CopyRetargetPermission::MayChooseNewTargets
        ));
        let sub = def.sub_ability.as_ref().unwrap();
        assert!(matches!(
            *sub.effect,
            Effect::CopySpell {
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                ..
            }
        ));
    }

    // --- parse_followup_continuation_ast: PutRest destination parsing ---

    fn make_dig_effect() -> Effect {
        Effect::Dig {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 3 },
            destination: None,
            keep_count: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: None,
            reveal: false,
        }
    }

    #[test]
    fn put_rest_bottom_of_library_with_any_order() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put the rest on the bottom of your library in any order.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Library,
                reorder_all: false,
            })
        );
    }

    #[test]
    fn put_rest_bottom_of_library_without_any_order() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put the rest on the bottom of your library.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Library,
                reorder_all: false,
            })
        );
    }

    #[test]
    fn put_rest_into_graveyard() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put the rest into your graveyard.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Graveyard,
                reorder_all: false,
            })
        );
    }

    #[test]
    fn put_rest_random_order_bottom() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put the rest on the bottom of your library in a random order.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Library,
                reorder_all: false,
            })
        );
    }

    #[test]
    fn put_them_back_any_order() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put them back in any order.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Library,
                reorder_all: true,
            })
        );
    }

    #[test]
    fn put_rest_into_hand() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put the rest into your hand.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Hand,
                reorder_all: false,
            })
        );
    }

    #[test]
    fn put_those_cards_on_bottom() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put those cards on the bottom of your library in any order.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Library,
                reorder_all: false,
            })
        );
    }

    // --- "put N of them" DigFromAmong continuation ---

    #[test]
    fn put_two_of_them_into_hand_with_rest_on_bottom() {
        // Stock Up / Dig Through Time pattern: keep count + rest destination in one clause.
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put two of them into your hand and the rest on the bottom of your library in any order.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::DigFromAmong {
                count: 2,
                up_to: false,
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Library),
            })
        );
    }

    #[test]
    fn put_one_of_them_into_hand_with_rest_on_bottom() {
        // Impulse / Anticipate pattern.
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put one of them into your hand and the rest on the bottom of your library in any order.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::DigFromAmong {
                count: 1,
                up_to: false,
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Library),
            })
        );
    }

    /// CR 401.1 + CR 401.4 + CR 701.20e: Sleight of Hand / Sea Gate Oracle /
    /// Sight Beyond Sight pattern. "Put one of them into your hand and the
    /// other on the bottom of your library." The anaphor "the other"
    /// (singular remainder of a count=2 look) must be recognized as
    /// equivalent to "the rest" (general remainder); both must yield
    /// `rest_destination: Some(Library)` — NOT the graveyard default.
    #[test]
    fn put_one_of_them_into_hand_with_other_on_bottom() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put one of them into your hand and the other on the bottom of your library.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::DigFromAmong {
                count: 1,
                up_to: false,
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Library),
            })
        );
    }

    #[test]
    fn put_two_of_them_into_hand_rest_into_graveyard() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put two of them into your hand and the rest into your graveyard.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::DigFromAmong {
                count: 2,
                up_to: false,
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Graveyard),
            })
        );
    }

    /// CR 701.17c + CR 608.2c: Dredger's Insight — "...You may put an
    /// artifact, creature, or land card from among the milled cards into your
    /// hand" after `Mill 4`. The continuation must fire for a preceding
    /// `Effect::Mill` (not just `Effect::Dig`) and recognize the
    /// "from among the milled cards" phrase.
    #[test]
    fn mill_from_among_milled_cards_emits_dig_from_among() {
        let mill = Effect::Mill {
            count: QuantityExpr::Fixed { value: 4 },
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        };
        let result = parse_followup_continuation_ast(
            "You may put an artifact, creature, or land card from among the milled cards into your hand.",
            &mill,
            &mut ParseContext::default(),
        );
        let Some(ContinuationAst::DigFromAmong {
            count,
            up_to,
            filter,
            destination,
            rest_destination,
        }) = result
        else {
            panic!("expected DigFromAmong continuation, got {result:?}");
        };
        assert_eq!(count, 1);
        assert!(up_to, "\"may put\" is optional → up_to");
        assert_eq!(destination, Some(Zone::Hand));
        assert_eq!(rest_destination, None);
        // The Or[Artifact, Creature, Land] filter is carried through verbatim.
        assert!(matches!(filter, TargetFilter::Or { .. }), "got {filter:?}");
    }

    /// CR 701.17c + CR 608.2c: Midnight Tilling uses the equivalent
    /// "return ... from among them to your hand" wording instead of
    /// "put ... from among the milled cards into your hand". It must still
    /// bind the follow-up choice to the just-milled tracked set.
    #[test]
    fn mill_return_from_among_them_to_hand_emits_dig_from_among() {
        let mill = Effect::Mill {
            count: QuantityExpr::Fixed { value: 4 },
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        };
        let result = parse_followup_continuation_ast(
            "You may return a permanent card from among them to your hand.",
            &mill,
            &mut ParseContext::default(),
        );
        let Some(ContinuationAst::DigFromAmong {
            count,
            up_to,
            filter,
            destination,
            rest_destination,
        }) = result
        else {
            panic!("expected DigFromAmong continuation, got {result:?}");
        };
        assert_eq!(count, 1);
        assert!(up_to);
        assert_eq!(destination, Some(Zone::Hand));
        assert_eq!(rest_destination, None);
        assert!(matches!(filter, TargetFilter::Typed(_)), "got {filter:?}");
    }

    /// CR 701.17c + CR 608.2c: Ripples of Undeath uses "a card milled this
    /// way" instead of "from among the milled cards". It must still bind the
    /// follow-up return to the cards moved by the preceding `Mill`.
    #[test]
    fn mill_return_card_milled_this_way_to_hand_emits_dig_from_among() {
        let mill = Effect::Mill {
            count: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        };
        let result = parse_followup_continuation_ast(
            "Return a card milled this way to your hand.",
            &mill,
            &mut ParseContext::default(),
        );
        let Some(ContinuationAst::DigFromAmong {
            count,
            up_to,
            filter,
            destination,
            rest_destination,
        }) = result
        else {
            panic!("expected DigFromAmong continuation, got {result:?}");
        };
        assert_eq!(count, 1);
        assert!(
            !up_to,
            "after the optional payment is made, returning a card is not optional"
        );
        assert_eq!(filter, TargetFilter::Any);
        assert_eq!(destination, Some(Zone::Hand));
        assert_eq!(rest_destination, None);
    }

    /// CR 701.17c: `apply_clause_continuation` must PUSH a `ChangeZone`
    /// sub-ability targeting `TrackedSetFiltered` when the preceding def is a
    /// `Mill` — scoping the zone-change to the milled cards rather than the
    /// raw `Or` filter (which would resolve against the battlefield).
    #[test]
    fn mill_from_among_pushes_tracked_set_filtered_change_zone() {
        let or_filter = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(crate::types::ability::TypedFilter::default()),
                TargetFilter::Typed(crate::types::ability::TypedFilter::default()),
            ],
        };
        let mut defs = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 4 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
        )];
        apply_clause_continuation(
            &mut defs,
            ContinuationAst::DigFromAmong {
                count: 1,
                up_to: true,
                filter: or_filter.clone(),
                destination: Some(Zone::Hand),
                rest_destination: None,
            },
            AbilityKind::Spell,
        );
        // The Mill is left intact; a new ChangeZone def is pushed.
        assert_eq!(defs.len(), 2, "expected Mill + pushed ChangeZone");
        assert!(matches!(&*defs[0].effect, Effect::Mill { .. }));
        let Effect::ChangeZone {
            origin,
            destination,
            target,
            up_to,
            ..
        } = &*defs[1].effect
        else {
            panic!("expected pushed ChangeZone, got {:?}", defs[1].effect);
        };
        assert_eq!(*origin, None);
        assert_eq!(*destination, Zone::Hand);
        assert!(*up_to);
        match target {
            TargetFilter::TrackedSetFiltered { id, filter } => {
                assert_eq!(id.0, 0, "sentinel TrackedSetId(0) — resolved at runtime");
                assert_eq!(**filter, or_filter, "inner filter preserved");
            }
            other => panic!("expected TrackedSetFiltered target, got {other:?}"),
        }
    }

    /// CR 118.3 + CR 608.2c: A payment clause between the mill and "milled this
    /// way" return is lookback-transparent. The return must patch the earlier
    /// `Mill`, not bind `ParentTarget` to the payment/current source.
    #[test]
    fn mill_pay_then_return_milled_this_way_uses_tracked_set() {
        use super::super::parse_effect_chain;

        let def = parse_effect_chain(
            "Mill three cards. Then you may pay {1} and 3 life. If you do, return a card milled this way to your hand.",
            AbilityKind::Spell,
        );

        let mut effects: Vec<&AbilityDefinition> = Vec::new();
        let mut node = Some(&def);
        while let Some(d) = node {
            effects.push(d);
            node = d.sub_ability.as_deref();
        }

        assert!(matches!(&*effects[0].effect, Effect::Mill { .. }));
        assert!(matches!(&*effects[1].effect, Effect::PayCost { .. }));
        let Effect::ChangeZone {
            destination,
            target,
            ..
        } = &*effects[2].effect
        else {
            panic!("expected ChangeZone return, got {:?}", effects[2].effect);
        };
        assert_eq!(*destination, Zone::Hand);
        match target {
            TargetFilter::TrackedSetFiltered { id, filter } => {
                assert_eq!(id.0, 0);
                assert_eq!(**filter, TargetFilter::Any);
            }
            other => panic!("expected TrackedSetFiltered target, got {other:?}"),
        }
    }

    /// Parser AST-shape test (issue #420). Birthing Ritual's full triggered-
    /// ability effect text must assemble into a `Dig` → `Sacrifice` chain where
    /// the "if you do, you may put a creature card ... onto the battlefield"
    /// continuation PATCHES the `Dig` (not the intervening `Sacrifice`), and
    /// the trailing "put the rest on the bottom" clause binds to the same
    /// `Dig`. Before the issue #420 fix, clause 3 fell through to a stray
    /// `Effect::ChangeZone { target: ParentTarget }` and the `Dig` kept
    /// `destination: None`, routing the kept card to the hand.
    ///
    /// CR 608.2c: the controller follows the card's instructions in written
    /// order — later text ("if you do, ... onto the battlefield") modifies the
    /// earlier "look at the top seven cards" instruction.
    #[test]
    fn birthing_ritual_assembles_dig_battlefield_sacrifice_chain() {
        use super::super::parse_effect_chain;
        use crate::types::ability::{
            Comparator, FilterProp, ObjectScope, QuantityRef, TypeFilter, TypedFilter,
        };

        // Effect text of the triggered ability — everything after the
        // "At the beginning of your end step, if you control a creature, "
        // trigger/intervening-if prefix that `oracle_trigger` strips.
        let def = parse_effect_chain(
            "look at the top seven cards of your library. Then you may sacrifice a creature. \
             If you do, you may put a creature card with mana value X or less from among those \
             cards onto the battlefield, where X is 1 plus the sacrificed creature's mana value. \
             Put the rest on the bottom of your library in a random order.",
            AbilityKind::Spell,
        );

        // Collect the effect chain by walking `sub_ability`.
        let mut effects: Vec<&Effect> = Vec::new();
        let mut node = Some(&def);
        while let Some(d) = node {
            effects.push(&d.effect);
            node = d.sub_ability.as_deref();
        }

        // Clause 1 — the `Dig`, patched by the DigFromAmong continuation.
        let Effect::Dig {
            destination,
            keep_count,
            up_to,
            reveal,
            rest_destination,
            filter,
            ..
        } = effects[0]
        else {
            panic!("expected Dig as first effect, got {:?}", effects[0]);
        };
        assert_eq!(*destination, Some(Zone::Battlefield));
        assert_eq!(*keep_count, Some(1));
        assert!(*up_to, "\"you may put\" → up_to");
        // CR 701.20e + CR 400.2: "look at the top seven cards" is a private
        // *look* — looking shows a card only to the specified player, unlike a
        // public *reveal* (CR 701.20a). The library is a hidden zone (CR 400.2),
        // so the kept card is routed directly to the battlefield with `reveal`
        // false. (`reveal` is only promoted to true for the destination-None
        // reveal-only form, where downstream sub-abilities consume a public
        // tracked set.)
        assert!(!*reveal, "\"look at\" is private — not a reveal-form");
        assert_eq!(
            *rest_destination,
            Some(Zone::Library),
            "\"put the rest on the bottom\" binds to the Dig (random order preserved)"
        );
        // Creature + mana-value-relative-to-sacrificed-creature filter.
        let TargetFilter::Typed(TypedFilter {
            type_filters,
            properties,
            ..
        }) = filter
        else {
            panic!("expected Typed creature+cmc filter, got {filter:?}");
        };
        assert!(
            type_filters.contains(&TypeFilter::Creature),
            "filter restricts to creature cards, got {type_filters:?}"
        );
        assert!(
            properties.iter().any(|p| matches!(
                p,
                FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Offset { inner, offset: 1 },
                } if matches!(
                    **inner,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectManaValue {
                            scope: ObjectScope::CostPaidObject,
                        },
                    },
                ),
            )),
            "filter has Cmc <= (sacrificed creature MV + 1), got {properties:?}"
        );

        // A `Sacrifice` step is present.
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Sacrifice { .. })),
            "expected a Sacrifice step in the chain, got {effects:?}"
        );

        // No stray `ChangeZone { target: ParentTarget }` from clause 3.
        assert!(
            !effects.iter().any(|e| matches!(
                e,
                Effect::ChangeZone {
                    target: TargetFilter::ParentTarget,
                    ..
                }
            )),
            "clause 3 must patch the Dig, not fall through to ChangeZone{{ParentTarget}}"
        );

        // No Unimplemented fallbacks.
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::Unimplemented { .. })),
            "no clause should fall back to Unimplemented, got {effects:?}"
        );
    }

    #[test]
    fn choose_from_zone_partitions_chosen_and_rest_destinations() {
        let choose = Effect::ChooseFromZone {
            count: 2,
            zone: Zone::Exile,
            additional_zones: Vec::new(),
            zone_owner: crate::types::ability::ZoneOwner::Controller,
            filter: None,
            chooser: Chooser::Opponent,
            up_to: false,
            constraint: None,
        };
        let result = parse_followup_continuation_ast(
            "Put the chosen cards into your graveyard and the rest into your hand.",
            &choose,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::ChoicePartitionDestinations {
                chosen_destination: Zone::Graveyard,
                rest_destination: Zone::Hand,
            })
        );
    }

    #[test]
    fn choose_from_zone_partitions_shuffle_chosen_and_rest_destinations() {
        let choose = Effect::ChooseFromZone {
            count: 2,
            zone: Zone::Exile,
            additional_zones: Vec::new(),
            zone_owner: crate::types::ability::ZoneOwner::Controller,
            filter: None,
            chooser: Chooser::Opponent,
            up_to: false,
            constraint: None,
        };
        let result = parse_followup_continuation_ast(
            "Shuffle the chosen cards into your library and put the rest into your hand.",
            &choose,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::ChoicePartitionDestinations {
                chosen_destination: Zone::Library,
                rest_destination: Zone::Hand,
            })
        );
    }

    #[test]
    fn put_those_cards_on_top_parses_as_library_position_continuation() {
        let shuffle = Effect::Shuffle {
            target: TargetFilter::Controller,
        };
        let result = parse_followup_continuation_ast(
            "Put those cards on top in any order.",
            &shuffle,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::PutChosenCardsAtLibraryPosition {
                position: LibraryPosition::Top,
            })
        );
    }

    #[test]
    fn put_those_cards_on_top_owner_library_variant_parses() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put those cards on top of their owner's library in any order.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::PutChosenCardsAtLibraryPosition {
                position: LibraryPosition::Top,
            })
        );
    }

    /// CR 201.2 + CR 608.2c: Mitotic-Manipulation-style name-match selection
    /// after a Dig emits a `DigFromAmong` continuation that patches the
    /// preceding Dig with destination = Battlefield, keep_count = 1,
    /// up_to = true (the "may" / "if" optional selection), and a
    /// `NameMatchesAnyPermanent` filter.
    #[test]
    fn put_one_of_those_cards_onto_battlefield_if_same_name() {
        use crate::types::ability::{FilterProp, TypedFilter};
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "You may put one of those cards onto the battlefield if it has the same name as a permanent.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::DigFromAmong {
                count: 1,
                up_to: true,
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::NameMatchesAnyPermanent { controller: None },
                ])),
                destination: Some(Zone::Battlefield),
                rest_destination: None,
            })
        );
    }

    // --- Subject-prefixed "you [verb]" splitting ---

    #[test]
    fn bare_and_splits_discard_and_you_gain() {
        // Basilica Bell-Haunt pattern: "each opponent discards a card and you gain 3 life"
        let chunks = clause_texts("each opponent discards a card and you gain 3 life");
        assert_eq!(
            chunks,
            vec!["each opponent discards a card", "you gain 3 life"]
        );
    }

    #[test]
    fn bare_and_splits_lose_and_you_gain() {
        // Blood Artist drain pattern: "target opponent loses 1 life and you gain 1 life"
        let chunks = clause_texts("target opponent loses 1 life and you gain 1 life");
        assert_eq!(
            chunks,
            vec!["target opponent loses 1 life", "you gain 1 life"]
        );
    }

    #[test]
    fn bare_and_splits_you_draw_clause() {
        let chunks = clause_texts("destroy target creature and you draw a card");
        assert_eq!(chunks, vec!["destroy target creature", "you draw a card"]);
    }

    #[test]
    fn bare_and_splits_you_may_clause() {
        let chunks = clause_texts("exile target creature and you may draw a card");
        assert_eq!(chunks, vec!["exile target creature", "you may draw a card"]);
    }

    #[test]
    fn bare_and_splits_its_controller_clause() {
        let chunks = clause_texts("destroy target creature and its controller loses 3 life");
        assert_eq!(
            chunks,
            vec!["destroy target creature", "its controller loses 3 life"]
        );
    }

    // --- B11: Temporal prefix suppresses bare "and" splitting ---

    #[test]
    fn temporal_prefix_suppresses_bare_and_split() {
        // CR 603.7a: "at the beginning of your next upkeep, draw a card and gain 3 life"
        // must NOT split at "and" — the compound inner effect is a single delayed trigger.
        let chunks =
            clause_texts("at the beginning of your next upkeep, draw a card and gain 3 life");
        assert_eq!(
            chunks,
            vec!["at the beginning of your next upkeep, draw a card and gain 3 life"]
        );
    }

    #[test]
    fn temporal_prefix_end_step_suppresses_bare_and_split() {
        let chunks =
            clause_texts("at the beginning of the next end step, return it and lose 2 life");
        assert_eq!(
            chunks,
            vec!["at the beginning of the next end step, return it and lose 2 life"]
        );
    }

    // --- Token enters with counters continuation ---

    #[test]
    fn token_enters_with_x_counters_where_x_is() {
        let result = try_parse_token_enters_with_counters(
            "the token enters with x +1/+1 counters on it, where x is the number of other creatures you control",
        );
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        }) = result
        {
            assert_eq!(counter_type, CounterType::Plus1Plus1);
            // Should be an ObjectCount ref for "the number of other creatures you control"
            assert!(matches!(count, QuantityExpr::Ref { .. }));
        } else {
            panic!("expected TokenEntersWithCounters");
        }
    }

    #[test]
    fn token_enters_with_it_prefix() {
        let result = try_parse_token_enters_with_counters(
            "it enters with x +1/+1 counters on it, where x is the number of creatures you control",
        );
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters { counter_type, .. }) = result {
            assert_eq!(counter_type, CounterType::Plus1Plus1);
        }
    }

    #[test]
    fn token_enters_with_fixed_counters() {
        let result = try_parse_token_enters_with_counters(
            "the token enters with three +1/+1 counters on it",
        );
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        }) = result
        {
            assert_eq!(counter_type, CounterType::Plus1Plus1);
            assert_eq!(count, QuantityExpr::Fixed { value: 3 });
        }
    }

    #[test]
    fn token_enters_with_counters_no_match() {
        // Should not match non-counter enters-with text
        let result = try_parse_token_enters_with_counters("the token enters tapped and attacking");
        assert!(result.is_none());
    }

    // --- "and put N counter(s) on it" imperative followup form ---

    #[test]
    fn put_counters_on_it_followup_x_variable() {
        // G'raha Tia: "create a 1/1 ... token and put X +1/+1 counters on it"
        // After clause splitting, the followup clause is "put x +1/+1 counters on it".
        let result = try_parse_put_counters_on_token_followup("put x +1/+1 counters on it");
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        }) = result
        {
            assert_eq!(counter_type, CounterType::Plus1Plus1);
            // Bare X without "where X is" — resolved from parent payment at runtime.
            assert!(matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable { .. }
                }
            ));
        } else {
            panic!("expected TokenEntersWithCounters");
        }
    }

    #[test]
    fn put_counters_on_it_followup_fixed_word() {
        // Fractal Tender: "... and put three +1/+1 counters on it"
        let result = try_parse_put_counters_on_token_followup("put three +1/+1 counters on it");
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        }) = result
        {
            assert_eq!(counter_type, CounterType::Plus1Plus1);
            assert_eq!(count, QuantityExpr::Fixed { value: 3 });
        } else {
            panic!("expected TokenEntersWithCounters");
        }
    }

    #[test]
    fn put_counters_on_it_followup_singular_article() {
        // "and put a +1/+1 counter on it" — singular article form.
        let result = try_parse_put_counters_on_token_followup("put a +1/+1 counter on it");
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        }) = result
        {
            assert_eq!(counter_type, CounterType::Plus1Plus1);
            assert_eq!(count, QuantityExpr::Fixed { value: 1 });
        } else {
            panic!("expected TokenEntersWithCounters");
        }
    }

    #[test]
    fn put_counters_on_it_followup_where_x_is() {
        // Fractal Anomaly: "... put X +1/+1 counters on it, where X is the
        // number of cards you've drawn this turn"
        let result = try_parse_put_counters_on_token_followup(
            "put x +1/+1 counters on it, where x is the number of cards you've drawn this turn",
        );
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters { counter_type, .. }) = result {
            assert_eq!(counter_type, CounterType::Plus1Plus1);
        } else {
            panic!("expected TokenEntersWithCounters");
        }
    }

    #[test]
    fn put_counters_on_token_followup_equal_to_damage_dealt() {
        // Primo, the Unbounded: "Put a number of +1/+1 counters on it equal to
        // the damage dealt." After sentence splitting the continuation sees:
        // "put a number of +1/+1 counters on it equal to the damage dealt"
        let result = try_parse_put_counters_on_token_followup(
            "put a number of +1/+1 counters on it equal to the damage dealt",
        );
        let Some(ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        }) = result
        else {
            panic!("expected TokenEntersWithCounters, got {result:?}");
        };
        assert_eq!(counter_type, CounterType::Plus1Plus1);
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            },
            "count must be EventContextAmount (the damage dealt)"
        );
    }

    #[test]
    fn put_counters_on_it_followup_minus_counters() {
        // -1/-1 counter form (uncommon for tokens, but the helper supports it).
        let result = try_parse_put_counters_on_token_followup("put a -1/-1 counter on it");
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters { counter_type, .. }) = result {
            assert_eq!(counter_type, CounterType::Minus1Minus1);
        } else {
            panic!("expected TokenEntersWithCounters");
        }
    }

    #[test]
    fn put_counters_on_it_followup_rejects_named_target() {
        // Rat King, Verminister: "... and put a +1/+1 counter on Rat King"
        // — "on Rat King" is NOT "on it"; must NOT match (the named target is
        // SelfRef = source card, not the just-created token).
        let result = try_parse_put_counters_on_token_followup("put a +1/+1 counter on rat king");
        assert!(result.is_none());
    }

    #[test]
    fn put_counters_on_it_followup_rejects_non_put_verb() {
        // Other verbs that happen to mention counters must not match.
        let result = try_parse_put_counters_on_token_followup("remove a +1/+1 counter on it");
        assert!(result.is_none());
    }

    /// CR 122.1 + CR 614.1c + CR 607.2a: Oversimplify's third-person
    /// dynamic followup. "Each player creates ... and puts a number of +1/+1
    /// counters on it equal to the total power of creatures they controlled
    /// that were exiled this way." After clause splitting the followup is
    /// "puts a number of +1/+1 counters on it equal to <quantity>". Must
    /// lower to `TokenEntersWithCounters{Plus1Plus1, Aggregate{Sum,Power,
    /// And[Typed{Creature,ScopedPlayer}, ExiledBySource]}}`.
    ///
    /// This is the building-block test for the "puts" (third-person) verb
    /// axis AND the dynamic "a number of … equal to …" quantity axis. Any
    /// future card with the same shape lights up through these axes; the
    /// test deliberately exercises both new axes simultaneously to lock in
    /// the Oversimplify-class repair.
    #[test]
    fn put_counters_on_it_followup_third_person_dynamic_quantity() {
        use crate::types::ability::{
            AggregateFunction, ControllerRef, ObjectProperty, QuantityRef, TargetFilter,
            TypedFilter,
        };

        let result = try_parse_put_counters_on_token_followup(
            "puts a number of +1/+1 counters on it equal to the total power of creatures they controlled that were exiled this way",
        )
        .expect("third-person dynamic followup must parse");
        let ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        } = result
        else {
            panic!("expected TokenEntersWithCounters, got {result:?}");
        };
        assert_eq!(counter_type, CounterType::Plus1Plus1);
        let expected_qty = QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
                filter: TargetFilter::And {
                    filters: vec![
                        TargetFilter::Typed(
                            TypedFilter::creature().controller(ControllerRef::ScopedPlayer),
                        ),
                        TargetFilter::ExiledBySource,
                    ],
                },
            },
        };
        assert_eq!(count, expected_qty);
    }

    /// CR 122.1 + CR 614.1c: The same dynamic-counter combinator also accepts
    /// the imperative "put a number of ..." form (no third-person -s). This
    /// guards the verb-axis `alt()` against future regressions.
    #[test]
    fn put_counters_on_it_followup_imperative_dynamic_quantity() {
        let result = try_parse_put_counters_on_token_followup(
            "put a number of +1/+1 counters on it equal to the number of creatures you control",
        )
        .expect("imperative dynamic followup must parse");
        let ContinuationAst::TokenEntersWithCounters { counter_type, .. } = result else {
            panic!("expected TokenEntersWithCounters, got {result:?}");
        };
        assert_eq!(counter_type, CounterType::Plus1Plus1);
    }

    #[test]
    fn bare_and_clause_starts_on_self_reference_continuous_subjects() {
        assert!(starts_bare_and_clause(
            "this creature gets +2/+0 until end of turn"
        ));
        assert!(starts_bare_and_clause("~ gets +2/+0 until end of turn"));
    }

    /// CR 608.2c: Anaphoric back-reference conjuncts. Nalia de'Arnise's third
    /// ability is the canonical exemplar — "put a +1/+1 counter on each
    /// creature you control and those creatures gain deathtouch until end of
    /// turn". Each plural / singular subject pair must split so the conjunct
    /// reaches the subject-predicate parser instead of falling through to the
    /// imperative-only path that produces `Effect::Unimplemented { name:
    /// "those", ... }`.
    #[test]
    fn bare_and_clause_starts_on_anaphoric_continuous_subjects() {
        assert!(starts_bare_and_clause(
            "those creatures gain deathtouch until end of turn"
        ));
        assert!(starts_bare_and_clause(
            "those creatures get +1/+1 until end of turn"
        ));
        assert!(starts_bare_and_clause("those creatures have flying"));
        assert!(starts_bare_and_clause("those creatures lose flying"));
        assert!(starts_bare_and_clause("those permanents gain hexproof"));
        assert!(starts_bare_and_clause(
            "that creature gains haste until end of turn"
        ));
        assert!(starts_bare_and_clause(
            "that creature gets +2/+2 until end of turn"
        ));
        assert!(starts_bare_and_clause("that creature has lifelink"));
        assert!(starts_bare_and_clause("that creature loses flying"));
        assert!(starts_bare_and_clause(
            "that permanent gains indestructible"
        ));
        // Token anaphors — "create N tokens. Those tokens gain haste."
        assert!(starts_bare_and_clause("those tokens gain haste"));
        assert!(starts_bare_and_clause(
            "those tokens get +1/+1 until end of turn"
        ));
        assert!(starts_bare_and_clause("those tokens have flying"));
        assert!(starts_bare_and_clause("those tokens lose flying"));
        assert!(starts_bare_and_clause("that token gains haste"));
        assert!(starts_bare_and_clause(
            "that token gets +1/+0 until end of turn"
        ));
        assert!(starts_bare_and_clause("that token has lifelink"));
        assert!(starts_bare_and_clause("that token loses flying"));
    }

    /// CR 702: "The same is true for <keyword list>." — Odric, Lunarch
    /// Marshal's exact 12-keyword continuation sentence parses into the full
    /// keyword list (Oxford-comma form with a trailing "and").
    #[test]
    fn same_is_true_continuation_parses_full_keyword_list() {
        let keywords = try_parse_same_is_true_continuation(
            "The same is true for flying, deathtouch, double strike, haste, hexproof, \
             indestructible, lifelink, menace, reach, skulk, trample, and vigilance.",
        )
        .expect("Odric continuation must parse");
        assert_eq!(keywords.len(), 12);
        assert_eq!(keywords[0], Keyword::Flying);
        assert_eq!(keywords[2], Keyword::DoubleStrike);
        assert_eq!(keywords[9], Keyword::Skulk);
        assert_eq!(keywords[11], Keyword::Vigilance);
    }

    /// A two-keyword "X and Y" form (no comma) parses both.
    #[test]
    fn same_is_true_continuation_parses_two_keyword_and_form() {
        let keywords =
            try_parse_same_is_true_continuation("The same is true for flying and trample.")
                .expect("two-keyword form must parse");
        assert_eq!(keywords, vec![Keyword::Flying, Keyword::Trample]);
    }

    /// A sentence that is not a "the same is true for" clause must not match.
    #[test]
    fn same_is_true_continuation_rejects_unrelated_sentence() {
        assert!(try_parse_same_is_true_continuation("Draw a card.").is_none());
    }

    /// A trailing non-keyword tail aborts the match — the clause must be a
    /// pure keyword list.
    #[test]
    fn same_is_true_continuation_rejects_trailing_non_keyword() {
        assert!(try_parse_same_is_true_continuation(
            "The same is true for flying when you attack."
        )
        .is_none());
    }

    // --- combat_requirement_conjunct_prepend: anchor-start arm (CR 508.1d / 509.1c) ---

    /// A chunk that BEGINS with the gain/get verb at offset 0 (subject already
    /// lifted by the enclosing compound-subject distribution) yields an empty
    /// prepend so the trailing combat-requirement conjunct still splits.
    #[test]
    fn combat_requirement_anchor_start_gain_haste() {
        assert_eq!(
            combat_requirement_conjunct_prepend(
                "gain haste until end of turn",
                "attack this turn if able"
            ),
            Some(String::new())
        );
    }

    /// Anchor-start with a "gains"-conjugated verb and a "must be blocked"
    /// standalone combat requirement remainder.
    #[test]
    fn combat_requirement_anchor_start_gains_must_be_blocked() {
        assert_eq!(
            combat_requirement_conjunct_prepend(
                "gains lifelink until end of turn",
                "must be blocked this turn"
            ),
            Some(String::new())
        );
    }

    /// Regression: a leading-space subject case still returns a non-empty
    /// prepend (targeted subject → anaphor "it ").
    #[test]
    fn combat_requirement_leading_space_subject_unchanged() {
        assert_eq!(
            combat_requirement_conjunct_prepend(
                "target creature gets +1/+1",
                "attacks this turn if able"
            ),
            Some("it ".to_string())
        );
    }

    /// Negative: a non-gain/get verb head with a combat-requirement remainder
    /// does not match (no continuous predicate to anchor on).
    #[test]
    fn combat_requirement_anchor_start_rejects_non_pump_verb() {
        assert_eq!(
            combat_requirement_conjunct_prepend("draw a card", "attack this turn if able"),
            None
        );
    }

    // --- split_clause_sequence: compound-subject-each sticky suppression (CR 109.5 / 115.1) ---

    /// The object-axis compound subject "~ and that creature each <body>" stays
    /// as a SINGLE chunk even though the body has internal " and "s.
    #[test]
    fn compound_subject_each_object_axis_one_chunk() {
        let input = "~ and that creature each get +2/+0 and gain haste until end of turn and attack this turn if able";
        let chunks = clause_texts(input);
        assert_eq!(chunks.len(), 1, "expected one chunk, got {chunks:?}");
        assert_eq!(chunks[0], input);
    }

    /// Negative control: a normal "you <verb> ... and <verb> ..." compound where
    /// the remainder does NOT start with a "<noun> each " distribution head must
    /// still split at the bare " and " — the sticky suppression must not engage.
    #[test]
    fn compound_subject_each_negative_control_splits() {
        let chunks = clause_texts("you draw a card and create a Treasure token");
        assert!(
            chunks.len() > 1,
            "expected a split (sticky must not engage), got single chunk: {chunks:?}"
        );
    }

    /// CR 603.12 + CR 115.1: an object-axis compound subject introduced by an
    /// "if you do," reflexive connector still stays one chunk — the
    /// sticky-detection strips the leading connector before the subject test.
    #[test]
    fn compound_subject_each_reflexive_connector_object_axis_one_chunk() {
        let input = "if you do, ~ and that creature each get +2/+0 and gain haste until end of turn and attack this turn if able";
        let chunks = clause_texts(input);
        assert_eq!(chunks.len(), 1, "expected one chunk, got {chunks:?}");
        assert_eq!(chunks[0], input);
    }

    /// CR 603.12 + CR 109.5: a player-axis compound subject introduced by a
    /// "when you do," connector stays one chunk despite the internal " and ".
    #[test]
    fn compound_subject_each_reflexive_connector_player_axis_one_chunk() {
        let input = "when you do, you and that player each draw a card and lose 1 life";
        let chunks = clause_texts(input);
        assert_eq!(chunks.len(), 1, "expected one chunk, got {chunks:?}");
        assert_eq!(chunks[0], input);
    }

    /// Negative: a reflexive "if you do," frame whose body is NOT a compound
    /// subject distribution still splits at the bare " and " — the connector
    /// strip must not spuriously engage the sticky suppression.
    #[test]
    fn compound_subject_each_reflexive_connector_negative_splits() {
        let chunks = clause_texts("if you do, draw a card and create a Treasure token");
        assert!(
            chunks.len() > 1,
            "expected a split (sticky must not engage), got single chunk: {chunks:?}"
        );
    }
}
