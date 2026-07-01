use crate::parser::oracle_nom::error::{OracleError, OracleResult};
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
use crate::parser::oracle_ir::effect_chain::DoesTheSameSubject;
use crate::parser::oracle_quantity::{parse_cda_quantity, parse_quantity_ref};
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, CastingPermission, Chooser,
    ContinuousModification, ControllerRef, CopyRetargetPermission, CounterSourceRider, DigSource,
    Duration, Effect, EffectScope, FaceDownBody, FaceDownProfile, LibraryPosition, MultiTargetSpec,
    PermissionGrantee, PtValue, QuantityExpr, QuantityRef, RevealUntilDisposition,
    SpellStackToGraveyardReplacement, StaticDefinition, TargetChoiceTiming, TargetFilter,
    TypeFilter, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::statics::StaticMode;
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
            alt((
                tag_no_case::<_, _, OracleError<'_>>("have"),
                tag_no_case("has"),
            )),
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

/// Whole-line dig continuation "put the rest on the bottom of your library
/// [in a random order | in any order]" following a `ChooseFromZone`.
///
/// CR 401.4: multi-card library placement defaults to owner-arranged order,
/// so "in any order" restates the default. The random-order variant is
/// currently collapsed into the same continuation (randomization is not yet
/// modeled at this seam) — the suffix axis is one `opt(alt(...))`, extended
/// there when it is.
fn parse_put_rest_on_bottom_line(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        all_consuming((
            tag("put the rest on the bottom of your library"),
            opt(alt((tag(" in a random order"), tag(" in any order")))),
        )),
    )
    .parse(input)
}

/// Whole-line dig continuation matcher — delegates to [`parse_put_rest_on_bottom_line`]
/// so continuation detection cannot drift from the combinator grammar.
fn matches_put_rest_on_bottom_line(lower: &str) -> bool {
    parse_put_rest_on_bottom_line(lower.trim()).is_ok()
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

fn append_definition_to_sub_chain(ability: &mut AbilityDefinition, mut next: AbilityDefinition) {
    let mut cursor = ability;
    loop {
        if cursor.sub_ability.is_none() {
            if cursor.optional
                && super::lower::is_linked_exile_cast_bottom_cleanup(&cursor.effect, &next.effect)
            {
                super::lower::normalize_linked_exile_cast_bottom_cleanup(&mut next.effect);
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

/// CR 701.20b + CR 608.2c: When a `RevealUntil` chains into library-scoped
/// `ForEachCategoryExile` (Sanar's Vivid), the reveal step must not route hits
/// to hand — the per-color exile reads from among cards still in the library.
pub(super) fn patch_reveal_until_for_library_category_exile(def: &mut AbilityDefinition) {
    if let Some(sub) = def.sub_ability.as_mut() {
        patch_reveal_until_for_library_category_exile(sub);
        if let (
            Effect::RevealUntil {
                matched_disposition,
                kept_destination,
                rest_destination,
                ..
            },
            Effect::ForEachCategoryExile {
                zone: Zone::Library,
                ..
            },
        ) = (&mut *def.effect, &*sub.effect)
        {
            *matched_disposition = RevealUntilDisposition::RevealOnly;
            *kept_destination = Zone::Library;
            *rest_destination = Zone::Library;
        }
    }
}

fn deepest_effect(ability: &AbilityDefinition) -> &Effect {
    let mut cursor = ability;
    while let Some(sub) = cursor.sub_ability.as_deref() {
        cursor = sub;
    }
    &cursor.effect
}

/// CR 608.2c: Resolve the grant target for an exile-zone cast-permission
/// designation continuation ("it becomes plotted" / "it becomes foretold").
/// Generic over the designation — a tracked-set exile binds the grant to the
/// just-published set (`TrackedSetId(0)` sentinel); a single-object exile binds
/// to the parent target (which falls back to the ability source for anaphoric
/// "it" with no explicit target slot).
fn exile_designation_grant_target(previous: &AbilityDefinition) -> TargetFilter {
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

/// CR 205.1a + CR 613.1d: the "become(s) " animation/type-change verb in both
/// conjugations (conjugated "becomes " and imperative "become "). Single source
/// of truth for the bare-become conjunct split in `split_clause_sequence` — used
/// both for the remainder peek and the word-boundary antecedent scan so the two
/// conjugations are never enumerated in two places.
fn parse_become_verb(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((tag::<_, _, OracleError<'_>>("becomes "), tag("become "))),
    )
    .parse(input)
}

fn parse_becomes_plotted_continuation(lower: &str) -> bool {
    // allow-noncombinator: punctuation cleanup before all_consuming
    let text = lower.trim().trim_end_matches('.').trim();
    // CR 702.170c-d: Accept an optional "if you do," gate. The Plot-grant
    // cards read "You may exile a card. If you do, it becomes plotted." The
    // continuation already attaches after the optional exile instruction, so
    // the prefix is part of the same plotted-card continuation grammar.
    all_consuming((
        opt(alt((
            tag::<_, _, OracleError<'_>>("if you do, "),
            tag::<_, _, OracleError<'_>>("if you do "),
        ))),
        alt((
            value((), tag::<_, _, OracleError<'_>>("it becomes plotted")),
            value((), tag("that card becomes plotted")),
            value((), tag("they become plotted")),
        )),
    ))
    .parse(text)
    .is_ok()
}

fn parse_becomes_foretold_continuation(lower: &str) -> bool {
    // allow-noncombinator: punctuation cleanup before all_consuming
    let text = lower.trim().trim_end_matches('.').trim();
    // CR 702.143d: Accept an optional "if you do," gate for symmetry with the
    // plotted grammar (a future "you may exile a card. If you do, it becomes
    // foretold." card slots in here). The continuation already attaches after
    // the preceding exile instruction, so the gate prefix is part of the same
    // become-foretold continuation grammar.
    all_consuming((
        opt(alt((
            tag::<_, _, OracleError<'_>>("if you do, "),
            tag::<_, _, OracleError<'_>>("if you do "),
        ))),
        alt((
            value((), tag::<_, _, OracleError<'_>>("it becomes foretold")),
            value((), tag("that card becomes foretold")),
            value((), tag("they become foretold")),
        )),
    ))
    .parse(text)
    .is_ok()
}

/// Detects "its foretell cost is <cost specification>" — an effect-defined
/// foretell cost sentence (e.g. Ethereal Valkyrie: "Its foretell cost is its
/// mana cost reduced by {2}."). When this sentence follows a
/// `BecomesForetold` continuation, the cost-bearing form is not yet
/// representable in `CastingPermission::Foretold`; the grant must be
/// suppressed to avoid emitting a wrong zero-cost permission
/// (CR 702.143d: effect-defined foretell costs are a separate class from
/// printed `Keyword::Foretell` costs).
pub(super) fn is_foretell_cost_override_sentence(lower: &str) -> bool {
    // allow-noncombinator: punctuation cleanup before prefix check
    let text = lower.trim().trim_end_matches('.').trim();
    tag::<_, _, OracleError<'_>>("its foretell cost is ")
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

/// CR 702.75a + CR 406.3: Recognize "exile one of them face down" — a player
/// CHOICE of one card from among the cards a preceding private `Dig` looked at
/// (the Gonti, Lord of Luxury class: "look at the top four cards of an
/// opponent's library, exile one of them face down ..."). Distinct from
/// `parse_exile_looked_at_card` ("exile it/them ...", the Gonti, Canny
/// Acquisitor wholesale impulse idiom): "one of them" means the controller
/// selects exactly one of the N looked-at cards. The "face down" suffix is the
/// CR 406.3 hidden-information marker required by this class; pure-peek Digs
/// (Delver of Secrets — no exile clause) never reach this recognizer.
fn parse_exile_one_of_them_face_down(lower: &str) -> bool {
    let trimmed = lower.trim().trim_end_matches('.').trim_end();
    (
        tag::<_, _, OracleError<'_>>("exile "),
        alt((tag("one of them"), tag("one of those cards"))),
        multispace1,
        tag("face down"),
        eof,
    )
        .parse(trimmed)
        .is_ok()
}

pub(super) fn split_clause_sequence(text: &str) -> Vec<ClauseChunk> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    let mut paren_depth = 0usize;
    let mut in_single_quote = false;
    // CR 111.3 (issue #4605): a created token has the abilities defined in its
    // creation effect. Tracks whether the currently-open single-quoted span is a
    // genuine NESTED ability quote — the inner ability of a token/permanent
    // created inside a double-quoted granted ability (Urza's Saga:
    // `... gains "... create a token with 'X.'"`). Only an anchored opener
    // (`with '` / `and '` / `or '` / `, '`) opens such a span; a possessive or
    // contraction apostrophe (`~'s`, `can't`) does not. Inside a genuine ability
    // quote a `.` is part of the quoted text, NOT a sentence boundary.
    let mut single_quote_is_ability = false;
    let mut in_double_quote = false;
    // CR 109.5 + CR 115.1: once a compound-subject-each head ("you and" / "~ and"
    // followed by "<noun> each") is detected, keep the WHOLE distributed body
    // intact for the rest of the current chunk — its internal " and "s
    // ("get +2/+0 and gain haste ... and attack this turn if able") are body
    // delimiters owned by `try_parse_compound_subject_each`, not clause splits.
    let mut compound_subject_each_sticky = false;
    // CR 701.55a + CR 701.55d: once a villainous-choice head ("[subject] face(s)
    // a villainous choice — ") is detected, keep the WHOLE choice block intact
    // for the rest of the current sentence. Everything after the em-dash is one
    // indivisible instruction whose internal "," / " then " / " and " / " or "
    // are branch delimiters owned by `try_parse_choose_one_of_inline`, not clause
    // splits. Without this latch the chunker bisects a branch body (e.g. Ensnared
    // by the Mara: "... until they exile a nonland card, then you may cast that
    // card ...") and the lead-in is severed into a failing `Unimplemented{face}`.
    let mut villainous_choice_sticky = false;

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
                    if in_single_quote {
                        in_single_quote = false;
                        single_quote_is_ability = false;
                    } else {
                        in_single_quote = true;
                        // Anchor mirrors `extract_token_static_abilities`'s
                        // single-quote pass: only `with '` / `and '` / `or '` /
                        // `, '` opens a genuine nested ability span.
                        single_quote_is_ability = ends_with_single_quote_ability_anchor(&current);
                    }
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
            ',' if paren_depth == 0
                && !in_single_quote
                && !in_double_quote
                // CR 701.55a: inside a latched villainous-choice block, "," is a
                // branch delimiter (", then" / ", or"), not a clause boundary.
                && villainous_choice_sticky =>
            {
                current.push(ch);
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
            // A sentence-ending period closes the clause (text-structure rule;
            // no governing CR). Real quoted ability text in Oracle text always
            // uses DOUBLE quotes, so a dangling single-quote here can only have
            // been opened by a possessive/contraction apostrophe (e.g. "~'s
            // power gains skulk until end of turn. Goad it." — The Master,
            // Mesmerist). Such a "quote" must not swallow a sentence boundary:
            // split here and reset the phantom single-quote state.
            // `in_double_quote` (a genuine quoted ability) still suppresses the
            // split.
            // CR 111.3 (issue #4605): inside a genuine nested ability quote
            // (`single_quote_is_ability`) the `.` belongs to the quoted text
            // (`... with 'This token gets +1/+1 for each artifact you control.'`)
            // and must NOT close the clause — otherwise the span is bisected and
            // the token-creation effect loses its inner ability (and dispatches
            // to the inner `Pump` instead of `Token`).
            '.' if paren_depth == 0
                && !in_double_quote
                && !(in_single_quote && single_quote_is_ability) =>
            {
                in_single_quote = false;
                single_quote_is_ability = false;
                push_clause_chunk(&mut chunks, &current, Some(ClauseBoundary::Sentence));
                current.clear();
                compound_subject_each_sticky = false;
                // CR 701.55a: a true sentence boundary ends the choice block.
                villainous_choice_sticky = false;
                while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
                    chars.next();
                }
            }
            _ => {
                current.push(ch);
                // CR 701.55a: latch the villainous-choice block once the chunk
                // accumulated so far contains the choice opener "a villainous
                // choice — " (em-dash + trailing space). Anchored on the full
                // opener so it covers "face"/"faces" and any chooser prefix
                // ("that creature's controller faces a villainous choice — ").
                if !villainous_choice_sticky
                    && nom_primitives::scan_contains(
                        &current.to_ascii_lowercase(),
                        "a villainous choice \u{2014} ",
                    )
                {
                    villainous_choice_sticky = true;
                }
                // Detect bare " and " at word boundary followed by an imperative verb.
                // Handles patterns like "you lose 1 life and create a Treasure token".
                // Uses a restricted verb list to avoid false positives on noun phrases
                // like "target creature and all other creatures" or "it and each other".
                if paren_depth == 0 && !in_double_quote && current_ends_with_bare_and(&current) {
                    let remainder: String = chars.clone().collect();
                    let remainder_trimmed = remainder.trim_start();
                    // CR 608.2c: Card-name possessives like "~'s controller …" open
                    // quote mode on the apostrophe; suppress bare-and splitting inside
                    // that quote so "~'s controller sacrifices it and draws a card"
                    // stays one clause (Hold for Ransom). Zack Fair's counter-move +
                    // attach tail is the sole exception: split at " and attach an
                    // Equipment that was attached …" even inside quote mode.
                    let allow_single_quote_attach_split = in_single_quote
                        && starts_attach_equipment_was_attached_clause(remainder_trimmed);
                    if !in_single_quote || allow_single_quote_attach_split {
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
                        // "with that name" or "with the same name as that {card,creature,…}"
                        // suffixes (Eradicate / Counterbore / Surgical Extraction class).
                        let has_search_prefix =
                            nom_primitives::scan_contains(&before_lower, "search ");
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
                            && remainder_trimmed_starts_with_compound_subject_each(
                                remainder_trimmed,
                            );
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
                        let roll_die_modifier_continuation =
                            ends_with_roll_die_phrase(&before_lower)
                                && alt((tag::<_, _, OracleError<'_>>("add "), tag("subtract ")))
                                    .parse(remainder_trimmed)
                                    .is_ok();
                        // CR 705 + CR 707.10c: Comma splitting already keeps `if …`
                        // prefix clauses intact (see `starts_prefix_clause` in
                        // `split_comma_clause_boundary`), but a blocked comma leaves
                        // `, and ` in the buffer — which then hits this bare-`and`
                        // path and bisects the body anyway. Krark, the Thumbless:
                        // "If you win the flip, copy that spell, and you may choose
                        // new targets for the copy" must reach coin-flip branch
                        // parsing as one chunk so the CopyMayRetarget continuation
                        // absorbs the retarget grant. Only suppress when the ` and `
                        // immediately follows a comma inside a prefix clause — bare
                        // ` and ` without a comma (Chain cycle, many copies) must still
                        // split so the retarget grant reaches followup absorption.
                        let trimmed_before = before_lower.trim_end();
                        let inside_prefix_comma_and_continuation = trimmed_before.ends_with(',')
                            && starts_prefix_clause(
                                trimmed_before.trim_end_matches(',').trim_end(),
                            );
                        // CR 205.1a + CR 613.1d: a bare "becomes <descriptor>" conjunct
                        // splits off as its OWN become clause only when the antecedent is
                        // ALSO a become predicate (a compound-become like Alacrian Armory's
                        // "becomes saddled … and becomes an artifact creature …"). When the
                        // antecedent is a continuous-modification predicate ("This creature
                        // gets +3/+3 and becomes a Bear Berserker until end of turn"), the
                        // "becomes" is a TYPE-CHANGE modifier on the same continuous effect
                        // (CR 613.1d Layer 4), absorbed by `parse_continuous_modifications` —
                        // not a separate clause. Suppress the bare-become split in that case
                        // so the single GenericEffect carries both the pump and the subtype
                        // change. The antecedent is a become predicate iff its text already
                        // contains a "become(s) " verb. A single `parse_become_verb`
                        // combinator (`alt` of the two conjugations) is the source of truth
                        // for both the remainder peek and the word-boundary antecedent scan,
                        // so the two conjugations are never enumerated twice.
                        let bare_becomes_remainder = parse_become_verb(remainder_trimmed).is_ok();
                        let antecedent_is_become = nom_primitives::scan_at_word_boundaries(
                            &before_lower,
                            parse_become_verb,
                        )
                        .is_some();
                        let bare_becomes_continuation =
                            bare_becomes_remainder && !antecedent_is_become;
                        // CR 205.2a + CR 205.3a + CR 608.2c: "exile all A, all B,
                        // and all C" — a bare "and" before a "[all|each] <type>"
                        // union leg is a union delimiter, not a clause boundary
                        // (Everything Comes to Dust; covers the non-"except" union
                        // form too). The legs span both card types (205.2a:
                        // creature/artifact/enchantment) and subtypes (205.3a).
                        let mass_exile_union_continuation = continues_mass_exile_union(
                            &before_lower,
                            &remainder_trimmed.to_ascii_lowercase(),
                        );
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
                        || villainous_choice_sticky // CR 701.55a: keep the whole villainous-choice block intact
                        || inside_otherwise_body
                        || have_base_pt_continuation
                        || continuous_modifier_conjunct
                        || roll_die_modifier_continuation
                        || bare_becomes_continuation
                        || mass_exile_union_continuation
                        || inside_prefix_comma_and_continuation;
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
                                    .or_else(|| {
                                        exile_conjunct_prepend(&before_lower, remainder_trimmed)
                                    })
                                    .or_else(|| {
                                        untap_restriction_conjunct_prepend(
                                            before_and,
                                            remainder_trimmed,
                                        )
                                    })
                            {
                                push_clause_chunk(
                                    &mut chunks,
                                    before_and,
                                    Some(ClauseBoundary::Comma),
                                );
                                current.clear();
                                compound_subject_each_sticky = false;
                                current.push_str(&prepend);
                            }
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
    if alt((
        tag::<_, _, OracleError<'_>>("then, if "),
        tag("then if "),
        tag("then "),
        tag("if "),
        tag("otherwise"),
    ))
    .parse(trimmed_lower.as_str())
    .is_ok()
    {
        return true;
    }
    // CR 608.2c: read the whole text and apply the rules of English — a
    // granted-ability quote that ends a sentence can be followed by a fresh
    // causative "may have …" sentence directed at the affected object's
    // controller ("…life." That creature's controller may have this artifact
    // deal 1 damage to it." — Requiem Monolith). Split only on that narrow
    // causative form; arbitrary capitalized continuations ("The token is
    // goaded", "It becomes a 2/2 …") must stay attached to the quote.
    nom_primitives::scan_at_word_boundaries(trimmed_lower.as_str(), |i| {
        tag::<_, _, OracleError<'_>>("may have ").parse(i)
    })
    .is_some()
}

fn parse_search_exile_name_suffix(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    let (rest, _) = take_until::<_, _, OracleError<'_>>("with ").parse(input)?;
    let (rest, _) = alt((
        value((), tag::<_, _, OracleError<'_>>("with that name")),
        value((), tag("with the chosen name")),
        value(
            (),
            (
                tag("with the same name as that "),
                alt((
                    tag("creature"),
                    tag("permanent"),
                    tag("planeswalker"),
                    tag("artifact"),
                    tag("enchantment"),
                    tag("land"),
                    tag("spell"),
                    tag("card"),
                )),
            ),
        ),
    ))
    .parse(rest)?;
    let (rest, _) = eof.parse(rest)?;
    Ok((rest, ()))
}

/// CR 205.2a + CR 205.3a + CR 608.2c: true when the clause-so-far is an "exile
/// all/each …" mass effect and `next_lower` (the text right after a comma or a
/// bare "and") is another "[all|each] <type>" union leg — where `<type>` spans
/// both card types (205.2a) and subtypes (205.3a). Such a separator is a within-effect
/// union delimiter — not a clause boundary — so the whole multi-type union
/// reaches `parse_exile_ast` (→ `parse_mass_type_union`) as one chunk rather than
/// fragmenting "all artifacts" / "all enchantments" into verb-less sub_abilities
/// (Everything Comes to Dust). Combinator-only; no string-method dispatch.
fn continues_mass_exile_union(current_lower: &str, next_lower: &str) -> bool {
    type E<'a> = OracleError<'a>;
    if alt((tag::<_, _, E>("exile all "), tag("exile each ")))
        .parse(current_lower.trim_start())
        .is_err()
    {
        return false;
    }
    // The comma path leaves a leading conjunction ("and …" / "or …") on `next`.
    let seg = alt((
        tag::<_, _, E>("and/or "),
        tag::<_, _, E>("and "),
        tag::<_, _, E>("or "),
    ))
    .parse(next_lower.trim_start())
    .map(|(rest, _)| rest)
    .unwrap_or_else(|_| next_lower.trim_start())
    .trim_start();
    match alt((tag::<_, _, E>("all "), tag::<_, _, E>("each "))).parse(seg) {
        Ok((rest, _)) => crate::parser::oracle_target::starts_with_type_word(rest.trim_start()),
        Err(_) => false,
    }
}

fn split_comma_clause_boundary(current: &str, remainder: &str) -> Option<(ClauseBoundary, usize)> {
    let current_lower = current.trim().to_ascii_lowercase();
    let trimmed = remainder.trim_start();
    let whitespace_len = remainder.len() - trimmed.len();
    let trimmed_lower = trimmed.to_ascii_lowercase();

    if starts_prefix_clause(&current_lower) {
        return None;
    }

    // CR 205.2a + CR 205.3a + CR 608.2c: keep an "exile all/each A, all B, and
    // all C" union intact — a comma before a "[all|each] <type>" continuation is
    // a union delimiter, not a clause boundary (Everything Comes to Dust). Legs
    // span card types (205.2a) and subtypes (205.3a).
    if continues_mass_exile_union(&current_lower, &trimmed_lower) {
        return None;
    }

    // CR 707.9a: ", except <body>, [and] <body> [, …]" — inside a copy-effect
    // (BecomeCopy / CopyTokenOf) except clause, a comma (with or without a
    // following "and") between recognised body shapes is an internal delimiter,
    // not a clause boundary. Suppress the split so the whole except body reaches
    // the shared `become_copy_except` parser. Without this, a trailing keyword
    // body like ", and has haste" (The Apprentice's Folly: "create a token
    // that's a copy of it, except it isn't legendary, is a Reflection in
    // addition to its other types, and has haste") is bisected at the comma —
    // "has haste" deconjugates to the clause verb "have" — and orphaned as an
    // Unimplemented sub_ability instead of becoming an `AddKeyword` modification.
    // Mirrors the `inside_except_clause` guard on the bare-`and` chunk path.
    //
    // `scan_contains` matches at word boundaries, so probing for the bare word
    // "except " (a leading comma never sits at a word start) detects the clause
    // regardless of the leading "[,] " before "except".
    if nom_primitives::scan_contains(&current_lower, "except ")
        && starts_except_body_continuation(trimmed_lower.as_str())
    {
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
        // CR 701.20e + CR 608.2c: "reveal up to N <filter> from among them, then
        // put that card <kept-destination> ..." is a single from-among selection
        // action — the ", then put that card …" placement tail is the KEPT-card
        // destination of the same `DigFromAmong` continuation, not a standalone
        // clause. `parse_dig_from_among` → `parse_dig_destination_tail` consumes
        // this tail to patch the preceding `Dig`'s destination, so it must stay
        // one chunk. Without this, the body is bisected and the look-`Dig` is
        // never promoted to a selecting dig (Fertile Thicket: no DigChoice).
        //
        // Scoped to a KEPT-card anaphor ("that card", "those cards", "it",
        // "them", "the card") — the exact set `parse_dig_destination_tail`
        // strips. A "then put THE REST <destination>" tail (Zimone's Experiment:
        // "reveal up to two ... from among them, then put the rest on the
        // bottom") is the REST placement, which must still split off into its own
        // `PutRest` continuation to set `rest_destination`; suppressing that
        // split would drop the "rest on the bottom" routing. Unrelated trailers
        // ("…, then shuffle") also still split.
        let put_kept_card_tail = preceded(
            tag::<_, _, OracleError<'_>>("put "),
            alt((
                tag("that card "),
                tag("those cards "),
                tag("the card "),
                tag("it "),
                tag("them "),
            )),
        )
        .parse(after_then_lower)
        .is_ok();
        if nom_primitives::scan_contains(&current_lower, "from among") && put_kept_card_tail {
            return None;
        }
        // CR 701.55a + CR 608.2c: "[subject] does X, then faces a villainous
        // choice — …" continues a previously-named subject (the target's owner)
        // into a villainous-choice clause. The bare "faces a villainous choice"
        // verb is not in the imperative-verb table that
        // `starts_clause_text_or_conjugated` checks, so without this guard the
        // whole tail is silently dropped (This Is How It Ends). Recognize it as
        // a `Then` boundary so the continuation reaches the ChooseOneOf parser.
        if tag::<_, _, OracleError<'_>>("faces a villainous choice")
            .parse(after_then_lower)
            .is_ok()
        {
            return Some((ClauseBoundary::Then, whitespace_len + "then ".len()));
        }
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

    // CR 613.4b + CR 611.2a: the no-"and" sibling form ", has base power and
    // toughness N/N" (e.g. the middle conjunct of "becomes a Spirit, has base
    // power and toughness 1/1, and gains ...") is a layer-7b continuous
    // modification, not an independent clause — keep it attached so
    // parse_continuous_modifications emits SetPower/SetToughness rather than
    // orphaning it as Unimplemented. Mirrors the bare-"and" guard below.
    if starts_have_base_power_toughness(trimmed) {
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
        // CR 613.1d + CR 613.4b: comma-list continuous modifiers such as
        // "loses all abilities, becomes a Coward ..., and has base power and
        // toughness 1/1" keep the base-P/T conjunct attached to the same
        // subject. It is a layer-7b modification, not an independent clause.
        if starts_have_base_power_toughness(after_and) {
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

/// CR 707.9a: True when `trimmed_lower` (post-comma text) begins with a
/// recognised "except ..." body continuation. Only meaningful when the
/// chunk-so-far is already inside a copy-effect except clause (the caller gates
/// on that). The leading "and " connector is optional — "..., and has haste"
/// and "..., is a Reflection" are both internal except-body continuations.
///
/// The recognised heads mirror the body shapes in
/// `become_copy_except::parse_except_body` (keyword grants, type additions,
/// supertype removal, "has this ability", possessive name/loyalty overrides) so
/// this gate stays in lockstep with what that parser actually consumes.
fn starts_except_body_continuation(trimmed_lower: &str) -> bool {
    let body = tag::<_, _, OracleError<'_>>("and ")
        .parse(trimmed_lower)
        .map(|(rest, _)| rest)
        .unwrap_or(trimmed_lower);
    alt((
        value((), tag::<_, _, OracleError<'_>>("has ")),
        value((), tag("have ")),
        value((), tag("it has ")),
        value((), tag("it's ")),
        value((), tag("is ")),
        value((), tag("isn't ")),
        value((), tag("isnt ")),
        value((), tag("doesn't ")),
        value((), tag("doesnt ")),
        value((), tag("its ")),
        value((), tag("his ")),
        value((), tag("her ")),
        value((), tag("they're ")),
        value((), tag("theyre ")),
    ))
    .parse(body)
    .is_ok()
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
        // CR 511.2 + CR 603.7a: bare "at end of combat, …" delayed-trigger prefix
        // — companion of the suffix form. Keep the deferred body attached so it
        // reaches `strip_temporal_prefix` instead of splitting at the comma
        // (Fortune, Loyal Steed: "at end of combat, exile it and …").
        tag("at end of combat"),
        tag("for as long as "),
        // CR 508.6: "During any turn [you attacked with X], [effect]" — temporal
        // attack-history gate (Neyali, Neriv, Boros Strike-Captain). Keep the
        // whole clause together so the leading-conditional splitter (which gates
        // the body on the parsed condition) sees the comma, not the chunker.
        tag("during any turn "),
        tag("during a turn "),
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

/// CR 613.1b + CR 110.2: True when `s` is a "<player-subject> gains control of …"
/// clause — i.e. the control-handoff predicate where a *player* (not the acting
/// controller) takes control of an object. The subject axis is the full set of
/// player-noun phrases (`an opponent`, `an opponent of your choice`, `target
/// opponent`, `that player`, `each opponent`, …) recognized by
/// `subject::starts_with_subject_prefix`; the predicate is the fixed verb phrase
/// "gains control of ". A player subject followed by this conjugated predicate is
/// always a standalone subject-predicate clause that lowers to
/// `Effect::GiveControl` (via the `GainControl → GiveControl` subject rewrite in
/// `oracle_effect::mod`), never a noun-phrase continuation of the prior conjunct.
/// So both the comma splitter and the bare-`and` splitter must peel it off as its
/// own clause — otherwise the control transfer is silently dropped (Slicer, Hired
/// Muscle: "untap it, goad it, and an opponent of your choice gains control of
/// it"). Scoped to the "gains control of" verb so plain GainControl (the acting
/// controller steals) stays on the un-split imperative path.
fn starts_player_gains_control_clause(s: &str) -> bool {
    let Ok((_predicate, subject)) =
        take_until::<_, _, OracleError<'_>>(" gains control of ").parse(s)
    else {
        return false;
    };
    if subject.trim().is_empty() {
        return false;
    }
    // The span before the predicate must be a recognized player-subject phrase.
    // Include the boundary space consumed by `take_until`; the predicate match
    // above guarantees the next byte is the ASCII space before "gains".
    let subject_phrase = &s[..subject.len() + 1];
    super::subject::starts_with_subject_prefix(subject_phrase)
}

/// CR 708.7 + CR 708.8: True when `s` is a "turn <object> face up" / "turn
/// <object> face down" face-toggle clause (Yarus, Roar of the Old Gods: "...,
/// then turn it face up"). CR 708.7 lets a face-down permanent's controller turn
/// it face up; CR 708.8 reverts its copiable values as it is turned face up.
///
/// Narrowly gated to the COMPLETE toggle shape so it never re-splits unrelated
/// "turn" phrasings ("turn it sideways", "each turn", "until your next turn"):
///   - `tag("turn ")` head,
///   - a recognized anaphoric/target object subject head (`it` / `this` / `that`
///     / `those` / `them` / `target`) consumed as a complete word — the same
///     object-subject axis the clause-starter table already recognizes,
///   - the comma-free remainder of the (possibly multi-word) subject up to the
///     `face ` marker,
///   - the `up` / `down` toggle leaf.
///
/// Only a full "turn &lt;object&gt; face up|down" may license the split.
fn starts_turn_face_up_down_clause(s: &str) -> bool {
    let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("turn ").parse(s) else {
        return false;
    };
    // Object-subject head as a complete word (anaphor / target determiner).
    let Ok((rest, _)) = terminated(
        alt((
            tag::<_, _, OracleError<'_>>("it"),
            tag("this"),
            tag("that"),
            tag("those"),
            tag("them"),
            tag("target"),
        )),
        tag::<_, _, OracleError<'_>>(" "),
    )
    .parse(rest) else {
        return false;
    };
    // Peel the comma-free remainder of the subject up to the " face " toggle
    // marker. take_until stops at the first occurrence, keeping the match local
    // to a single clause.
    let Ok((tail, subject_rest)) = terminated(
        take_until::<_, _, OracleError<'_>>("face "),
        tag::<_, _, OracleError<'_>>("face "),
    )
    .parse(rest) else {
        return false;
    };
    // A clause boundary inside the subject span means the " face " marker belongs
    // to a different clause — reject (byte scan, not a dispatch string-method).
    if subject_rest.bytes().any(|b| b == b',') {
        return false;
    }
    alt((
        value((), tag::<_, _, OracleError<'_>>("up")),
        value((), tag("down")),
    ))
    .parse(tail)
    .is_ok()
}

/// Inner implementation operating on pre-lowercased input.
fn starts_clause_text_lower(s: &str) -> bool {
    if starts_multiword_keyword_continuation(s) {
        return false;
    }

    // CR 708.7 + CR 708.8: "turn <object> face up|down" face-toggle clause-starter
    // (Yarus, Roar of the Old Gods). A complete toggle shape is always its own
    // clause ("..., then turn it face up"), so it must split off to reach the
    // TurnFaceUp lowering; the narrow match leaves other "turn" phrasings intact.
    if starts_turn_face_up_down_clause(s) {
        return true;
    }

    // CR 613.1b + CR 110.2: "<player-subject> gains control of …" control-handoff
    // clause (Slicer, Hired Muscle). A player subject + "gains control of"
    // predicate is never a noun-phrase continuation, so it must split off as its
    // own clause to reach the GiveControl subject-rewrite path.
    if starts_player_gains_control_clause(s) {
        return true;
    }

    if alt((
        value((), tag::<_, _, OracleError<'_>>("assemble ")),
        value((), tag("reassemble ")),
        value((), tag("it assemble ")),
        value((), tag("it assembles ")),
        value((), tag("it reassemble ")),
        value((), tag("it reassembles ")),
    ))
    .parse(s)
    .is_ok()
    {
        return true;
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
///   The parser binds their exact player scope after the splitter preserves the
///   full clause.
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
    if result.is_ok() {
        return true;
    }
    controlled_creature_each_subject_starts(&lower)
}

fn controlled_creature_each_subject_starts(lower: &str) -> bool {
    let Ok((_, type_phrase)) = terminated(
        take_until::<_, _, OracleError<'_>>(" you control each "),
        tag::<_, _, OracleError<'_>>(" you control each "),
    )
    .parse(lower) else {
        return false;
    };
    let type_phrase = type_phrase.trim();
    !type_phrase.is_empty()
        && take_until::<_, _, OracleError<'_>>(" and ")
            .parse(type_phrase)
            .is_err()
}

/// CR 608.2c: Zack Fair — "put ~'s counters … and attach an Equipment that was
/// attached …". Recognized as a bare-and tail even when ~'s quote mode is active.
fn starts_attach_equipment_was_attached_clause(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let result = tag::<_, _, OracleError<'_>>("attach an equipment that was attached ")
        .parse(lower.as_str());
    result.is_ok()
}

/// True when `current` ends with the bare-and delimiter during character-by-
/// character clause chunking. Must match only the terminal suffix — a naive
/// `take_until(" and ")` from the start binds the first internal " and " (e.g.
/// Gogo's "~ and that creature each get +2/+0 and gain …") and returns false.
fn current_ends_with_bare_and(current: &str) -> bool {
    // allow-noncombinator: terminal suffix probe on incremental chunk buffer during char scan, not parsing dispatch.
    current.ends_with(" and ")
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

fn starts_they_continuous_clause_lower(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = tag("they ").parse(input)?;
    alt((
        value(
            (),
            preceded(
                tag("gain "),
                alt((value((), parse_keyword_name), value((), tag("\"")))),
            ),
        ),
        value((), preceded(tag("have "), parse_keyword_name)),
        value((), preceded(tag("lose "), parse_keyword_name)),
    ))
    .parse(input)
}

/// CR 601.2c + CR 611.2c: A second `"target <noun>"` clause joined by a bare
/// `" and "` opens its OWN target (each instance of the word "target" is a
/// distinct target — CR 601.2c) and applies a continuous modification
/// (CR 611.2c). Skulduggery: "... target creature you control gets +1/+1 and
/// target creature an opponent controls gets -1/-1." (also Monoist
/// Circuit-Feeder). Without this arm the second conjunct is not recognized as a
/// clause start and is swallowed by the first `Pump`, so the opponent-debuff is
/// dropped. The discriminator is a conjugated continuous-modification verb
/// (gets/gains/has/loses) somewhere after the `"target <noun>"` subject — a
/// genuine noun-phrase continuation ("... and target land") has no such verb
/// and is left un-split. `take_until` consumes the intervening qualifier
/// ("creature you control", "creature an opponent controls") up to the verb.
fn starts_target_continuous_clause_lower(s: &str) -> OracleResult<'_, ()> {
    let (rest, _) = tag("target ").parse(s)?;
    alt((
        value((), (take_until(" gets "), tag(" gets "))),
        value((), (take_until(" gains "), tag(" gains "))),
        value((), (take_until(" has "), tag(" has "))),
        value((), (take_until(" loses "), tag(" loses "))),
    ))
    .parse(rest)
}

/// CR 102.2 + CR 119.3 + CR 121.1 + CR 608.2c: a second "each opponent"/"each
/// player" clause joined by a bare " and " is a fresh player-scoped clause start
/// (Slitherwisp "you draw a card and each opponent loses 1 life"; Curry Favor;
/// Disinformation Campaign; Bad Deal; Clockwork Fox). Without this arm the
/// conjunct is swallowed by the first effect and the player-scoped half is
/// dropped. The discriminator is a conjugated player-action verb immediately
/// after the "each opponent "/"each player " subject — a bare-noun continuation
/// (Goblin Chainwhirler's "... and each creature you control") has no such verb
/// and is left un-split, preserving the single DamageAll. Player-scope sibling of
/// `starts_target_continuous_clause_lower`.
fn starts_each_player_predicate_clause_lower(s: &str) -> OracleResult<'_, ()> {
    let (rest, _) = alt((tag("each opponent "), tag("each player "))).parse(s)?;
    value(
        (),
        alt((
            tag("loses "),
            tag("gains "),
            tag("draws "),
            tag("discards "),
            tag("mills "),
            tag("sacrifices "),
            tag("exiles "),
        )),
    )
    .parse(rest)
}

/// CR 608.2c + CR 122.1: "remove <quantity> <type> counter[s] [from it]" as a
/// bare-`" and "` conjunct is an independent imperative instruction, not a
/// noun-phrase continuation of the prior conjunct. Amy Pond: "choose a suspended
/// card you own and remove that many time counters from it" — without this split
/// the counter-removal clause is swallowed and never reaches the RemoveCounter
/// dispatcher. The discriminator is a `counter` token within THIS conjunct's
/// segment (bounded at the next `" and "` boundary so a later unrelated
/// `counter`/`counter target` token can't trigger a false split), which keeps
/// non-counter `remove ` conjuncts ("remove it from combat" — Gustcloak) on the
/// un-split path. Mirrors the trailing `.or()` arms
/// (`starts_target_continuous_clause_lower`) rather than expanding the verb-prefix
/// `alt(...)` tuple, which is already at nom's arity limit.
fn starts_remove_counter_clause_lower(s: &str) -> OracleResult<'_, ()> {
    let (rest, _) = tag("remove ").parse(s)?;
    // Bound the counter-token search to THIS conjunct segment only, so a
    // "counter" token in a LATER conjunct ("remove it from combat and counter
    // target spell") cannot trigger a false split.
    let segment = match take_until::<_, _, OracleError<'_>>(" and ").parse(rest) {
        Ok((_, seg)) => seg,
        Err(_) => rest,
    };
    // Check on the bounded segment but return `rest` (not the truncated segment
    // remainder) as the remaining input — correct nom discipline.
    let _ = value((), (take_until("counter"), tag("counter"))).parse(segment)?;
    Ok((rest, ()))
}

/// Inner implementation operating on pre-lowercased input.
fn starts_bare_and_clause_lower(s: &str) -> bool {
    // CR 613.1b + CR 110.2: "<player-subject> gains control of …" control-handoff
    // clause (Slicer, Hired Muscle: "untap it, goad it, and an opponent of your
    // choice gains control of it"). A player subject + "gains control of"
    // predicate is always a standalone subject-predicate clause, never a
    // noun-phrase continuation of the prior conjunct — split it off so it reaches
    // the GiveControl subject-rewrite path instead of being dropped.
    if starts_player_gains_control_clause(s) {
        return true;
    }
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
    // CR 701.26b + CR 701.26a + CR 608.2c: third-person "untaps"/"taps"
    // conjugation. Control-handoff class: "the attacking player gains control
    // of ~ and untaps it" (Contested Game Ball) — the followup tap-state clause
    // must split as its own conjunct so it deconjugates ("untaps it" → "untap
    // it") and reaches the tap dispatcher. Mirrors the imperative `untap `/`tap
    // ` axis above, different conjugation; sits in the `.or()` chain because the
    // first `alt()` tuple is at nom's 21-arm limit.
    .or(value((), tag("untaps ")))
    .or(value((), tag("taps ")))
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
    // CR 301.5b + CR 608.2c: these attach forms are imperative game actions,
    // not noun-phrase continuations. Keep the matcher narrow so name-based
    // chains like "put counters on it and attach Fractal Harness to it" stay
    // available to the token-counter attach rewriter.
    .or(alt((
        value((), tag("attach this equipment ")),
        value((), tag("attach an equipment that was attached ")),
    )))
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
        // CR 105.3 + CR 205.1a + CR 613.1d/e: "it becomes <descriptor>" is always
        // a subject (anaphor) + animation predicate, never a noun-phrase
        // continuation. Possessed Goat: "Put three +1/+1 counters on this creature
        // and it becomes a black Demon in addition to its other colors and types"
        // — without this split the conjunct is fed to the imperative-only path,
        // where it fails closed to an unimplemented effect named "it". Splitting
        // routes it through `parse_clause_ast` → `try_parse_subject_become_clause`,
        // where "it" resolves to the anaphoric/self subject and
        // `build_become_clause` produces the additive color/type modifications.
        value((), tag("it becomes ")),
        // CR 205.1a + CR 205.1b + CR 613.1d: the copula form "it's <descriptor>"
        // ("it's" = "it is") is always a subject (anaphor) + animation/type
        // predicate, never a noun-phrase continuation. Brilliance Unleashed:
        // "Otherwise, return it to the battlefield and it's a 3/3 Robot artifact
        // creature with flying" — without this split the conjunct is fed to the
        // imperative-only path and fails closed to an Unimplemented effect named
        // "it's". Splitting routes it through `parse_clause_ast` →
        // `try_parse_subject_clause` → the contracted "it's a …" handler, which
        // emits the animation (non-additive) or AddType/AddSubtype (additive)
        // modifications on the referenced (ParentTarget) permanent. The
        // straight-apostrophe and typographic-apostrophe forms are leaf variants
        // of the same contraction (CLAUDE.md "don't nest leaf variants").
        value((), alt((tag("it's "), tag("it’s ")))),
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
        // tuples, and the overall `alt(...)` arity stays
        // under nom's 21-tuple limit. The first inner `tag` binds the error
        // type for the rest of the tree.
        //
        // CR 608.2c + CR 611.2c: The bare pronoun "they" back-references the
        // objects established by a prior conjunct in the same chain — e.g.
        // Unbreakable Formation's "put a +1/+1 counter on each of those
        // creatures and they gain vigilance until end of turn", Overseer of
        // Vault 76's "put a +1/+1 counter on each creature you control and they
        // gain vigilance until end of turn". Without this split the compound
        // stays one chunk; `try_split_targeted_compound` then bisects it and
        // feeds the conjunct to the imperative-only `parse_imperative_effect`,
        // which has no subject-predicate path and emits an Unimplemented effect
        // named "they". Splitting here routes the conjunct through
        // `parse_clause_ast` → `try_parse_subject_continuous_clause`, where
        // "they" resolves to `ParentTarget` (CR 608.2c) and the comma-and
        // keyword list ("vigilance, indestructible, and haste") lowers to one
        // `AddKeyword` per keyword with the grant's duration (CR 611.2c). Safe
        // to split: a plural anaphoric subject followed by a conjugated
        // continuous-modification verb cannot be a continuation noun phrase.
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
        value((), starts_they_continuous_clause_lower),
        // Singular anaphoric subjects: "that {creature,land,permanent,token}" +
        // singular-conjugation continuous verb (gains/gets/has/loses).
        // Single-token grants ("create one X token, that token gains haste")
        // are rarer than the plural form but real, so all three subject
        // nouns are paired with the singular verb set.
        value(
            (),
            (
                alt((
                    tag("that creature "),
                    tag("that land "),
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
    // CR 601.2c + CR 611.2c: a fresh "target <noun> gets/gains/has/loses ..."
    // conjunct opens its own target and continuous modification (Skulduggery).
    // Wired as a trailing `.or()` arm (mirroring the
    // `starts_they_continuous_clause_lower` helper) rather than a new tuple
    // element so the enclosing `alt(...)` cluster stays under nom's 21-arm limit.
    .or(value((), starts_target_continuous_clause_lower))
    // CR 102.2 + CR 119.3 + CR 121.1 + CR 608.2c: a fresh "each opponent"/"each
    // player" conjunct + conjugated player-action verb is a player-scoped clause
    // start (Slitherwisp, Curry Favor, Disinformation Campaign, Bad Deal,
    // Clockwork Fox). Trailing `.or()` arm (mirroring
    // `starts_target_continuous_clause_lower`) so the `alt(...)` cluster stays
    // under nom's 21-arm limit.
    .or(value((), starts_each_player_predicate_clause_lower))
    // CR 608.2c + CR 122.1: a bare "remove <quantity> <type> counter[s] …"
    // conjunct is an independent counter-removal instruction (Amy Pond). Trailing
    // `.or()` arm (mirroring `starts_target_continuous_clause_lower`) so the
    // verb-prefix `alt(...)` cluster stays under nom's 21-arm limit.
    .or(value((), starts_remove_counter_clause_lower))
    // CR 205.1a + CR 613.1d + CR 702.171b: a bare "becomes <descriptor>"
    // conjunct joined by " and " is a second animation/designation predicate whose
    // subject is carried over (anaphorically) from the prior conjunct — the same
    // demonstrative subject the first "becomes" clause used. Alacrian Armory:
    // "that permanent becomes saddled if it's a Mount and becomes an artifact
    // creature if it's a Vehicle" — without this split the compound stays one
    // chunk, the trailing-conditional peel only catches the LAST "if it's a …"
    // gate, and the residual fails closed to an Unimplemented effect named
    // "become". Splitting routes the second conjunct through `parse_clause_ast` →
    // `try_parse_subject_become_clause`, where the empty (carried-over) subject
    // resolves to the parent target and each conjunct's "if it's a <type>" gate
    // parses exactly as the standalone "[subject] becomes <descriptor> if it's a
    // <type>" clause does. A bare conjugated "becomes" (or imperative "become") is
    // always a verb predicate, never a noun-phrase continuation, so the split is
    // safe. Reuses the shared `parse_become_verb` combinator. Mirrors the anaphoric
    // "it becomes " arm above for the subject-carried form.
    .or(parse_become_verb)
    // CR 608.2c + CR 400.7i: "may play <card-anaphor>" / "may cast
    // <card-anaphor>" — a bare optional play/cast grant whose subject was
    // established by the prior conjunct (Lightstall Inquisitor: "each opponent
    // exiles a card from their hand and may play that card for as long as it
    // remains exiled"). "may play"/"may cast" always begins a verb phrase,
    // never a noun-phrase continuation of the prior conjunct, so the split
    // routes the conjunct to the per-grantee play-from-exile grant parser
    // (`try_parse_per_grantee_play_grant`). The "you may " subject-led form is
    // already covered by the `you may ` arm above; this arm catches the
    // subject-elided form after a player-scoped exile.
    .or(alt((
        value((), tag::<_, _, OracleError<'_>>("may play ")),
        value((), tag("may cast ")),
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
/// CR 701.13 + CR 608.2c: verb-gapping in a conjoined exile cost — "exile a
/// Human you control **and** an artifact you control" elides the second
/// "exile". The two conjuncts name *distinct* objects (a Human and an artifact),
/// so they lower to two separate `ChangeZone(Exile)` effects, not one `And`
/// filter. The bare-`and` recognizer never matches a noun-phrase continuation
/// (correctly — it must not over-split "target creature and all other
/// creatures"), so we restore the elided verb here: when conjunct 1 begins with
/// `"exile "` and conjunct 2 is a bare article-led object phrase, return
/// `"exile "` to prepend so the second conjunct reaches the exile parser as its
/// own clause (Fugitive of the Judoon III).
///
/// Tightly gated so it never fires on a genuine single-object continuation: the
/// remainder must be `a/an <phrase>` AND must parse to a concrete typed filter
/// with no unparsed remainder (other than an optional possessive tail).
fn exile_conjunct_prepend(before_lower: &str, remainder_trimmed: &str) -> Option<String> {
    // Conjunct 1 must contain the elided exile verb (the chunk may begin with an
    // optional "you may " frame: "You may exile a Human you control and …").
    if !nom_primitives::scan_contains(before_lower, "exile ") {
        return None;
    }
    let remainder_lower = remainder_trimmed.to_ascii_lowercase();
    // Conjunct 2 must be an article-led noun phrase (never a verb-headed clause —
    // those are already handled by `starts_bare_and_clause`).
    if alt((
        tag::<_, _, OracleError<'_>>("a "),
        tag::<_, _, OracleError<'_>>("an "),
    ))
    .parse(remainder_lower.as_str())
    .is_err()
    {
        return None;
    }
    // Isolate the object phrase: it ends at the first sentence/clause boundary
    // (". If you do, …" continuation, or a comma). Bounding the slice prevents
    // the trailing continuation from defeating the "no stray remainder" check.
    // allow-noncombinator: structural clause-boundary scan (locate the sentence/comma terminator), not parsing dispatch.
    let phrase_end = remainder_trimmed
        .find(['.', ','])
        .unwrap_or(remainder_trimmed.len());
    let phrase = remainder_trimmed[..phrase_end].trim();
    // The phrase must parse to a concrete typed object filter with no stray
    // remainder — this rejects open-ended continuations ("... and an opponent
    // gains control of it") and incomplete noun phrases.
    let (filter, rem) = parse_target(phrase);
    if !rem.trim().is_empty() || matches!(filter, TargetFilter::Any) {
        return None;
    }
    Some("exile ".to_string())
}

fn combat_requirement_conjunct_prepend(
    before_and: &str,
    remainder_trimmed: &str,
) -> Option<String> {
    let remainder_lower = remainder_trimmed.to_ascii_lowercase();
    let cant_be_blocked_restriction =
        super::subject::is_cant_be_blocked_restriction_predicate(&remainder_lower);
    if !super::imperative::is_standalone_combat_requirement(&remainder_lower)
        && !super::subject::is_can_block_extra_predicate(&remainder_lower)
        && !super::subject::is_can_attack_despite_defender_predicate(&remainder_lower)
        && !(cant_be_blocked_restriction
            && cant_be_blocked_restriction_needs_subject_reattach(&remainder_lower))
    {
        return None;
    }
    continuous_grant_subject_prepend(before_and)
}

/// CR 502.3 + CR 611.2a: "<subj> gains <keyword> until end of turn and doesn't
/// untap during [its controller's | your] next untap step" (Homarid Warrior).
/// Like the combat-requirement case, the trailing "doesn't untap …" conjunct
/// is a restriction predicate that is NOT verb-headed by any entry in
/// `starts_bare_and_clause`, so the bare-and logic never splits it; left
/// unified, the keyword grant's mid-clause "until end of turn" is hidden from
/// the suffix-only `strip_trailing_duration` and the grant lowers to
/// `duration: None` (granted permanently, wrong). Split here and prepend
/// conjunct 1's subject so the restriction reaches `build_restriction_clause`
/// with the correct `affected`. Gated on the typed "next untap step" restriction
/// phrase only, so multi-keyword lists ("gains flying and haste …") stay on the
/// untouched single-clause path.
fn untap_restriction_conjunct_prepend(before_and: &str, remainder_trimmed: &str) -> Option<String> {
    let remainder_lower = remainder_trimmed.to_ascii_lowercase();
    let is_doesnt_untap = (nom_primitives::scan_contains(&remainder_lower, "doesn't untap")
        || nom_primitives::scan_contains(&remainder_lower, "does not untap"))
        && nom_primitives::scan_contains(&remainder_lower, "next untap step");
    if !is_doesnt_untap {
        return None;
    }
    continuous_grant_subject_prepend(before_and)
}

/// Shared subject-reattach for a continuous "gain(s)/get(s)" conjunct-1: returns
/// the subject text (with a trailing space) to prepend onto conjunct-2 so it
/// parses as its own subject-predicate clause. `""` when the subject was already
/// lifted (verb at offset 0); `"it "` for a targeted subject (anaphor); the
/// literal subject otherwise. `None` when conjunct-1 is not a continuous grant.
fn continuous_grant_subject_prepend(before_and: &str) -> Option<String> {
    let before_lower = before_and.to_ascii_lowercase();
    // CR 508.1d / CR 509.1c: chunk begins with the gain/get verb at offset 0
    // (subject already lifted by the enclosing compound-subject distribution);
    // emit an empty subject so the trailing conjunct splits.
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
            // Map the verb position back onto the original-case slice and keep
            // only the local sentence's subject.
            let subject = local_subject_before_continuous_verb(before_and, before_verb.len())?;
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
                    // Map the verb position back onto the original-case slice
                    // and keep only the local sentence's subject.
                    let subject =
                        local_subject_before_continuous_verb(before_and, before_verb.len())?;
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

fn cant_be_blocked_restriction_needs_subject_reattach(remainder_lower: &str) -> bool {
    // Plain inline evasion grants are owned by `parse_continuous_modifications`
    // and must stay in one static definition. The where-suffixed form needs a
    // split so the first conjunct's duration is not hidden behind the trailing
    // variable definition.
    nom_primitives::scan_contains(remainder_lower, "where ")
}

fn local_subject_before_continuous_verb(before_and: &str, before_verb_len: usize) -> Option<&str> {
    let mut subject = before_and[..before_verb_len].trim();
    let mut remaining = subject;
    while let Ok((after_sentence, _)) = terminated(
        take_until::<_, _, OracleError<'_>>(". "),
        tag::<_, _, OracleError<'_>>(". "),
    )
    .parse(remaining)
    {
        subject = after_sentence.trim();
        remaining = subject;
    }
    (!subject.is_empty()).then_some(subject)
}

/// CR 121.1 / CR 119.1: Returns true when the token immediately following a
/// `gain `/`lose ` prefix is a count expression — i.e. digits, or `X`/`x`
/// terminated by a non-alphanumeric boundary so we don't false-match "x" inside
/// "x-cost" (only `X ` / `X,` / `X.` / end-of-string). Distinguishes imperative
/// "gain 3 life" / "lose X life" from continuous-modification "gain lifelink".
fn next_token_is_count(s: &str) -> bool {
    let trimmed = s.trim_start();
    // CR 107.1a + CR 121.1 + CR 119.3: a fraction-led amount ("half their life")
    // is a valid player-action count head; the divisor word is the split
    // discriminator. `parse_fraction_divisor` matches only half/third/tenth
    // (with trailing space), never keyword grants ("flying"/"all abilities").
    if crate::parser::oracle_nom::quantity::parse_fraction_divisor(trimmed).is_ok() {
        return true;
    }
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
    let trimmed = s.trim_start();
    // CR 107.1a + CR 121.1 + CR 119.3: a fraction-led amount ("half their life")
    // is a valid player-action count head; the divisor word is the split
    // discriminator. `parse_fraction_divisor` matches only half/third/tenth
    // (with trailing space), never keyword grants ("flying"/"all abilities").
    if crate::parser::oracle_nom::quantity::parse_fraction_divisor(trimmed).is_ok() {
        return true;
    }
    let count = alt((
        value((), nom_primitives::parse_number),
        value((), tag::<_, _, OracleError<'_>>("x")),
    ));
    let noun = alt((tag("cards"), tag("card"), tag("life")));
    (count, multispace1, noun).parse(trimmed).is_ok()
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

/// CR 111.3 (issue #4605): true when the text accumulated right before an
/// apostrophe ends at a phrase boundary that opens a nested quoted ability
/// (`with '` / `and '` / `or '` / `, '`). Mirrors the opener anchors in
/// `extract_token_static_abilities`'s single-quote pass, so a possessive
/// (`~'s`) or contraction (`can't`) apostrophe is never treated as an
/// ability-quote opener and a sentence-ending `.` keeps splitting normally.
fn ends_with_single_quote_ability_anchor(current: &str) -> bool {
    // allow-noncombinator: terminal suffix probe on the incremental chunk buffer during the char scan in `split_clause_sequence`, not parsing dispatch against raw oracle text (mirrors `current_ends_with_bare_and`).
    ["with ", "and ", "or ", ", "]
        .iter()
        .any(|anchor| current.ends_with(anchor))
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

/// CR 701.6a + CR 614.1a: nom recognizer for the "if that spell is countered
/// this way, put it <zone> instead of into that player's graveyard"
/// continuation clause (Memory Lapse, Remand, Spell Crumple). Operates on
/// lowercased text; tolerates a trailing period/whitespace. Returns the parsed
/// [`SpellStackToGraveyardReplacement`], or `None` for an unsupported destination
/// (Hinder's "your choice of the top or bottom", Transcendent Dragon's "exile
/// it instead") so those cards are honestly gapped rather than misparsed.
///
/// Composed from independent axes rather than enumerated as full strings:
///   - spell anaphor ("that spell" / "that card" / "it").
///   - destination ("on top of its owner's library" / "on the bottom of its
///     owner's library" / "into its owner's hand").
fn recognize_counter_spell_zone_redirect(lower: &str) -> Option<SpellStackToGraveyardReplacement> {
    let clause = lower.trim().trim_end_matches('.').trim_end();
    let mut parser = (
        tag::<_, _, OracleError<'_>>("if "),
        alt((tag("that spell"), tag("that card"), tag("it"))),
        tag(" is countered this way, put it "),
        alt((
            value(
                SpellStackToGraveyardReplacement::Library {
                    position: LibraryPosition::Top,
                },
                tag("on top of its owner's library"),
            ),
            value(
                SpellStackToGraveyardReplacement::Library {
                    position: LibraryPosition::Bottom,
                },
                tag("on the bottom of its owner's library"),
            ),
            value(
                SpellStackToGraveyardReplacement::Hand,
                tag("into its owner's hand"),
            ),
        )),
        tag(" instead of into that player's graveyard"),
        eof,
    );
    parser
        .parse(clause)
        .ok()
        .map(|(_, (_, _, _, destination, _, _))| destination)
}

/// CR 707.10c: nom parser for the "[you] may choose [a] new target[s] for
/// {the,that} copy/copies" continuation clause that grants copy retargeting.
pub(super) fn parse_copy_retarget_clause(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            opt(alt((tag(", and "), tag("and ")))),
            opt(tag("you ")),
            tag("may choose "),
            alt((tag("a new target "), tag("new targets "))),
            tag("for "),
            alt((tag("the copies"), tag("the copy"), tag("that copy"))),
            opt(alt((tag("."), tag(",")))),
        ),
    )
    .parse(input)
}

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
pub(super) fn recognize_copy_retarget_clause(lower: &str) -> bool {
    all_consuming(parse_copy_retarget_clause)
        .parse(lower.trim())
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
            enters_under,
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
                apply_search_destination_to_ability_chain(
                    previous,
                    destination,
                    enter_tapped,
                    enters_under.clone(),
                );
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
                    enters_under,
                    enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped),
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
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
        ContinuationAst::CounterSpellZoneRedirect { destination } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Counter {
                countered_spell_zone,
                ..
            } = &mut *previous.effect
            {
                // CR 701.6a + CR 614.1a: "put it <zone> instead of into that
                // player's graveyard" redirect (Memory Lapse, Remand, Spell
                // Crumple).
                *countered_spell_zone = Some(destination);
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
                    scope: EffectScope::Single,
                },
            ));
        }
        ContinuationAst::GoadLastCreated { duration } => {
            // CR 701.15b: Goaded is a static ability on the just-created tokens.
            defs.push(AbilityDefinition::new(
                kind,
                Effect::GenericEffect {
                    static_abilities: vec![StaticDefinition::continuous()
                        .affected(TargetFilter::LastCreated)
                        .modifications(vec![ContinuousModification::AddStaticMode {
                            mode: StaticMode::Goaded,
                        }])
                        .description("goaded".to_string())],
                    duration: duration.or(Some(Duration::Permanent)),
                    target: Some(TargetFilter::LastCreated),
                },
            ));
        }
        ContinuationAst::SelfCostKeywordCostClarification => {}
        ContinuationAst::CantRegenerate => {
            // CR 608.2c: walk backward through the definition chain to find
            // the nearest Destroy/DestroyAll. The regen clause may not be
            // adjacent — e.g. Kirtar's Wrath threshold has a Token creation
            // between the DestroyAll and "Creatures destroyed this way can't
            // be regenerated."
            if let Some(def) = defs.iter_mut().rev().find(|d| {
                matches!(
                    &*d.effect,
                    Effect::Destroy { .. } | Effect::DestroyAll { .. }
                )
            }) {
                match &mut *def.effect {
                    Effect::Destroy {
                        cant_regenerate, ..
                    }
                    | Effect::DestroyAll {
                        cant_regenerate, ..
                    } => {
                        *cant_regenerate = true;
                    }
                    _ => unreachable!(),
                }
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
            quantity,
            filter: card_filter,
            destination: kept_dest,
            rest_destination: rest_dest,
            enters_under,
            face_down_profile,
            enter_tapped,
            reveal_verb,
        } => {
            // CR 608.2c: the "from among those cards" continuation patches the
            // earlier "look at the top N" instruction. When a transparent
            // clause (e.g. `Sacrifice` — Birthing Ritual) sits between the
            // `Dig` and this continuation, `defs.last()` is that intervening
            // clause, not the `Dig`. Search back for the nearest `Dig`/`Mill`.
            //
            // Special case — optional sacrifice interleaving (Birthing Ritual):
            // "look at top 7. Then you MAY sacrifice a creature. If you do, you
            // may put a creature card ... from among those cards onto the
            // battlefield." The choice must happen AFTER the sacrifice, not
            // before (CR 608.2c: follow written order). Detect this pattern and
            // restructure: root Dig becomes look-only; a `from_prior_look` Dig
            // gated on OptionalEffectPerformed is wired as the Sacrifice's
            // sub_ability; a decline branch Dig (keep_count=0, rest→Library)
            // is its else_ability, routing all 7 cards to library bottom if
            // the player declined the sacrifice.
            let dig_pos = defs
                .iter()
                .rposition(|d| matches!(&*d.effect, Effect::Dig { .. } | Effect::Mill { .. }));
            if let Some(dig_pos) = dig_pos {
                if matches!(&*defs[dig_pos].effect, Effect::Dig { .. }) {
                    // Find an optional, lookback-transparent sacrifice/pay-cost
                    // clause between the root Dig and the end of defs.
                    let sac_pos = defs[dig_pos + 1..]
                        .iter()
                        .position(|d| {
                            d.optional
                                && clause_is_dig_lookback_transparent(&d.effect)
                                && matches!(
                                    &*d.effect,
                                    Effect::Sacrifice { .. } | Effect::PayCost { .. }
                                )
                        })
                        .map(|i| dig_pos + 1 + i);

                    if let Some(sac_pos) = sac_pos {
                        // CR 608.2c + CR 701.20e: Birthing Ritual pattern.
                        debug_assert!(
                            face_down_profile.is_none() && enters_under.is_none(),
                            "Dig-source face-down from-among with intervening sacrifice \
                             is not yet supported"
                        );

                        // 1. Demote the root Dig to look-only (no choice yet).
                        if let Effect::Dig {
                            keep_count,
                            destination,
                            filter,
                            rest_destination,
                            reveal,
                            up_to,
                            ..
                        } = &mut *defs[dig_pos].effect
                        {
                            *keep_count = Some(0);
                            *destination = None;
                            *filter = TargetFilter::Any;
                            *rest_destination = None;
                            *reveal = false;
                            *up_to = false;
                        }

                        // 2. Map quantity → keep_count / up_to for the choice Dig.
                        let (choice_keep_count, choice_up_to) = match quantity {
                            PutCount::All => (Some(u32::MAX), false),
                            PutCount::AnyNumber => (Some(u32::MAX), true),
                            PutCount::Up(n) => (Some(n), true),
                            PutCount::Exactly(n) => (Some(n), false),
                        };

                        // 3. Decline branch: put all looked-at cards on library
                        // bottom with no interactive choice (player declined
                        // the optional sacrifice).
                        let put_on_bottom = AbilityDefinition::new(
                            kind,
                            Effect::Dig {
                                player: TargetFilter::Controller,
                                count: QuantityExpr::Fixed { value: 0 },
                                keep_count: Some(0),
                                up_to: false,
                                filter: TargetFilter::Any,
                                destination: None,
                                rest_destination: Some(Zone::Library),
                                reveal: false,
                                enter_tapped: false,
                                source: DigSource::PriorLook,
                            },
                        );

                        // 4. Choice Dig: reads private_look_ids, evaluates
                        // CMC filter with sacrifice snapshot in context.
                        // CR 701.20e: "put the rest on the bottom" is
                        // unconditional — route unchosen cards to Library.
                        let mut from_prior_look_dig = AbilityDefinition::new(
                            kind,
                            Effect::Dig {
                                player: TargetFilter::Controller,
                                count: QuantityExpr::Fixed { value: 0 },
                                keep_count: choice_keep_count,
                                up_to: choice_up_to,
                                filter: card_filter,
                                destination: kept_dest,
                                rest_destination: Some(rest_dest.unwrap_or(Zone::Library)),
                                reveal: false,
                                enter_tapped,
                                source: DigSource::PriorLook,
                            },
                        );
                        // Gate on sacrifice having been performed.
                        from_prior_look_dig.condition = Some(AbilityCondition::effect_performed());
                        // Decline branch fires when sacrifice was not performed.
                        from_prior_look_dig.else_ability = Some(Box::new(put_on_bottom));

                        // 5. Wire: Sacrifice.sub_ability = from_prior_look_dig.
                        defs[sac_pos].sub_ability = Some(Box::new(from_prior_look_dig));
                        return;
                    }
                }
            }

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
                enter_tapped: dig_enter_tapped,
                ..
            } = &mut *previous.effect
            {
                // NOTE (deferred, latent): the `Dig`-source from-among form does
                // not yet honor `enters_under` / `face_down_profile`. The `Dig`
                // resolver routes kept cards to `destination` itself (face up,
                // under the owner's control) and `Effect::Dig` has no controller/
                // face-down fields, so a hypothetical "look at the top N, then put
                // those creature cards onto the battlefield face down under your
                // control" card would silently ignore those clauses. No printed
                // card routes face-down/under-your-control entry through the `Dig`
                // form today (the Cyber class uses the `Mill` branch below). When
                // such a card appears, mirror the `Mill` branch: publish the dug
                // cards into the chain tracked set and PUSH a
                // `ChangeZoneAll`/`ChangeZone { face_down_profile, enters_under }`
                // reading `TrackedSetFiltered`, rather than patching the `Dig`.
                debug_assert!(
                    face_down_profile.is_none() && enters_under.is_none(),
                    "Dig-source face-down/under-control from-among form is not yet \
                     supported; route via the tracked-set push (see Mill branch)"
                );
                // CR 701.20e: Map the typed `PutCount` onto the `Dig`'s
                // `keep_count`/`up_to`. `u32::MAX` is an unbounded parser
                // sentinel here: the Dig resolver clamps it to the number of
                // seen cards, preserving "all" and "any number" without the old
                // arbitrary 255 cap or overloading `None`'s default meaning.
                match quantity {
                    PutCount::All => {
                        *keep_count = Some(u32::MAX);
                        *up_to = false;
                    }
                    PutCount::AnyNumber => {
                        *keep_count = Some(u32::MAX);
                        *up_to = true;
                    }
                    PutCount::Up(n) => {
                        *keep_count = Some(n);
                        *up_to = true;
                    }
                    PutCount::Exactly(n) => {
                        *keep_count = Some(n);
                        *up_to = false;
                    }
                }
                *filter = card_filter;
                // CR 701.20b + CR 608.2c: When `destination` is None the kept
                // cards are NOT auto-routed by the Dig resolver; downstream sub_abilities
                // read the tracked set and route by type. Also promote the
                // Dig to reveal:true — "from among them" is a reveal-form.
                //
                // CR 701.20a vs 701.20e: a "reveal ... from among them" clause is
                // public, so promote to reveal:true even when the kept card routes
                // to a fixed library position (Fertile Thicket: reveal_verb=true,
                // kept_dest=Some(Library)). Look-only digs (reveal_verb=false,
                // kept_dest=Some(...)) keep reveal=false (private look, CR 701.20e).
                *destination = kept_dest;
                if kept_dest.is_none() || reveal_verb {
                    *reveal = true;
                }
                if let Some(rd) = rest_dest {
                    *rest_destination = Some(rd);
                }
                *dig_enter_tapped = enter_tapped;
            } else if let Effect::Mill {
                destination: mill_destination,
                ..
            } = &*previous.effect
            {
                // CR 701.17c + CR 608.2c: "...from among the milled cards" after
                // a `Mill`. The `Mill` already mills its `count` cards to its
                // `destination` (CR 701.17a); the continuation reads that milled
                // set. `Mill` keeps its own `count`/`destination` intact and we
                // PUSH a fresh sub-ability that selects from the milled cards.
                // `TrackedSetFiltered` with the sentinel `TrackedSetId(0)`
                // resolves to the most recent tracked set at resolution — the
                // engine auto-publishes the `Mill`'s affected objects (the milled
                // cards) into that set. Phase-2 sub-chain assembly folds this
                // pushed def into `Mill.sub_ability`.
                //
                // CR 400.3: `TrackedSetFiltered` contributes no scan zone, so the
                // move's origin must be the zone the milled cards actually sit in
                // (the Mill's destination — Graveyard by default). Otherwise
                // `resolve_all` would default to scanning the battlefield and move
                // nothing.
                let mill_destination = *mill_destination;
                match quantity {
                    PutCount::All => {
                        // CR 701.17c: "put ALL <filter> milled this way ..." moves
                        // the entire matching set → `ChangeZoneAll`.
                        defs.push(AbilityDefinition::new(
                            kind,
                            Effect::ChangeZoneAll {
                                origin: Some(mill_destination),
                                destination: kept_dest.unwrap_or(Zone::Battlefield),
                                target: TargetFilter::TrackedSetFiltered {
                                    id: crate::types::identifiers::TrackedSetId(0),
                                    filter: Box::new(card_filter),
                                    // "from among the milled cards" is a
                                    // selection anaphor over a single-producer
                                    // set — zone-agnostic (every member is in the
                                    // mill destination already).
                                    caused_by: None,
                                },
                                enters_under,
                                enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(
                                    enter_tapped,
                                ),
                                enter_with_counters: vec![],
                                face_down_profile,
                                library_position: None,
                                random_order: false,
                            },
                        ));
                    }
                    PutCount::AnyNumber | PutCount::Up(_) | PutCount::Exactly(_) => {
                        let is_any_number = matches!(quantity, PutCount::AnyNumber);
                        let is_up_to = matches!(quantity, PutCount::AnyNumber | PutCount::Up(_));
                        let mut def = AbilityDefinition::new(
                            kind,
                            Effect::ChangeZone {
                                // CR 400.3: a bounded "put up to N <filter> milled
                                // this way onto the battlefield face down" form
                                // must scan the milled cards' actual zone (the
                                // Mill destination), mirroring the `All` branch —
                                // `TrackedSetFiltered` contributes no scan zone.
                                // The default (hand-return) form keeps `origin:
                                // None`; the singular `resolve` path already
                                // derives the zone via `tracked_set_member_zone`.
                                origin: face_down_profile.as_ref().map(|_| mill_destination),
                                destination: kept_dest.unwrap_or(Zone::Hand),
                                target: TargetFilter::TrackedSetFiltered {
                                    id: crate::types::identifiers::TrackedSetId(0),
                                    filter: Box::new(card_filter),
                                    // Selection anaphor over the single milled
                                    // set — zone-agnostic (see the `All` arm).
                                    caused_by: None,
                                },
                                owner_library: false,
                                enter_transformed: false,
                                enters_under,
                                enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(
                                    enter_tapped,
                                ),
                                enters_attacking: false,
                                up_to: is_up_to,
                                enter_with_counters: vec![],
                                conditional_enter_with_counters: vec![],
                                face_down_profile,
                                enters_modified_if: None,
                            },
                        );
                        if is_any_number {
                            def = def
                                .multi_target(MultiTargetSpec::unlimited(0))
                                .target_choice_timing(TargetChoiceTiming::Resolution);
                        }
                        defs.push(def);
                    }
                }
            }
        }
        ContinuationAst::FaceDownProfileSpec { profile } => {
            // CR 708.2a / CR 205.1a: "They're N/M ... creatures." refines the
            // characteristics of the preceding put-face-down move. Walk back to
            // the nearest `ChangeZoneAll`/`ChangeZone` already carrying a
            // `face_down_profile` (set by the "... face down ..." put-clause) and
            // overwrite it with the specified profile. Mirror the DigFromAmong /
            // PutRest backward-walk idiom.
            for def in defs.iter_mut().rev() {
                match &mut *def.effect {
                    Effect::ChangeZoneAll {
                        face_down_profile: fdp @ Some(_),
                        library_position: None,
                        random_order: false,
                        ..
                    }
                    | Effect::ChangeZone {
                        face_down_profile: fdp @ Some(_),
                        ..
                    }
                    // CR 708.2a: "put the top N ... onto the battlefield face
                    // down" lowers to `Effect::Manifest` (Cybership). Overwrite
                    // the seeded vanilla 2/2 profile with the spec's
                    // characteristics ("2/2 Cyberman artifact creatures").
                    | Effect::Manifest {
                        profile: fdp @ Some(_),
                        ..
                    }
                    // CR 708.2a: "Turn target creature face down" seeds a vanilla
                    // 2/2 profile; overwrite it with the parsed "It's a 2/2
                    // Cyberman artifact creature." characteristics (Cyber
                    // Conversion).
                    | Effect::TurnFaceDown {
                        profile: fdp @ Some(_),
                        ..
                    } => {
                        *fdp = Some(profile);
                        break;
                    }
                    _ => {}
                }
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
                    selection: crate::types::ability::CardSelectionMode::Chosen,
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
                        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        face_down_profile: None,
                        enters_modified_if: None,
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
                        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        face_down_profile: None,
                        enters_modified_if: None,
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
                // Preserve an explicit kept destination (Hand, Battlefield, etc.)
                // from an earlier "put one into your hand" clause; only default
                // destination to Library for reveal-only digs.
                if destination.is_none() {
                    *destination = Some(Zone::Library);
                }
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
                    target: exile_designation_grant_target(previous),
                    grantee: PermissionGrantee::ObjectOwner,
                },
            );
            append_definition_to_sub_chain(previous, grant_def);
        }
        ContinuationAst::BecomesForetold => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            // CR 702.143d: The card becomes foretold. `cost` is a placeholder
            // filled at resolution from the card's own Foretell keyword
            // (`casting::foretell_cost`, the single authority for "any foretell
            // cost it has"); `turn_foretold` is stamped at resolution, mirroring
            // Plotted's `turn_plotted: 0` placeholder.
            let grant_def = AbilityDefinition::new(
                kind,
                Effect::GrantCastingPermission {
                    permission: CastingPermission::Foretold {
                        cost: ManaCost::zero(),
                        turn_foretold: 0,
                    },
                    target: exile_designation_grant_target(previous),
                    grantee: PermissionGrantee::ObjectOwner,
                },
            );
            append_definition_to_sub_chain(previous, grant_def);
        }
        ContinuationAst::EntersTappedAttacking { moved_filter } => {
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
                    enters_modified_if,
                    ..
                } => {
                    *enters_attacking = true;
                    *enter_tapped = crate::types::zones::EtbTapState::Tapped;
                    // CR 614.12: when the followup carried a leading "if that
                    // card is a/an <type>", gate the riders on the moved
                    // object's type at ChangeZone resolution instead of
                    // applying them unconditionally.
                    *enters_modified_if = moved_filter;
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
            any_number,
            rest_destination: rest_dest,
            enters_under,
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
                matched_disposition,
                enters_under: effect_enters_under,
                ..
            } = &mut *previous.effect
            {
                // CR 701.20a + CR 608.2c: "put any number of those [filter] cards
                // onto [destination]" dispenses the *matched set* through a
                // controller choice (Aurora Awakener). Set the disposition and the
                // kept/rest destinations directly — the single-hit optional/decline
                // refinement below does not apply to a set selection.
                if any_number {
                    *matched_disposition = RevealUntilDisposition::ChooseAnyNumber;
                    *kept_destination = destination;
                    if destination == Zone::Battlefield {
                        *enter_tapped = crate::types::zones::EtbTapState::from_legacy_bool(tapped);
                        *enters_attacking = attacking;
                    }
                    if let Some(rest) = rest_dest {
                        *rest_destination = rest;
                    }
                    return;
                }
                match optional_decline {
                    // CR 701.20a + CR 608.2c: optional kept clause ("you may put
                    // that card onto the battlefield"). `destination` is the
                    // accept zone, `decline` the decline zone. `enter_tapped`
                    // applies to the accept zone (always Battlefield).
                    Some(decline) => {
                        *kept_optional_to = Some(destination);
                        *kept_destination = decline;
                        *enter_tapped = crate::types::zones::EtbTapState::from_legacy_bool(tapped);
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
                            *enter_tapped =
                                crate::types::zones::EtbTapState::from_legacy_bool(tapped);
                            // CR 508.4: "put that card onto the battlefield
                            // tapped and attacking" (Raph & Mikey, Fireflux Squad).
                            *enters_attacking = attacking;
                        }
                    }
                }
                if let Some(rest) = rest_dest {
                    *rest_destination = rest;
                }
                *effect_enters_under = enters_under;
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
        // CR 702.75a + CR 406.3: "exile one of them face down" patches the
        // preceding private `Dig` into the Hideaway shape — the controller
        // selects ONE looked-at card and the `DigChoice` flow routes it to
        // exile. Mirror `database/hideaway.rs`: keep_count:1, destination:Exile,
        // reveal stays false (CR 701.20e — the look was private), and chain a
        // `HideawayConceal` sub-ability to turn the chosen card face down and
        // link it to the source (so the trailing "you may cast that card ..."
        // permission, which reads the published tracked set / `ExiledBySource`,
        // binds to the dug card). Without this fusion the Dig short-circuited as
        // a keep_count:0 pure-peek and a sibling `ChangeZone { ParentTarget }`
        // exiled the trigger source itself (#1146).
        ContinuationAst::ExileOneOfThemFaceDown => {
            let Some(previous) = defs
                .iter_mut()
                .rev()
                .find(|d| matches!(&*d.effect, Effect::Dig { .. }))
            else {
                return;
            };
            if let Effect::Dig {
                keep_count,
                up_to,
                destination,
                ..
            } = &mut *previous.effect
            {
                *keep_count = Some(1);
                *up_to = false;
                *destination = Some(Zone::Exile);
            }
            // CR 608.2c: chain the conceal continuation onto the Dig. The
            // `DigChoice` resolution binds the chosen (exiled) card onto this
            // sub-ability's `ParentTarget`; `HideawayConceal` then flips it face
            // down (CR 406.3) and links it to the source (CR 607.2a / CR 702.75a).
            append_conceal_sub_ability(previous);
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

/// CR 702.75a + CR 608.2c: Append the Hideaway conceal continuation to the
/// deepest point of `dig`'s sub-ability chain. Mirrors `database/hideaway.rs`:
/// the chained `HideawayConceal { target: ParentTarget }` flips the just-exiled
/// dug card face down (CR 406.3) and links it to the source. Appended at the
/// deepest sub so it never clobbers an existing continuation (e.g. a trailing
/// "put the rest on the bottom" patch lives on the Dig itself, not as a sub).
fn append_conceal_sub_ability(dig: &mut AbilityDefinition) {
    let conceal = Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::HideawayConceal {
            target: TargetFilter::ParentTarget,
        },
    ));
    let mut cursor = dig;
    while cursor.sub_ability.is_some() {
        cursor = cursor
            .sub_ability
            .as_mut()
            .expect("sub_ability checked above");
    }
    cursor.sub_ability = Some(conceal);
}

fn apply_search_destination_to_ability_chain(
    ability: &mut AbilityDefinition,
    destination: Zone,
    enter_tapped: bool,
    enters_under: Option<ControllerRef>,
) {
    let mut cursor = Some(ability);
    while let Some(sub_ability) = cursor {
        if let Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: existing_destination,
            enter_tapped: existing_enter_tapped,
            enters_under: existing_enters_under,
            ..
        } = &mut *sub_ability.effect
        {
            *existing_destination = destination;
            *existing_enter_tapped =
                crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped);
            // CR 110.2a: only overwrite when the clause specified a controller, so a value set by an earlier clause is preserved.
            if enters_under.is_some() {
                *existing_enters_under = enters_under.clone();
            }
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
        | ContinuationAst::CounterSourceRiderDestroy
        | ContinuationAst::CounterSpellZoneRedirect { .. } => true,
        // CR 707.10c: recognition was already gated on a preceding CopySpell in
        // parse_followup_continuation_ast, so absorption is unconditional —
        // identical to the CounterSourceStatic precedent.
        ContinuationAst::CopyMayRetarget => true,
        ContinuationAst::SelfCostKeywordCostClarification => true,
        ContinuationAst::SearchDestination { .. } => false,
        ContinuationAst::SuspectLastCreated => matches!(current_effect, Effect::Suspect { .. }),
        ContinuationAst::GoadLastCreated { .. } => true,
        ContinuationAst::CantRegenerate => true,
        ContinuationAst::PutRest { .. } => true,
        ContinuationAst::ChooseFromExile { .. } => true,
        ContinuationAst::SearchRevealResult => true,
        ContinuationAst::SearchResultClauseHandled => true,
        ContinuationAst::PutChoiceRemainderOnBottom => true,
        ContinuationAst::ChoicePartitionDestinations { .. } => true,
        ContinuationAst::PutChosenCardsAtLibraryPosition { .. } => true,
        ContinuationAst::BecomesPlotted => true,
        ContinuationAst::BecomesForetold => true,
        ContinuationAst::EntersTappedAttacking { .. } => true,
        ContinuationAst::TokenEntersWithCounters { .. } => true,
        ContinuationAst::DigFromAmong { .. } => true,
        ContinuationAst::FaceDownProfileSpec { .. } => true,
        ContinuationAst::GrantExtraTurnAfterControlledTurn => true,
        ContinuationAst::RevealUntilKept { .. } => true,
        ContinuationAst::RevealUntilAllToZone { .. } => true,
        // Recognition was already gated on a preceding `Dig` in
        // parse_followup_continuation_ast; the "exile it [face down]" clause is
        // folded into that Dig (rewritten to ExileTop) and emits no sibling def.
        ContinuationAst::ExileLookedAtCard { .. } => true,
        // Recognition was already gated on a preceding `Dig`; the "exile one of
        // them face down" clause patches that Dig (keep_count:1 / Exile) and
        // pushes the conceal sub-ability — it emits no sibling def.
        ContinuationAst::ExileOneOfThemFaceDown => true,
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
            // CR 400.7 + CR 701.23 + CR 701.24: Name-hate compounds ("search … graveyard,
            // hand, and library … with the same name as that {creature,spell,…} and exile
            // them") lower to `ChangeZoneAll { SameNameAsParentTarget }`, not SearchLibrary
            // + SearchDestination. Suppress the generic put/exile step when the full
            // sentence matches the multi-zone same-name exile recognizer.
            if super::imperative::try_parse_multi_zone_same_name_exile(&full_lower).is_some() {
                return None;
            }
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
                // CR 110.2a: "... under your control" routes the found card to the ability controller.
                enters_under: nom_primitives::scan_contains(&full_lower, "under your control")
                    .then_some(ControllerRef::You),
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
    let (destination, enter_tapped) = parse_dig_kept_destination(lower);

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
        // CR 701.20a vs 701.20e: capture whether the stripped verb is "reveal"
        // (public) so the patch arm promotes the Dig to `reveal: true`.
        let (after_put, prefix_optional, reveal_verb) = if let Ok((rest, is_reveal)) = alt((
            value(false, tag::<_, _, OracleError<'_>>("you may put ")),
            value(true, tag("you may reveal ")),
            value(false, tag("you may return ")),
        ))
        .parse(before_milled)
        {
            (rest, true, is_reveal)
        } else if let Ok((rest, is_reveal)) = alt((
            value(false, tag::<_, _, OracleError<'_>>("put ")),
            value(true, tag("reveal ")),
            value(false, tag("return ")),
        ))
        .parse(before_milled)
        {
            (rest, false, is_reveal)
        } else {
            (before_milled, false, false)
        };

        // CR 701.17c: A mass quantifier ("put all/each creature cards milled this
        // way ...") moves the entire matching set, not a bounded count. Try it
        // before the count cascade so it lowers to a `ChangeZoneAll`.
        let (after_put_q, mass) =
            match alt((tag::<_, _, OracleError<'_>>("all "), tag("each "))).parse(after_put) {
                Ok((rest, _)) => (rest, true),
                Err(_) => (after_put, false),
            };

        let (quantity, filter_text) = if mass {
            (PutCount::All, after_put_q)
        } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("up to ").parse(after_put) {
            if let Ok((remainder, n)) = nom_primitives::parse_number.parse(rest) {
                (PutCount::Up(n), remainder.trim())
            } else {
                (PutCount::Up(1), rest)
            }
        } else if let Ok((rest, _)) =
            tag::<_, _, OracleError<'_>>("any number of ").parse(after_put)
        {
            (PutCount::AnyNumber, rest)
        } else if let Ok((rest, _)) = nom_primitives::parse_article.parse(after_put) {
            (
                if prefix_optional {
                    PutCount::Up(1)
                } else {
                    PutCount::Exactly(1)
                },
                rest,
            )
        } else if let Ok((remainder, n)) = nom_primitives::parse_number.parse(after_put) {
            (
                if prefix_optional {
                    PutCount::Up(n)
                } else {
                    PutCount::Exactly(n)
                },
                remainder.trim(),
            )
        } else {
            (
                if prefix_optional {
                    PutCount::Up(1)
                } else {
                    PutCount::Exactly(1)
                },
                after_put,
            )
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

        // CR 110.2a: "... under your control" routes the kept cards to the
        // ability controller. Scan the FULL clause — the controller phrase
        // follows "milled this way", not the filter prefix.
        let enters_under = if nom_primitives::scan_contains(lower, "under your control") {
            Some(ControllerRef::You)
        } else {
            None
        };
        // CR 708.2a + CR 708.3: "... face down ..." puts the kept cards onto the
        // battlefield face down. The default profile (vanilla 2/2) is refined by
        // a trailing "They're N/M ..." clause (`FaceDownProfileSpec`).
        let face_down_profile = if nom_primitives::scan_contains(lower, "face down") {
            Some(FaceDownProfile::vanilla_2_2())
        } else {
            None
        };

        return Some(ContinuationAst::DigFromAmong {
            quantity,
            filter,
            destination,
            rest_destination: None,
            enters_under,
            face_down_profile,
            enter_tapped,
            reveal_verb,
        });
    }

    // CR 701.20e + CR 608.2c: "put/return/reveal … <filter> from among them/those
    // cards …" — the more specific grammar; try before the bare "put N of them"
    // idiom so "any number of Equipment cards from among them" never falls into
    // the count-only arm.
    if let Ok((_, before_from)) = take_until::<_, _, OracleError<'_>>("from among").parse(lower) {
        let before_from = before_from.trim();

        // Strip leading "put " or "you may reveal " using nom combinators.
        // CR 701.20a vs 701.20e: capture whether the stripped verb was "reveal"
        // (a public action) vs "put"/"return" (a private look) so the patch arm can
        // promote the Dig to `reveal: true` even when the kept card routes to a
        // fixed destination (Fertile Thicket).
        let (after_put, reveal_verb) = alt((
            value(true, tag::<_, _, OracleError<'_>>("you may reveal ")),
            value(false, tag("you may put ")),
            value(false, tag("you may return ")),
            value(false, tag("put ")),
            value(true, tag("reveal ")),
            value(false, tag("return ")),
        ))
        .parse(before_from)
        .unwrap_or((before_from, false));

        // CR 701.20e: Mass quantifier ("put all/each <filter> from among them onto
        // the battlefield ...") moves the entire matching set → `PutCount::All`.
        let (after_put_q, mass) =
            match alt((tag::<_, _, OracleError<'_>>("all "), tag("each "))).parse(after_put) {
                Ok((rest, _)) => (rest, true),
                Err(_) => (after_put, false),
            };

        // Parse "up to N" or "a/an" or just a number
        // Delegate to nom combinator (input already lowercase from lower).
        let (quantity, filter_text) = if mass {
            (PutCount::All, after_put_q)
        } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("up to ").parse(after_put) {
            if let Ok((remainder, n)) = nom_primitives::parse_number.parse(rest) {
                (PutCount::Up(n), remainder.trim())
            } else {
                (PutCount::Up(1), rest)
            }
        } else if let Ok((rest, _)) =
            tag::<_, _, OracleError<'_>>("any number of ").parse(after_put)
        {
            (PutCount::AnyNumber, rest)
        } else if let Ok((rest, _)) = nom_primitives::parse_article.parse(after_put) {
            // "a creature card" / "an artifact card" — up_to 1 (player may choose none)
            (PutCount::Up(1), rest)
        } else if let Ok((remainder, n)) = nom_primitives::parse_number.parse(after_put) {
            // Explicit numeric count: "two creature cards" → exactly 2
            (PutCount::Exactly(n), remainder.trim())
        } else {
            (PutCount::Up(1), after_put)
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

        // CR 110.2a + CR 708.2a/708.3: detect "under your control" / "face down" on
        // the full clause for the from-among put-step.
        let enters_under = if nom_primitives::scan_contains(lower, "under your control") {
            Some(ControllerRef::You)
        } else {
            None
        };
        let face_down_profile = if nom_primitives::scan_contains(lower, "face down") {
            Some(FaceDownProfile::vanilla_2_2())
        } else {
            None
        };

        // CR 608.2c: A trailing "and the rest on the bottom ..." rider sits in the
        // SAME clause as the from-among put-step when the rest-subject ("the rest")
        // does not begin with a recognized imperative verb, so `split_clause_sequence`
        // never splits it off into a standalone `PutRest` continuation (Muxus, Goblin
        // Grandee: "Put all ... from among them onto the battlefield and the rest on
        // the bottom of your library in a random order"). Capture it here with the
        // shared rest-anaphor matcher so the rest pile is routed correctly instead of
        // falling through to the `None`→Graveyard default. A genuinely separate
        // "Put the rest ..." sentence still patches via its own PutRest continuation.
        let rest_destination = parse_of_them_rest_destination(lower);

        return Some(ContinuationAst::DigFromAmong {
            quantity,
            filter,
            destination,
            rest_destination,
            enters_under,
            face_down_profile,
            enter_tapped,
            reveal_verb,
        });
    }

    // CR 701.20e + CR 608.2c: bare "put N of them into your hand [and the rest on
    // the bottom]" — no filter, count explicit. Requires a put/reveal/return verb
    // prefix so unrelated "of them" anaphors (Gilgamesh's "attach one of them to …")
    // do not match.
    if let Ok((_, before_of)) = alt((
        take_until::<_, _, OracleError<'_>>(" of those cards"),
        take_until(" of those"),
        take_until(" of them"),
    ))
    .parse(lower)
    {
        let before_of = before_of.trim();
        if let Ok((after_put, _)) = alt((
            tag::<_, _, OracleError<'_>>("you may put "),
            tag("put "),
            tag("you may reveal "),
            tag("reveal "),
            tag("you may return "),
            tag("return "),
        ))
        .parse(before_of)
        {
            let quantity = if let Ok((_rest, _)) = alt((
                tag::<_, _, OracleError<'_>>("any number of "),
                tag("any number"),
            ))
            .parse(after_put)
            {
                PutCount::AnyNumber
            } else if all_consuming(alt((tag::<_, _, OracleError<'_>>("all"), tag("each"))))
                .parse(after_put)
                .is_ok()
            {
                PutCount::All
            } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("up to ").parse(after_put) {
                nom_primitives::parse_number
                    .parse(rest)
                    .map_or(PutCount::Up(1), |(_, n)| PutCount::Up(n))
            } else if let Ok((_, n)) = nom_primitives::parse_number.parse(after_put) {
                PutCount::Exactly(n)
            } else {
                PutCount::Up(1)
            };

            let rest_destination = parse_of_them_rest_destination(lower);

            return Some(ContinuationAst::DigFromAmong {
                quantity,
                filter: TargetFilter::Any,
                destination,
                rest_destination,
                enters_under: None,
                face_down_profile: None,
                enter_tapped,
                reveal_verb: false,
            });
        }
    }

    None
}

fn parse_dig_kept_destination(lower: &str) -> (Option<Zone>, bool) {
    if let Some(parsed) = parse_dig_from_among_destination(lower) {
        return parsed;
    }

    if let Some(parsed) = parse_milled_this_way_destination(lower) {
        return parsed;
    }

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
    (destination, false)
}

fn parse_milled_this_way_destination(lower: &str) -> Option<(Option<Zone>, bool)> {
    let (tail, _) = preceded(
        take_until::<_, _, OracleError<'_>>("milled this way"),
        tag::<_, _, OracleError<'_>>("milled this way"),
    )
    .parse(lower)
    .ok()?;
    parse_dig_destination_tail(tail)
}

fn parse_dig_from_among_destination(lower: &str) -> Option<(Option<Zone>, bool)> {
    let (tail, _) = preceded(
        take_until::<_, _, OracleError<'_>>("from among"),
        (
            tag::<_, _, OracleError<'_>>("from among "),
            alt((tag("them"), tag("those cards"), tag("those"))),
        ),
    )
    .parse(lower)
    .ok()?;
    parse_dig_destination_tail(tail)
}

fn parse_dig_destination_tail(input: &str) -> Option<(Option<Zone>, bool)> {
    // Strip a leading clause separator: "from among them, then put that card on
    // top ..." (Fertile Thicket) leaves a ", " before the "then put" verb.
    let input = input.trim_start();
    let (input, _) = opt(tag::<_, _, OracleError<'_>>(",")).parse(input).ok()?;
    let input = input.trim_start();
    let (input, _) = opt(alt((tag::<_, _, OracleError<'_>>("and "), tag("then "))))
        .parse(input)
        .ok()?;
    let input = input.trim_start();
    let (input, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("put it "),
        tag("put them "),
        tag("put that card "),
        tag("put those cards "),
        tag("put the card "),
        tag("return it "),
        tag("return them "),
        tag("return that card "),
    )))
    .parse(input)
    .ok()?;
    let input = input.trim_start();

    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("onto the battlefield"),
        tag("to the battlefield"),
    ))
    .parse(input)
    {
        let (_, tapped) = opt(tag::<_, _, OracleError<'_>>(" tapped"))
            .parse(rest)
            .ok()?;
        return Some((Some(Zone::Battlefield), tapped.is_some()));
    }

    if alt((
        tag::<_, _, OracleError<'_>>("into your hand"),
        tag("into their hand"),
        tag("to your hand"),
        tag("to their hand"),
    ))
    .parse(input)
    .is_ok()
    {
        return Some((Some(Zone::Hand), false));
    }

    // CR 401.4: cards put at a specific library position (top) are arranged by
    // owner; the DigChoice resolver inserts the kept card at index 0 and the
    // rest at the bottom. Mirrors `parse_put_one_dig_card_on_top` (the
    // unfiltered "back on top" phrasing) for the "reveal ... then put that card
    // on top of your library" form (Fertile Thicket).
    if alt((
        tag::<_, _, OracleError<'_>>("on top of your library"),
        tag("on top of their library"),
        tag("on top"),
    ))
    .parse(input)
    .is_ok()
    {
        return Some((Some(Zone::Library), false));
    }

    None
}

/// CR 708.2a + CR 205.1a: Parse a "They're N/M [types] [subtypes] creatures."
/// sentence into a `FaceDownProfile`. This is the "otherwise specified" override
/// for cards put onto the battlefield face down (Cyber-Controller: "They're 2/2
/// Cyberman artifact creatures."). Returns `None` when the sentence is not a
/// they're-characteristics clause.
///
/// Built entirely from typed combinators (no card-named hardcode): the P/T comes
/// from `parse_pt_value`, extra core types from a `tag` alt over the noncreature
/// core-type words (Creature is implicit per CR 708.2a), and subtypes from the
/// shared `oracle_util::parse_subtype` canonical-subtype matcher. Terminates on
/// "creature"/"creatures" + optional period.
pub(super) fn parse_theyre_face_down_profile(lower: &str) -> Option<FaceDownProfile> {
    // CR 205.1a: "They're / They are <characteristics> creatures."
    let (mut rest, _) = alt((tag::<_, _, OracleError<'_>>("they're "), tag("they are ")))
        .parse(lower)
        .ok()?;

    // CR 208.1: Optional N/M power/toughness. `parse_pt_value` returns
    // `(PtValue, PtValue)`; only literal `Fixed` values map to a profile P/T —
    // a `*`/`X` face-down P/T is not a supported "specified" characteristic.
    let (power, toughness) = match nom_primitives::parse_pt_value(rest) {
        Ok((after_pt, (p, t))) => {
            let power = match p {
                PtValue::Fixed(n) => Some(n),
                _ => return None,
            };
            let toughness = match t {
                PtValue::Fixed(n) => Some(n),
                _ => return None,
            };
            rest = after_pt.trim_start();
            (power, toughness)
        }
        Err(_) => (None, None),
    };

    // CR 205.1a: Loop over the characteristic words between the P/T and the
    // terminating "creature(s)". Extra core types (artifact/enchantment/land/
    // planeswalker — NOT creature, which is always present per CR 708.2a) and
    // creature subtypes ("Cyberman") accumulate; the loop ends at "creature(s)".
    let mut extra_core_types: Vec<CoreType> = Vec::new();
    let mut subtypes: Vec<String> = Vec::new();
    loop {
        rest = rest.trim_start();
        // Terminator: "creature" / "creatures" then optional ".".
        if let Ok((after, _)) =
            alt((tag::<_, _, OracleError<'_>>("creatures"), tag("creature"))).parse(rest)
        {
            let after = after.trim_start();
            let after = opt(tag::<_, _, OracleError<'_>>("."))
                .parse(after)
                .map_or(after, |(r, _)| r);
            if !after.trim().is_empty() {
                // Trailing text we didn't consume → not a clean they're clause.
                return None;
            }
            return Some(FaceDownProfile {
                power,
                toughness,
                // CR 708.2a: "They're ... creatures." is always a creature body —
                // Creature is implicit and `extra_core_types` layer on top.
                body: crate::types::ability::FaceDownBody::Creature,
                extra_core_types,
                subtypes,
                ward: None,
            });
        }
        // Extra core type word (Creature excluded — always implicit).
        if let Ok((after, ct)) = alt((
            value(CoreType::Artifact, tag::<_, _, OracleError<'_>>("artifact")),
            value(CoreType::Enchantment, tag("enchantment")),
            value(CoreType::Land, tag("land")),
            value(CoreType::Planeswalker, tag("planeswalker")),
        ))
        .parse(rest)
        {
            // Guard against matching a prefix of a longer word (word boundary).
            let boundary_ok = after.is_empty() || after.starts_with(char::is_whitespace);
            if boundary_ok {
                if !extra_core_types.contains(&ct) {
                    extra_core_types.push(ct);
                }
                rest = after;
                continue;
            }
        }
        // Creature subtype ("Cyberman", "Spirit", ...).
        if let Some((canonical, consumed)) = crate::parser::oracle_util::parse_subtype(rest) {
            subtypes.push(canonical);
            rest = &rest[consumed..];
            continue;
        }
        // Unrecognized token before "creature(s)" → not a parseable profile.
        return None;
    }
}

/// CR 708.2a + CR 205.1a: Parse the singular "It's a/an <characteristics>
/// <core-type-noun>." face-down characteristic clause for a permanent put onto
/// the battlefield face down (Yedora, Grave Gardener: "It's a Forest land.").
/// Also accepts the "It becomes a/an ..." copula variant used after a
/// turn-face-down effect (Mondassian Colony Ship: "Turn target creature face
/// down. It becomes a 2/2 Cyberman artifact creature."); both copulas describe
/// the same CR 205.1a characteristic-setting and refine the same face-down
/// profile via the `FaceDownProfileSpec` continuation. Returns `None` when the
/// sentence is not an it's/it-becomes-characteristics clause.
///
/// Built entirely from typed combinators (no card-named hardcode), mirroring
/// `parse_theyre_face_down_profile` for the plural creature form: optional N/M
/// P/T, then accumulated extra core types and subtypes, terminating on a
/// core-type noun. The terminating noun chooses the body — "creature(s)" →
/// `FaceDownBody::Creature` (Creature implicit, 2/2 default), any other core
/// type ("land", "artifact", ...) → `FaceDownBody::Noncreature` with that core
/// type explicit and no power/toughness (CR 208.1).
pub(super) fn parse_its_face_down_profile(lower: &str) -> Option<FaceDownProfile> {
    // CR 205.1a: "It's a / It is a / It becomes a <characteristics> <core-type>."
    // The "becomes" copula is the turn-face-down variant (Mondassian Colony
    // Ship); it sets the same characteristics as the "is" copula (Cyber
    // Conversion).
    let (mut rest, _) = alt((
        tag::<_, _, OracleError<'_>>("it's an "),
        tag("it's a "),
        tag("it is an "),
        tag("it is a "),
        tag("it becomes an "),
        tag("it becomes a "),
    ))
    .parse(lower)
    .ok()?;

    // CR 208.1: optional leading N/M power/toughness ("It's a 0/0 ... creature.").
    let (power, toughness) = match nom_primitives::parse_pt_value(rest) {
        Ok((after_pt, (PtValue::Fixed(p), PtValue::Fixed(t)))) => {
            rest = after_pt.trim_start();
            (Some(p), Some(t))
        }
        // A non-literal (`*`/`X`) face-down P/T is not a "specified" characteristic.
        Ok(_) => return None,
        Err(_) => (None, None),
    };

    // Accumulate extra core types and subtypes until a terminating core-type
    // noun. Creature is the only core type that maps to a creature body; every
    // other core-type noun ("land", "artifact", ...) terminates a non-creature
    // body whose core types come entirely from the effect.
    let mut extra_core_types: Vec<CoreType> = Vec::new();
    let mut subtypes: Vec<String> = Vec::new();
    loop {
        rest = rest.trim_start();
        // Non-terminal extra core type word (e.g. "artifact creature"). A
        // core-type word followed by a space is a *modifier*, not the clause's
        // terminating noun, so it must be tried BEFORE the bare-noun terminator
        // below. Otherwise "artifact creature" matches the terminal
        // `tag("artifact")`, leaves a non-empty " creature" tail, and rejects
        // the whole clause (the latent bug behind Missy / Cyber Conversion's
        // "It's a 2/2 Cyberman artifact creature.").
        if let Ok((after, ct)) = alt((
            value(
                CoreType::Artifact,
                tag::<_, _, OracleError<'_>>("artifact "),
            ),
            value(CoreType::Enchantment, tag("enchantment ")),
            value(CoreType::Land, tag("land ")),
            value(CoreType::Planeswalker, tag("planeswalker ")),
        ))
        .parse(rest)
        {
            if !extra_core_types.contains(&ct) {
                extra_core_types.push(ct);
            }
            rest = after;
            continue;
        }
        // Terminator: a core-type noun then optional ".". The terminal noun maps
        // directly to its `CoreType` — Creature is the only one that yields a
        // creature body; every other core type ("land", "artifact", ...)
        // terminates a non-creature body whose core types come from the effect.
        if let Ok((after, terminal)) = alt((
            value(
                CoreType::Creature,
                alt((tag::<_, _, OracleError<'_>>("creatures"), tag("creature"))),
            ),
            value(CoreType::Land, tag("land")),
            value(CoreType::Artifact, tag("artifact")),
            value(CoreType::Enchantment, tag("enchantment")),
            value(CoreType::Planeswalker, tag("planeswalker")),
        ))
        .parse(rest)
        {
            let after = after.trim_start();
            let after = opt(tag::<_, _, OracleError<'_>>("."))
                .parse(after)
                .map_or(after, |(r, _)| r);
            if !after.trim().is_empty() {
                return None;
            }
            return match terminal {
                // "... creature(s)." — creature body, P/T defaults to 2/2.
                CoreType::Creature => Some(FaceDownProfile {
                    power,
                    toughness,
                    body: FaceDownBody::Creature,
                    extra_core_types,
                    subtypes,
                    ward: None,
                }),
                // "... land/artifact/enchantment/planeswalker." — non-creature
                // body whose core type is the terminal noun; no implicit
                // Creature, no power/toughness.
                ct => {
                    if power.is_some() || toughness.is_some() {
                        return None;
                    }
                    if !extra_core_types.contains(&ct) {
                        extra_core_types.push(ct);
                    }
                    Some(FaceDownProfile {
                        power: None,
                        toughness: None,
                        body: FaceDownBody::Noncreature,
                        extra_core_types,
                        subtypes,
                        ward: None,
                    })
                }
            };
        }
        // Subtype (land type "Forest", creature type "Spirit", ...).
        if let Some((canonical, consumed)) = crate::parser::oracle_util::parse_subtype(rest) {
            subtypes.push(canonical);
            rest = &rest[consumed..];
            continue;
        }
        // Unrecognized token before a core-type noun → not a parseable profile.
        return None;
    }
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
        // CR 406.3: turning the exiled card face up is its own resolving effect,
        // not a Dig-lookback-transparent clause.
        Effect::TurnFaceUp { .. } => false,
        // CR 708.2a: turning a permanent face down is its own resolving effect,
        // not a Dig-lookback-transparent clause.
        Effect::TurnFaceDown { .. } => false,
        Effect::StartYourEngines { .. }
        | Effect::EpicCopy { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::DealDamage { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::CounterAll { .. }
        | Effect::Token { .. }
        | Effect::GainLife { .. }
        | Effect::LoseLife { .. }
        // CR 701.26a/b: all tap/untap scopes are treated identically here.
        | Effect::SetTapState { .. }
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
        | Effect::GainControlAll { .. }
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
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::ProliferateTarget { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Populate
        | Effect::Clash
        | Effect::Vote { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::Meld { .. }
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
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
        | Effect::Unsuspect { .. }
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::ForceAttack { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::SetClassLevel { .. }
        | Effect::CreateDelayedTrigger { .. }
        | Effect::AddTargetReplacement { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::CreateEmblem { .. }
        | Effect::CastFromZone { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard
        | Effect::PreventDamage { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CreateDrawReplacement { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RollDie { .. }
        | Effect::FlipCoin { .. }
        | Effect::FlipCoins { .. }
        | Effect::FlipCoinUntilLose { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::Planeswalk
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::PutSticker { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::RememberCard { .. }
        | Effect::ForEachCategoryExile { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::Heist { .. }
        | Effect::HeistExile
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Cloak { .. }
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
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::Conjure { .. }
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::Intensify { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::ChooseOneOf { .. }
        // CR 614.12 + CR 303.4: Return-as-Aura is its own emitted sub-effect
        // following a `ChangeZone`; it is not a lookback-transparent clause
        // for the Dig-from-among continuation search.
        | Effect::ReturnAsAura { .. }
        | Effect::Specialize
        | Effect::Unimplemented { .. } => false,
    }
}

/// CR 702.34a / CR 702.128a / CR 702.180a: Recognize the redundant cost
/// clarification sentence that trails a self-cost graveyard keyword grant —
/// "the/its [flashback|embalm|harmonize] cost is equal to its/that card's mana
/// cost". The caller gates on the preceding GenericEffect carrying the matching
/// granted keyword, so this combinator only needs to confirm the grammatical
/// shape. Composed from chained `alt()` over the determiner, the self-cost
/// keyword token, and the possessive reference — not an enumeration of
/// whole-string permutations.
fn parse_self_cost_keyword_clarification(lower: &str) -> bool {
    let lower = lower.trim().trim_end_matches('.');
    all_consuming((
        opt(alt((tag::<_, _, OracleError<'_>>("the "), tag("its ")))),
        alt((tag("flashback"), tag("embalm"), tag("harmonize"))),
        tag(" cost is equal to "),
        alt((tag("its"), tag("that card's"), tag("that card\u{2019}s"))),
        tag(" mana cost"),
    ))
    .parse(lower)
    .is_ok()
}

/// CR 205.2a + CR 205.3a: Strip the moved-object subject prefix ("that card is
/// a/an", "it's a/an", "it is a/an") from a recognized card-type / subtype
/// condition clause, returning the type tail (`"enchantment card"`, a card type
/// per CR 205.2a; subtype tails per CR 205.3a). Only the affirmative
/// subjects are listed: a negated clause ("that card is not a/an") has no
/// matching prefix and falls through to `None`, so the caller declines to
/// emit a gate it cannot represent (the moved-object filter is affirmative).
fn strip_moved_object_subject(clause: &str) -> Option<&str> {
    alt((
        tag::<_, _, OracleError<'_>>("that card is a "),
        tag("that card is an "),
        tag("it's a "),
        tag("it's an "),
        tag("it is a "),
        tag("it is an "),
    ))
    .parse(clause.trim_start())
    .ok()
    .map(|(rest, _)| rest)
}

/// CR 614.12: recognize the optional leading moved-object type condition on a
/// ChangeZone "enters tapped and attacking" followup (Summoner's Grimoire:
/// "If that card is an enchantment card, it enters tapped and attacking").
///
/// Returns the continuation to emit, or `None` to emit NOTHING:
/// - no leading "if"  -> `EntersTappedAttacking { moved_filter: None }`
///   (Stangg / Shark Shredder; byte-identical to the prior unconditional patch).
/// - recognized moved-object card-type condition -> `{ moved_filter: Some(..) }`.
/// - leading "if" present but NOT a recognized, materializable affirmative
///   card-type condition -> `None`. Because `apply_clause_continuation` sets
///   both riders UNCONDITIONALLY whenever this continuation is emitted, the only
///   correct behavior for an unrepresentable condition is to emit nothing,
///   leaving the bare " if " for the honest `Swallow:Condition_If` diagnostic.
fn parse_change_zone_enters_tapped_attacking(
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ContinuationAst> {
    let trimmed = lower.trim_start();
    // No leading "if": unconditional riders (unchanged behavior).
    let Ok((after_if, _)) = tag::<_, _, OracleError<'_>>("if ").parse(trimmed) else {
        return Some(ContinuationAst::EntersTappedAttacking { moved_filter: None });
    };
    // CR 614.12: the condition clause runs up to the first comma
    // ("that card is an enchantment card" | ", it enters tapped and attacking").
    let (_, condition_clause) = take_until::<_, _, OracleError<'_>>(",")
        .parse(after_if)
        .ok()?;
    // Single condition-recognition authority: confirm this is a recognized
    // moved-object card-type condition before materializing a filter.
    let recognized = matches!(
        super::conditions::try_nom_condition_as_ability_condition(condition_clause, ctx),
        Some(
            AbilityCondition::RevealedHasCardType { .. }
                | AbilityCondition::TargetMatchesFilter { .. }
        )
    );
    if !recognized {
        return None;
    }
    // N-D: materialize TargetFilter::Typed via parse_type_phrase on the type
    // tail — RevealedHasCardType carries no TargetFilter, so reuse the canonical
    // materializer rather than reconstructing from the recognized condition.
    let type_tail = strip_moved_object_subject(condition_clause)?;
    let (filter, leftover) = crate::parser::oracle_target::parse_type_phrase(type_tail);
    if matches!(filter, TargetFilter::Any | TargetFilter::None) || !leftover.trim().is_empty() {
        return None;
    }
    Some(ContinuationAst::EntersTappedAttacking {
        moved_filter: Some(filter),
    })
}

/// CR 614.12: returns true when `sentence` is a moved-object type-conditional
/// enters-modifier clause — a leading "if that card is a/an <type>" (or
/// "it's/it is a/an <type>") gating an "enters tapped and attacking" tail
/// (Summoner's Grimoire). Shared with the `Condition_If` swallow detector so the
/// represented clause can be located and stripped text-scoped, reusing the same
/// leading-condition combinators (`strip_moved_object_subject` +
/// `parse_type_phrase`) that `parse_change_zone_enters_tapped_attacking` uses —
/// not a verbatim Oracle-string match.
pub(crate) fn is_moved_object_enters_modifier_clause(sentence: &str) -> bool {
    let lower = sentence.to_lowercase();
    let lower = lower.trim();
    if !(nom_primitives::scan_contains(lower, "enters tapped and attacking")
        || nom_primitives::scan_contains(lower, "enter tapped and attacking"))
    {
        return false;
    }
    let Ok((after_if, _)) = tag::<_, _, OracleError<'_>>("if ").parse(lower) else {
        return false;
    };
    let Some(type_tail) = strip_moved_object_subject(after_if) else {
        return false;
    };
    let (filter, _leftover) = crate::parser::oracle_target::parse_type_phrase(type_tail);
    !matches!(filter, TargetFilter::Any | TargetFilter::None)
}

pub(super) fn parse_followup_continuation_ast(
    text: &str,
    previous_effect: &Effect,
    ctx: &mut ParseContext,
) -> Option<ContinuationAst> {
    let lower = text.to_lowercase();
    let face_down_profile_spec =
        parse_theyre_face_down_profile(&lower).or_else(|| parse_its_face_down_profile(&lower));

    match previous_effect {
        Effect::ChooseAndSacrificeRest { .. } => parse_choose_and_sacrifice_rest_followup(&lower),
        Effect::SearchLibrary { split: Some(_), .. }
            if super::search::is_zone_pair_search_split_clause(&lower) =>
        {
            Some(ContinuationAst::SearchResultClauseHandled)
        }
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
            // CR 106.6: Only absorb a parsed spend restriction when at least one of
            // its branches is payable at a reachable production payment site (see
            // `ManaSpendRestriction::has_payable_branch`). An all-dead restriction
            // (every branch's runtime gate is hardcoded-false or never reached — X
            // costs, face-down casts, turn-face-up) is deliberately left unabsorbed
            // here so this `Effect::Mana` line lowers to `Effect::Unimplemented`
            // (honest coverage red) instead of masquerading as supported. Liveness
            // is decided once on the fully assembled restriction, so a mixed
            // disjunction keeps its live branches (the `Any` short-circuit) rather
            // than being dropped wholesale. A dropped all-dead restriction also
            // drops any paired `grants`; that is intentional (no real card pairs a
            // grant with an all-dead restriction).
            if let Some((restriction, grants)) = super::mana::parse_mana_spend_restriction(&lower) {
                if restriction.has_payable_branch() {
                    return Some(ContinuationAst::ManaRestriction {
                        restriction,
                        grants,
                    });
                }
            }
            // CR 106.6: "that spell can't be countered" as a standalone clause
            // after comma-splitting from the restriction text.
            if let Some(grants) = super::mana::parse_mana_spell_grant(&lower) {
                return Some(ContinuationAst::ManaGrant { grants });
            }
            None
        }
        // CR 702.34a / CR 702.128a / CR 702.180a: the redundant cost-clarification
        // sentence ("The/Its [flashback|embalm|harmonize] cost is equal to
        // its/that card's mana cost") that follows a self-cost graveyard keyword
        // grant. The grant already carries `ManaCost::SelfManaCost`, so this
        // sentence adds no semantics — absorb it so it never lowers to
        // `Effect::Unimplemented`. Gated on the preceding GenericEffect actually
        // carrying a self-cost graveyard keyword (Flashback/Embalm/Harmonize).
        Effect::GenericEffect {
            static_abilities, ..
        } if parse_self_cost_keyword_clarification(&lower)
            && static_abilities.iter().any(|def| {
                def.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        crate::types::ability::ContinuousModification::AddKeyword {
                            keyword: crate::types::keywords::Keyword::Flashback(_)
                                | crate::types::keywords::Keyword::Embalm(_)
                                | crate::types::keywords::Keyword::Harmonize(_)
                        }
                    )
                })
            }) =>
        {
            Some(ContinuationAst::SelfCostKeywordCostClarification)
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
        // CR 701.6a + CR 614.1a: "If that spell is countered this way, put it
        // <zone> instead of into that player's graveyard." (Memory Lapse,
        // Remand, Spell Crumple). The `scan_contains` pre-guard mirrors the
        // source-rider arms above; the real classification is the combinator
        // recognizer, which returns `None` for unsupported destinations
        // (Hinder, Transcendent Dragon) so those cards stay honestly gapped.
        Effect::Counter { .. }
            if nom_primitives::scan_contains(&lower, "countered this way") => // allow-noncombinator
        {
            recognize_counter_spell_zone_redirect(&lower)
                .map(|destination| ContinuationAst::CounterSpellZoneRedirect { destination })
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
        // CR 702.75a + CR 406.3: "exile one of them face down" after a private
        // `Dig` (the "look at the top N cards of <player>'s library" look step)
        // — the Gonti, Lord of Luxury class. The controller selects ONE of the N
        // looked-at cards and exiles it face down. Patches the `Dig` into the
        // Hideaway shape (keep_count: 1, destination: Exile) + chains a
        // `HideawayConceal` so the player-selected dug card is the one exiled
        // face down and linked to the source — NOT a sibling
        // `ChangeZone { ParentTarget }`, which exiled the trigger source itself.
        // `reveal: false` scopes this to the private look form.
        Effect::Dig { reveal: false, .. } if parse_exile_one_of_them_face_down(&lower) => {
            Some(ContinuationAst::ExileOneOfThemFaceDown)
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
                quantity: PutCount::Up(1),
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::NameMatchesAnyPermanent { controller: None },
                ])),
                destination: Some(Zone::Battlefield),
                rest_destination: None,
                enters_under: None,
                face_down_profile: None,
                enter_tapped: false,
                // "put one of those cards onto the battlefield" — a put, not a reveal.
                reveal_verb: false,
            })
        }
        // "You may put one of those cards back on top of your library" after
        // Dig — keep up to one looked-at card on top, leaving the remainder
        // for a following rest-placement clause.
        Effect::Dig { .. } if parse_put_one_dig_card_on_top(&lower) => {
            Some(ContinuationAst::DigFromAmong {
                quantity: PutCount::Up(1),
                filter: TargetFilter::Any,
                destination: Some(Zone::Library),
                rest_destination: None,
                enters_under: None,
                face_down_profile: None,
                enter_tapped: false,
                // "put one ... back on top" — a put, not a reveal.
                reveal_verb: false,
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
            if parse_becomes_foretold_continuation(&lower) =>
        {
            Some(ContinuationAst::BecomesForetold)
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
        // CR 701.20a + CR 608.2c: "Put any number of those [filter] cards onto the
        // battlefield, then put the rest … on the bottom … in a random order"
        // (Aurora Awakener). This is the multi-match disposition over the *set* of
        // matched cards: the controller chooses any subset for the battlefield and
        // every other revealed card goes to the rest pile. Absorbs into
        // `RevealUntilDisposition::ChooseAnyNumber` via the `any_number` flag.
        // Checked before "put that card" because "any number of those" is a
        // distinct disposition and never contains the singular "that card".
        Effect::RevealUntil { .. }
            if nom_primitives::scan_contains(&lower, "put any number of those")
                || nom_primitives::scan_contains(&lower, "puts any number of those") =>
        {
            // CR 701.20a: the only destination this disposition currently targets
            // is the battlefield ("put any number of those permanent cards onto
            // the battlefield"); a hand variant would slot in here identically.
            let (destination, enter_tapped, enters_attacking) =
                if nom_primitives::scan_contains(&lower, "onto the battlefield") {
                    (
                        Zone::Battlefield,
                        nom_primitives::scan_contains(&lower, "tapped"),
                        nom_primitives::scan_contains(&lower, "attacking"),
                    )
                } else {
                    (Zone::Hand, false, false)
                };
            let rest_destination = parse_reveal_until_rest_zone(&lower);
            // "under your control" stamps the controller of the kept cards; absent
            // the clause they enter under the revealing player's control by default.
            // Mirrors the singular "put that card" arm so the set-disposition path
            // inherits the same enters-under building block.
            let enters_under = if nom_primitives::scan_contains(&lower, "under your control") {
                Some(ControllerRef::You)
            } else {
                None
            };
            Some(ContinuationAst::RevealUntilKept {
                destination,
                enter_tapped,
                enters_attacking,
                any_number: true,
                rest_destination,
                enters_under,
                optional_decline: None,
            })
        }
        // CR 701.20a: "put that card into your hand / onto the battlefield" after RevealUntil
        // — overrides kept_destination. Also extracts rest_destination from a compound
        // rest clause merged on "and" (suppressed split because the rest-subject — "the
        // rest", "all other cards", "the other cards" — does not start with a recognized
        // imperative verb). Both bare imperative ("put that card", second-person
        // reveal-until) and third-person ("the player puts that card",
        // Polymorph / Proteus Staff / Transmogrify) forms are accepted.
        //
        // Plural filtered kept clauses ("put those land cards onto the battlefield tapped",
        // The Ring Goes South) use the same RevealUntilKept patch — checked before the
        // RevealUntilAllToZone arm because "those land cards" is not a "those cards"
        // substring and must not fall through with the default Hand kept destination.
        Effect::RevealUntil { .. }
            if nom_primitives::scan_contains(&lower, "put that card")
                || nom_primitives::scan_contains(&lower, "puts that card")
                || nom_primitives::scan_contains(&lower, "put it")
                || nom_primitives::scan_contains(&lower, "puts it")
                || ((nom_primitives::scan_contains(&lower, "put those")
                    || nom_primitives::scan_contains(&lower, "puts those"))
                    && nom_primitives::scan_contains(&lower, "onto the battlefield")) =>
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
            let enters_under = if nom_primitives::scan_contains(&lower, "under your control") {
                Some(ControllerRef::You)
            } else {
                None
            };
            Some(ContinuationAst::RevealUntilKept {
                destination,
                enter_tapped,
                enters_attacking,
                any_number: false,
                rest_destination: rest,
                enters_under,
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
        Effect::ChooseFromZone { .. } if parse_put_rest_on_bottom_line(&lower).is_ok() => {
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
                | "exile them"
                | "exile them face down"
                | "exile those cards"
                | "exile those cards face down"
        ) =>
        {
            Some(ContinuationAst::SearchResultClauseHandled)
        }
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Hand,
            ..
        } if matches_put_rest_on_bottom_line(&lower) =>
        {
            Some(ContinuationAst::PutChoiceRemainderOnBottom)
        }
        // CR 708.2a + CR 205.1a: "They're N/M [types] [subtypes] creatures." (plural,
        // Cyber-Controller) or "It's a/an [types] [subtype] <core-type>." (singular,
        // Yedora's "It's a Forest land.") after a put-face-down clause (the
        // preceding Mill/ChangeZone/ChangeZoneAll carries `face_down_profile`).
        // Refines the face-down profile with the specified characteristics. Placed
        // BEFORE the broad Mill/Dig from-among arm so it claims the spec sentence.
        Effect::Mill { .. }
        | Effect::ChangeZone { .. }
        | Effect::ChangeZoneAll { .. }
        // CR 701.40a + CR 708.2a: "put the top N ... onto the battlefield face
        // down" lowers to `Effect::Manifest` (Cybership), seeded with a
        // `Some(_)` profile by the put-clause. The trailing "They're 2/2 Cyberman
        // artifact creatures." spec refines that seed via the back-walk patcher.
        | Effect::Manifest { profile: Some(_), .. }
        // CR 708.2a + CR 205.1a: "Turn target creature face down. It's a 2/2
        // Cyberman artifact creature." (Cyber Conversion) — the seeded vanilla
        // 2/2 profile on the preceding `TurnFaceDown` is refined by this spec
        // sentence via the back-walk patcher.
        | Effect::TurnFaceDown { profile: Some(_), .. }
            if face_down_profile_spec.is_some() =>
        {
            let profile = face_down_profile_spec.clone()?;
            Some(ContinuationAst::FaceDownProfileSpec { profile })
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
        // after CopyTokenOf or Token. Tokens/copies always enter
        // unconditionally — no moved-object type gate applies.
        Effect::CopyTokenOf { .. } | Effect::Token { .. }
            if nom_primitives::scan_contains(&lower, "enters tapped and attacking")
                || nom_primitives::scan_contains(&lower, "enter tapped and attacking") =>
        {
            Some(ContinuationAst::EntersTappedAttacking { moved_filter: None })
        }
        // CR 508.4 / CR 614.1 / CR 614.12: the same followup after a ChangeZone,
        // but the riders may be gated on the MOVED object's type by a leading
        // "if that card is a/an <type>" (Summoner's Grimoire). Condition-aware so
        // the leading clause is no longer silently dropped (Swallow:Condition_If).
        Effect::ChangeZone { .. }
            if nom_primitives::scan_contains(&lower, "enters tapped and attacking")
                || nom_primitives::scan_contains(&lower, "enter tapped and attacking") =>
        {
            parse_change_zone_enters_tapped_attacking(&lower, ctx)
        }
        // CR 701.15a + CR 701.15b: "The token(s) (is|are) goaded [duration]" after token creation.
        Effect::CopyTokenOf { .. } | Effect::Token { .. } | Effect::Populate
            if let Some(continuation) = try_parse_tokens_goaded_continuation(&lower) =>
        {
            Some(continuation)
        }
        Effect::ControlNextTurn { .. }
            if nom_primitives::scan_contains(&lower, "after that turn")
                && nom_primitives::scan_contains(&lower, "takes an extra turn") =>
        {
            Some(ContinuationAst::GrantExtraTurnAfterControlledTurn)
        }
        // CR 701.19c + CR 608.2c: "Creatures/A creature destroyed this way
        // can't be regenerated" after any effect — including Token creation
        // (e.g. Kirtar's Wrath threshold: DestroyAll → Token → this clause).
        // Must be checked before the Effect::Token arm so a Token preceding
        // this phrase doesn't shadow the catch-all guard. The Destroy/DestroyAll
        // target is found by `apply_clause_continuation` walking backward.
        _ if nom_primitives::scan_contains(&lower, "destroyed this way can't be regenerated")
            || nom_primitives::scan_contains(
                &lower,
                "destroyed this way cannot be regenerated",
            ) =>
        {
            Some(ContinuationAst::CantRegenerate)
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
    let (input, filter) =
        alt((parse_nonland_permanent_domain, parse_creature_domain)).parse(input)?;
    Ok((input, Some(filter)))
}

/// CR 701.21a: Parse "creatures [they/you/that player] control[s]" — the sacrifice
/// domain for Slaughter the Strong's "sacrifices all other creatures they control".
fn parse_creature_domain(input: &str) -> Result<(&str, TargetFilter), nom::Err<OracleError<'_>>> {
    let (input, _) = tag::<_, _, OracleError<'_>>("creatures ").parse(input)?;
    let (input, _) = alt((
        tag::<_, _, OracleError<'_>>("they control"),
        tag("you control"),
        tag("that player controls"),
    ))
    .parse(input)?;
    Ok((input, TargetFilter::Typed(TypedFilter::creature())))
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

/// CR 701.15a + CR 701.15b: Parse "the token(s) (is|are) goaded [duration]" after token creation.
/// Prefix stripping mirrors `rewrite_token_created_this_way_unimplemented` so the
/// predicate (`are goaded`) stays in the remainder for duration stripping.
fn try_parse_tokens_goaded_continuation(lower: &str) -> Option<ContinuationAst> {
    let lower = lower.trim().trim_end_matches('.');
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("the tokens created this way "),
        tag("the token created this way "),
        tag("the tokens "),
        tag("the token "),
    ))
    .parse(lower)
    .ok()?;
    let (mod_text, duration) = super::lower::strip_trailing_duration(rest.trim());
    let mods = crate::parser::oracle_static::parse_continuous_modifications(mod_text);
    if !mods.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::AddStaticMode {
                mode: StaticMode::Goaded
            }
        )
    }) {
        return None;
    }
    Some(ContinuationAst::GoadLastCreated { duration })
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
/// CR 122.6a + CR 301.5b: Returns true when the counter followup consumed the
/// entire clause. A trailing `"and attach …"` conjunct (Fractal Harness) must
/// not be absorbed here — the bare-and splitter needs to peel it into its own
/// attach clause.
fn token_counter_followup_tail_is_clean(rest: &str) -> bool {
    rest.trim().trim_start_matches(['.', ' ']).is_empty()
}

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
    if let Ok((remainder, (counter_type, count))) =
        super::parse_dynamic_counter_suffix_body(rest.trim_end_matches('.').trim_end())
    {
        if !token_counter_followup_tail_is_clean(remainder) {
            return None;
        }
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

    if quantity.is_none() && !token_counter_followup_tail_is_clean(rest) {
        return None;
    }

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

/// Parse a trailing evergreen keyword grant list (e.g. "deathtouch and lifelink").
/// The input must start at the first keyword; returns unconsumed tail.
pub(super) fn parse_keyword_grant_list(input: &str) -> Option<(Vec<Keyword>, &str)> {
    let (rest, keywords) = parse_keyword_list(input).ok()?;
    if keywords.is_empty() {
        return None;
    }
    Some((keywords, rest))
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

/// CR 608.2c: Parse a counter-class keyword-list continuation —
/// "Repeat this process for <keyword list>." (Kathril, Aspect Warper) or
/// "Do the same for <keyword list>." (Super-Adaptoid). Returns the keyword
/// list; the chunk loop wraps it in `SpecialClause::RepeatProcessForKeywords`
/// and lowering replicates the antecedent conditional keyword-counter clause
/// once per keyword. Both phrasings are leaf-level variants of the same
/// "replicate the prior keyword-counter clause for each listed keyword"
/// directive, so they share one combinator and one `SpecialClause`. Mirrors
/// `try_parse_same_is_true_continuation`; covers every card of this class, not
/// Kathril or Super-Adaptoid alone.
pub(super) fn try_parse_repeat_process_for_keywords(text: &str) -> Option<Vec<Keyword>> {
    let lower = text.to_lowercase();
    let (keywords, rest) = nom_on_lower(text, &lower, |i| {
        let (i, _) = alt((
            tag::<_, _, OracleError<'_>>("repeat this process for "),
            tag("do the same for "),
        ))
        .parse(i)?;
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

/// CR 608.2c + CR 601.2c: Parse "[then] target opponent does the same / does so."
/// — an effect-replication directive (The Wedding of River Song). The clause has
/// no effect of its own; it *would* replicate the immediately-preceding sibling
/// clause for a targeted opponent. NOT YET IMPLEMENTED: there is no
/// `SpecialClause::DoesTheSame` variant and no antecedent-cloning lowering. The
/// chunk loop instead emits a *documented* `Effect::Unimplemented` (keyed by
/// `does_the_same_unimplemented_name`) for a recognized match — a typed
/// strict-failure, never a silent misparse. The future fix (a `DoesTheSame`
/// special clause whose lowering clones the antecedent action and retargets it
/// onto the opponent's own objects, CR 608.2d) is tracked as backlog work; the
/// typed `DoesTheSameSubject` return is preserved so it can slot in cleanly.
///
/// Combinators only — `opt`/`value`/`tag`/`alt`. The whole chunk must be consumed
/// (modulo a trailing period) so unrelated phrases ("… at the same time", "does
/// the same for enchantment cards") fall through to normal dispatch rather than
/// being silently swallowed. The "each opponent … does the same" player-set
/// fanout is a deferred sibling of `DoesTheSameSubject` (see its doc comment).
pub(super) fn try_parse_does_the_same_clause(text: &str) -> Option<DoesTheSameSubject> {
    let lower = text.to_lowercase();
    let (subject, rest) = nom_on_lower(text, &lower, |i| {
        let (i, _) = opt(tag("then ")).parse(i)?;
        let (i, subject) =
            value(DoesTheSameSubject::TargetOpponent, tag("target opponent")).parse(i)?;
        // Prefix nesting (sum, not product): " do" + optional "es" +
        // (" the same" | " so") covers does/do × the-same/so.
        let (i, _) = tag(" do").parse(i)?;
        let (i, _) = opt(tag("es")).parse(i)?;
        let (i, _) = alt((tag(" the same"), tag(" so"))).parse(i)?;
        Ok((i, subject))
    })?;
    if rest.trim().trim_end_matches('.').trim().is_empty() {
        Some(subject)
    } else {
        None
    }
}

/// Stable `Effect::Unimplemented` name for a recognized-but-deferred "does the
/// same" subject. The replication's runtime needs cross-cutting engine targeting
/// work (mid-chain `TargetOnly` slot collection + opponent-choice routing), so
/// the clause is emitted as a *documented* strict-failure keyed on the typed
/// subject — never a silently-degenerate misparse.
pub(super) fn does_the_same_unimplemented_name(subject: DoesTheSameSubject) -> String {
    match subject {
        DoesTheSameSubject::TargetOpponent => "target_opponent_does_the_same".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::QuantityExpr;

    // CR 608.2c + CR 601.2c: "target opponent does the same / does so" replicates
    // the preceding sibling effect for a targeted opponent. The recognizer must
    // accept the "does the same", "does so", and leading-"then" phrasings.
    #[test]
    fn does_the_same_recognizes_target_opponent_phrasings() {
        for phrasing in [
            "target opponent does the same",
            "Then target opponent does the same.",
            "then target opponent does so",
            "Target opponent does so.",
        ] {
            assert_eq!(
                try_parse_does_the_same_clause(phrasing),
                Some(DoesTheSameSubject::TargetOpponent),
                "should recognize {phrasing:?}"
            );
        }
    }

    // Guard: the recognizer must NOT swallow unrelated "same" phrases or a
    // "does the same for <category>" player-set/category fanout (deferred), so
    // they fall through to normal dispatch instead of being silently dropped.
    #[test]
    fn does_the_same_rejects_unrelated_phrases() {
        for phrasing in [
            "all changes happen at the same time",
            "target opponent does the same for enchantment cards",
            "each opponent does the same",
            "target opponent draws a card",
        ] {
            assert_eq!(
                try_parse_does_the_same_clause(phrasing),
                None,
                "should reject {phrasing:?}"
            );
        }
    }

    // CR 701.13 + CR 608.2c: verb-gapping in a conjoined exile — "exile a Human
    // you control and an artifact you control" splits into two clauses, the
    // second prefixed with the elided "exile " so both objects are exiled.
    // Building block for Fugitive of the Judoon III.
    #[test]
    fn conjoined_exile_restores_elided_verb() {
        let chunks = clause_texts("exile a Human you control and an artifact you control");
        assert_eq!(
            chunks,
            vec!["exile a Human you control", "exile an artifact you control"],
            "second conjunct must regain the elided exile verb"
        );
    }

    // Guard: the verb-gapping recognizer must NOT split a genuine single-object
    // noun-phrase continuation that is not an exile object ("... and an opponent
    // gains control of it" is a verb-headed clause handled elsewhere; a bare
    // noun continuation with no exile head stays one clause).
    #[test]
    fn exile_conjunct_prepend_rejects_non_object_continuation() {
        assert_eq!(
            exile_conjunct_prepend(
                "exile a creature you control",
                "an opponent gains control of it"
            ),
            None
        );
    }

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

    // CR 707.9a: a copy-effect except clause that ends in ", and has <keyword>"
    // must NOT be bisected at the comma. "has" deconjugates to the clause verb
    // "have", so without the `inside_except_clause` guard in the comma splitter
    // the trailing keyword body is orphaned (The Apprentice's Folly I/II:
    // "create a token that's a copy of it, except it isn't legendary, is a
    // Reflection in addition to its other types, and has haste").
    #[test]
    fn copy_except_comma_and_keyword_body_stays_one_chunk() {
        let chunks = clause_texts(
            "create a token that's a copy of it, except it isn't legendary, is a Reflection in addition to its other types, and has haste",
        );
        assert_eq!(
            chunks,
            vec![
                "create a token that's a copy of it, except it isn't legendary, is a Reflection in addition to its other types, and has haste"
            ],
            "the entire except clause must remain a single chunk so the trailing \", and has haste\" body reaches the except parser"
        );
    }

    /// CR 708.7: "..., then turn it face up" is its own clause (Yarus, Roar of
    /// the Old Gods). The face-toggle clause must split off so it lowers to a
    /// `TurnFaceUp` sub-ability. Reverting the `starts_turn_face_up_down_clause`
    /// starter leaves the tail glued to the return clause (one chunk).
    #[test]
    fn turn_face_up_toggle_splits_off_after_then() {
        let chunks = clause_texts("return it to the battlefield, then turn it face up");
        assert_eq!(
            chunks,
            vec!["return it to the battlefield", "turn it face up"],
            "the face-up toggle must split into its own clause"
        );
    }

    /// The "turn <object> face up|down" starter is narrow: it matches ONLY a
    /// complete toggle, never unrelated "turn" phrasings.
    #[test]
    fn turn_face_up_starter_is_narrow() {
        assert!(starts_turn_face_up_down_clause("turn it face up"));
        assert!(starts_turn_face_up_down_clause("turn it face down"));
        assert!(starts_turn_face_up_down_clause(
            "turn that creature face up"
        ));
        assert!(starts_turn_face_up_down_clause(
            "turn target permanent face down"
        ));
        // Negatives — no face toggle / no recognized subject / not clause-initial.
        assert!(!starts_turn_face_up_down_clause("turn it sideways"));
        assert!(!starts_turn_face_up_down_clause("each turn"));
        assert!(!starts_turn_face_up_down_clause("until your next turn"));
        assert!(!starts_turn_face_up_down_clause("turn face up"));
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

    #[test]
    fn quoted_grant_splits_before_following_sentence() {
        // Requiem Monolith: period inside the granted trigger quote, then a new
        // optional sentence starting with an uppercase letter.
        let chunks = clause_texts(
            "Until end of turn, target creature gains \"Whenever this creature is dealt damage, you draw that many cards and lose that much life.\" That creature's controller may have this artifact deal 1 damage to it.",
        );
        assert_eq!(
            chunks,
            vec![
                "Until end of turn, target creature gains \"Whenever this creature is dealt damage, you draw that many cards and lose that much life.\"",
                "That creature's controller may have this artifact deal 1 damage to it",
            ]
        );
    }

    #[test]
    fn quoted_grant_keeps_nonrecognized_capitalized_continuation() {
        // CR 608.2c: a granted quote followed by a capitalized continuation that
        // is NOT a "may have" causative sentence ("The token is goaded …",
        // Nettling Nuisance-style) must stay a single chunk — the prior
        // uppercase-letter fallback over-split these.
        let chunks = clause_texts(
            "create a 1/1 red Goblin creature token with \"This creature attacks each combat if able.\" The token is goaded.",
        );
        assert_eq!(chunks.len(), 1, "unexpected split: {chunks:?}");
    }

    // --- Bare " and " splitting: positive cases (should split) ---

    #[test]
    fn bare_and_splits_lose_life_and_create_token() {
        // Lotho: "you lose 1 life and create a Treasure token"
        let chunks = clause_texts("you lose 1 life and create a Treasure token");
        assert_eq!(chunks, vec!["you lose 1 life", "create a Treasure token"]);
    }

    #[test]
    fn bare_and_splits_create_token_and_attach() {
        // Field-Tested Frying Pan (#835): "create a 1/1 white Halfling creature
        // token and attach this Equipment to it" — "attach " is an imperative game
        // action, so the conjunct must peel into its own clause and lower to a
        // Token -> Attach sibling (rewire_token_attach_sibling rebinds onto
        // LastCreated). Without the split the attach is silently dropped.
        let chunks = clause_texts(
            "create a 1/1 white Halfling creature token and attach this Equipment to it",
        );
        assert_eq!(
            chunks,
            vec![
                "create a 1/1 white Halfling creature token",
                "attach this Equipment to it"
            ]
        );
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
    fn bare_and_keeps_same_name_creature_search_exile_compound() {
        // CR 201.2 + CR 701.23a + CR 701.18a: Eradicate-class "same name as that
        // creature" must stay one compound so `MultiZoneSameNameExile` wins.
        let chunks = clause_texts(
            "search its controller's graveyard, hand, and library for all cards with the same name as that creature and exile them",
        );
        assert_eq!(chunks.len(), 1, "unexpected split: {chunks:?}");
    }

    #[test]
    fn bare_and_keeps_same_name_spell_search_exile_compound() {
        let chunks = clause_texts(
            "search its controller's graveyard, hand, and library for all cards with the same name as that spell and exile them",
        );
        assert_eq!(chunks.len(), 1, "unexpected split: {chunks:?}");
    }

    #[test]
    fn bare_and_keeps_same_name_land_search_exile_compound() {
        let chunks = clause_texts(
            "search its controller's graveyard, hand, and library for any number of cards with the same name as that land and exile them",
        );
        assert_eq!(chunks.len(), 1, "unexpected split: {chunks:?}");
    }

    #[test]
    fn search_exile_them_followup_is_absorbed_after_library_exile_destination() {
        let previous = Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Exile,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        };
        let result =
            parse_followup_continuation_ast("exile them", &previous, &mut ParseContext::default());
        assert_eq!(result, Some(ContinuationAst::SearchResultClauseHandled));
    }

    /// CR 701.23a + CR 701.18a (cluster 35 / Mana Severance): comma-split
    /// "search …, exile them, then shuffle" must lower to one SearchLibrary
    /// compound with a single library→exile destination and shuffle — not a
    /// duplicate ChangeZone from the redundant "exile them" restatement.
    #[test]
    fn mana_severance_search_any_number_lands_exile_then_shuffle() {
        use super::super::parse_effect_chain;

        let def = parse_effect_chain(
            "Search your library for any number of land cards, exile them, then shuffle.",
            AbilityKind::Spell,
        );

        let mut searches = 0usize;
        let mut library_exiles = 0usize;
        let mut shuffles = 0usize;
        let mut node = Some(&def);
        while let Some(d) = node {
            match &*d.effect {
                Effect::SearchLibrary { filter, count, .. } => {
                    searches += 1;
                    assert!(
                        matches!(count, QuantityExpr::UpTo { .. }),
                        "any-number search must be up-to bounded, got {count:?}"
                    );
                    assert!(
                        matches!(
                            filter,
                            TargetFilter::Typed(typed) if typed.type_filters.contains(&TypeFilter::Land)
                        ),
                        "expected land filter, got {filter:?}"
                    );
                }
                Effect::ChangeZone {
                    origin: Some(Zone::Library),
                    destination: Zone::Exile,
                    ..
                } => {
                    library_exiles += 1;
                }
                Effect::Shuffle { .. } => {
                    shuffles += 1;
                }
                _ => {}
            }
            node = d.sub_ability.as_deref();
        }

        assert_eq!(searches, 1, "expected exactly one SearchLibrary");
        assert_eq!(
            library_exiles, 1,
            "expected exactly one library→exile destination"
        );
        assert_eq!(shuffles, 1, "expected exactly one Shuffle");
    }

    #[test]
    fn bare_and_splits_draw_and_lose() {
        let chunks = clause_texts("draw a card and lose 1 life");
        assert_eq!(chunks, vec!["draw a card", "lose 1 life"]);
    }

    #[test]
    fn bare_and_preserves_tilde_possessive_controller_draw() {
        let chunks = clause_texts("~'s controller sacrifices it and draws a card");
        assert_eq!(
            chunks,
            vec!["~'s controller sacrifices it and draws a card"]
        );
    }

    #[test]
    fn bare_and_starts_attach_clause() {
        assert!(starts_bare_and_clause(
            "attach an Equipment that was attached to ~ to that creature"
        ));
    }

    #[test]
    fn bare_and_splits_zack_fair_counter_move_and_attach() {
        let chunks = clause_texts(
            "put ~'s counters on that creature and attach an Equipment that was attached to ~ to that creature",
        );
        assert_eq!(
            chunks,
            vec![
                "put ~'s counters on that creature",
                "attach an Equipment that was attached to ~ to that creature",
            ]
        );
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

    /// CR 509.1b + CR 613.4b: Atomic Microsizer — "has" must suppress the bare-and
    /// split the same way "have" does when it introduces a base P/T conjunct.
    #[test]
    fn bare_and_does_not_split_cant_be_blocked_and_has_base_pt() {
        let chunks = clause_texts(
            "That creature can't be blocked this turn and has base power and toughness 1/1 until end of turn",
        );
        assert_eq!(
            chunks,
            vec![
                "That creature can't be blocked this turn and has base power and toughness 1/1 until end of turn"
            ]
        );
    }

    /// CR 613.1d + CR 613.4b: Curious Colossus — a comma-separated chain with
    /// additive type change and trailing "and has base power and toughness N/N"
    /// must stay one continuous-modification clause so the target/affected
    /// subject applies to every layer-4/6/7b modification.
    #[test]
    fn comma_and_does_not_split_type_change_and_has_base_pt() {
        let chunks = clause_texts(
            "each creature target opponent controls loses all abilities, becomes a Coward in addition to its other types, and has base power and toughness 1/1",
        );
        assert_eq!(
            chunks,
            vec![
                "each creature target opponent controls loses all abilities, becomes a Coward in addition to its other types, and has base power and toughness 1/1"
            ]
        );
    }

    /// CR 613.4b: the no-"and" sibling form ", has base power and toughness N/N"
    /// (the middle conjunct of e.g. "becomes a Spirit, has base power and
    /// toughness 1/1, and gains ...") must also stay attached — the bare-"and"
    /// guard above only covers the trailing ", and has ..." form.
    #[test]
    fn comma_no_and_does_not_split_has_base_pt() {
        let chunks =
            clause_texts("target creature loses all abilities, has base power and toughness 2/2");
        assert_eq!(
            chunks,
            vec!["target creature loses all abilities, has base power and toughness 2/2"]
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
            total_power_cap: None,
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

    /// CR 701.15b: plural "the tokens are goaded" after CreateToken.
    #[test]
    fn tokens_goaded_continuation_after_create_token() {
        let token_effect = Effect::Token {
            name: "Warrior".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".to_string()],
            colors: vec![crate::types::mana::ManaColor::White],
            keywords: vec![],
            tapped: true,
            count: QuantityExpr::Fixed { value: 3 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        };
        assert_eq!(
            parse_followup_continuation_ast(
                "the tokens are goaded for the rest of the game",
                &token_effect,
                &mut ParseContext::default(),
            ),
            Some(ContinuationAst::GoadLastCreated {
                duration: Some(Duration::Permanent),
            })
        );
    }

    /// CR 701.15b + CR 611.2b: Saga-scoped goad persists while the Saga remains.
    #[test]
    fn tokens_goaded_continuation_after_create_token_until_host_leaves_play() {
        let token_effect = Effect::Token {
            name: "Warrior".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".to_string()],
            colors: vec![crate::types::mana::ManaColor::White],
            keywords: vec![],
            tapped: true,
            count: QuantityExpr::Fixed { value: 3 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        };
        assert_eq!(
            parse_followup_continuation_ast(
                "the tokens are goaded for as long as this Saga remains on the battlefield",
                &token_effect,
                &mut ParseContext::default(),
            ),
            Some(ContinuationAst::GoadLastCreated {
                duration: Some(Duration::UntilHostLeavesPlay),
            })
        );
    }

    /// CR 701.15b: singular form must still recognize (Nettling Nuisance class).
    #[test]
    fn token_goaded_continuation_after_create_token() {
        let token_effect = Effect::Token {
            name: "Goblin".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".to_string()],
            colors: vec![crate::types::mana::ManaColor::Red],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        };
        assert_eq!(
            parse_followup_continuation_ast(
                "the token is goaded for the rest of the game",
                &token_effect,
                &mut ParseContext::default(),
            ),
            Some(ContinuationAst::GoadLastCreated {
                duration: Some(Duration::Permanent),
            })
        );
    }

    /// CR 701.15b: absorbed continuation grants permanent goad on LastCreated.
    #[test]
    fn create_tokens_then_goad_chain_has_no_effect_the_gap() {
        use super::super::parse_effect_chain;

        let def = parse_effect_chain(
            "each player creates three tapped 1/1 white Warrior creature tokens. the tokens are goaded for the rest of the game",
            AbilityKind::Spell,
        );
        let sub = def.sub_ability.as_ref().expect("goad sub_ability");
        match sub.effect.as_ref() {
            Effect::GenericEffect {
                static_abilities,
                duration,
                target,
            } => {
                assert_eq!(*target, Some(TargetFilter::LastCreated));
                assert_eq!(*duration, Some(Duration::Permanent));
                assert!(static_abilities[0].modifications.iter().any(|m| matches!(
                    m,
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::Goaded
                    }
                )));
            }
            Effect::Unimplemented { name, .. } => {
                panic!("expected GenericEffect goad sub, got Unimplemented({name})")
            }
            other => panic!("expected GenericEffect goad sub, got {other:?}"),
        }
    }

    /// CR 701.15b + CR 611.2b: The War Games class keeps the Saga-scoped duration.
    #[test]
    fn create_tokens_then_saga_scoped_goad_chain_has_host_duration() {
        use super::super::parse_effect_chain;

        let def = parse_effect_chain(
            "each player creates three tapped 1/1 white Warrior creature tokens. the tokens are goaded for as long as this Saga remains on the battlefield",
            AbilityKind::Spell,
        );
        let sub = def.sub_ability.as_ref().expect("goad sub_ability");
        match sub.effect.as_ref() {
            Effect::GenericEffect {
                static_abilities,
                duration,
                target,
            } => {
                assert_eq!(*target, Some(TargetFilter::LastCreated));
                assert_eq!(*duration, Some(Duration::UntilHostLeavesPlay));
                assert!(static_abilities[0].modifications.iter().any(|m| matches!(
                    m,
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::Goaded
                    }
                )));
            }
            other => panic!("expected GenericEffect goad sub, got {other:?}"),
        }
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

    /// CR 707.10c: Bare ` and ` (no comma) inside an `if` clause must still split.
    #[test]
    fn if_clause_bare_and_copy_retarget_splits() {
        let text = "if you win the flip, copy that spell and may choose new targets for the copy";
        let chunks = clause_texts(text);
        assert_eq!(
            chunks,
            vec![
                "if you win the flip, copy that spell",
                "may choose new targets for the copy"
            ]
        );
    }

    /// CR 705 + CR 707.10c: Krark, the Thumbless — coin-flip win branch must not
    /// bare-`and` split off the copy-retarget grant.
    #[test]
    fn krark_coin_flip_win_branch_stays_single_chunk() {
        let text = "flip a coin. If you lose the flip, return that spell to its owner's hand. \
            If you win the flip, copy that spell, and you may choose new targets for the copy.";
        let chunks = clause_texts(text);
        assert_eq!(
            chunks.len(),
            3,
            "expected three sentence chunks, got {chunks:?}"
        );
        assert!(
            nom_primitives::scan_contains(&chunks[2].to_ascii_lowercase(), "choose new targets "),
            "win chunk must include retarget clause: {:?}",
            chunks[2]
        );
    }

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
        assert!(recognize_copy_retarget_clause(
            "and you may choose new targets for the copy"
        ));
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
            copier: None,
            additional_modifications: Vec::new(),
            starting_loyalty_from_casualty_sacrifice: false,
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
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
        );
        def.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
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
            enter_tapped: false,
            source: DigSource::Library,
        }
    }

    #[test]
    fn reveal_until_ring_goes_south_followup_continuation() {
        use crate::types::ability::{
            RevealUntilDisposition, TargetFilter, TypeFilter, TypedFilter,
        };
        let reveal = Effect::RevealUntil {
            player: TargetFilter::Controller,
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Land],
                ..Default::default()
            }),
            count: QuantityExpr::Fixed { value: 1 },
            matched_disposition: RevealUntilDisposition::KeepEach,
            kept_destination: Zone::Hand,
            rest_destination: Zone::Library,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            kept_optional_to: None,
            enters_under: None,
        };
        let result = parse_followup_continuation_ast(
            "Put those land cards onto the battlefield tapped and the rest on the bottom of your library in a random order.",
            &reveal,
            &mut ParseContext::default(),
        );
        assert!(
            matches!(
                result,
                Some(ContinuationAst::RevealUntilKept {
                    destination: Zone::Battlefield,
                    enter_tapped: true,
                    ..
                })
            ),
            "expected RevealUntilKept to battlefield tapped, got {result:?}"
        );
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
                quantity: PutCount::Exactly(2),
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Library),
                enters_under: None,
                face_down_profile: None,
                enter_tapped: false,
                reveal_verb: false,
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
                quantity: PutCount::Exactly(1),
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Library),
                enters_under: None,
                face_down_profile: None,
                enter_tapped: false,
                reveal_verb: false,
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
                quantity: PutCount::Exactly(1),
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Library),
                enters_under: None,
                face_down_profile: None,
                enter_tapped: false,
                reveal_verb: false,
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
                quantity: PutCount::Exactly(2),
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Graveyard),
                enters_under: None,
                face_down_profile: None,
                enter_tapped: false,
                reveal_verb: false,
            })
        );
    }

    #[test]
    fn put_any_number_of_them_into_hand_is_uncapped() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put any number of them into your hand and the rest on the bottom of your library in any order.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::DigFromAmong {
                quantity: PutCount::AnyNumber,
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Library),
                enters_under: None,
                face_down_profile: None,
                enter_tapped: false,
                reveal_verb: false,
            })
        );
    }

    #[test]
    fn put_mass_of_them_into_hand_is_all() {
        for text in [
            "Put all of them into your hand.",
            "Put each of them into your hand.",
            "Put all of those cards into your hand.",
            "Put each of those cards into your hand.",
        ] {
            let dig = make_dig_effect();
            let result = parse_followup_continuation_ast(text, &dig, &mut ParseContext::default());
            assert_eq!(
                result,
                Some(ContinuationAst::DigFromAmong {
                    quantity: PutCount::All,
                    filter: TargetFilter::Any,
                    destination: Some(Zone::Hand),
                    rest_destination: None,
                    enters_under: None,
                    face_down_profile: None,
                    enter_tapped: false,
                    reveal_verb: false,
                }),
                "{text}"
            );
        }
    }

    #[test]
    fn put_all_milled_cards_onto_battlefield_tapped_patches_enter_tapped() {
        let mill = Effect::Mill {
            count: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        };
        let result = parse_followup_continuation_ast(
            "Put all creature cards milled this way onto the battlefield tapped.",
            &mill,
            &mut ParseContext::default(),
        );
        let Some(ContinuationAst::DigFromAmong {
            destination,
            enter_tapped,
            ..
        }) = result
        else {
            panic!("expected DigFromAmong continuation, got {result:?}");
        };
        assert_eq!(destination, Some(Zone::Battlefield));
        assert!(
            enter_tapped,
            "enter_tapped must be true for tapped battlefield returns"
        );
    }

    #[test]
    fn fertile_thicket_reveal_basic_land_to_top_rest_on_bottom() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Reveal up to one basic land card from among them, then put that card on top of your library and the rest on the bottom in any order.",
            &dig,
            &mut ParseContext::default(),
        );
        let Some(ContinuationAst::DigFromAmong {
            quantity,
            filter,
            destination,
            rest_destination,
            reveal_verb,
            ..
        }) = result
        else {
            panic!("expected DigFromAmong continuation, got {result:?}");
        };
        assert_eq!(quantity, PutCount::Up(1));
        // CR 401.4: kept card goes on TOP of the library; the rest go to the bottom.
        assert_eq!(destination, Some(Zone::Library));
        assert_eq!(rest_destination, Some(Zone::Library));
        // CR 701.20a vs 701.20e: "reveal ... from among them" is a public reveal,
        // so the Dig must be promoted to reveal:true even though the kept card
        // routes to a fixed library position.
        assert!(reveal_verb, "the 'reveal' verb must set reveal_verb");
        // The from-among filter restricts the kept card to a basic land card.
        let (expected_filter, _) = parse_target("basic land card");
        assert_eq!(filter, expected_filter);
    }

    #[test]
    fn from_among_enter_tapped_is_local_to_kept_destination() {
        assert_eq!(
            parse_dig_kept_destination(
                "put a land card from among them onto the battlefield tapped. put the rest on the bottom of your library.",
            ),
            (Some(Zone::Battlefield), true)
        );
        assert_eq!(
            parse_dig_kept_destination(
                "put a land card from among them onto the battlefield. put the rest onto the battlefield tapped.",
            ),
            (Some(Zone::Battlefield), false)
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
            quantity,
            filter,
            destination,
            rest_destination,
            ..
        }) = result
        else {
            panic!("expected DigFromAmong continuation, got {result:?}");
        };
        assert_eq!(
            quantity,
            PutCount::Up(1),
            "\"may put a\" is optional → up_to 1"
        );
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
            quantity,
            filter,
            destination,
            rest_destination,
            ..
        }) = result
        else {
            panic!("expected DigFromAmong continuation, got {result:?}");
        };
        assert_eq!(quantity, PutCount::Up(1));
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
            quantity,
            filter,
            destination,
            rest_destination,
            ..
        }) = result
        else {
            panic!("expected DigFromAmong continuation, got {result:?}");
        };
        assert_eq!(
            quantity,
            PutCount::Exactly(1),
            "after the optional payment is made, returning a card is not optional"
        );
        assert_eq!(filter, TargetFilter::Any);
        assert_eq!(destination, Some(Zone::Hand));
        assert_eq!(rest_destination, None);
    }

    #[test]
    fn mill_return_any_number_milled_this_way_is_uncapped() {
        let mill = Effect::Mill {
            count: QuantityExpr::Fixed { value: 6 },
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        };
        let result = parse_followup_continuation_ast(
            "Return any number of creature cards milled this way to your hand.",
            &mill,
            &mut ParseContext::default(),
        );
        let Some(ContinuationAst::DigFromAmong {
            quantity,
            filter,
            destination,
            ..
        }) = result
        else {
            panic!("expected DigFromAmong continuation, got {result:?}");
        };
        assert_eq!(quantity, PutCount::AnyNumber);
        assert!(matches!(filter, TargetFilter::Typed(_)), "got {filter:?}");
        assert_eq!(destination, Some(Zone::Hand));
    }

    #[test]
    fn from_among_any_number_is_uncapped() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put any number of creature cards from among them into your hand.",
            &dig,
            &mut ParseContext::default(),
        );
        let Some(ContinuationAst::DigFromAmong {
            quantity,
            filter,
            destination,
            ..
        }) = result
        else {
            panic!("expected DigFromAmong continuation, got {result:?}");
        };
        assert_eq!(quantity, PutCount::AnyNumber);
        assert!(matches!(filter, TargetFilter::Typed(_)), "got {filter:?}");
        assert_eq!(destination, Some(Zone::Hand));
    }

    #[test]
    fn dig_any_number_from_among_lowers_to_up_to_all_seen_cards() {
        let mut defs = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            make_dig_effect(),
        )];
        apply_clause_continuation(
            &mut defs,
            ContinuationAst::DigFromAmong {
                quantity: PutCount::AnyNumber,
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Library),
                enters_under: None,
                face_down_profile: None,
                enter_tapped: false,
                reveal_verb: false,
            },
            AbilityKind::Spell,
        );

        let Effect::Dig {
            keep_count,
            up_to,
            destination,
            rest_destination,
            ..
        } = &*defs[0].effect
        else {
            panic!("expected patched Dig, got {:?}", defs[0].effect);
        };
        assert_eq!(*keep_count, Some(u32::MAX));
        assert!(*up_to);
        assert_eq!(*destination, Some(Zone::Hand));
        assert_eq!(*rest_destination, Some(Zone::Library));
    }

    #[test]
    fn dig_all_from_among_lowers_to_all_seen_cards() {
        let mut defs = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            make_dig_effect(),
        )];
        apply_clause_continuation(
            &mut defs,
            ContinuationAst::DigFromAmong {
                quantity: PutCount::All,
                filter: TargetFilter::Typed(TypedFilter::creature()),
                destination: Some(Zone::Hand),
                rest_destination: None,
                enters_under: None,
                face_down_profile: None,
                enter_tapped: false,
                reveal_verb: false,
            },
            AbilityKind::Spell,
        );

        let Effect::Dig {
            keep_count,
            up_to,
            destination,
            filter,
            ..
        } = &*defs[0].effect
        else {
            panic!("expected patched Dig, got {:?}", defs[0].effect);
        };
        assert_eq!(*keep_count, Some(u32::MAX));
        assert!(!*up_to);
        assert_eq!(*destination, Some(Zone::Hand));
        assert!(matches!(filter, TargetFilter::Typed(_)), "got {filter:?}");
    }

    #[test]
    fn mill_any_number_milled_this_way_lowers_to_unlimited_resolution_choice() {
        let mut defs = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 6 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
        )];
        apply_clause_continuation(
            &mut defs,
            ContinuationAst::DigFromAmong {
                quantity: PutCount::AnyNumber,
                filter: TargetFilter::Typed(TypedFilter::creature()),
                destination: Some(Zone::Hand),
                rest_destination: None,
                enters_under: None,
                face_down_profile: None,
                enter_tapped: false,
                reveal_verb: false,
            },
            AbilityKind::Spell,
        );

        assert_eq!(defs.len(), 2, "expected Mill + pushed ChangeZone");
        let pushed = &defs[1];
        let Effect::ChangeZone {
            target,
            up_to,
            destination,
            ..
        } = &*pushed.effect
        else {
            panic!("expected pushed ChangeZone, got {:?}", pushed.effect);
        };
        assert!(*up_to);
        assert_eq!(*destination, Zone::Hand);
        assert_eq!(pushed.multi_target, Some(MultiTargetSpec::unlimited(0)));
        assert_eq!(pushed.target_choice_timing, TargetChoiceTiming::Resolution);
        assert!(matches!(
            target,
            TargetFilter::TrackedSetFiltered {
                filter,
                ..
            } if matches!(filter.as_ref(), TargetFilter::Typed(_))
        ));
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
                quantity: PutCount::Up(1),
                filter: or_filter.clone(),
                destination: Some(Zone::Hand),
                rest_destination: None,
                enters_under: None,
                face_down_profile: None,
                enter_tapped: false,
                reveal_verb: false,
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
            TargetFilter::TrackedSetFiltered { id, filter, .. } => {
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
            TargetFilter::TrackedSetFiltered { id, filter, .. } => {
                assert_eq!(id.0, 0);
                assert_eq!(**filter, TargetFilter::Any);
            }
            other => panic!("expected TrackedSetFiltered target, got {other:?}"),
        }
    }

    /// CR 205.2a + CR 701.17c + CR 608.2c: Szarekh, the Silent King (#1537).
    /// "Mill three cards. You may put an artifact creature card or Vehicle card
    /// from among the cards milled this way into your hand." The disjunction
    /// "artifact creature card or Vehicle card" must parse into a
    /// `TrackedSetFiltered` whose inner filter ANDs `Artifact` with `Creature`
    /// on the left branch — dropping the trailing `Creature` would let any
    /// milled artifact (e.g. an Equipment, an artifact land) be moved to hand.
    ///
    /// End-to-end guard: the building-block fix lives in
    /// `parse_type_phrase_with_ctx` (`oracle_target.rs`); this test pins the
    /// full Oracle-text → typed-AST contract for the milled-card retrieval
    /// path so future refactors to the dig-from-among lowering can't silently
    /// regress the AND-of-types semantics.
    #[test]
    fn szarekh_mill_artifact_creature_or_vehicle_filter() {
        use super::super::parse_effect_chain;
        use crate::types::ability::{TypeFilter, TypedFilter};

        let def = parse_effect_chain(
            "Mill three cards. You may put an artifact creature card or Vehicle card from among the cards milled this way into your hand.",
            AbilityKind::Spell,
        );

        let mut effects: Vec<&AbilityDefinition> = Vec::new();
        let mut node = Some(&def);
        while let Some(d) = node {
            effects.push(d);
            node = d.sub_ability.as_deref();
        }
        assert!(
            matches!(&*effects[0].effect, Effect::Mill { .. }),
            "first effect should be Mill, got {:?}",
            effects[0].effect
        );
        let Effect::ChangeZone {
            destination,
            target,
            up_to,
            ..
        } = &*effects[1].effect
        else {
            panic!("expected ChangeZone retrieval, got {:?}", effects[1].effect);
        };
        assert_eq!(*destination, Zone::Hand);
        assert!(*up_to, "\"you may put\" → up_to (optional)");
        let TargetFilter::TrackedSetFiltered { id, filter, .. } = target else {
            panic!("expected TrackedSetFiltered target, got {target:?}");
        };
        assert_eq!(id.0, 0, "sentinel TrackedSetId(0)");
        let TargetFilter::Or { filters } = filter.as_ref() else {
            panic!("expected Or filter for milled set, got {filter:?}");
        };
        assert_eq!(filters.len(), 2, "expected two disjuncts, got {filters:?}");

        // Left: artifact creature card → Typed must contain BOTH Artifact and Creature.
        let TargetFilter::Typed(left) = &filters[0] else {
            panic!("expected left Typed, got {:?}", filters[0]);
        };
        assert!(
            left.type_filters.contains(&TypeFilter::Artifact),
            "left branch missing Artifact: {left:?}",
        );
        assert!(
            left.type_filters.contains(&TypeFilter::Creature),
            "left branch missing Creature — Szarekh regression (#1537): {left:?}",
        );

        // Right: Vehicle card → subtype Vehicle (inferred core type may also
        // be added by normalization; only the subtype is load-bearing).
        let TargetFilter::Typed(right) = &filters[1] else {
            panic!("expected right Typed, got {:?}", filters[1]);
        };
        assert!(
            right
                .type_filters
                .contains(&TypeFilter::Subtype("Vehicle".into())),
            "right branch missing Vehicle subtype: {right:?}",
        );

        // Sanity: the left branch is strictly stricter than `Typed{Artifact}`.
        // Construct the buggy filter and confirm we don't match it.
        let buggy_left = TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact));
        assert_ne!(
            &filters[0], &buggy_left,
            "left branch parsed to the buggy filter shape (just Artifact) — \
             this is the exact #1537 regression",
        );
    }

    /// CR 708.2a + CR 205.1a: `parse_theyre_face_down_profile` building-block
    /// tests — Cyberman and a non-Cyberman sibling form must parse from parts.
    #[test]
    fn parse_theyre_face_down_profile_cyberman_and_sibling() {
        // Cyber-Controller: "They're 2/2 Cyberman artifact creatures."
        let cyber =
            parse_theyre_face_down_profile("they're 2/2 cyberman artifact creatures.").unwrap();
        assert_eq!(cyber.power, Some(2));
        assert_eq!(cyber.toughness, Some(2));
        assert_eq!(cyber.extra_core_types, vec![CoreType::Artifact]);
        assert_eq!(cyber.subtypes, vec!["Cyberman".to_string()]);

        // Sibling form with a different P/T and only a subtype (no extra core
        // type) — proves the building block is not Cyberman-specific.
        let spirit = parse_theyre_face_down_profile("they're 1/1 spirit creatures").unwrap();
        assert_eq!(spirit.power, Some(1));
        assert_eq!(spirit.toughness, Some(1));
        assert!(spirit.extra_core_types.is_empty());
        assert_eq!(spirit.subtypes, vec!["Spirit".to_string()]);

        // A no-P/T form ("they're artifacts") still parses with default P/T.
        let no_pt = parse_theyre_face_down_profile("they're artifact creatures").unwrap();
        assert_eq!(no_pt.power, None);
        assert_eq!(no_pt.toughness, None);
        assert_eq!(no_pt.extra_core_types, vec![CoreType::Artifact]);

        // Not a they're clause → None.
        assert!(parse_theyre_face_down_profile("draw a card.").is_none());
    }

    /// CR 708.2a + CR 205.1a: the singular "It's a/an ... <core-type>." face-down
    /// characteristic clause parses from parts for both non-creature (Yedora's
    /// "It's a Forest land.") and creature bodies — no card-named hardcode.
    #[test]
    fn parse_its_face_down_profile_forest_land_and_siblings() {
        // Yedora: "It's a Forest land." — a non-creature Land with the Forest
        // land type, no power/toughness.
        let forest = parse_its_face_down_profile("it's a forest land.").unwrap();
        assert_eq!(forest.body, FaceDownBody::Noncreature);
        assert_eq!(forest.extra_core_types, vec![CoreType::Land]);
        assert_eq!(forest.subtypes, vec!["Forest".to_string()]);
        assert_eq!(forest.power, None);
        assert_eq!(forest.toughness, None);

        // Sibling land type proves the class coverage (not Forest-specific).
        let island = parse_its_face_down_profile("it's an island land").unwrap();
        assert_eq!(island.body, FaceDownBody::Noncreature);
        assert_eq!(island.extra_core_types, vec![CoreType::Land]);
        assert_eq!(island.subtypes, vec!["Island".to_string()]);

        // Non-creature artifact body (no land type).
        let artifact = parse_its_face_down_profile("it's an artifact.").unwrap();
        assert_eq!(artifact.body, FaceDownBody::Noncreature);
        assert_eq!(artifact.extra_core_types, vec![CoreType::Artifact]);
        assert!(artifact.power.is_none() && artifact.toughness.is_none());

        // Singular creature body with P/T → creature body, explicit P/T override.
        let creature = parse_its_face_down_profile("it's a 3/3 creature.").unwrap();
        assert_eq!(creature.body, FaceDownBody::Creature);
        assert_eq!(creature.power, Some(3));
        assert_eq!(creature.toughness, Some(3));

        // CR 708.2a + CR 205.1a: Cyber Conversion's singular creature body with an
        // extra core type AND a subtype — "It's a 2/2 Cyberman artifact creature."
        // The non-terminal "artifact " modifier must be consumed before the
        // terminal "creature" noun; the subtype "Cyberman" is retained.
        let cyberman =
            parse_its_face_down_profile("it's a 2/2 cyberman artifact creature.").unwrap();
        assert_eq!(cyberman.body, FaceDownBody::Creature);
        assert_eq!(cyberman.power, Some(2));
        assert_eq!(cyberman.toughness, Some(2));
        assert_eq!(cyberman.extra_core_types, vec![CoreType::Artifact]);
        assert_eq!(cyberman.subtypes, vec!["Cyberman".to_string()]);

        // Sibling without an extra core type proves the reorder did not regress
        // the plain "<subtype> creature" form.
        let zombie = parse_its_face_down_profile("it's a 1/1 zombie creature").unwrap();
        assert_eq!(zombie.body, FaceDownBody::Creature);
        assert_eq!(zombie.power, Some(1));
        assert_eq!(zombie.toughness, Some(1));
        assert!(zombie.extra_core_types.is_empty());
        assert_eq!(zombie.subtypes, vec!["Zombie".to_string()]);

        // A non-creature body must reject a stray P/T ("It's a 2/2 land." is not
        // a valid characteristic line — lands have no power/toughness).
        assert!(parse_its_face_down_profile("it's a 2/2 land.").is_none());

        // CR 205.1a: the "It becomes a/an ..." copula (Mondassian Colony Ship)
        // sets the same characteristics as the "It's a ..." copula, so the same
        // Cyberman creature body parses from the "becomes" lead-in.
        let becomes_cyberman =
            parse_its_face_down_profile("it becomes a 2/2 cyberman artifact creature.").unwrap();
        assert_eq!(becomes_cyberman.body, FaceDownBody::Creature);
        assert_eq!(becomes_cyberman.power, Some(2));
        assert_eq!(becomes_cyberman.toughness, Some(2));
        assert_eq!(becomes_cyberman.extra_core_types, vec![CoreType::Artifact]);
        assert_eq!(becomes_cyberman.subtypes, vec!["Cyberman".to_string()]);
        // Sibling "becomes an" article proves class coverage, not Cyberman-specific.
        let becomes_artifact = parse_its_face_down_profile("it becomes an artifact.").unwrap();
        assert_eq!(becomes_artifact.body, FaceDownBody::Noncreature);
        assert_eq!(becomes_artifact.extra_core_types, vec![CoreType::Artifact]);

        // Not an it's-characteristics clause → None.
        assert!(parse_its_face_down_profile("draw a card.").is_none());
    }

    /// CR 708.2a + CR 708.3 + CR 110.2a: Cyber-Controller's full two-sentence
    /// put + characteristics clause, chained after a Mill, must assemble into a
    /// `ChangeZoneAll { target: TrackedSetFiltered{creature}, enters_under:
    /// You, face_down_profile: Some(2/2 Cyberman artifact), origin: Graveyard }`
    /// — NOT a singular `ChangeZone`, and with NO `Unimplemented { they're }`.
    #[test]
    fn cyber_controller_mill_put_all_face_down_chain() {
        use super::super::parse_effect_chain;

        let def = parse_effect_chain(
            "Each opponent mills three cards. Put all creature cards milled this way onto the battlefield face down under your control. They're 2/2 Cyberman artifact creatures.",
            AbilityKind::Spell,
        );

        let mut effects: Vec<&AbilityDefinition> = Vec::new();
        let mut node = Some(&def);
        while let Some(d) = node {
            effects.push(d);
            node = d.sub_ability.as_deref();
        }

        // No Unimplemented{they're} anywhere in the chain.
        for d in &effects {
            assert!(
                !matches!(&*d.effect, Effect::Unimplemented { name, .. } if name == "they're"),
                "the 'They're ...' clause must not produce Unimplemented, got {:?}",
                d.effect
            );
        }

        // Find the mass put-step.
        let put = effects
            .iter()
            .find(|d| matches!(&*d.effect, Effect::ChangeZoneAll { .. }))
            .unwrap_or_else(|| {
                panic!(
                    "expected a ChangeZoneAll put-step, chain was {:?}",
                    effects.iter().map(|d| &d.effect).collect::<Vec<_>>()
                )
            });

        // It must NOT be a singular ChangeZone.
        assert!(
            !effects.iter().any(|d| matches!(
                &*d.effect,
                Effect::ChangeZone {
                    target: TargetFilter::TrackedSetFiltered { .. },
                    ..
                }
            )),
            "the mass 'put all' form must lower to ChangeZoneAll, not ChangeZone"
        );

        let Effect::ChangeZoneAll {
            origin,
            destination,
            target,
            enters_under,
            face_down_profile,
            ..
        } = &*put.effect
        else {
            unreachable!()
        };
        assert_eq!(
            *origin,
            Some(Zone::Graveyard),
            "BLOCKER 3: scan the milled zone"
        );
        assert_eq!(*destination, Zone::Battlefield);
        assert_eq!(*enters_under, Some(ControllerRef::You));
        match target {
            TargetFilter::TrackedSetFiltered { id, filter, .. } => {
                assert_eq!(id.0, 0, "sentinel TrackedSetId(0)");
                assert!(
                    matches!(&**filter, TargetFilter::Typed(_)),
                    "filter must restrict to creature cards, got {filter:?}"
                );
            }
            other => panic!("expected TrackedSetFiltered target, got {other:?}"),
        }
        let profile = face_down_profile
            .as_ref()
            .expect("face_down_profile must be set");
        assert_eq!(profile.power, Some(2));
        assert_eq!(profile.toughness, Some(2));
        assert_eq!(profile.extra_core_types, vec![CoreType::Artifact]);
        assert_eq!(profile.subtypes, vec!["Cyberman".to_string()]);
    }

    /// CR 400.7 + CR 608.2c + CR 110.2a + CR 205.1b: Breach the Multiverse's
    /// full four-clause text must assemble into a `Mill → ChooseFromZone{
    /// zone_owner: EachPlayer, filter: creature-or-planeswalker } →
    /// ChangeZoneAll{ origin: Graveyard, enters_under: You, target: tracked-set }
    /// → GenericEffect(AddSubtype("Phyrexian"))` chain — with NO `Unimplemented`.
    /// Clause 1 (Mill) already parsed before this fix; clauses 2-4 were dropped.
    #[test]
    fn breach_multiverse_assembles_per_player_reanimation_chain() {
        use super::super::parse_effect_chain;
        use crate::types::ability::ZoneOwner;

        let def = parse_effect_chain(
            "Each player mills ten cards. For each player, choose a creature or planeswalker card in that player's graveyard. Put those cards onto the battlefield under your control. Then each creature you control becomes a Phyrexian in addition to its other types.",
            AbilityKind::Spell,
        );

        let mut effects: Vec<&AbilityDefinition> = Vec::new();
        let mut node = Some(&def);
        while let Some(d) = node {
            effects.push(d);
            node = d.sub_ability.as_deref();
        }

        // No clause may fall through to Unimplemented.
        for d in &effects {
            assert!(
                !matches!(&*d.effect, Effect::Unimplemented { .. }),
                "no clause may be Unimplemented, got {:?}",
                d.effect
            );
        }

        // Clause 1: the ten-card mill survives untouched.
        assert!(
            effects
                .iter()
                .any(|d| matches!(&*d.effect, Effect::Mill { .. })),
            "clause 1 mill must remain in the chain, chain was {:?}",
            effects.iter().map(|d| &d.effect).collect::<Vec<_>>()
        );

        // Clause 2: per-player graveyard choice (controller chooses one
        // creature-or-planeswalker card from EACH player's graveyard).
        let choose = effects
            .iter()
            .find(|d| matches!(&*d.effect, Effect::ChooseFromZone { .. }))
            .unwrap_or_else(|| {
                panic!(
                    "clause 2 must lower to ChooseFromZone, chain was {:?}",
                    effects.iter().map(|d| &d.effect).collect::<Vec<_>>()
                )
            });
        let Effect::ChooseFromZone {
            count,
            zone,
            zone_owner,
            filter,
            chooser,
            ..
        } = &*choose.effect
        else {
            unreachable!()
        };
        assert_eq!(*count, 1, "one card per player");
        assert_eq!(*zone, Zone::Graveyard);
        assert_eq!(
            *zone_owner,
            ZoneOwner::EachPlayer,
            "BLOCKER: 'for each player ... in that player's graveyard' must iterate every player"
        );
        assert_eq!(
            *chooser,
            Chooser::Controller,
            "the spell's controller chooses"
        );
        let filter = filter
            .as_ref()
            .expect("clause 2 filter must restrict to creature/planeswalker cards");
        // The filter must admit a creature AND a planeswalker (Or/Typed over both).
        assert!(
            matches!(filter, TargetFilter::Or { .. } | TargetFilter::Typed(_)),
            "filter must be a creature-or-planeswalker type filter, got {filter:?}"
        );

        // Clause 3: "put those cards onto the battlefield under your control"
        // must bind the tracked set from clause 2 and enter under the controller.
        let put = effects
            .iter()
            .find(|d| matches!(&*d.effect, Effect::ChangeZoneAll { .. }))
            .unwrap_or_else(|| {
                panic!(
                    "clause 3 must lower to ChangeZoneAll, chain was {:?}",
                    effects.iter().map(|d| &d.effect).collect::<Vec<_>>()
                )
            });
        let Effect::ChangeZoneAll {
            origin,
            destination,
            target,
            enters_under,
            ..
        } = &*put.effect
        else {
            unreachable!()
        };
        assert_eq!(*origin, Some(Zone::Graveyard), "scan the graveyards");
        assert_eq!(*destination, Zone::Battlefield);
        assert_eq!(*enters_under, Some(ControllerRef::You));
        assert!(
            matches!(
                target,
                TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. }
            ),
            "'those cards' must bind the clause-2 tracked set, got {target:?}"
        );

        // Clause 4: "each creature you control becomes a Phyrexian in addition
        // to its other types" → GenericEffect granting AddSubtype("Phyrexian").
        let phyrexian = effects.iter().any(|d| match &*d.effect {
            Effect::GenericEffect {
                static_abilities, ..
            } => static_abilities.iter().any(|sa| {
                sa.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddSubtype { subtype } if subtype == "Phyrexian"
                    )
                })
            }),
            _ => false,
        });
        assert!(
            phyrexian,
            "clause 4 must grant AddSubtype(\"Phyrexian\"), chain was {:?}",
            effects.iter().map(|d| &d.effect).collect::<Vec<_>>()
        );
    }

    /// CR 708.2a: A bare "put all creature cards milled this way onto the
    /// battlefield face down." with no trailing characteristics sentence keeps
    /// the default vanilla 2/2 profile.
    #[test]
    fn mill_put_all_face_down_defaults_to_vanilla_profile() {
        let mill = Effect::Mill {
            count: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        };
        let result = parse_followup_continuation_ast(
            "Put all creature cards milled this way onto the battlefield face down.",
            &mill,
            &mut ParseContext::default(),
        );
        let Some(ContinuationAst::DigFromAmong {
            quantity,
            face_down_profile,
            ..
        }) = result
        else {
            panic!("expected DigFromAmong continuation, got {result:?}");
        };
        assert_eq!(quantity, PutCount::All);
        assert_eq!(face_down_profile, Some(FaceDownProfile::vanilla_2_2()));
    }

    /// CR 701.40a + CR 708.2a + CR 110.2a: Cybership's two-sentence body —
    /// "put the top two cards of [a player]'s library onto the battlefield face
    /// down under your control. They're 2/2 Cyberman artifact creatures." — must
    /// assemble into an `Effect::Manifest` whose `profile` is refined from the
    /// seeded vanilla 2/2 to the Cyberman characteristics by the back-walk
    /// patcher, and whose `enters_under` carries the controller override.
    #[test]
    fn cybership_put_top_face_down_manifest_profile_chain() {
        use super::super::parse_effect_chain;

        let def = parse_effect_chain(
            "Put the top two cards of your library onto the battlefield face down under your control. They're 2/2 Cyberman artifact creatures.",
            AbilityKind::Spell,
        );

        let mut effects: Vec<&AbilityDefinition> = Vec::new();
        let mut node = Some(&def);
        while let Some(d) = node {
            effects.push(d);
            node = d.sub_ability.as_deref();
        }

        // No Unimplemented{they're} anywhere in the chain.
        for d in &effects {
            assert!(
                !matches!(&*d.effect, Effect::Unimplemented { name, .. } if name == "they're"),
                "the 'They're ...' clause must not produce Unimplemented, got {:?}",
                d.effect
            );
        }

        let manifest = effects
            .iter()
            .find(|d| matches!(&*d.effect, Effect::Manifest { .. }))
            .unwrap_or_else(|| {
                panic!(
                    "expected a Manifest effect, chain was {:?}",
                    effects.iter().map(|d| &d.effect).collect::<Vec<_>>()
                )
            });

        let Effect::Manifest {
            count,
            profile,
            enters_under,
            ..
        } = &*manifest.effect
        else {
            unreachable!()
        };
        assert_eq!(*count, QuantityExpr::Fixed { value: 2 }, "top TWO cards");
        assert_eq!(
            *enters_under,
            Some(ControllerRef::You),
            "under your control"
        );
        let profile = profile.as_ref().expect("manifest profile must be set");
        assert_eq!(profile.power, Some(2));
        assert_eq!(profile.toughness, Some(2));
        assert_eq!(profile.extra_core_types, vec![CoreType::Artifact]);
        assert_eq!(profile.subtypes, vec!["Cyberman".to_string()]);
    }

    /// Parser AST-shape test (issue #420 / issue #4273). Birthing Ritual's
    /// full triggered-ability effect text must assemble into:
    ///   Dig(look-only, count=7)
    ///     → Sacrifice(optional)
    ///       → Dig(from_prior_look, dest=Battlefield, filter=Creature+CmcLE(X+1),
    ///             condition: OptionalEffectPerformed,
    ///             else_ability: Dig(from_prior_look, keep_count=0, rest→Library))
    ///
    /// This ordering matches the Oracle text (CR 608.2c: follow written order):
    /// 1. look at top 7 (no player choice)
    /// 2. may sacrifice (interactive)
    /// 3. if you sacrificed: choose from among the looked-at cards (interactive)
    /// 4. put the rest on the bottom (unconditional cleanup — via rest_destination
    ///    in the choice Dig, or via the else_ability decline branch)
    ///
    /// Before the issue #4273 fix the Dig was assembled with dest=Battlefield and
    /// the full filter, presenting WaitingFor::DigChoice BEFORE the sacrifice.
    #[test]
    fn birthing_ritual_assembles_dig_battlefield_sacrifice_chain() {
        use super::super::parse_effect_chain;
        use crate::types::ability::{
            AbilityCondition, Comparator, FilterProp, ObjectScope, QuantityRef, TypeFilter,
            TypedFilter,
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

        // Collect the sub_ability chain.
        let mut chain_nodes: Vec<&crate::types::ability::AbilityDefinition> = Vec::new();
        let mut node = Some(&def);
        while let Some(d) = node {
            chain_nodes.push(d);
            node = d.sub_ability.as_deref();
        }
        let effects: Vec<&Effect> = chain_nodes.iter().map(|d| d.effect.as_ref()).collect();

        // Step 1: look-only Dig (stores private_look_ids, no player choice).
        let Effect::Dig {
            count,
            destination,
            keep_count,
            filter,
            rest_destination,
            source,
            ..
        } = effects[0]
        else {
            panic!(
                "expected look-only Dig as first effect, got {:?}",
                effects[0]
            );
        };
        assert!(
            matches!(
                count,
                QuantityExpr::Fixed { value: 7 }
                    | QuantityExpr::Ref {
                        qty: QuantityRef::Variable { .. }
                    }
            ),
            "look-only Dig should count 7 cards (or equivalent), got {count:?}"
        );
        assert_eq!(
            *keep_count,
            Some(0),
            "look-only Dig: no cards kept (pure peek)"
        );
        assert_eq!(*destination, None, "look-only Dig: no destination");
        // The PutRest continuation ("put the rest on the bottom …") is a
        // separate clause that patches this field after the DigFromAmong
        // restructuring. The resolver ignores rest_destination on a look-only
        // Dig (keep_count=0, reveal=false) because it takes an early return
        // after populating private_look_ids (dig.rs:123). Some(Library) here
        // is correct and harmless.
        assert_eq!(
            *rest_destination,
            Some(Zone::Library),
            "look-only Dig: PutRest patches rest_destination (unused at runtime)"
        );
        assert!(
            matches!(filter, TargetFilter::Any),
            "look-only Dig: no filter on the peek"
        );
        assert_eq!(
            *source,
            DigSource::Library,
            "look-only Dig reads from library"
        );

        // Step 2: optional Sacrifice.
        let Effect::Sacrifice { .. } = effects[1] else {
            panic!("expected Sacrifice as second effect, got {:?}", effects[1]);
        };
        assert!(
            chain_nodes[1].optional,
            "Sacrifice must be optional (\"you may sacrifice\")"
        );

        // The PriorLook choice Dig is wired as Sacrifice.sub_ability.
        let sac_sub = chain_nodes[1]
            .sub_ability
            .as_deref()
            .expect("Sacrifice must have a sub_ability (the PriorLook choice Dig)");

        // Step 3: PriorLook choice Dig, gated on sacrifice performed.
        let Effect::Dig {
            destination: choice_dest,
            keep_count: choice_keep,
            up_to: choice_up_to,
            filter: choice_filter,
            rest_destination: choice_rest,
            source: choice_src,
            reveal: choice_reveal,
            ..
        } = sac_sub.effect.as_ref()
        else {
            panic!(
                "Sacrifice.sub_ability must be a Dig, got {:?}",
                sac_sub.effect
            );
        };
        assert_eq!(
            *choice_src,
            DigSource::PriorLook,
            "choice Dig must be PriorLook"
        );
        assert_eq!(
            *choice_dest,
            Some(Zone::Battlefield),
            "choice Dig puts kept card on battlefield"
        );
        assert_eq!(*choice_keep, Some(1), "\"you may put\" → keep_count 1");
        assert!(*choice_up_to, "\"you may put\" → up_to");
        assert!(!*choice_reveal, "choice Dig is a private look, not reveal");
        assert_eq!(
            *choice_rest,
            Some(Zone::Library),
            "\"put the rest on the bottom\" → rest_destination Library"
        );
        // Creature + mana-value-relative-to-sacrificed-creature filter.
        let TargetFilter::Typed(TypedFilter {
            type_filters,
            properties,
            ..
        }) = choice_filter
        else {
            panic!("expected Typed creature+cmc filter, got {choice_filter:?}");
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
        // Gate: choice Dig only fires if sacrifice was performed.
        assert_eq!(
            sac_sub.condition,
            Some(AbilityCondition::effect_performed()),
            "choice Dig must be gated on OptionalEffectPerformed"
        );
        // Decline branch: all looked-at cards go to library bottom.
        let else_ab = sac_sub
            .else_ability
            .as_deref()
            .expect("choice Dig must carry an else_ability (decline: all on bottom)");
        let Effect::Dig {
            keep_count: else_keep,
            rest_destination: else_rest,
            source: else_src,
            ..
        } = else_ab.effect.as_ref()
        else {
            panic!(
                "else_ability must be a Dig (put all on bottom), got {:?}",
                else_ab.effect
            );
        };
        assert_eq!(
            *else_src,
            DigSource::PriorLook,
            "else_ability Dig must be PriorLook"
        );
        assert_eq!(*else_keep, Some(0), "else_ability Dig keeps no cards");
        assert_eq!(
            *else_rest,
            Some(Zone::Library),
            "else_ability Dig routes all to library bottom"
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
            "clause 3 must NOT emit a stray ChangeZone{{ParentTarget}}"
        );

        // No Unimplemented fallbacks in the main chain.
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::Unimplemented { .. })),
            "no clause should fall back to Unimplemented, got {effects:?}"
        );
    }

    #[test]
    fn mill_put_all_milled_cards_onto_battlefield_tapped_preserves_enter_tapped() {
        use super::super::parse_effect_chain;

        let def = parse_effect_chain(
            "Mill three cards. Put all creature cards milled this way onto the battlefield tapped.",
            AbilityKind::Spell,
        );

        let mut chain: Vec<&Effect> = vec![];
        let mut node = Some(&def);
        while let Some(d) = node {
            chain.push(d.effect.as_ref());
            node = d.sub_ability.as_deref();
        }

        let put = chain
            .iter()
            .find(|effect| matches!(***effect, Effect::ChangeZoneAll { .. }))
            .expect("expected a ChangeZoneAll effect");
        let put = *put;
        match put {
            Effect::ChangeZoneAll { enter_tapped, .. } => {
                assert!(
                    enter_tapped.is_tapped(),
                    "expected milled cards to enter tapped"
                );
            }
            _ => unreachable!(),
        }
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
            selection: crate::types::ability::CardSelectionMode::Chosen,
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
            selection: crate::types::ability::CardSelectionMode::Chosen,
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
                quantity: PutCount::Up(1),
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::NameMatchesAnyPermanent { controller: None },
                ])),
                destination: Some(Zone::Battlefield),
                rest_destination: None,
                enters_under: None,
                face_down_profile: None,
                enter_tapped: false,
                reveal_verb: false,
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
    fn put_counters_on_it_followup_rejects_trailing_attach_conjunct() {
        assert!(
            try_parse_put_counters_on_token_followup(
                "put x +1/+1 counters on it and attach this equipment to it"
            )
            .is_none(),
            "trailing attach conjunct must split as its own clause (Fractal Harness)"
        );
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
        assert!(starts_bare_and_clause(
            "that land gains indestructible until end of turn"
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

    /// CR 608.2c + CR 611.2c: The bare plural pronoun "they" is a valid
    /// anaphoric continuous-clause subject after a bare " and ". Unbreakable
    /// Formation / Overseer of Vault 76 ("put a +1/+1 counter on each [of
    /// those] creature[s] and they gain vigilance until end of turn") and the
    /// multi-keyword list form must all split so the conjunct reaches the
    /// subject-predicate parser. The bare pronoun stays scoped to keyword
    /// predicates; P/T `they get ...` forms can carry conditional riders that
    /// need a separate condition-capable parser path.
    #[test]
    fn bare_and_clause_starts_on_they_anaphoric_continuous_subject() {
        assert!(starts_bare_and_clause(
            "they gain vigilance until end of turn"
        ));
        assert!(starts_bare_and_clause(
            "they gain vigilance, indestructible, and haste until end of turn"
        ));
        assert!(starts_bare_and_clause("they have flying"));
        assert!(starts_bare_and_clause("they lose flying"));
        // Guard: a bare "they" noun-phrase continuation (no continuous verb)
        // must NOT split — e.g. "destroy target creature and they ..." never
        // occurs, but a non-continuous tail must stay un-split here.
        assert!(!starts_bare_and_clause("they attack this turn"));
        assert!(!starts_bare_and_clause("they get +1/+1 until end of turn"));
        assert!(!starts_bare_and_clause("they lose 6 life"));
    }

    /// CR 601.2c + CR 611.2c: A second `"target <noun>"` conjunct joined by a
    /// bare `" and "` is a fresh clause start (its own target + continuous
    /// modification), not a noun-phrase continuation of the first Pump.
    /// Skulduggery / Monoist Circuit-Feeder: "... target creature you control
    /// gets +1/+1 and target creature an opponent controls gets -1/-1". The
    /// conjugated CM verb (gets/gains/has/loses) after the subject is the
    /// discriminator; a bare noun continuation ("target land") has no such verb.
    #[test]
    fn bare_and_clause_starts_on_second_target_continuous_subject() {
        // Skulduggery's two conjuncts.
        assert!(starts_bare_and_clause(
            "target creature an opponent controls gets -1/-1"
        ));
        assert!(starts_bare_and_clause(
            "target creature you control gets +1/+1"
        ));
        // Monoist Circuit-Feeder's debuff conjunct (X distributes downstream).
        assert!(starts_bare_and_clause(
            "target creature an opponent controls gets -0/-x"
        ));
        // Verb-axis coverage: gains / has / loses.
        assert!(starts_bare_and_clause(
            "target creature an opponent controls gains flying"
        ));
        assert!(starts_bare_and_clause(
            "target creature an opponent controls has flying"
        ));
        assert!(starts_bare_and_clause(
            "target creature an opponent controls loses flying"
        ));
        // NO-REGRESSION negatives:
        // Anaphoric shared-target rider — no fresh "target", must NOT split.
        assert!(!starts_bare_and_clause("gains flying"));
        // Genuine noun-phrase continuation — "target land" with no CM verb.
        assert!(!starts_bare_and_clause("target land"));
        assert!(!starts_bare_and_clause("target creature you control"));
    }

    /// CR 102.2 + CR 119.3 + CR 121.1 + CR 608.2c: A second "each opponent"/"each
    /// player" conjunct joined by a bare " and " is a fresh player-scoped clause
    /// start when followed by a conjugated player-action verb. Slitherwisp ("you
    /// draw a card and each opponent loses 1 life"), Curry Favor, Disinformation
    /// Campaign, Bad Deal, Clockwork Fox. The conjugated verb immediately after
    /// the subject is the discriminator; a bare-noun continuation (Goblin
    /// Chainwhirler's "and each creature you control") has no such verb.
    #[test]
    fn bare_and_clause_starts_on_each_player_predicate_subject() {
        // Verb-axis coverage for "each opponent".
        assert!(starts_bare_and_clause("each opponent loses 1 life"));
        assert!(starts_bare_and_clause("each opponent discards a card"));
        assert!(starts_bare_and_clause("each opponent discards two cards"));
        assert!(starts_bare_and_clause("each opponent draws a card"));
        assert!(starts_bare_and_clause("each opponent loses x life"));
        // "each player" subject.
        assert!(starts_bare_and_clause("each player loses 2 life"));
        // NO-REGRESSION negatives:
        // Bare-noun continuation — no conjugated player-action verb.
        assert!(!starts_bare_and_clause("each creature you control"));
        // Goblin Chainwhirler — "each opponent" with no verb before "and each
        // creature ...": immediate-tag cannot match a downstream verb.
        assert!(!starts_bare_and_clause(
            "each opponent and each creature you control"
        ));
        // Possessive noun phrase — not a predicate clause.
        assert!(!starts_bare_and_clause("each opponent's creatures"));
    }

    /// CR 608.2c + CR 122.1: a bare "remove <quantity> <type> counter[s] …"
    /// conjunct is an independent clause start (Amy Pond's "choose a suspended
    /// card you own and remove that many time counters from it"). The "counter"
    /// token within the conjunct is the discriminator; a non-counter "remove …"
    /// continuation (Gustcloak's "remove it from combat") is left un-split.
    #[test]
    fn bare_and_clause_starts_on_remove_counter() {
        assert!(starts_bare_and_clause(
            "remove that many time counters from it"
        ));
        assert!(starts_bare_and_clause("remove a +1/+1 counter"));
        assert!(starts_bare_and_clause("remove two time counters from it"));
        // NO-REGRESSION negatives: non-counter "remove …" conjuncts stay un-split.
        assert!(!starts_bare_and_clause("remove it from combat"));
        assert!(!starts_bare_and_clause(
            "remove that creature from the game"
        ));
        // The counter token must be in THIS conjunct, not a later one: a
        // non-counter "remove" segment whose downstream conjunct mentions a
        // "counter" must not false-split.
        assert!(!starts_bare_and_clause(
            "remove it from combat and counter target spell"
        ));
    }

    /// Regression evidence for the four cards flagged in the §C parse-diff
    /// report (Magma Pummeler, Midnight Oil, Ventifact Bottle, Jumbo Imp).
    /// Each card's "and remove … counter" conjunct IS a genuine independent
    /// instruction in the Oracle text, so splitting it is semantically correct
    /// and improves coverage — the pre-PR parse silently dropped the counter
    /// removal from all four. These asserts document the INTENDED behavior and
    /// serve as evidence that the drift is expected, not a regression.
    #[test]
    fn remove_counter_split_expected_drift_cards() {
        // Magma Pummeler: "prevent that damage and remove that many +1/+1
        // counters from it" — conjunct passed to starts_bare_and_clause.
        assert!(starts_bare_and_clause(
            "remove that many +1/+1 counters from it"
        ));
        // Midnight Oil: "draw an additional card and remove two hour counters
        // from this enchantment" — counter-removal is a separate instruction.
        assert!(starts_bare_and_clause(
            "remove two hour counters from this enchantment"
        ));
        // Ventifact Bottle: "tap it and remove all charge counters from it".
        assert!(starts_bare_and_clause("remove all charge counters from it"));
        // Jumbo Imp: "roll a six-sided die and remove a number of +1/+1
        // counters from this creature equal to the result".
        assert!(starts_bare_and_clause(
            "remove a number of +1/+1 counters from this creature equal to the result"
        ));
    }

    /// CR 102.2 + CR 608.2c: end-to-end chunk split. The "you draw ... and each
    /// opponent <verb> ..." compound must split into TWO chunks (previously the
    /// player-scoped conjunct was swallowed by the first effect and dropped).
    /// Goblin Chainwhirler's bare-noun "and each creature you control" must stay
    /// ONE chunk (no conjugated verb → no split).
    #[test]
    fn bare_and_splits_each_player_predicate_conjunct() {
        assert_eq!(
            clause_texts("you draw a card and each opponent loses 1 life"),
            vec!["you draw a card", "each opponent loses 1 life"]
        );
        assert_eq!(
            clause_texts("you draw two cards and each opponent discards two cards"),
            vec!["you draw two cards", "each opponent discards two cards"]
        );
        // No-regression: Goblin Chainwhirler stays a single DamageAll chunk.
        assert_eq!(
            clause_texts("deals 1 damage to each opponent and each creature you control"),
            vec!["deals 1 damage to each opponent and each creature you control"]
        );
    }

    /// CR 602.5 + CR 611.2c: Skulduggery — symmetric dual-target pump. The
    /// bare `" and "` between the two `"target creature ... gets +/-"` conjuncts
    /// must split so BOTH Pumps survive; previously the second (opponent-debuff)
    /// conjunct was swallowed by the first Pump and dropped. Conjunct 2's
    /// "an opponent controls" resolves the target's controller to
    /// `ControllerRef::Opponent`. Pump carries no duration field — the leading
    /// "Until end of turn," applies at resolution (Pump defaults to until end of
    /// turn), so this test asserts only the two Pump shapes + controllers.
    #[test]
    fn skulduggery_parses_both_pump_targets() {
        use super::super::parse_effect_chain;

        // Real Oracle text (AtomicCards.json), leading "Until end of turn,".
        let def = parse_effect_chain(
            "Until end of turn, target creature you control gets +1/+1 and target creature an opponent controls gets -1/-1.",
            AbilityKind::Spell,
        );

        let mut pumps: Vec<(&PtValue, &PtValue, &TargetFilter)> = Vec::new();
        let mut node = Some(&def);
        while let Some(d) = node {
            if let Effect::Pump {
                power,
                toughness,
                target,
            } = &*d.effect
            {
                pumps.push((power, toughness, target));
            }
            node = d.sub_ability.as_deref();
        }

        assert_eq!(
            pumps.len(),
            2,
            "Skulduggery must produce TWO Pumps (second conjunct was dropped before the fix); got {pumps:?}"
        );

        // Conjunct 1: +1/+1 on a creature you control.
        let (p0, t0, tgt0) = pumps[0];
        assert_eq!(*p0, PtValue::Fixed(1));
        assert_eq!(*t0, PtValue::Fixed(1));
        let TargetFilter::Typed(f0) = tgt0 else {
            panic!("conjunct 1 target should be Typed, got {tgt0:?}");
        };
        assert_eq!(f0.controller, Some(ControllerRef::You));

        // Conjunct 2: -1/-1 on a creature an opponent controls (the dropped one).
        let (p1, t1, tgt1) = pumps[1];
        assert_eq!(*p1, PtValue::Fixed(-1));
        assert_eq!(*t1, PtValue::Fixed(-1));
        let TargetFilter::Typed(f1) = tgt1 else {
            panic!("conjunct 2 target should be Typed, got {tgt1:?}");
        };
        assert_eq!(
            f1.controller,
            Some(ControllerRef::Opponent),
            "conjunct 2 must target an opponent's creature"
        );
    }

    /// CR 102.2 + CR 119.3 + CR 121.1 + CR 608.2c: full-card discriminating gate
    /// for the bare-and "each opponent/player <verb>" split. Slitherwisp ("you
    /// draw a card and each opponent loses 1 life"), Curry Favor (where-X bound to
    /// BOTH conjuncts), Bad Deal (each-opponent discards two). Before the fix the
    /// second conjunct was swallowed by the first effect and the player-scoped
    /// half (life loss / discard) was dropped entirely.
    #[test]
    fn each_player_predicate_conjunct_parses_end_to_end() {
        use super::super::parse_effect_chain;
        use crate::types::ability::PlayerFilter;

        // --- Slitherwisp: Draw (controller) + LoseLife (opponent, amount 1). ---
        let def = parse_effect_chain(
            "You draw a card and each opponent loses 1 life.",
            AbilityKind::Spell,
        );
        let mut nodes: Vec<&AbilityDefinition> = Vec::new();
        let mut node = Some(&def);
        while let Some(d) = node {
            nodes.push(d);
            node = d.sub_ability.as_deref();
        }
        let has_draw = nodes
            .iter()
            .any(|d| matches!(&*d.effect, Effect::Draw { .. }));
        assert!(
            has_draw,
            "Slitherwisp must keep the controller Draw: {nodes:?}"
        );
        let lose = nodes
            .iter()
            .find(|d| matches!(&*d.effect, Effect::LoseLife { .. }))
            .expect("Slitherwisp's each-opponent LoseLife conjunct was dropped before the fix");
        assert_eq!(
            lose.player_scope,
            Some(PlayerFilter::Opponent),
            "life loss is scoped to each opponent"
        );
        let Effect::LoseLife { amount, .. } = &*lose.effect else {
            unreachable!()
        };
        assert_eq!(*amount, QuantityExpr::Fixed { value: 1 });

        // --- Curry Favor: GainLife{Knights} + LoseLife{opponent, X=Knights}. ---
        let def = parse_effect_chain(
            "You gain X life and each opponent loses X life, where X is the number of Knights you control.",
            AbilityKind::Spell,
        );
        let mut nodes: Vec<&AbilityDefinition> = Vec::new();
        let mut node = Some(&def);
        while let Some(d) = node {
            nodes.push(d);
            node = d.sub_ability.as_deref();
        }
        let gain = nodes
            .iter()
            .find(|d| matches!(&*d.effect, Effect::GainLife { .. }))
            .expect("Curry Favor must keep the controller GainLife conjunct");
        let lose = nodes
            .iter()
            .find(|d| matches!(&*d.effect, Effect::LoseLife { .. }))
            .expect("Curry Favor's each-opponent LoseLife conjunct was dropped before the fix");
        assert_eq!(
            lose.player_scope,
            Some(PlayerFilter::Opponent),
            "life loss is scoped to each opponent"
        );
        // where-X binds to BOTH conjuncts: neither amount is a bare Fixed(0).
        let Effect::GainLife {
            amount: gain_amt, ..
        } = &*gain.effect
        else {
            unreachable!()
        };
        let Effect::LoseLife {
            amount: lose_amt, ..
        } = &*lose.effect
        else {
            unreachable!()
        };
        assert_ne!(
            *gain_amt,
            QuantityExpr::Fixed { value: 0 },
            "X must bind on the gain conjunct (got {gain_amt:?})"
        );
        assert_ne!(
            *lose_amt,
            QuantityExpr::Fixed { value: 0 },
            "X must bind on the loss conjunct (got {lose_amt:?})"
        );
        assert_eq!(
            gain_amt, lose_amt,
            "both conjuncts share the same where-X quantity"
        );

        // --- Bad Deal: each-opponent Discard (count 2, opponent scope) present. ---
        let def = parse_effect_chain(
            "You draw two cards and each opponent discards two cards. You lose 2 life and each opponent loses 4 life.",
            AbilityKind::Spell,
        );
        let mut nodes: Vec<&AbilityDefinition> = Vec::new();
        let mut node = Some(&def);
        while let Some(d) = node {
            nodes.push(d);
            node = d.sub_ability.as_deref();
        }
        let discard = nodes
            .iter()
            .find(|d| matches!(&*d.effect, Effect::Discard { .. }))
            .expect("Bad Deal's each-opponent Discard conjunct was dropped before the fix");
        assert_eq!(
            discard.player_scope,
            Some(PlayerFilter::Opponent),
            "discard is scoped to each opponent"
        );
        let Effect::Discard { count, .. } = &*discard.effect else {
            unreachable!()
        };
        assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
    }

    /// CR 611.2c: No-regression guard for the shared-target rider shape. A
    /// "gets +1/+1 and gains <kw>" rider with NO second "target" must stay a
    /// SINGLE shared-target continuous effect — one `GenericEffect` whose one
    /// target (Typed Creature you control) carries both the P/T and keyword
    /// modifications — and must NOT be split into a second targeted clause. The
    /// new `starts_target_continuous_clause_lower` arm only fires on a fresh
    /// "target <noun>" after the bare " and "; this rider's "gains flying ..."
    /// remainder has no such "target", so the arm must leave it un-split.
    #[test]
    fn shared_target_pump_then_keyword_rider_is_not_split() {
        use super::super::parse_effect_chain;

        let def = parse_effect_chain(
            "Target creature you control gets +1/+1 and gains flying until end of turn.",
            AbilityKind::Spell,
        );

        // Exactly one effect node — not split into two targeted clauses.
        let mut effects: Vec<&AbilityDefinition> = Vec::new();
        let mut node = Some(&def);
        while let Some(d) = node {
            effects.push(d);
            node = d.sub_ability.as_deref();
        }
        assert_eq!(
            effects.len(),
            1,
            "rider must stay a single shared-target clause, got {effects:?}"
        );

        let Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } = &*effects[0].effect
        else {
            panic!(
                "rider should lower to one GenericEffect, got {:?}",
                effects[0].effect
            );
        };

        // ONE shared target — Typed Creature you control (not a second target).
        let Some(TargetFilter::Typed(f)) = target else {
            panic!("rider target should be a single Typed filter, got {target:?}");
        };
        assert_eq!(f.controller, Some(ControllerRef::You));

        // The single target carries BOTH the P/T pump and the keyword grant.
        assert_eq!(static_abilities.len(), 1);
        let mods = &static_abilities[0].modifications;
        assert!(
            mods.iter()
                .any(|m| matches!(m, ContinuousModification::AddPower { value: 1 })),
            "missing +1 power on shared target: {mods:?}"
        );
        assert!(
            mods.iter()
                .any(|m| matches!(m, ContinuousModification::AddKeyword { keyword } if *keyword == Keyword::Flying)),
            "missing flying keyword on shared target: {mods:?}"
        );
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

    /// Build a private-look (`reveal: false`) peek `Dig` matching the shape a
    /// "look at the top N cards" clause lowers to before any exile follow-on.
    fn make_peek_dig() -> Effect {
        Effect::Dig {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 4 },
            destination: None,
            keep_count: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: None,
            reveal: false,
            enter_tapped: false,
            source: DigSource::Library,
        }
    }

    /// #1146: "exile one of them face down" after a private `Dig` is recognized
    /// as the Gonti-class continuation (not the wholesale impulse `ExileTop`).
    #[test]
    fn exile_one_of_them_face_down_recognized_after_peek_dig() {
        let dig = make_peek_dig();
        let result = parse_followup_continuation_ast(
            "Exile one of them face down.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(
            result,
            Some(ContinuationAst::ExileOneOfThemFaceDown),
            "the Gonti-class exile-the-dug-card clause must be recognized"
        );
    }

    /// #1146: the variant phrasing "exile one of those cards face down" maps to
    /// the same continuation.
    #[test]
    fn exile_one_of_those_cards_face_down_recognized_after_peek_dig() {
        let dig = make_peek_dig();
        let result = parse_followup_continuation_ast(
            "Exile one of those cards face down.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_eq!(result, Some(ContinuationAst::ExileOneOfThemFaceDown));
    }

    /// #1146 scope guard: the recognizer requires the "face down" CR 406.3
    /// marker — a "one of them" clause WITHOUT it is NOT this continuation, so a
    /// card with different selection semantics is not mis-fused.
    #[test]
    fn exile_one_of_them_without_face_down_is_not_gonti_continuation() {
        assert!(
            !parse_exile_one_of_them_face_down("exile one of them"),
            "without the 'face down' marker this is not the Gonti look-and-exile class"
        );
    }

    /// #1146 regression guard: a genuine pure-peek `Dig` with NO exile clause
    /// (the Delver of Secrets idiom — "look at the top card … you may reveal it")
    /// must NOT lower to the Gonti exile-the-dug-card continuation, so it stays a
    /// `keep_count: 0` peek and never surfaces a `DigChoice`.
    #[test]
    fn delver_pure_peek_is_not_gonti_continuation() {
        let dig = make_peek_dig();
        let result = parse_followup_continuation_ast(
            "You may reveal it.",
            &dig,
            &mut ParseContext::default(),
        );
        assert_ne!(
            result,
            Some(ContinuationAst::ExileOneOfThemFaceDown),
            "a pure-peek 'you may reveal it' must not be fused into the Gonti exile continuation"
        );
    }

    #[test]
    fn from_among_any_number_equipment_not_misrouted_as_of_them() {
        use crate::types::ability::{TypeFilter, TypedFilter};

        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "You may put any number of Equipment cards from among them onto the battlefield.",
            &dig,
            &mut ParseContext::default(),
        );
        let Some(ContinuationAst::DigFromAmong {
            quantity,
            filter,
            destination,
            ..
        }) = result
        else {
            panic!("expected DigFromAmong continuation, got {result:?}");
        };
        assert_eq!(quantity, PutCount::AnyNumber);
        assert!(
            matches!(
                filter,
                TargetFilter::Typed(TypedFilter {
                    ref type_filters,
                    ..
                }) if type_filters.iter().any(
                    |f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Equipment"))
                )
            ),
            "expected Equipment filter, got {filter:?}"
        );
        assert_eq!(destination, Some(Zone::Battlefield));
    }

    #[test]
    fn put_two_of_them_into_hand_still_uses_of_them_arm() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put two of them into your hand.",
            &dig,
            &mut ParseContext::default(),
        );
        assert!(
            matches!(
                result,
                Some(ContinuationAst::DigFromAmong {
                    quantity: PutCount::Exactly(2),
                    filter: TargetFilter::Any,
                    destination: Some(Zone::Hand),
                    ..
                })
            ),
            "expected bare of-them DigFromAmong, got {result:?}"
        );
    }

    #[test]
    fn attach_one_of_them_reflexive_gate_is_not_dig_from_among() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "When you put one or more Equipment onto the battlefield this way, you may attach one of them to a Samurai you control.",
            &dig,
            &mut ParseContext::default(),
        );
        assert!(
            result.is_none(),
            "reflexive attach gate must not re-patch the Dig, got {result:?}"
        );
    }
}
