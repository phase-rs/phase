// CR 613.1f (Layer 6) — keyword-grant static abilities (ability-adding effects).

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;
use nom::character::complete::alphanumeric1;
use nom::combinator::not;
use nom::multi::many1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuleStaticPredicate {
    CantUntap,
    CantAttack,
    CantBlock,
    CantAttackOrBlock,
    CantCrew,
    CantBeActivated,
    CantBeSacrificed,
    MustAttack,
    MustBlock,
    MustBeBlocked,
    Goaded,
    BlockOnlyCreaturesWithFlying,
    Shroud,
    Hexproof,
    MayLookAtTopOfLibrary,
    LoseAllAbilities,
    NoMaximumHandSize,
    MayPlayAdditionalLand,
}

/// CR 702.34a / CR 702.138a / CR 702.187b / CR 702.97 / CR 702.141: maps the
/// leading keyword token of a graveyard-cast-keyword grant ("flashback",
/// "escape", "mayhem", "scavenge", "encore") to its `GrantedCastKeywordKind`.
/// Single authority for the keyword-word → kind dispatch, shared by the static
/// "each ... has <kw>" clause below and the targeted/imperative grant front door
/// in `oracle_effect` so both forms recognize the same keyword set.
pub(crate) fn parse_graveyard_granted_keyword_kind(
    input: &str,
) -> OracleResult<'_, GrantedCastKeywordKind> {
    alt((
        value(GrantedCastKeywordKind::Flashback, tag("flashback")),
        value(GrantedCastKeywordKind::Escape, tag("escape")),
        value(GrantedCastKeywordKind::Mayhem, tag("mayhem")),
        // CR 702.97 / CR 702.141 / CR 702.128: Varolz, Young Deathclaws
        // (scavenge); Wire Surgeons (encore); Naktamun (embalm) grant
        // activated graveyard keywords.
        value(GrantedCastKeywordKind::Scavenge, tag("scavenge")),
        value(GrantedCastKeywordKind::Encore, tag("encore")),
        value(GrantedCastKeywordKind::Embalm, tag("embalm")),
        // CR 702.143a / CR 702.94a: Dream Devourer grants foretell, Aminatou
        // grants miracle — hand-zone cast keywords (gated by `grant_zone`).
        value(GrantedCastKeywordKind::Foretell, tag("foretell")),
        value(GrantedCastKeywordKind::Miracle, tag("miracle")),
    ))
    .parse(input)
}

/// `Some(Ok)` delegates a framed production; `Some(Err)` refuses its whole
/// source. Conditional recursion strictly shortens the source and is outside
/// the unframed callback used by the structural activation query.
fn consult_grant_root_frame(text: &str) -> Option<Result<(), &str>> {
    let lower = text.to_lowercase();
    if crate::parser::oracle_classifier::has_trigger_prefix(&lower)
        || super::oracle_modal::strip_ability_word_with_name(text).is_some()
    {
        return Some(Ok(()));
    }
    if tag::<_, _, OracleError<'_>>("as long as ")
        .parse(lower.as_str())
        .is_ok()
    {
        let owns_body = super::shared::try_split_inverted_as_long_as(&TextPair::new(text, &lower))
            .is_some_and(|split| {
                try_parse_graveyard_keyword_grant_source(&split.effect_text).is_some()
            });
        return Some(if owns_body { Err(text) } else { Ok(()) });
    }
    super::oracle::find_root_activated_colon(text, |prefix| {
        select_grant_head_unframed(prefix)
            .filter(|(subject, payload)| nominates_off_zone_grant(subject, payload))
    })
    .map(|_| Ok(()))
}

/// The same purely lexical nomination protects a provisional label prefix and
/// admits a selected source to the existing complete population/cost proof.
fn nominates_off_zone_grant(subject: &str, payload: &str) -> bool {
    let lower = subject.to_lowercase();
    let has_card_noun = nom_primitives::scan_at_word_boundaries(&lower, |input| {
        terminated(
            alt((tag("cards"), tag("card"))),
            nom::combinator::peek(alt((space1, eof))),
        )
        .parse(input)
    })
    .is_some();
    (has_card_noun || parse_graveyard_granted_keyword_kind(&payload.to_lowercase()).is_ok())
        && nom_primitives::scan_at_word_boundaries(&lower, |input| {
            alt((tag("in "), tag("from "), tag("on "))).parse(input)
        })
        .is_some()
}

fn grant_boundary_has_foreign_owner(
    body: &str,
    subject: &str,
    boundary: usize,
    verb_len: usize,
) -> bool {
    let (nominal, remainder) = parse_type_phrase_folding(subject);
    let complete_nominal = remainder.trim().is_empty() && !matches!(nominal, TargetFilter::Any);
    // A fully parsed name is noun content, including relative-looking words.
    if complete_nominal
        && crate::types::ability::filter_contains_filter_prop(&nominal, &|property| {
            matches!(property, FilterProp::Named { .. })
        })
    {
        return false;
    }
    let lower = body.to_lowercase();
    let embedded = nom_primitives::scan_at_word_boundaries(&lower, |suffix| {
        let start = lower.len() - suffix.len();
        if start < boundary {
            if crate::parser::oracle_target::relative_card_predicate_owns_boundary(
                suffix,
                boundary - start,
            ) {
                return Ok((suffix, ()));
            }
            if let Ok((rest, _)) = nom_quantity::parse_quantity_ref(suffix) {
                let end = lower.len() - rest.len();
                if start < boundary && boundary + verb_len < end {
                    return Ok((rest, ()));
                }
            }
        }
        Err(super::oracle_nom::error::oracle_err(suffix))
    })
    .is_some();
    if embedded {
        return true;
    }
    if complete_nominal {
        return false;
    }
    let subject_lower = subject.to_lowercase();
    if nom_primitives::scan_at_word_boundaries(&subject_lower, |input| {
        all_consuming((tag("that"), opt(preceded(space1, tag("each"))), space0)).parse(input)
    })
    .is_some()
    {
        return true;
    }
    // A canonical verb is foreign only with a complete INITIAL subject. Failed
    // parsing of an unsupported relative does not erase its later outer grant.
    nom_primitives::scan_at_word_boundaries(&lower[..boundary], |suffix| {
        let (rest, ()) = super::oracle_effect::subject::parse_predicate_verb_head(suffix)?;
        let start = boundary - suffix.len();
        let (filter, remainder) = crate::parser::oracle_target::parse_target(body[..start].trim());
        let concrete = match &filter {
            TargetFilter::Any => false,
            TargetFilter::Typed(filter) => {
                !filter.type_filters.is_empty()
                    || !filter.properties.is_empty()
                    || filter.controller.is_some()
            }
            _ => true,
        };
        if start > 0 && concrete && remainder.trim().is_empty() {
            Ok((rest, ()))
        } else {
            Err(super::oracle_nom::error::oracle_err(suffix))
        }
    })
    .is_some()
}

/// Unframed grammar selection only: no source admission, root consultation,
/// cost/effect/static probe or ordinary-keyword semantic decline occurs here.
fn select_grant_head_unframed(body: &str) -> Option<(String, String)> {
    let body = body.trim();
    let lower = body.to_lowercase();
    let accept = |subject: &str, predicate: &str, payload: &str, verb_len| {
        let boundary = body.len() - predicate.len();
        if grant_boundary_has_foreign_owner(body, subject, boundary, verb_len) {
            return None;
        }
        let (_, subject) = nom_on_lower(subject, &subject.to_lowercase(), |input| {
            opt(value((), tag("each "))).parse(input)
        })?;
        Some((subject.trim().to_owned(), payload.trim().to_owned()))
    };
    super::shared::parse_continuous_subject_predicate(
        &TextPair::new(body, &lower),
        |subject, predicate| {
            let (verb_len, payload) =
                nom_on_lower(predicate, &predicate.to_lowercase(), |input| {
                    alt((value(3, tag("has ")), value(4, tag("have ")))).parse(input)
                })?;
            accept(subject, predicate, payload, verb_len)
        },
    )
    .or_else(|| {
        let (subject, verb, _) = nom_primitives::scan_preceded(body, |input| {
            terminated(alt((tag_no_case("has"), tag_no_case("have"))), eof).parse(input)
        })?;
        (!subject.trim().is_empty())
            .then(|| accept(subject.trim(), verb, "", verb.len()))
            .flatten()
    })
}

fn parse_graveyard_keyword_grant_head(text: &str) -> Option<(String, String)> {
    let stripped = strip_reminder_text(text);
    let (_, body) = nom_on_lower(&stripped, &stripped.to_lowercase(), |input| {
        opt(super::parse_leading_turn_scope).parse(input)
    })?;
    if consult_grant_root_frame(body).is_some() {
        return None;
    }
    select_grant_head_unframed(body)
}

fn complete_ordinary_keyword(payload: &str) -> Option<Keyword> {
    let payload = payload.trim();
    let phrase = if let Ok((_, sentence)) =
        all_consuming(terminated(nom_primitives::parse_period_sentence, space0)).parse(payload)
    {
        // The sentence combinator proves exactly one structural terminator.
        sentence.get(..sentence.len().checked_sub(1)?)?
    } else {
        all_consuming(terminated(
            take_while1::<_, _, OracleError<'_>>(|c| c != '.'),
            space0,
        ))
        .parse(payload)
        .ok()?
        .1
    };
    if phrase.trim().is_empty() {
        return None;
    }
    super::oracle_keyword::parse_router_keyword_fragment(&phrase.trim().to_lowercase())
        .filter(|keyword| !matches!(keyword, Keyword::Unknown(_)))
}

pub(crate) fn try_parse_graveyard_keyword_grant_clause(
    text: &str,
) -> Option<(TargetFilter, GrantedCastKeywordKind, String)> {
    let (subject, keyword_text) = parse_graveyard_keyword_grant_head(text)?;
    let keyword_text = keyword_text.trim_end_matches('.').to_owned();

    let kind = nom_on_lower(
        &keyword_text,
        &keyword_text.to_lowercase(),
        parse_graveyard_granted_keyword_kind,
    )?
    .0;

    let (filter, remainder) = parse_type_phrase_folding(&subject);
    // CR 113.6b: the affected filter's zone must match the keyword's functional
    // zone (graveyard for flashback/escape/…, hand for foretell/miracle). A
    // mismatch (foretell-in-graveyard, flashback-in-hand) declines the grant.
    if !remainder.trim().is_empty()
        || !super::shared::target_filter_matches_granted_population(
            &filter,
            &subject,
            kind.grant_zone(),
        )
    {
        return None;
    }

    Some((filter, kind, keyword_text))
}

/// CR 702.97a / CR 702.141a: Resolve the keyword phrase on a graveyard grant
/// line — fixed costs ("encore {5}"), inline variable costs ("encore {X}, where
/// X is its mana value" → `SelfManaValue`), or bare keyword tokens when the cost
/// arrives in a separate continuation sentence (handled upstream).
fn parse_graveyard_granted_keyword_phrase(
    keyword_text: &str,
    kind: GrantedCastKeywordKind,
) -> Option<Keyword> {
    let (_, keyword_text) = all_consuming(terminated(
        take_while1::<_, _, OracleError<'_>>(|character: char| character != '.'),
        (opt(tag(".")), space0),
    ))
    .parse(keyword_text.trim())
    .ok()?;
    let keyword_text = keyword_text.trim();
    let lower = keyword_text.to_lowercase();
    let (_, tail) = nom_on_lower(keyword_text, &lower, parse_graveyard_granted_keyword_kind)?;
    if tail.trim().is_empty() {
        return None;
    }
    let (phrase, where_x) =
        super::oracle_nom::bridge::split_once_on_lower(keyword_text, &lower, ", where x is ")
            .map_or(Some((keyword_text, None)), |(phrase, quantity)| {
                let (_, quantity) =
                    nom_quantity::parse_quantity_ref_complete(quantity.trim()).ok()?;
                Some((phrase, Some(quantity)))
            })?;
    let keyword = super::oracle_keyword::parse_router_keyword_fragment(&phrase.to_lowercase())?;
    normalize_graveyard_granted_keyword(keyword, where_x, kind)
}

/// CR 109.5 + CR 113.6b: prove the full population, live turn scope and cost
/// before any dedicated off-zone grant can reach a permissive static fallback.
/// `Some(Err(..))` owns a refusal of the complete semantic unit.
pub(crate) fn try_parse_graveyard_keyword_grant_source(
    line: &str,
) -> Option<Result<StaticDefinition, &str>> {
    // Ability-word conditions belong to the enclosing static/document owner.
    if super::oracle_modal::strip_ability_word_with_name(line).is_some() {
        return None;
    }
    let stripped = strip_reminder_text(line);
    let lower = stripped.to_lowercase();
    if super::type_change::parse_arcane_adaptation_chosen_type_static(
        &TextPair::new(&stripped, &lower),
        &stripped,
    )
    .is_some()
    {
        return None;
    }
    let (condition, body) = nom_on_lower(&stripped, &lower, |input| {
        opt(super::parse_leading_turn_scope).parse(input)
    })?;
    let body_lower = body.to_lowercase();
    if let Some(frame) = consult_grant_root_frame(body) {
        return frame.err().map(|_| Err(line));
    }
    let (grant, continuation) =
        super::oracle_nom::bridge::split_once_on_lower(body, &body_lower, ". ")
            .map_or((body, None), |(grant, cost)| (grant, Some(cost)));
    let (subject, keyword_text) = parse_graveyard_keyword_grant_head(grant)?;
    let keyword_lower = keyword_text.to_lowercase();
    if parse_graveyard_granted_keyword_kind(&keyword_lower).is_err()
        && continuation.is_none()
        && complete_ordinary_keyword(&keyword_text).is_some()
    {
        return None;
    }
    if !nominates_off_zone_grant(&subject, &keyword_text) {
        return None;
    }
    // Nominate off-zone card-grant syntax independently of noun/kind validity.
    // This also captures empty or unknown payloads without claiming success.
    let complete = || {
        let (affected, kind, _) = try_parse_graveyard_keyword_grant_clause(grant)?;
        let keyword = match continuation {
            Some(cost) => {
                let keyword_lower = keyword_text.to_lowercase();
                let (_, first_kind) =
                    all_consuming(terminated(parse_graveyard_granted_keyword_kind, space0))
                        .parse(keyword_lower.as_str())
                        .ok()?;
                if first_kind != kind {
                    return None;
                }
                let (_, cost) = all_consuming(terminated(
                    take_while1::<_, _, OracleError<'_>>(|character: char| character != '.'),
                    (opt(tag(".")), space0),
                ))
                .parse(cost.trim())
                .ok()?;
                super::oracle::parse_graveyard_keyword_continuation(cost.trim(), kind)?
            }
            None => parse_graveyard_granted_keyword_phrase(&keyword_text, kind)?,
        };
        if !kind.matches_keyword(&keyword) {
            return None;
        }
        let mut definition = StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::AddKeyword { keyword }])
            .description(line.to_owned());
        definition.condition = condition.clone();
        Some(definition)
    };
    Some(complete().ok_or(line))
}

/// CR 702.97a / CR 702.141a: When a graveyard grant binds X to the recipient
/// card's mana value, lower to `ManaCost::SelfManaValue` so runtime synthesis
/// concretizes the activated ability's mana sub-cost as generic mana.
fn binds_recipient_mana_value(where_x: &Option<QuantityRef>) -> bool {
    matches!(
        where_x,
        Some(QuantityRef::SelfManaValue)
            | Some(QuantityRef::ObjectManaValue {
                scope: ObjectScope::Recipient,
            })
    )
}

fn graveyard_granted_kind_for_keyword(keyword: &Keyword) -> Option<GrantedCastKeywordKind> {
    [
        GrantedCastKeywordKind::Flashback,
        GrantedCastKeywordKind::Escape,
        GrantedCastKeywordKind::Mayhem,
        GrantedCastKeywordKind::Scavenge,
        GrantedCastKeywordKind::Encore,
        GrantedCastKeywordKind::Embalm,
        GrantedCastKeywordKind::Foretell,
        GrantedCastKeywordKind::Miracle,
    ]
    .into_iter()
    .find(|kind| kind.matches_keyword(keyword))
}

fn finalize_graveyard_zone_grant_keyword(
    keyword: Keyword,
    where_x: Option<QuantityRef>,
) -> Keyword {
    let Some(kind) = graveyard_granted_kind_for_keyword(&keyword) else {
        return keyword;
    };
    normalize_graveyard_granted_keyword(keyword.clone(), where_x, kind).unwrap_or(keyword)
}

fn normalize_graveyard_granted_keyword(
    keyword: Keyword,
    where_x: Option<QuantityRef>,
    kind: GrantedCastKeywordKind,
) -> Option<Keyword> {
    if !kind.matches_keyword(&keyword) {
        return None;
    }
    match (keyword, &where_x) {
        (Keyword::Encore(_), where_x) if binds_recipient_mana_value(where_x) => {
            Some(Keyword::Encore(ManaCost::SelfManaValue))
        }
        (Keyword::Scavenge(_), where_x) if binds_recipient_mana_value(where_x) => {
            Some(Keyword::Scavenge(ManaCost::SelfManaValue))
        }
        (keyword, None) => Some(keyword),
        _ => None,
    }
}

/// CR 702.97 / CR 702.141: Parse a single-sentence graveyard keyword grant whose
/// keyword (and optional inline "where X is its mana value" binding) lives on
/// the same line — Sliver Gravemother's "encore {X}, where X is its mana value".
/// Continuation-sentence grants (Wire Surgeons / Varolz) return `None`.
#[cfg(test)]
pub(crate) fn try_parse_graveyard_keyword_grant_static(line: &str) -> Option<StaticDefinition> {
    let stripped = strip_reminder_text(line);
    let lower = stripped.to_lowercase();
    // Same period boundary as `try_parse_graveyard_keyword_static_with_continuation`
    // in oracle.rs — if a continuation sentence is present, the inline path must
    // not parse a bare keyword off the first sentence and drop the cost clause.
    if super::oracle_nom::bridge::split_once_on_lower(&stripped, &lower, ". ").is_some() {
        return None;
    }

    try_parse_graveyard_keyword_grant_source(line)?.ok()
}

pub(crate) fn parse_keyword_with_where_x(input: &str) -> Option<(Keyword, Option<QuantityRef>)> {
    type VE<'a> = OracleError<'a>;

    let input = input.trim().trim_end_matches('.');
    let (rest, keyword_text) = nom::bytes::complete::take_till::<_, _, VE<'_>>(|c| c == ',')
        .parse(input)
        .ok()?;
    let keyword = super::oracle_keyword::parse_granted_keyword_fragment(keyword_text.trim())?;
    let rest = rest.trim();
    if rest.is_empty() {
        return Some((keyword, None));
    }

    let (_, qty_text) = preceded(
        tag_no_case::<_, _, VE<'_>>(", where x is "),
        nom::combinator::rest,
    )
    .parse(rest)
    .ok()?;
    let (_, qty) =
        super::oracle_nom::quantity::parse_quantity_ref_complete(qty_text.trim()).ok()?;
    Some((keyword, Some(qty)))
}

#[cfg(test)]
pub(crate) fn parse_spells_have_keyword_for_test(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    parse_spells_have_keyword(&tp, text)
}

/// CR 702.152a + CR 118.9 + CR 601.2f: A static grant of an alternative-cost
/// keyword whose cost is the affected spell's own mana cost — Henzie "Toolbox"
/// Torre: "Each creature spell you cast with mana value 4 or greater has blitz.
/// The blitz cost is equal to its mana cost." The two-sentence form (keyword +
/// "The <kw> cost is equal to its mana cost" continuation) lowers to a single
/// `CastWithKeyword` whose keyword carries `ManaCost::SelfManaCost`, resolved
/// per-spell at cast time — the same self-referential cost the granted flashback
/// path uses (Dream Devourer). Table-driven so further cast-variant alt-cost
/// keywords that use this exact template slot in without new control flow. Only
/// keywords the casting flow already surfaces from `effective_spell_keywords`
/// belong here, so the grant actually functions:
///   - `blitz` (Henzie "Toolbox" Torre) → granted `CastingVariant::Blitz`.
///   - `replicate` (Hatchery Sliver: "Each Sliver spell you cast has replicate.
///     The replicate cost is equal to its mana cost.") — CR 702.56a: the granted
///     `CastWithKeyword { Replicate(SelfManaCost) }` is read as a repeatable
///     additional cost by `effective_replicate_additional_cost_instances`, and
///     the per-instance copy trigger is synthesized by the
///     `dynamically_granted_replicate` seam in `game/triggers.rs`, so granted
///     Replicate functions end-to-end with no engine change.
fn parse_granted_self_cost_keyword(keyword_str: &str) -> Option<Keyword> {
    [
        // CR 702.152a: Blitz's cost enum (`BlitzCost`) admits a non-mana residual
        // (Sabin/Underdog), but a *granted* self-referential blitz is always pure
        // mana ("equal to its mana cost"), so the grant binds `BlitzCost::Mana`.
        (
            "blitz",
            (|c| Keyword::Blitz(crate::types::keywords::BlitzCost::Mana(c)))
                as fn(ManaCost) -> Keyword,
        ),
        ("replicate", Keyword::Replicate as fn(ManaCost) -> Keyword),
    ]
    .into_iter()
    .find_map(|(name, ctor)| {
        let parsed: OracleResult<'_, ()> = (|| {
            let (i, _) = tag::<_, _, OracleError<'_>>(name).parse(keyword_str)?;
            let (i, _) = tag(". the ").parse(i)?;
            let (i, _) = tag(name).parse(i)?;
            let (i, _) = tag(" cost is equal to ").parse(i)?;
            let (i, _) = alt((tag("its"), tag("that card's"), tag("the card's"))).parse(i)?;
            let (i, _) = tag(" mana cost").parse(i)?;
            Ok((i, ()))
        })();
        let (rest, ()) = parsed.ok()?;
        rest.trim()
            .trim_end_matches('.')
            .trim()
            .is_empty()
            .then(|| ctor(ManaCost::SelfManaCost))
    })
}

/// Parse "[Type] spells you cast [from zone] have [keyword]" patterns.
/// CR 702.51a: Grants a keyword (typically convoke) to spells matching a filter during casting.
/// Also handles "Creature cards you own that aren't on the battlefield have flash."
pub(crate) fn parse_spells_have_keyword(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;

    let scoped_tp = nom_tag_tp(tp, "during your turn, ");
    let condition = scoped_tp.as_ref().map(|_| StaticCondition::DuringYourTurn);
    let tp = scoped_tp.as_ref().unwrap_or(tp);

    // CR 702.74a: keyword-grant lines that read "... gain <keyword> as you cast
    // them" (Ashling, the Limitless) carry a trailing " as you cast them" after
    // the keyword text. Strip it structurally (period then suffix) BEFORE the
    // separator split so the keyword residue is "evoke {4}" rather than
    // "evoke {4} as you cast them" — `parse_keyword_with_where_x` takes up to the
    // first comma as keyword_text, and `parse_granted_keyword_fragment` would reject
    // the trailing clause. Mirror the existing trailing-period handling.
    let trimmed_tp = tp.trim_end_matches('.');
    let trimmed_tp = trimmed_tp
        // allow-noncombinator: structural trailing-clause cleanup on the pre-delimited grant phrase, not parsing dispatch (mirrors the trim_end_matches period strip above).
        .strip_suffix(" as you cast them")
        .unwrap_or(trimmed_tp);
    let tp = &trimmed_tp;

    // Pattern 1: "[type] spell(s) you cast [from zone] have/has/gain/gains [keyword]."
    // Find the predicate separator to split subject from keyword.
    // CR 702.74a: "... spells you cast ... gain <keyword>" (Ashling) uses "gain"/
    // "gains" as the grant verb instead of "have"/"has". The grant verb is tried
    // in the fixed priority order have → has → gain → gains (first verb in this
    // list that appears anywhere wins); the real card class carries exactly one
    // grant verb, so the order only disambiguates hypothetical mixed-verb text.
    let (have_pos, have_len) = tp
        .lower
        .match_indices(" have ")
        .next()
        .map(|(pos, sep)| (pos, sep.len()))
        .or_else(|| {
            tp.lower
                .match_indices(" has ")
                .next()
                .map(|(pos, sep)| (pos, sep.len()))
        })
        .or_else(|| {
            tp.lower
                .match_indices(" gain ")
                .next()
                .map(|(pos, sep)| (pos, sep.len()))
        })
        .or_else(|| {
            tp.lower
                .match_indices(" gains ")
                .next()
                .map(|(pos, sep)| (pos, sep.len()))
        })?;
    let subject = &tp.lower[..have_pos];
    let keyword_str = tp.lower[have_pos + have_len..].trim();

    // Parse the keyword — must be a valid keyword. A trailing "where X is …"
    // clause binds an earlier variable-X mana-value qualifier on the subject.
    // CR 702.152a: an alternative-cost keyword granted with "The <kw> cost is
    // equal to its mana cost" (Henzie "Toolbox" Torre) carries the self-cost and
    // consumes its continuation sentence here, ahead of the plain keyword parse.
    let (keyword, where_x) = match parse_granted_self_cost_keyword(keyword_str) {
        Some(kw) => (kw, None),
        None => parse_keyword_with_where_x(keyword_str)?,
    };

    // CR 611.2f: "The first <qualifier> spell you cast [from <zone>] <timing> has
    // [keyword]" — a once-per-turn keyword grant gated on the first qualifying
    // spell of the turn (Peri Brown, The Twelfth Doctor, Maelstrom Nexus,
    // Wild-Magic Sorcerer, Current Curriculum). Reuses the same
    // `parse_nth_qualified_spell_filter` grammar + `nth_qualified_spell_condition`
    // gate as the paired cost-modifier consumer, so the qualifying spell filter,
    // cast-origin restriction, and `SpellsCastThisTurn == 0` gate are all preserved
    // instead of collapsing to "every spell you cast".
    match parse_nth_qualified_spell_filter(subject) {
        // Not an Nth-qualified-spell line — fall through to the ordinary
        // "[type] spells you cast [from zone] have [keyword]" patterns below.
        NthQualifiedSpell::NotApplicable => {}
        // The shape is present but the qualifier/timing isn't representable. Fall
        // through (NOT a `return None`) so the existing gateless static is
        // preserved for not-yet-representable qualifiers — no regression.
        NthQualifiedSpell::UnsupportedQualifier => {}
        NthQualifiedSpell::Supported {
            filter,
            timing,
            ordinal,
        } => {
            // CR 601.2f: trailing-residue guard. `parse_nth_qualified_spell_filter`
            // discards any text after the timing phrase; if that region is
            // non-empty an unrepresentable qualifier was dropped (Rain of Riches'
            // "that mana from a Treasure was spent to cast"; TARDIS Bay's
            // post-timing "with mana value 2 or greater"). Decline rather than emit
            // a residue-blind gate — fall through to the existing gateless static.
            if nth_qualified_spell_subject_fully_consumed(subject) {
                // CR 601.2a: scope `ControllerRef::You` to every leaf (And/Not
                // recursion) so an opponent's qualifying spell never qualifies.
                let affected =
                    apply_spell_keyword_subject_constraints(filter.clone(), None, None, Vec::new());
                // CR 611.2f: `SpellsCastThisTurn(filter) == 0` first-spell gate,
                // plus `DuringYourTurn` for the controller-turn timing (a clean
                // "during each of your turns" subject with no post-timing residue;
                // TARDIS Bay itself declines above because its MV qualifier follows
                // the timing). When a leading "during your turn," scope was already
                // stripped, combine both rather than dropping either.
                let nth_qualified = nth_qualified_spell_condition(&filter, &timing, ordinal);
                let combined_condition = match condition.clone() {
                    Some(leading) => StaticCondition::And {
                        conditions: vec![leading, nth_qualified],
                    },
                    None => nth_qualified,
                };
                return Some(
                    StaticDefinition::new(StaticMode::CastWithKeyword { keyword })
                        .affected(affected)
                        .condition(combined_condition)
                        .description(text.to_string())
                        .active_zones(vec![Zone::Battlefield]),
                );
            }
        }
    }

    // Find "spells you cast" in the subject — may be preceded by a type descriptor
    let spell_marker = subject
        .match_indices("spells you cast")
        .next()
        .map(|(pos, matched)| (pos, matched.len()))
        .or_else(|| {
            subject
                .match_indices("spell you cast")
                .next()
                .map(|(pos, matched)| (pos, matched.len()))
        });
    if let Some((marker_pos, marker_len)) = spell_marker {
        let raw_type_part = subject[..marker_pos].trim();
        let type_part = tag::<_, _, VE<'_>>("each ")
            .parse(raw_type_part)
            .map_or(raw_type_part, |(rest, _)| rest.trim());
        let after_spells = subject[marker_pos + marker_len..].trim();

        // Walk a cursor through optional qualifiers — zone first, then MV —
        // so combinations like "from exile with mana value 4 or greater" parse
        // correctly. Each qualifier consumes its own bytes.
        let mut cursor = after_spells;

        // Parse optional zone qualifier via the shared authority below.
        let zone_filter = if let Some((rest, prop)) = parse_cast_origin_zone_qualifier(cursor) {
            cursor = rest;
            Some(prop)
        } else {
            None
        };

        // CR 202.3: Optional "with mana value N or greater/less" qualifier
        // (Imoti, Celebrant of Bounty: "Spells you cast with mana value 6 or
        // greater have cascade."). Variable-X thresholds may be bound by the
        // keyword clause's trailing "where X is …" quantity (Abaddon class).
        let mv_filter = parse_mana_value_suffix(cursor, &mut ParseContext::default()).and_then(
            |(prop, consumed)| {
                let FilterProp::Cmc { comparator, value } = prop else {
                    return None;
                };
                let value = match where_x.as_ref() {
                    Some(qty) => bind_where_x_in_quantity_expr(value, qty)?,
                    None => match value {
                        QuantityExpr::Fixed { .. } => value,
                        _ => return None,
                    },
                };
                cursor = cursor[consumed..].trim_start();
                Some(FilterProp::Cmc { comparator, value })
            },
        );
        // CR 105.2: trailing "that's one or more colors"/"that's exactly N colors" relative clause → ColorCount.
        let color_props = if let Some((props, consumed)) =
            crate::parser::oracle_target::parse_that_clause_suffix(cursor, None)
        {
            cursor = cursor[consumed..].trim_start();
            props
        } else {
            Vec::new()
        };
        let _ = cursor; // qualifiers are optional; remaining slice is unused

        let mut supertype_props: Vec<FilterProp> = Vec::new();
        let base_filter = if type_part.is_empty() {
            // "Spells you cast" (no type prefix) — applies to all spells
            TargetFilter::Typed(TypedFilter::card())
        } else {
            // CR 205.4a: peel leading supertype word(s) BEFORE parse_type_phrase_folding, which only
            // emits HasSupertype for a supertype prefixed before a type word (requires a trailing
            // space); a bare "legendary" would otherwise be dropped, and an un-peeled prefix would
            // double-emit. Peel here (emit once) and pass only the remainder to parse_type_phrase_folding.
            let type_prefix_original = tp.original[..marker_pos].trim();
            let lower_prefix = type_prefix_original.to_lowercase();
            let prefix_tp = TextPair::new(type_prefix_original, &lower_prefix);
            let prefix_tp = nom_tag_tp(&prefix_tp, "each ").unwrap_or(prefix_tp);
            let mut peel_lower = prefix_tp.lower;
            let mut peel_offset = 0usize;
            while let Ok((rest, supertype)) = nom_target::parse_supertype_word(peel_lower) {
                // CR 205.4a: parse_supertype_word consumes no boundary by contract, so the
                // caller must require a word boundary (space, punctuation, or end-of-string)
                // after the supertype — otherwise a longer word with a supertype prefix
                // ("snow" in "snowman") would be mis-peeled. A bare trailing supertype
                // ("legendary") legitimately ends at end-of-string.
                let at_boundary = rest
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_');
                if !at_boundary {
                    break;
                }
                supertype_props.push(FilterProp::HasSupertype { value: supertype });
                // Consume the supertype word plus its trailing whitespace boundary
                // via nom (space0 — a bare trailing supertype has no following space).
                let rest = space0::<_, VE<'_>>
                    .parse(rest)
                    .map_or(rest, |(after, _)| after);
                peel_offset += peel_lower.len() - rest.len();
                peel_lower = rest;
            }
            let type_remainder = prefix_tp.original[peel_offset..].trim();
            if type_remainder.is_empty() {
                TargetFilter::Typed(TypedFilter::card())
            } else {
                parse_type_phrase_folding(type_remainder).0
            }
        };
        let mut extra_props = supertype_props;
        extra_props.extend(color_props);
        // CR-correct affected scope: `apply_spell_keyword_subject_constraints`
        // recurses into `TargetFilter::Or` so compound type prefixes ("instant
        // and sorcery spells you cast have affinity for creatures") preserve
        // each branch instead of collapsing to all spells.
        let affected = apply_spell_keyword_subject_constraints(
            base_filter,
            zone_filter,
            mv_filter,
            extra_props,
        );

        let mut def = StaticDefinition::new(StaticMode::CastWithKeyword { keyword })
            .affected(affected)
            .description(text.to_string())
            .active_zones(vec![Zone::Battlefield]);
        if let Some(condition) = condition.clone() {
            def = def.condition(condition);
        }
        return Some(def);
    }

    // Pattern 2: "Creature cards you own that aren't on the battlefield have flash"
    // This grants flash to cards in non-battlefield zones. The subject-to-filter
    // grammar is shared with the land type-change compound handler (Dune
    // Chanter) via `parse_owned_off_battlefield_subject_filter`.
    if nom_primitives::scan_contains(subject, "cards you own that aren't on the battlefield") {
        let subject_original = &tp.original[..subject.len()];
        let affected = parse_owned_off_battlefield_subject_filter(subject_original)?;
        let mut def = StaticDefinition::new(StaticMode::CastWithKeyword { keyword })
            .affected(affected)
            .description(text.to_string())
            .active_zones(vec![Zone::Battlefield]);
        if let Some(condition) = condition.clone() {
            def = def.condition(condition);
        }
        return Some(def);
    }
    // Pattern 3: "[type] cards in/from [your zone] have [keyword]"
    // CR 702.81a (Retrace): Grants a casting keyword to cards in a specific zone.
    // Six grants retrace to nonland permanent cards in your graveyard.
    // Emits a Continuous static with AddKeyword so the off-zone keyword-grant
    // path (`effective_off_zone_keywords`) sees the grant and the card becomes
    // castable from the graveyard.
    {
        let (base_filter, rest) = parse_type_phrase_folding(subject);
        if rest.trim().is_empty() && target_filter_is_your_graveyard(&base_filter) {
            let keyword = finalize_graveyard_zone_grant_keyword(keyword, where_x.clone());
            let mut def = StaticDefinition::continuous()
                .affected(base_filter)
                .modifications(vec![ContinuousModification::AddKeyword { keyword }])
                .description(text.to_string());
            if let Some(condition) = condition.clone() {
                def = def.condition(condition);
            }
            return Some(def);
        }
    }

    // Pattern 4: "[type] spells have [keyword]" — the NON-possessive, all-players
    // form (Ood Sphere: "Noncreature spells have convoke."). Planes and other
    // global statics grant a casting keyword to EVERY player's matching spells,
    // not just the controller's.
    //
    // CR 702.51a + CR 113.6b: convoke (and any casting keyword) "functions while
    // the spell with convoke is on the stack" — it is read during casting via
    // `granted_spell_keywords`, which consumes ONLY `StaticMode::CastWithKeyword`.
    // The generic anthem `AddKeyword` continuous static (the fallthrough this
    // branch preempts) applies in Layer 6 to battlefield objects and is never seen
    // by the casting-keyword path, so the grant was runtime-inert. Emit
    // `CastWithKeyword` so it actually functions.
    //
    // Unlike Pattern 1's possessive "spells you cast" form, the affected filter
    // stays controller-agnostic: `apply_spell_keyword_subject_constraints` would
    // force-inject `ControllerRef::You`, wrongly restricting the grant to the
    // plane-controller's spells. The `you cast`/`you own`/graveyard subjects are
    // already claimed by Patterns 1-3 above, so reaching here means the subject
    // carries no possessive; the trailing noun being `spell`/`spells` (a
    // word-boundary last-word scan, CLAUDE.md `rsplit(' ').next()` idiom) is the
    // sole discriminator.
    let last_word = subject.rsplit(' ').next().unwrap_or("");
    if matches!(last_word, "spell" | "spells") {
        // `type_part` is everything before the trailing noun. The offset idiom
        // (`len - last_word.len()`) is correct for the last space-delimited token.
        let type_part = subject[..subject.len() - last_word.len()].trim();
        let base_filter = if type_part.is_empty() {
            TargetFilter::Typed(TypedFilter::card())
        } else {
            parse_type_phrase_folding(type_part).0
        };
        let mut def = StaticDefinition::new(StaticMode::CastWithKeyword { keyword })
            .affected(base_filter)
            .description(text.to_string())
            .active_zones(vec![Zone::Battlefield]);
        if let Some(condition) = condition.clone() {
            def = def.condition(condition);
        }
        return Some(def);
    }
    None
}

/// Parse the static permission "You may cast [type] spells as though they had
/// flash." (Leyline of Anticipation, Vedalken Orrery, Vivien, Champion of the
/// Wilds' first ability).
///
/// CR 601.3b: An effect that lets a player cast a spell "as though it had flash"
/// lets that player begin to cast it at instant speed. CR 702.8a: flash means
/// the spell may be cast any time its controller could cast an instant.
///
/// This must emit `StaticMode::CastWithKeyword { keyword: Flash }` with the
/// spell-type filter in `affected` — that is the ONLY static mode the
/// flash-timing path (`granted_spell_keywords` in casting.rs) actually reads.
/// The legacy `StaticMode::CastWithFlash` carries no spell filter and is never
/// consumed by that path, so it silently dropped both the timing grant and the
/// "creature spells" restriction (issue #1957). Mirrors the activated/triggered
/// `try_parse_cast_as_though_flash_permission` (oracle_effect) so the static and
/// duration-scoped forms share one filter-construction contract.
pub(crate) fn parse_cast_as_though_flash_static(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    let (type_text, all_players) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, all_players) = alt((
            value(false, tag::<_, _, OracleError<'_>>("you may ")),
            value(true, tag("players may ")),
            value(true, tag("any player may ")),
            value(false, tag("")),
        ))
        .parse(i)?;
        let (i, _) = tag("cast ").parse(i)?;
        // "[type] spells as though they had flash" — the bare "spells" form
        // (no type prefix) grants flash to every spell (Leyline of Anticipation).
        let (i, type_part) = alt((
            value("", tag("spells as though they had flash")),
            map(
                terminated(
                    take_until(" spells as though they had flash"),
                    tag(" spells as though they had flash"),
                ),
                str::trim,
            ),
        ))
        .parse(i)?;
        let (i, _) = opt(tag(".")).parse(i)?;
        let (i, _) = eof.parse(i)?;
        Ok((i, (type_part.to_string(), all_players)))
    })?
    .0;

    // CR 601.3b: scope the grant to the spell class. A bare "spells" grant
    // applies to every spell the controller casts; a typed grant ("creature
    // spells") constrains to that type. "Players may" / "Any player may" forms
    // intentionally remain unscoped, while "you may" forms recurse through
    // `TargetFilter::Or` via `apply_spell_keyword_subject_constraints`.
    let base_filter = if type_text.is_empty() {
        TargetFilter::Typed(TypedFilter::card())
    } else {
        let phrase = format!("{type_text} spells");
        parse_type_phrase_folding(&phrase).0
    };
    let affected = if all_players {
        base_filter
    } else {
        apply_spell_keyword_subject_constraints(base_filter, None, None, Vec::new())
    };

    Some(
        StaticDefinition::new(StaticMode::CastWithKeyword {
            keyword: Keyword::Flash,
        })
        .affected(affected)
        .description(text.to_string())
        .active_zones(vec![Zone::Battlefield]),
    )
}

/// CR 601.2a: Optional origin-zone qualifier on a "spell(s) you cast" subject —
/// "from exile" / "from your hand". Single authority for the recognized zone
/// list, shared by the keyword-grant subject walk and the alternative-cost
/// grant lowering (`parse_spells_alternative_cost`). The runtime spell-filter
/// path compares `FilterProp::InZone` against the cast's ORIGIN zone
/// (`spell_object_matches_filter_inner`), so the lowered prop means "cast from
/// that zone", not "currently in that zone".
pub(crate) fn parse_cast_origin_zone_qualifier(input: &str) -> Option<(&str, FilterProp)> {
    // Word boundary: "from exile" must not accept "from exiled..." (the repo's
    // parser doctrine — near-miss tokens reject, PR #7543's pattern).
    terminated(
        alt((
            value(Zone::Exile, tag::<_, _, OracleError<'_>>("from exile")),
            value(Zone::Hand, tag("from your hand")),
        )),
        not(alphanumeric1),
    )
    .parse(input)
    .ok()
    .map(|(rest, zone)| (rest.trim_start(), FilterProp::InZone { zone }))
}

pub(crate) fn apply_spell_keyword_subject_constraints(
    filter: TargetFilter,
    zone_filter: Option<FilterProp>,
    mv_filter: Option<FilterProp>,
    extra_props: Vec<FilterProp>,
) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            typed = typed.controller(ControllerRef::You);
            if let Some(prop) = zone_filter {
                typed.properties.push(prop);
            }
            if let Some(prop) = mv_filter {
                typed.properties.push(prop);
            }
            typed.properties.extend(extra_props);
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| {
                    apply_spell_keyword_subject_constraints(
                        filter,
                        zone_filter.clone(),
                        mv_filter.clone(),
                        extra_props.clone(),
                    )
                })
                .collect(),
        },
        // CR 601.2a: compound subjects ("<type> spell you cast from <zone>") lower
        // to And/Not; recurse so `ControllerRef::You` reaches every leaf, otherwise
        // the grant would scope to opponents' qualifying spells too. The And arm is
        // live on the first-qualified-spell keyword-grant path (type + origin-zone
        // both present); the Not arm is defensive.
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|filter| {
                    apply_spell_keyword_subject_constraints(
                        filter,
                        zone_filter.clone(),
                        mv_filter.clone(),
                        extra_props.clone(),
                    )
                })
                .collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(apply_spell_keyword_subject_constraints(
                *filter,
                zone_filter.clone(),
                mv_filter.clone(),
                extra_props.clone(),
            )),
        },
        other => other,
    }
}

/// Parse creature subject phrases containing "of the chosen color/type" qualifiers.
/// Handles patterns like:
/// - "Creatures you control of the chosen color"
/// - "Creatures of the chosen color"
/// - "Creatures of the chosen type your opponents control"
/// - "creature you control of the chosen type other than this Vehicle"
/// - "creatures of that color" (CR 608.2c anaphor form after a `Choose a color`)
/// - "creatures of that type" (CR 608.2c anaphor form after a `Choose a creature type`)
///
/// CR 105.4: "of the chosen color" / "of that color" → `FilterProp::IsChosenColor`
/// CR 205.3m: "of the chosen type" / "of that type" → `FilterProp::IsChosenCreatureType`
///
/// Issue #327: the "of that color" / "of that type" anaphor forms are
/// equivalent to "of the chosen color" / "of the chosen type" — same typed
/// reference, same runtime resolution. They differ only orthographically
/// (CR 608.2c anaphor vs CR 113.6 explicit chosen-attribute reference).
pub(crate) fn parse_chosen_qualifier_subject(tp: &TextPair<'_>) -> Option<TargetFilter> {
    type VE<'a> = OracleError<'a>;

    // Must start with "creature" or "creatures"
    let rest = if let Ok((r, _)) = tag::<_, _, VE<'_>>("creatures ")(tp.lower) {
        r
    } else if let Ok((r, _)) = tag::<_, _, VE<'_>>("creature ")(tp.lower) {
        r
    } else {
        return None;
    };

    // Try to find "of the chosen color" / "of that color" / "of the chosen
    // type" / "of that type" somewhere in the rest. Same typed reference for
    // both anaphor forms — see fn doc.
    let chosen_prop: FilterProp;
    let before_chosen: &str;
    let after_chosen: &str;

    let color_split = nom_primitives::split_once_on(rest, "of the chosen color")
        .or_else(|_| nom_primitives::split_once_on(rest, "of that color"));
    let type_split = nom_primitives::split_once_on(rest, "of the chosen type")
        .or_else(|_| nom_primitives::split_once_on(rest, "of that type"));

    if let Ok((_, (before, after))) = color_split {
        chosen_prop = FilterProp::IsChosenColor;
        before_chosen = before.trim();
        after_chosen = after.trim();
    } else if let Ok((_, (before, after))) = type_split {
        chosen_prop = FilterProp::IsChosenCreatureType;
        before_chosen = before.trim();
        after_chosen = after.trim();
    } else {
        return None;
    };

    // Parse controller from before or after the chosen qualifier
    let mut controller = None;
    let mut extra_props = vec![chosen_prop];

    // Check "you control" before the qualifier
    if before_chosen == "you control" {
        controller = Some(ControllerRef::You);
    } else if !before_chosen.is_empty() {
        return None;
    }

    // Check controller/qualifiers after the qualifier
    let remaining = after_chosen;
    if nom_tag_lower(remaining, remaining, "your opponents control").is_some() {
        controller = Some(ControllerRef::Opponent);
    } else if nom_tag_lower(remaining, remaining, "you control").is_some() {
        controller = Some(ControllerRef::You);
    }

    // Check for "other than" suffix (e.g., "other than this Vehicle")
    if nom_primitives::scan_contains(remaining, "other than") {
        extra_props.push(FilterProp::Another);
    }

    let mut typed = TypedFilter::creature().properties(extra_props);
    if let Some(ctrl) = controller {
        typed = typed.controller(ctrl);
    }
    Some(TargetFilter::Typed(typed))
}

/// CR 602.1 + CR 603.1 + CR 606.2: The set of ability categories a "[~ has]
/// all [activated|triggered|loyalty|activated and triggered] abilities of
/// [source]" grant donates. Activated and triggered abilities land in
/// different stores via different continuous modifications
/// (`GrantAllActivatedAbilitiesOf` → `obj.abilities`;
/// `GrantAllTriggeredAbilitiesOf` → `obj.trigger_definitions`), and loyalty
/// abilities donate via the kind selector on the activated authority
/// (`GrantAllActivatedAbilitiesOf { kind: Some(Loyalty) }` → `obj.abilities`,
/// since loyalty abilities ARE activated abilities),
/// so the parser captures which categories the phrase named.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GrantedAbilityKinds {
    Activated,
    Triggered,
    Loyalty,
    ActivatedAndTriggered,
}

/// CR 602.1 + CR 603.1 + CR 606.2: The grant-phrase category axis. The
/// conjunction form ("activated and triggered" / "triggered and activated",
/// order-insensitive) is tried before the single-category leaves so the longer
/// phrase wins. The plural and the singular-distributive activated form ("each
/// activated ability of", Locus of Enlightenment) map to the same activated
/// set; the loyalty leaf ("all loyalty abilities of", Nicol Bolas,
/// Dragon-God) maps to the loyalty subset.
fn parse_granted_ability_kinds(input: &str) -> OracleResult<'_, GrantedAbilityKinds> {
    alt((
        value(
            GrantedAbilityKinds::ActivatedAndTriggered,
            alt((
                tag("all activated and triggered abilities of "),
                tag("all triggered and activated abilities of "),
            )),
        ),
        value(
            GrantedAbilityKinds::Triggered,
            tag("all triggered abilities of "),
        ),
        value(
            GrantedAbilityKinds::Loyalty,
            tag("all loyalty abilities of "),
        ),
        value(
            GrantedAbilityKinds::Activated,
            alt((
                tag("all activated abilities of "),
                tag("each activated ability of "),
            )),
        ),
    ))
    .parse(input)
}

/// CR 613.1f + CR 113.3: Recognize "[~ has] all [category] abilities of [source]"
/// and return the donated category set plus the provider `source` filter.
///
/// Composed from nom combinators along three independent axes: the optional
/// leading verb (`has`/`have`), the category axis ([`parse_granted_ability_kinds`]),
/// and the source-set noun phrase ([`grant_source_noun_phrase`]) —
/// `ExiledBySource` (Myr Welder / Agatha), the same-name exclusion (Marvin),
/// `ChosenCard` ("the last chosen card", Koh), etc.
///
/// Returns `None` for forms still needing extra infrastructure (counter-gated
/// exile sets) so they stay a loud gap rather than over-granting.
fn parse_grant_all_abilities_clause(
    lower: &str,
) -> Option<(GrantedAbilityKinds, crate::types::ability::TargetFilter)> {
    let p = lower.trim().trim_end_matches('.').trim();
    all_consuming((
        opt(alt((tag::<_, _, OracleError<'_>>("has "), tag("have ")))),
        parse_granted_ability_kinds,
        grant_source_noun_phrase,
    ))
    .parse(p)
    .ok()
    .map(|(_, (_, kinds, source))| (kinds, source))
}

/// CR 602.5b + CR 602.5c: The "you may activate each of those abilities only once
/// each turn" use-restriction rider that follows an ability-grant sentence, mapped
/// to the typed `ActivationRestriction::OnlyOnceEachTurn`. Decomposed into its
/// grammatical axes — permission (`you may activate`), the granted-set anaphor
/// (`each of those abilities`), and the frequency cap (`only once each turn`, the
/// semantic key) — so the cap is a *meaningfully parsed* restriction, not a
/// verbatim sentence consumed and discarded.
fn parse_activate_once_each_turn_rider(input: &str) -> OracleResult<'_, ActivationRestriction> {
    value(
        ActivationRestriction::OnlyOnceEachTurn,
        (
            tag("you may activate "),
            tag("each of those abilities"),
            tag(" only once each turn"),
        ),
    )
    .parse(input)
}

/// CR 602.5b + CR 602.5c: Fold an "activate ... only once each turn" use-restriction
/// rider (a trailing sentence that yields no standalone static) into the `cap` of
/// the most recently emitted `GrantAllActivatedAbilitiesOf` modification, returning
/// `true` when the fold lands. Returns `false` when `segment` is not the rider, or
/// when there is no still-uncapped ability grant preceding it to attach to, or
/// the nearest grant has an unsupported kind-selected rider.
///
/// This is the SHARED grant-rider primitive: it composes the once-per-turn cap with
/// the STANDARD grant parse (sentence 1 → `GrantAllActivatedAbilitiesOf` via the
/// ordinary continuous-clause dispatch) during normal sentence splitting
/// (`parse_multi_sentence_statics`), so any "<grant activated abilities>. You may
/// activate each of those abilities only once each turn." card is capped — over any
/// grant source, with no card-specific whole-line hook. The restriction travels
/// with the granted abilities (CR 602.5c — a use-restriction acquired with an
/// ability applies to that acquired ability), which the layer-6 expansion injects
/// and `game/restrictions.rs` enforces per `(recipient, ability_index)`.
pub(super) fn fold_grant_cap_rider(segment: &str, defs: &mut [StaticDefinition]) -> bool {
    let lower = segment.trim().trim_end_matches('.').trim().to_lowercase();
    let Ok((_, restriction)) =
        all_consuming(parse_activate_once_each_turn_rider).parse(lower.as_str())
    else {
        return false;
    };
    // Kind-selected grant riders are unsupported here. Stop at that grant so
    // the caller declines the whole line instead of capping an older sibling.
    for def in defs.iter_mut().rev() {
        for modification in def.modifications.iter_mut().rev() {
            if let ContinuousModification::GrantAllActivatedAbilitiesOf { cap, kind, .. } =
                modification
            {
                if kind.is_some() {
                    return false;
                }
                if cap.is_none() {
                    *cap = Some(restriction);
                    return true;
                }
            }
        }
    }
    false
}

/// CR 613.1f + CR 607.2a + CR 201.2: The source-set noun phrase of an
/// ability-grant-by-reference static. Each arm is a leaf of the source-set axis;
/// adding a new referenced set is one more `alt` arm here, never a new variant.
fn grant_source_noun_phrase(input: &str) -> OracleResult<'_, crate::types::ability::TargetFilter> {
    use crate::types::counter::CounterType;
    alt((
        // CR 607.2a: cards exiled with the host. Optional card-type qualifier
        // narrows the granted set (Agatha grants creature cards only).
        grant_exiled_source,
        // CR 201.2: "creatures you control that don't have the same name as
        // it/~" (Marvin) — battlefield creatures you control, excluding ones
        // sharing the recipient's name.
        value(
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Not {
                        prop: Box::new(FilterProp::SameName),
                    }]),
            ),
            (
                tag("creatures you control that don't have the same name as "),
                alt((tag("it"), tag("~"))),
            ),
        ),
        // CR 613.1f: "each other creature with a +1/+1 counter on it"
        // (Experiment Kraj) — all creatures except self with at least one
        // +1/+1 counter.
        value(
            TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature().properties(vec![
                        FilterProp::Counters {
                            counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                            comparator: Comparator::GE,
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ])),
                    TargetFilter::Not {
                        filter: Box::new(TargetFilter::SelfRef),
                    },
                ],
            },
            (
                tag("each other creature with a +1/+1 counter on "),
                alt((tag("it"), tag("them"))),
            ),
        ),
        // CR 613.1f: "all creatures your opponents control" (Drana and Linvala)
        // — battlefield permanents; scope to InZone { Battlefield } so dead
        // or exiled creatures of theirs do not donate abilities.
        value(
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::Opponent)
                    .properties(vec![FilterProp::InZone {
                        zone: Zone::Battlefield,
                    }]),
            ),
            tag("all creatures your opponents control"),
        ),
        // CR 613.1f + CR 108.3: "all <type> cards in your graveyard | all
        // graveyards" (Necrotic Ooze, Thranduil, the Elvenking).
        grant_graveyard_source,
        // CR 613.1f: "all lands on the battlefield" (Manascape Refractor)
        // — zone is explicit in the phrase; encode it so graveyard/hand land
        // cards are excluded from the runtime provider scan.
        value(
            TargetFilter::Typed(TypedFilter::land().properties(vec![FilterProp::InZone {
                zone: Zone::Battlefield,
            }])),
            tag("all lands on the battlefield"),
        ),
        // CR 613.1f: "all other planeswalkers on the battlefield" (Nicol Bolas,
        // Dragon-God) — zone is explicit in the phrase, encoded so planeswalker
        // cards in other zones are excluded from the runtime provider scan; the
        // English "other" is the self-exclusion conjunct (Kraj precedent).
        value(
            TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker).properties(
                        vec![FilterProp::InZone {
                            zone: Zone::Battlefield,
                        }],
                    )),
                    TargetFilter::Not {
                        filter: Box::new(TargetFilter::SelfRef),
                    },
                ],
            },
            tag("all other planeswalkers on the battlefield"),
        ),
        // CR 613.1f: "all legendary creatures you control" (Robaran Mercenaries)
        // — battlefield permanents; scope to InZone { Battlefield } so
        // legendary creature cards in hand/graveyard do not donate abilities.
        value(
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![
                        FilterProp::HasSupertype {
                            value: Supertype::Legendary,
                        },
                        FilterProp::InZone {
                            zone: Zone::Battlefield,
                        },
                    ]),
            ),
            tag("all legendary creatures you control"),
        ),
        // CR 607.2a + CR 607.2d: "the last chosen card" (Koh, the Face Stealer) —
        // the card most recently recorded on the host via `Effect::RememberCard`
        // (`ChosenAttribute::Card`, CR 608.2c) AND still in the exile zone. The
        // CR 607.2a exile pinning of the linked reference is composed here so
        // the shared `ChosenCard` reader stays zone-agnostic (CR 607.2d).
        value(
            TargetFilter::And {
                filters: vec![
                    TargetFilter::ChosenCard,
                    TargetFilter::Typed(
                        TypedFilter::default()
                            .properties(vec![FilterProp::InZone { zone: Zone::Exile }]),
                    ),
                ],
            },
            tag("the last chosen card"),
        ),
    ))
    .parse(input)
}

/// CR 607.2a: "[the exiled card] | all [<card type>] cards exiled with it/~".
/// The optional card-type qualifier intersects `ExiledBySource` with a typed
/// filter so the grant tracks only matching exiled cards.
fn grant_exiled_source(input: &str) -> OracleResult<'_, crate::types::ability::TargetFilter> {
    alt((
        // CR 702.167c: "the exiled card[s] used to craft it/~" — the craft pile
        // (cards exiled to pay the craft cost that returned this permanent). The
        // craft materials are linked to the host by `ExileLinkKind::CraftMaterial`,
        // which `ExiledBySource` reads kind-agnostically. Tried before the bare
        // "the exiled card" arm so the longer craft phrase wins (Locus of
        // Enlightenment).
        value(
            TargetFilter::ExiledBySource,
            (
                tag("the exiled card"),
                opt(tag("s")),
                tag(" used to craft "),
                alt((tag("it"), tag("~"))),
            ),
        ),
        value(TargetFilter::ExiledBySource, tag("the exiled card")),
        // "all [creature|land] cards exiled with it/~". The optional card-type
        // qualifier intersects `ExiledBySource` with the matching type filter
        // (CR 205.2a — a creature/land card is type Creature/Land in exile) so
        // Agatha grants only creature cards' abilities and Steward of the
        // Harvest only land cards'; the untyped form (Myr Welder, Territory
        // Forge) stays a bare `ExiledBySource`.
        (
            tag("all "),
            opt(grant_exiled_card_type_qualifier),
            tag("cards exiled with "),
            alt((tag("it"), tag("~"))),
        )
            .map(|(_, qualifier, _, _)| match qualifier {
                Some(typed) => TargetFilter::And {
                    filters: vec![TargetFilter::Typed(typed), TargetFilter::ExiledBySource],
                },
                None => TargetFilter::ExiledBySource,
            }),
    ))
    .parse(input)
}

/// CR 205.2a + CR 607.2a: card-type qualifier of the exiled-cards grant set.
/// Only card types printed with this phrase are accepted; any other word
/// (nonland, artifact, ...) fails the following `cards exiled with ` tag so the
/// whole clause declines rather than mis-scoping the set. The trailing space is
/// part of each tag so "lands"/"landfall" cannot match.
fn grant_exiled_card_type_qualifier(input: &str) -> OracleResult<'_, TypedFilter> {
    alt((
        value(TypedFilter::creature(), tag("creature ")),
        value(TypedFilter::land(), tag("land ")),
    ))
    .parse(input)
}

/// CR 613.1f + CR 113.3: "all <type> cards in your graveyard | all graveyards" —
/// the graveyard-card source set of an ability-grant-by-reference static
/// (Necrotic Ooze: creature cards in all graveyards; Thranduil, the Elvenking:
/// Elf cards in your graveyard). Composed along two independent axes rather
/// than one arm per printed type:
///   - the type word, via the shared [`nom_target::parse_type_filter_word`]
///     alphabet — a card type ("creature", "land", "artifact") or a subtype
///     (CR 205.3m: the creature type "Elf");
///   - the graveyard owner scope ([`grant_graveyard_owner`]).
///
/// CR 109.2a: a description naming "card" and a zone means a card matching it
/// in that zone, so the set is pinned to `Zone::Graveyard`.
fn grant_graveyard_source(input: &str) -> OracleResult<'_, TargetFilter> {
    (
        tag("all "),
        nom_target::parse_type_filter_word,
        tag(" cards in "),
        grant_graveyard_owner,
    )
        .map(|(_, type_filter, _, owner)| {
            // CR 108.3: a card in a graveyard is "yours" by ownership, not
            // control — scope with `FilterProp::Owned`, never
            // `TypedFilter::controller`. "All graveyards" carries no owner axis.
            let properties = owner
                .map(|controller| FilterProp::Owned { controller })
                .into_iter()
                .chain(std::iter::once(FilterProp::InZone {
                    zone: Zone::Graveyard,
                }))
                .collect();
            TargetFilter::Typed(TypedFilter::new(type_filter).properties(properties))
        })
        .parse(input)
}

/// CR 108.3 + CR 109.5: the graveyard owner axis of [`grant_graveyard_source`].
/// "your graveyard" names the graveyard of the player who controls the object
/// with the ability (`ControllerRef::You` resolves against the recipient's
/// controller in the layer-6 expansion); "all graveyards" spans every
/// player's (`None`).
fn grant_graveyard_owner(input: &str) -> OracleResult<'_, Option<ControllerRef>> {
    alt((
        value(Some(ControllerRef::You), tag("your graveyard")),
        value(None, tag("all graveyards")),
    ))
    .parse(input)
}

/// CR 613.1d: Parse a layer-4 type-removal predicate `"isn't a/an <core type>"`
/// (e.g. `"isn't a creature"`). Scans at word boundaries so the predicate can
/// appear anywhere in the modification text (the subject and any trailing
/// duration are stripped by the caller). Returns the removed [`CoreType`], or
/// `None` when no such predicate is present.
fn parse_isnt_a_core_type(lower: &str) -> Option<CoreType> {
    fn core_type_word(input: &str) -> OracleResult<'_, CoreType> {
        alt((
            value(CoreType::Artifact, tag("artifact")),
            value(CoreType::Battle, tag("battle")),
            value(CoreType::Creature, tag("creature")),
            value(CoreType::Enchantment, tag("enchantment")),
            value(CoreType::Land, tag("land")),
            value(CoreType::Planeswalker, tag("planeswalker")),
        ))
        .parse(input)
    }
    fn predicate(input: &str) -> OracleResult<'_, CoreType> {
        preceded(alt((tag("isn't an "), tag("isn't a "))), core_type_word).parse(input)
    }
    nom_primitives::scan_split_at_phrase(lower, |i| predicate(i))
        .and_then(|(_, clause)| predicate(clause).ok().map(|(_, ct)| ct))
}

/// CR 305.6 + CR 305.7 + CR 205.3i: Recognize a "gain all basic land types" /
/// "gain all land types" predicate (and the `has`/`have`/`are`/`is` copula
/// variants) and map it to the matching all-land-type continuous modification.
///
/// `AddAllBasicLandTypes` adds the five basic land subtypes (Plains, Island,
/// Swamp, Mountain, Forest — CR 305.6) in addition to a land's existing types;
/// each grants its intrinsic mana ability per CR 305.6 / CR 305.7.
/// `AddAllLandTypes` adds every one of the 17 land subtypes (CR 205.3i). Built
/// for the whole "[lands you control] gain all basic land types until <duration>"
/// class (Energybending), not a single card. The verb/copula is matched but
/// otherwise discarded — the affected filter and duration are owned by the
/// caller (`build_continuous_clause` / the static/anthem parsers).
fn parse_all_land_types_modification(text: &str) -> Option<ContinuousModification> {
    let lower = text.trim().trim_end_matches('.').trim().to_lowercase();
    super::oracle_nom::bridge::nom_parse_lower(&lower, |i| {
        let (i, _) = opt(alt((
            tag::<_, _, OracleError<'_>>("gains "),
            tag("gain "),
            tag("has "),
            tag("have "),
            tag("are "),
            tag("is "),
        )))
        .parse(i)?;
        // Longer phrase first so "all basic land types" wins over "all land types".
        all_consuming(terminated(
            alt((
                value(
                    ContinuousModification::AddAllBasicLandTypes,
                    alt((tag("all basic land types"), tag("every basic land type"))),
                ),
                value(
                    ContinuousModification::AddAllLandTypes,
                    alt((tag("all land types"), tag("every land type"))),
                ),
            )),
            opt((
                tag(" in addition to "),
                alt((tag("its"), tag("their"))),
                tag(" other "),
                opt(tag("land ")),
                tag("types"),
            )),
        ))
        .parse(i)
    })
}

/// One characteristic listed in an "its `<X>` is/are the last chosen `<X>`"
/// clause. Parser-local — maps to the chosen-attribute read modification(s) for
/// that characteristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastChosenCharacteristic {
    Name,
    CreatureType,
}

/// CR 612.8 + CR 205.1a / CR 613.1d: Parse the SUBJECT list of an "its
/// `<characteristics>` is/are the last chosen `<characteristics>`" clause
/// (Psychic Paper: "its name and creature type are the last chosen name and
/// creature type"). The mandatory `"its "` prefix distinguishes this clause from
/// the `"it can't be blocked"` restriction anaphor. The subject characteristic
/// list drives the emitted modifications; the trailing object list ("the last
/// chosen name and creature type") is the read source and is left unconsumed.
/// One `alt()` per axis (separator, characteristic) rather than enumerating the
/// cross-product, per the combinator-composition mandate.
fn parse_last_chosen_characteristic_list(
    input: &str,
) -> OracleResult<'_, Vec<LastChosenCharacteristic>> {
    let characteristic = || {
        alt((
            value(LastChosenCharacteristic::CreatureType, tag("creature type")),
            value(LastChosenCharacteristic::Name, tag("name")),
        ))
    };
    let (input, subjects) = preceded(
        tag("its "),
        terminated(
            separated_list1(
                alt((tag(", and "), tag(" and "), tag(", "))),
                characteristic(),
            ),
            (alt((tag(" is "), tag(" are "))), tag("the last chosen ")),
        ),
    )
    .parse(input)?;
    let (input, objects) = separated_list1(
        alt((tag(", and "), tag(" and "), tag(", "))),
        characteristic(),
    )
    .parse(input)?;
    if subjects != objects {
        return Err(super::oracle_nom::error::oracle_err(input));
    }
    Ok((input, subjects))
}

pub(crate) fn parse_continuous_modifications(text: &str) -> Vec<ContinuousModification> {
    // Strip "where X is [quantity]" before parsing modifications,
    // but only if the text doesn't contain quoted abilities (which have their
    // own "where X is" handling inside the quote).
    let text_lower = text.to_lowercase();
    let text_tp = TextPair::new(text, &text_lower);
    let (stripped_tp, where_x_expression) = if text.contains('"') {
        (text_tp, None)
    } else {
        super::oracle_effect::strip_trailing_where_x(text_tp)
    };
    let tp = nom_tag_tp(&stripped_tp, "also ").unwrap_or(stripped_tp);
    let text_stripped = tp.original;
    let unquoted_text = strip_quoted_segments(text_stripped);
    let unquoted_lower = unquoted_text.to_lowercase();
    let unquoted_tp = TextPair::new(&unquoted_text, &unquoted_lower);

    // CR 613.1f + CR 113.3: "all [activated|triggered|activated and triggered]
    // abilities of [the exiled card | all cards exiled with it | the last chosen
    // card | …]" — grant the host the named ability categories of the source set
    // (Myr Welder / Agatha activated; Koh, the Face Stealer activated AND
    // triggered, source = the last chosen card). Typed/counter-gated sources stay
    // a gap (follow-ups).
    if let Some((kinds, source)) = parse_grant_all_abilities_clause(&grant_clause_probe_lower(text))
    {
        // CR 607.5 + CR 606.2 + Bolas ruling 2024-01-12: a kind-selected loyalty
        // grant donates linked pairs (e.g. Karn −3 + Ashiok −X onto one Bolas)
        // that the source-level exile ledger cannot attribute to their donor pair
        // (no donor binding on the installed grant, activation, or links).
        // Refuse at emission so the sentence stays a loud `static_structure` gap
        // rather than over-granting into the recipient's source-wide pile.
        // Unblocks when the
        // ledger carries donor/pair identity (see plan-r2 §4).
        if matches!(kinds, GrantedAbilityKinds::Loyalty) {
            return Vec::new();
        }
        // CR 602.1 + CR 603.1: emit one continuous modification per donated
        // category. Activated and triggered land in different stores, so a
        // conjunction grant produces BOTH mods over the same `source`.
        // CR 602.5b: the grant sentence itself carries no use-restriction — the
        // once-per-turn cap (Locus) is folded in separately by `fold_grant_cap_rider`
        // when the trailing rider sentence is present, so the activated grant stays
        // uncapped; triggered abilities take no cap.
        let mut mods = Vec::new();
        if matches!(
            kinds,
            GrantedAbilityKinds::Activated | GrantedAbilityKinds::ActivatedAndTriggered
        ) {
            mods.push(ContinuousModification::GrantAllActivatedAbilitiesOf {
                source: source.clone(),
                cap: None,
                kind: None,
            });
        }
        // CR 606.1 + CR 606.2: a loyalty grant donates the loyalty abilities
        // (activated abilities with a loyalty symbol in the cost) of the source
        // set, via the kind selector on the activated authority. Kind-selected
        // grant riders remain unsupported by `fold_grant_cap_rider`, which
        // declines the whole line when one follows this grant.
        if matches!(kinds, GrantedAbilityKinds::Loyalty) {
            mods.push(ContinuousModification::GrantAllActivatedAbilitiesOf {
                source: source.clone(),
                cap: None,
                kind: Some(ActivatedAbilityKind::Loyalty),
            });
        }
        if matches!(
            kinds,
            GrantedAbilityKinds::Triggered | GrantedAbilityKinds::ActivatedAndTriggered
        ) {
            mods.push(ContinuousModification::GrantAllTriggeredAbilitiesOf { source });
        }
        return mods;
    }

    // CR 305.6 + CR 305.7 + CR 205.3i: "gain all basic land types" / "gain all
    // land types" (and the copula variants) — the whole predicate maps to a
    // single all-land-type modification. Checked early so the trailing "types"
    // noun is never mistaken for a P/T or keyword token by the parsers below.
    if let Some(modification) = parse_all_land_types_modification(unquoted_tp.original) {
        return vec![modification];
    }

    let mut modifications = Vec::new();

    // CR 205.1a + CR 205.3i + CR 613.1d/f: "loses all [other] abilities, card
    // types, creature types, and land types" — a comma-and enumeration parsed
    // with nom. Each member maps to one modification. `CardTypes` requires the
    // granted core-type list, which only the "is a [type]" caller
    // (`parse_enchanted_is_type`) owns — in the standalone path it has no type
    // set and is a no-op (such text does not occur outside the "is a [type]"
    // frame). `LandTypes` (Alpine Moon, Lithoform Blight, Ultima, Origin of
    // Oblivion — "loses all land types and abilities") reuses the same
    // `RemoveAllSubtypes` runtime the `CreatureTypes` arm already exercises;
    // `game/layers.rs` already has a working `SubtypeSet::Land` arm, so this is
    // pure parser wiring, no engine change.
    for member in scan_loss_enumeration(unquoted_tp.lower) {
        match member {
            LossMember::Abilities => {
                modifications.push(ContinuousModification::RemoveAllAbilities);
            }
            LossMember::CreatureTypes => {
                modifications.push(ContinuousModification::RemoveAllSubtypes {
                    set: crate::types::card_type::SubtypeSet::Creature,
                });
            }
            LossMember::LandTypes => {
                modifications.push(ContinuousModification::RemoveAllSubtypes {
                    set: crate::types::card_type::SubtypeSet::Land,
                });
            }
            LossMember::CardTypes => {}
        }
    }

    if let Some(dynamic_mods) = parse_dynamic_for_each_pt_modifications(&unquoted_text) {
        modifications.extend(dynamic_mods);
    } else if let Some(rest_tp) =
        nom_tag_tp(&unquoted_tp, "gets ").or_else(|| nom_tag_tp(&unquoted_tp, "get "))
    {
        let after = rest_tp.original.trim();
        if let Some((p, t)) = parse_pt_mod(after) {
            modifications.push(ContinuousModification::AddPower { value: p });
            modifications.push(ContinuousModification::AddToughness { value: t });
        }
    } else if let Some((p, t)) = parse_fixed_pt_in_text(unquoted_tp.lower) {
        modifications.push(ContinuousModification::AddPower { value: p });
        modifications.push(ContinuousModification::AddToughness { value: t });
    }

    // CR 205.4a + CR 205.4b: additive supertype grant on a compound aura/equip
    // predicate body ("... is snow", "... is legendary, gets +1/+1, ..."). The
    // recognizer returns the specific supertype, so Legendary/Basic/Snow all
    // flow through this one seam (Glittering Frost, In Bolas's Clutches).
    if let Some(supertype) = parse_supertype_grant(unquoted_tp.lower) {
        modifications.push(ContinuousModification::AddSupertype { supertype });
    }

    // CR 613.1d: Layer 4 type removal — "isn't a/an <core type>" (e.g. Blink's
    // Alien Angel token: "this token isn't a creature until end of turn"). The
    // duration is already stripped by the caller. This is the building-block
    // analogue of the static-dispatch "~ isn't a <type> as long as <cond>" arm,
    // so the same removal works as a one-shot continuous effect on a token.
    if let Some(core_type) = parse_isnt_a_core_type(unquoted_tp.lower) {
        modifications.push(ContinuousModification::RemoveType { core_type });
    }

    // CR 510.1c: Aura/Equipment-style compound statics can attach the
    // toughness-combat-damage rule to the same affected object as a P/T
    // modification ("Enchanted creature gets +0/+2 and assigns…"). The same
    // predicate also rides one-shot duration-bound continuous effects whose
    // subject is plural ("creatures you control … assign combat damage equal to
    // their toughness rather than their power" — The Kingpin of Crime), so this
    // accepts both the singular ("its…its") and plural ("their…their") surface
    // forms via the shared predicate combinator.
    if nom_primitives::scan_at_word_boundaries(
        unquoted_lower.as_str(),
        super::evasion::parse_assigns_damage_from_toughness_predicate,
    )
    .is_some()
    {
        modifications.push(ContinuousModification::AssignDamageFromToughness);
    }

    // CR 701.15b: Positive goaded designation on token anaphors and compound
    // statics ("The tokens are goaded for the rest of the game", Life of the
    // Party; "Enchanted creature … is goaded").
    if nom_primitives::scan_contains(unquoted_lower.as_str(), "is goaded")
        || nom_primitives::scan_contains(unquoted_lower.as_str(), "are goaded")
    {
        modifications.push(ContinuousModification::AddStaticMode {
            mode: StaticMode::Goaded,
        });
    }

    // CR 701.60a + CR 701.60d: "can't become suspected" prohibition riding on a
    // compound static (Airtight Alibi: "Enchanted creature gets +2/+2 and can't
    // become suspected"). Confers a `CantBecomeSuspected` static onto the
    // affected creature; the suspect resolver gates on it. Mirrors the goaded
    // designation rider above.
    if nom_primitives::scan_contains(unquoted_lower.as_str(), "can't become suspected")
        || nom_primitives::scan_contains(unquoted_lower.as_str(), "cant become suspected")
    {
        modifications.push(ContinuousModification::AddStaticMode {
            mode: StaticMode::CantBecomeSuspected,
        });
    }

    // CR 702.73a + CR 205.3 + CR 613.1d: Conjunctive "is/are every creature
    // type" predicate — the Changeling-class type grant when it appears as
    // one conjunct in an Aura/Equipment compound static ("Enchanted creature
    // gets +2/+2, has reach, and is every creature type", "Equipped creature
    // gets +1/+1 and is every creature type"). The top-level grant form
    // ("Creatures you control are every creature type", "~ is every creature
    // type") is owned by `parse_all_creature_types_grant` and never reaches
    // this helper. Both copulas are scanned because subject number drives
    // verb agreement at the outer parser layer.
    if nom_primitives::scan_contains(unquoted_lower.as_str(), "is every creature type")
        || nom_primitives::scan_contains(unquoted_lower.as_str(), "are every creature type")
    {
        modifications.push(ContinuousModification::AddAllCreatureTypes);
    }

    // CR 612.8 (name, Layer 3) + CR 205.1a / CR 613.1d (creature type, Layer 4):
    // "its <characteristics> is/are the last chosen <characteristics>" — set each
    // listed characteristic to the granting source's persisted ChosenAttribute
    // (Psychic Paper). `split_keyword_list` shreds this clause across its commas
    // and "and"s, so it is recognized HERE on the intact predicate, ahead of the
    // keyword-list path. It is a distinct clause type (not a restriction, so no
    // overlap with `parse_restriction_modes`). Built for the class of "its <X> is
    // the last chosen <X>" equipment-choice readbacks, not the single card.
    if let Some(characteristics) = nom_primitives::scan_at_word_boundaries(
        unquoted_lower.as_str(),
        parse_last_chosen_characteristic_list,
    ) {
        for characteristic in characteristics {
            match characteristic {
                LastChosenCharacteristic::Name => {
                    modifications.push(ContinuousModification::SetChosenName);
                }
                LastChosenCharacteristic::CreatureType => {
                    // CR 205.1a + CR 613.1d: setting a creature's creature type
                    // REPLACES its existing creature subtypes (Layer 4), so remove
                    // all current creature subtypes before adding the chosen one.
                    // Emission order is the intra-layer timestamp order (CR 613.7a).
                    modifications.push(ContinuousModification::RemoveAllSubtypes {
                        set: SubtypeSet::Creature,
                    });
                    modifications.push(ContinuousModification::AddChosenSubtype {
                        kind: ChosenSubtypeKind::CreatureType,
                    });
                }
            }
        }
    }

    // CR 613.4c: Scan for "get +X/+X" / "gets +X/+X" anywhere in the text
    // for dynamic P/T modification (e.g., Craterhoof Behemoth)
    if let Some(dynamic_mods) =
        parse_dynamic_pt_in_text(&unquoted_lower, where_x_expression.as_deref())
    {
        modifications.extend(dynamic_mods);
    }

    // CR 613.4b + CR 107.3m: "have base power and toughness X/X" — dynamic set
    // at layer 7b. Checked before the fixed-literal parser so X-bearing patterns
    // are not mis-parsed as literal integers.
    if let Some((power, toughness)) =
        parse_base_pt_dynamic(&unquoted_text, where_x_expression.as_deref())
    {
        modifications.push(ContinuousModification::SetPowerDynamic { value: power });
        modifications.push(ContinuousModification::SetToughnessDynamic { value: toughness });
    } else if !push_base_pt_mana_value_dynamic_modifications(&mut modifications, &unquoted_lower) {
        if let Some((power, toughness)) = parse_base_pt_mod(&unquoted_text) {
            modifications.push(ContinuousModification::SetPower { value: power });
            modifications.push(ContinuousModification::SetToughness { value: toughness });
        }
    }
    if let Some(power) = parse_base_power_mod(&unquoted_text) {
        modifications.push(ContinuousModification::SetPower { value: power });
    }
    if let Some(toughness) = parse_base_toughness_mod(&unquoted_text) {
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }

    // CR 509.1b + CR 613.4b: "can't be blocked [this turn] and has/have base power
    // and toughness N/N" — the restriction conjunct precedes the base-PT conjunct.
    // Compatibility keyword extraction skips base-stat verbs. The leading
    // restriction keeps its own complete owner (Atomic Microsizer).
    if let Some((restriction_text, _)) =
        nom_primitives::scan_split_at_phrase(&unquoted_lower, |i| {
            (
                tag("and "),
                alt((tag("has"), tag("have"))),
                tag(" base power"),
            )
                .parse(i)
        })
    {
        let restriction_text = restriction_text.trim();
        if let Some(modes) = parse_restriction_modes(restriction_text) {
            for mode in modes {
                if static_mode_needs_grant_propagation(&mode)
                    && !modifications.iter().any(|existing| {
                        matches!(
                            existing,
                            ContinuousModification::AddStaticMode { mode: existing_mode }
                                if *existing_mode == mode
                        )
                    })
                {
                    modifications.push(ContinuousModification::AddStaticMode { mode });
                }
            }
        }
    }

    // CR 508.1d + CR 509.1a + CR 205.1b: A one-shot combat trick that leads with
    // a movement restriction and then a type/stat change — "can't block this
    // turn and becomes a Coward in addition to its other types" (Coward); the
    // generalized class is "can't <restriction> [this turn] and <continuous
    // mod>". The trailing change conjunct (becomes/gets/gains/has …) is already
    // recovered by the dedicated scans above and below; recover the LEADING
    // restriction conjunct here so it is not silently dropped. Anchored on
    // " and <change-verb>" so a "can't attack, block, or crew" restriction list
    // (separated by ", or "/", "/" or ") is never split mid-list.
    // `parse_restriction_modes` itself gates on the "can't"/"cannot" prefix and
    // is `all_consuming`, so a non-restriction prefix yields `None`. The
    // grant-propagation dedup mirrors the base-PT restriction block above.
    if let Some((restriction_prefix, _)) =
        nom_primitives::scan_split_at_phrase(&unquoted_lower, |i| {
            (
                tag("and "),
                alt((
                    tag("becomes "),
                    tag("become "),
                    tag("gets "),
                    tag("get "),
                    tag("gains "),
                    tag("gain "),
                    tag("has "),
                    tag("have "),
                )),
            )
                .parse(i)
        })
    {
        // Strip the embedded " this turn" duration off the restriction chunk
        // ("can't block this turn" → "can't block") via the shared combinator
        // duration grammar before delegating dispatch to
        // `parse_restriction_modes`; the bare CantBlock/CantAttack atoms do not
        // themselves consume a trailing " this turn" (unlike "be blocked").
        let (restriction_prefix, _) = strip_trailing_duration(restriction_prefix.trim());
        if let Some(modes) = parse_restriction_modes(restriction_prefix) {
            for mode in modes {
                if static_mode_needs_grant_propagation(&mode)
                    && !modifications.iter().any(|existing| {
                        matches!(
                            existing,
                            ContinuousModification::AddStaticMode { mode: existing_mode }
                                if *existing_mode == mode
                        )
                    })
                {
                    modifications.push(ContinuousModification::AddStaticMode { mode });
                }
            }
        }
    }

    for modification in parse_quoted_ability_modifications(text_stripped) {
        modifications.push(modification);
    }

    if let Some(additive_modifications) = parse_additive_type_clause_modifications(&unquoted_text) {
        modifications.extend(additive_modifications);
    }

    // CR 702: Guard "can't have or gain [keyword]" from extract_keyword_clause —
    // "have" inside "can't have" must NOT produce AddKeyword.
    if nom_primitives::scan_contains(&unquoted_lower, "can't have")
        || nom_primitives::scan_contains(&unquoted_lower, "can't have or gain")
    {
        // Parse the keyword from "can't have or gain [keyword]" / "can't have [keyword]"
        // allow-noncombinator: punctuation cleanup after parser dispatch, not dispatch itself.
        let stripped_lower = unquoted_lower.strip_suffix('.').unwrap_or(&unquoted_lower); // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        let cant_text = if let Ok((_, (_, after))) =
            nom_primitives::split_once_on(stripped_lower, "can't have or gain ")
        {
            Some(after)
        } else if let Ok((_, (_, after))) =
            nom_primitives::split_once_on(stripped_lower, "can't have ")
        {
            Some(after)
        } else {
            None
        };
        if let Some(kw_text) = cant_text {
            if let Some(kw) = map_keyword(kw_text.trim().trim_end_matches('.')) {
                modifications.push(ContinuousModification::RemoveKeyword {
                    keyword: kw.clone(),
                });
                // Note: CantHaveKeyword is a StaticMode variant, not a ContinuousModification.
                // It will be handled at the static definition level.
            }
        }
    } else if let Some(keyword_text) = extract_keyword_clause(&unquoted_text) {
        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
            push_grant_clause_modifications(
                &mut modifications,
                part.as_ref(),
                where_x_expression.as_deref(),
            );
        }
    }

    // CR 613.1f: Pre-quote keyword recovery for compound lines like Swashbuckler's
    // Whip: 'has reach, "{2}, {T}: ...," and "{8}, {T}: ...".' Stripping the quoted
    // segments can mangle the boundary between the leading bare keyword and the
    // first quote, so compatibility extraction also scans the original slice
    // BEFORE the first quote independently. GUARD: only run when the post-strip
    // path produced no AddKeyword (prevents double-adding a keyword).
    if !modifications
        .iter()
        .any(|m| matches!(m, ContinuousModification::AddKeyword { .. }))
    {
        if let Ok((_, pre_quote)) = take_until::<_, _, OracleError<'_>>("\"").parse(text_stripped) {
            if let Some(keyword_text) = extract_keyword_clause(pre_quote) {
                for part in split_keyword_list(keyword_text.trim().trim_end_matches(',').trim()) {
                    push_grant_clause_modifications(&mut modifications, part.as_ref(), None);
                }
            }
        }
    }

    // CR 702: "lose [keyword]" / "loses [keyword]" — keyword removal.
    if let Some(keyword_text) = extract_lose_keyword_clause(&unquoted_text) {
        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
            if let Some(kw) = map_keyword(part.trim().trim_end_matches('.')) {
                modifications.push(ContinuousModification::RemoveKeyword { keyword: kw });
            }
        }
    }

    // CR 205.1a + CR 205.2 + CR 205.3 + CR 613.1c: "becomes a [subtype]*
    // [core-type]+ in addition to its other types" — delegates to the shared
    // animation type-sequence combinator so one CR-205 type-line decomposes
    // into one AddType/AddSubtype modification per token (not a single
    // whole-phrase AddSubtype string).
    modifications.extend(parse_becomes_type_addition_modifications(&unquoted_tp));
    modifications.extend(parse_bare_becomes_type_replacement_modifications(
        &unquoted_tp,
    ));

    modifications
}

/// The clause-probe recipe the by-reference grant match evaluates: lowercase →
/// where-X strip (unless quoted) → `nom_tag_tp "also "` →
/// `strip_quoted_segments` → lowercase. Shared by the `:1351` emission-site
/// call and [`find_refused_loyalty_grant_sentence`] so the finder and the
/// emission site can never disagree about what the clause parser saw.
fn grant_clause_probe_lower(text: &str) -> String {
    let text_lower = text.to_lowercase();
    let text_tp = TextPair::new(text, &text_lower);
    // allow-noncombinator: structural normalization branch, not dispatch
    let (stripped_tp, _) = if text.contains('"') {
        (text_tp, None)
    } else {
        super::oracle_effect::strip_trailing_where_x(text_tp)
    };
    let tp = nom_tag_tp(&stripped_tp, "also ").unwrap_or(stripped_tp);
    let text_stripped = tp.original;
    let unquoted_text = strip_quoted_segments(text_stripped);
    unquoted_text.to_lowercase()
}

/// CR 606.2 + CR 607.5: locate the first refused by-reference loyalty sentence
/// (`:1360` shape) on an adopted static line, so the Priority-7 gate can carry
/// the recognized refusal as the same typed `static_structure` residual 14a
/// would have emitted (Matt review 5564241267 MED).
///
/// Exact-mirror contract: the verdict runs the EXACT clause parser
/// ([`parse_grant_all_abilities_clause`]) over the EXACT emission-site clause
/// probe ([`grant_clause_probe_lower`]) per quote-aware sentence, so
/// finder-verdict ⟺ `:1360` would-have-refused for direct-dispatch sentences.
/// Bound: emission applies anthem-level predicate transforms the finder does
/// not replicate (turn-suffix strip, for-each/as-long-as routing), so a
/// turn-suffixed loyalty sentence refuses at emission yet fails the finder's
/// clause — a false negative for which the old silent drop persists; no
/// in-repo or printed instance exists.
///
/// Re-admission coupling trap: deleting the `:1360` early return makes this
/// finder a FALSE POSITIVE (honestly-emitted loyalty grants would also match),
/// so re-admission must delete or gate the finder in the same change.
///
/// One-gap-per-line boundary: returns the FIRST refused sentence only. If a
/// multi-leaf composed line ever prints, widen to one gap per leaf plus a
/// multi-leaf positive leg.
pub(crate) fn find_refused_loyalty_grant_sentence(static_line: &str) -> Option<String> {
    let line_lower = static_line.to_lowercase();
    if !nom_primitives::scan_contains(&line_lower, "loyalty abilities of") {
        return None;
    }
    for sentence in split_static_sentences(static_line) {
        let sentence_lower = sentence.to_lowercase();
        let tp = TextPair::new(&sentence, &sentence_lower);
        if parse_continuous_subject_predicate(&tp, |subject, predicate| {
            parse_continuous_subject_filter(subject)?;
            let probe = grant_clause_probe_lower(predicate);
            let (kinds, _) = parse_grant_all_abilities_clause(&probe)?;
            // Same expression as the `:1360` early return — grep-able coupling.
            matches!(kinds, GrantedAbilityKinds::Loyalty).then_some(())
        })
        .is_some()
        {
            return Some(sentence);
        }
    }
    None
}

/// A complete predicate publishes no modification until every top-level clause
/// has an owner. Quoted bodies and protection riders keep their own grammar.
pub(crate) fn parse_complete_continuous_modifications(
    text: &str,
) -> Option<Vec<ContinuousModification>> {
    if complete_predicate_is_keyword_choice(text) {
        return None;
    }
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    if let Some((_, _)) = super::split_type_retention_clause(&lower) {
        return complete_modification_clause(text, None, ContinuousVerb::Unbound)
            .map(|(modifications, _)| modifications);
    }
    let (predicate, protection_rider) =
        if let Some((first, rider)) = tp.split_around_outside_quotes(". ") {
            complete_protection_rider(rider.original)?;
            (first, true)
        } else {
            (tp, false)
        };
    let (predicate, where_x, remaining) = complete_static_where_x(predicate)?;
    let (mut modifications, verb) =
        complete_modification_clause(predicate.original, where_x, ContinuousVerb::Unbound)?;
    if let Some(remaining) = remaining {
        modifications.extend(complete_modification_clause(remaining, where_x, verb)?.0);
    }
    if protection_rider
        && !modifications.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Protection(_)
                }
            )
        })
    {
        return None;
    }
    Some(modifications)
}

/// Complete member transactions retain recipient gates separately from board
/// conditions. Hosted callers keep these predicates live; resolving callers
/// check each gate once before installing their ordinary transient effect.
pub(crate) type CompleteContinuousMember = (
    Vec<ContinuousModification>,
    Option<StaticCondition>,
    Option<TargetFilter>,
    Option<crate::types::ability::Duration>,
);

/// Grammar tokens only: `If` additionally limits a suffix to its explicit
/// member, while `AsLongAs` and `Unless` retain their existing gate ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContinuousConditionSeparator {
    If,
    AsLongAs,
    Unless,
}

impl ContinuousConditionSeparator {
    /// Preserve the incumbent marker-class priority when several classes occur.
    pub(super) const PRIORITY: [Self; 3] = [Self::If, Self::AsLongAs, Self::Unless];

    pub(super) fn parse(input: &str) -> OracleResult<'_, Self> {
        alt((
            value(Self::If, tag(" if ")),
            value(Self::AsLongAs, tag(" as long as ")),
            value(Self::Unless, tag(" unless ")),
        ))
        .parse(input)
    }

    pub(super) fn apply(self, condition: StaticCondition) -> StaticCondition {
        match self {
            Self::If | Self::AsLongAs => condition,
            Self::Unless => StaticCondition::Not {
                condition: Box::new(condition),
            },
        }
    }
}

pub(crate) fn parse_complete_continuous_members(
    text: &str,
) -> Option<Vec<CompleteContinuousMember>> {
    if complete_predicate_is_keyword_choice(text) {
        return None;
    }
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let (predicate, where_x, remaining) = complete_static_where_x(tp)?;
    let (mut members, verb) =
        complete_continuous_members(predicate.original, where_x, ContinuousVerb::Unbound)?;
    if let Some(remaining) = remaining {
        members.extend(complete_continuous_members(remaining, where_x, verb)?.0);
    }
    Some(members)
}

/// A resolving keyword menu has its own choice owner. It cannot become a
/// conjunction of unconditional modifications, and retrying each list separator
/// cannot change that grammatical role.
fn complete_predicate_is_keyword_choice(text: &str) -> bool {
    let lower = text.to_lowercase();
    let mut parser = (
        alt((
            tag::<_, _, OracleError<'_>>("gains "),
            tag("gain "),
            tag("has "),
            tag("have "),
        )),
        tag("your choice of "),
    );
    parser.parse(lower.trim()).is_ok()
}

fn complete_continuous_members(
    text: &str,
    where_x: Option<&str>,
    inherited_verb: ContinuousVerb,
) -> Option<(Vec<CompleteContinuousMember>, ContinuousVerb)> {
    let text = text.trim().trim_end_matches('.').trim();
    let (without_duration, duration) = strip_trailing_duration(text);
    if duration.is_some() {
        let (mut members, verb) =
            complete_continuous_members(without_duration, where_x, inherited_verb)?;
        for (_, _, _, member_duration) in &mut members {
            if member_duration.is_none() {
                *member_duration = duration.clone();
            }
        }
        return Some((members, verb));
    }
    if let Some((mods, verb)) = complete_modification_clause(text, where_x, inherited_verb) {
        return Some((vec![(mods, None, None, None)], verb));
    }
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    // A complete leaf owns its own condition. If the prefix or suffix contains
    // another member this transaction fails and the connector recursion below
    // tries the next outside-quote boundary without publishing a prefix.
    for marker in ContinuousConditionSeparator::PRIORITY {
        if let Some(result) = tp.scan_around_outside_quotes(
            ContinuousConditionSeparator::parse,
            |body, separator, gate| {
                if separator != marker {
                    return None;
                }
                let (members, verb) =
                    complete_continuous_members(body.original, where_x, inherited_verb)?;
                // A condition belongs to one complete, previously ungated leaf.
                // Its own duration must survive the same transaction as its mods.
                let [(mods, None, None, duration)] = members.as_slice() else {
                    return None;
                };
                // A suffix "if" on a later explicit member cannot gate the
                // preceding members. Bare keyword lists stay atomic.
                if separator == ContinuousConditionSeparator::If
                    && nom_primitives::scan_at_word_boundaries(body.lower, |candidate| {
                        let (rest, _) = alt((
                            tag::<_, _, OracleError<'_>>("gets "),
                            tag("get "),
                            tag("has "),
                            tag("have "),
                            tag("gains "),
                            tag("gain "),
                        ))
                        .parse(candidate)?;
                        let offset = body.len() - candidate.len();
                        if offset == 0 || !body.is_outside_quotes_at(offset) {
                            return Err(nom::Err::Error(OracleError::new(
                                candidate,
                                nom::error::ErrorKind::Fail,
                            )));
                        }
                        Ok((rest, ()))
                    })
                    .is_some()
                {
                    return None;
                }
                let gate = gate.trim_start().trim_end().trim_end_matches('.');
                let condition = separator.apply(parse_static_condition(gate.original)?);
                let (condition, recipient) = partition_continuous_member_gate(condition)?;
                Some((
                    vec![(mods.clone(), condition, recipient, duration.clone())],
                    verb,
                ))
            },
        ) {
            return Some(result);
        }
    }
    tp.scan_around_outside_quotes(
        |input| alt((tag(", and "), tag(" and "), tag(", "))).parse(input),
        |left, _, right| {
            let (mut left, verb) =
                complete_continuous_members(left.original, where_x, inherited_verb)?;
            let (right, verb) = complete_continuous_members(right.original, where_x, verb)?;
            left.extend(right);
            Some((left, verb))
        },
    )
}

/// Extract only a wholly recipient-relative typed predicate. Ordinary board
/// gates stay on their static definition; mixed disjunctions cannot be split.
fn continuous_recipient_gate(condition: &StaticCondition) -> Option<TargetFilter> {
    match condition {
        StaticCondition::RecipientMatchesFilter { filter } => Some(filter.clone()),
        StaticCondition::Not { condition } => Some(TargetFilter::Not {
            filter: Box::new(continuous_recipient_gate(condition)?),
        }),
        StaticCondition::And { conditions } => Some(TargetFilter::And {
            filters: conditions
                .iter()
                .map(continuous_recipient_gate)
                .collect::<Option<Vec<_>>>()?,
        }),
        StaticCondition::Or { conditions } => Some(TargetFilter::Or {
            filters: conditions
                .iter()
                .map(continuous_recipient_gate)
                .collect::<Option<Vec<_>>>()?,
        }),
        _ => None,
    }
}

fn continuous_condition_has_recipient(condition: &StaticCondition) -> bool {
    match condition {
        StaticCondition::RecipientMatchesFilter { .. } => true,
        StaticCondition::Not { condition } => continuous_condition_has_recipient(condition),
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            conditions.iter().any(continuous_condition_has_recipient)
        }
        _ => false,
    }
}

fn partition_continuous_member_gate(
    condition: StaticCondition,
) -> Option<(Option<StaticCondition>, Option<TargetFilter>)> {
    if let Some(filter) = continuous_recipient_gate(&condition) {
        return Some((None, Some(filter)));
    }
    if let StaticCondition::And { conditions } = condition {
        let mut board = Vec::new();
        let mut recipients = Vec::new();
        for condition in conditions {
            let (ordinary, recipient) = partition_continuous_member_gate(condition)?;
            board.extend(ordinary);
            recipients.extend(recipient);
        }
        let board = match board.len() {
            0 => None,
            1 => board.pop(),
            _ => Some(StaticCondition::And { conditions: board }),
        };
        let recipients = match recipients.len() {
            0 => None,
            1 => recipients.pop(),
            _ => Some(TargetFilter::And {
                filters: recipients,
            }),
        };
        Some((board, recipients))
    } else if continuous_condition_has_recipient(&condition) {
        None
    } else {
        Some((Some(condition), None))
    }
}

fn complete_static_where_x<'a>(
    tp: TextPair<'a>,
) -> Option<(TextPair<'a>, Option<&'a str>, Option<&'a str>)> {
    fn marker(input: &str) -> OracleResult<'_, &str> {
        alt((tag(", where x is "), tag(" where x is "))).parse(input)
    }
    // The inner classifier owns quoted where-X clauses. Only an unquoted
    // binding belongs to this predicate. The quantity grammar owns its span;
    // any remaining clause must have a complete modification owner as well.
    let (rest, _) = many0(alt((
        recognize(nom::sequence::delimited(
            tag::<_, _, OracleError<'_>>("\""),
            take_until("\""),
            tag("\""),
        )),
        recognize(preceded(not(marker), nom::character::complete::anychar)),
    )))
    .parse(tp.lower)
    .ok()?;
    if rest.is_empty() {
        return Some((tp, None, None));
    }
    let (expression, _) = marker(rest).ok()?;
    let expression = tp
        .slice(tp.len() - expression.len(), tp.len())
        .trim_start()
        .trim_end()
        .trim_end_matches('.')
        .trim_start()
        .trim_end();
    if let Some((x, y)) = expression.split_around_outside_quotes(", and y is ") {
        all_consuming(super::oracle_nom::quantity::parse_quantity)
            .parse(x.lower)
            .ok()?;
        all_consuming(super::oracle_nom::quantity::parse_quantity)
            .parse(y.lower)
            .ok()?;
        let predicate = tp
            .slice(0, tp.len() - rest.len())
            .trim_end_matches(',')
            .trim_end();
        return Some((predicate, Some(expression.original), None));
    }
    let (remaining, _) = super::oracle_nom::quantity::parse_quantity
        .parse(expression.lower)
        .ok()?;
    let expression_end = expression.len() - remaining.len();
    let remaining = if remaining.trim().is_empty() {
        None
    } else {
        let (tail, _) = alt((
            tag::<_, _, OracleError<'_>>(", and "),
            tag(" and "),
            tag(", "),
        ))
        .parse(remaining)
        .ok()?;
        Some(
            expression
                .slice(expression.len() - tail.len(), expression.len())
                .original,
        )
    };
    let predicate = tp
        .slice(0, tp.len() - rest.len())
        .trim_end_matches(',')
        .trim_end();
    Some((
        predicate,
        Some(expression.slice(0, expression_end).original),
        remaining,
    ))
}

fn complete_protection_rider(text: &str) -> Option<crate::types::ability::ProtectionDoesNotRemove> {
    let lower = text.trim().trim_end_matches('.').to_lowercase();
    let (object, _) = alt((
        tag::<_, _, OracleError<'_>>("this effect doesn't remove "),
        tag("this effect does not remove "),
    ))
    .parse(lower.as_str())
    .ok()?;
    parse_does_not_remove_object(object)
}

#[derive(Clone, Copy)]
enum ContinuousVerb {
    Unbound,
    Pump,
    Grant,
    Remove,
}

fn complete_modification_clause(
    text: &str,
    where_x: Option<&str>,
    inherited_verb: ContinuousVerb,
) -> Option<(Vec<ContinuousModification>, ContinuousVerb)> {
    let text = text.trim().trim_end_matches('.').trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    let text = nom_tag_lower(text, &lower, "it ")
        .or_else(|| nom_tag_lower(text, &lower, "they "))
        .or_else(|| nom_tag_lower(text, &lower, "also "))
        .unwrap_or(text);
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    if let Some(modification) = parse_all_land_types_modification(text) {
        return Some((vec![modification], inherited_verb));
    }
    let (body, verb) = match alt((
        value(
            ContinuousVerb::Pump,
            alt((tag::<_, _, OracleError<'_>>("gets "), tag("get "))),
        ),
        value(
            ContinuousVerb::Grant,
            alt((tag("gains "), tag("gain "), tag("has "), tag("have "))),
        ),
        value(ContinuousVerb::Remove, alt((tag("loses "), tag("lose ")))),
    ))
    .parse(lower.as_str())
    {
        Ok((rest, verb)) => (&text[text.len() - rest.len()..], verb),
        Err(_) => (text, inherited_verb),
    };
    if let Some(((), descriptor)) = nom_on_lower(text, &lower, |input| {
        value(
            (),
            alt((
                tag::<_, _, OracleError<'_>>("becomes "),
                tag("become "),
                tag("is "),
                tag("are "),
            )),
        )
        .parse(input)
    }) {
        // The basic-land setter owns printed-ability suppression as well as
        // subtype replacement; a generic animation is not equivalent.
        let land_descriptor = nom_primitives::parse_article
            .parse(descriptor)
            .map_or(descriptor, |(rest, _)| rest);
        let land_sentence = format!("Lands are {land_descriptor}");
        let land_lower = land_sentence.to_lowercase();
        if let Some(definition) = super::type_change::parse_land_type_change(
            &TextPair::new(&land_sentence, &land_lower),
            &land_sentence,
        ) {
            return Some((definition.modifications, ContinuousVerb::Unbound));
        }
        let descriptor_lower = descriptor.to_lowercase();
        let descriptor_tp = TextPair::new(descriptor, &descriptor_lower);
        let animation_descriptor = where_x.map(|expression| {
            // Keep a separately owned retention sentence last: its complete
            // parser must not inherit the quantity expression as unread tail.
            if let Some((before, _)) = super::split_type_retention_clause(&descriptor_lower) {
                let (before, retention) = descriptor.split_at(before.len());
                format!(
                    "{}, where X is {expression}. {retention}",
                    before.trim().trim_end_matches('.').trim(),
                )
            } else {
                format!("{descriptor}, where X is {expression}")
            }
        });
        let animation = super::oracle_effect::animation::parse_animation_spec(
            animation_descriptor.as_deref().unwrap_or(descriptor),
            &mut ParseContext::default(),
        )
        .map(|spec| (spec, Vec::new()))
        .or_else(|| {
            let (base, grant) = descriptor_tp.split_around_outside_quotes(" with ")?;
            let grant = if let Some((before, _)) = super::split_type_retention_clause(grant.lower) {
                grant
                    .slice(0, before.len())
                    .trim_end()
                    .trim_end_matches('.')
            } else {
                grant
            };
            let (_, inner) = all_consuming(nom::sequence::delimited(
                tag::<_, _, OracleError<'_>>("\""),
                take_until("\""),
                tag("\""),
            ))
            .parse(grant.original.trim())
            .ok()?;
            let modifications = classify_quoted_inner(inner);
            if modifications.is_empty() {
                return None;
            }
            let spec = super::oracle_effect::animation::parse_animation_spec(
                base.original,
                &mut ParseContext::default(),
            )?;
            Some((spec, modifications))
        });
        if let Some((spec, grants)) = animation {
            let additive =
                super::oracle_effect::animation::has_in_addition_to_other_types(descriptor);
            let mut modifications =
                super::oracle_effect::animation::animation_modifications_with_replacement(
                    &spec, additive,
                );
            modifications.extend(grants);
            if super::oracle_effect::animation::has_in_addition_to_other_colors(descriptor) {
                modifications = modifications
                    .into_iter()
                    .flat_map(|modification| match modification {
                        ContinuousModification::SetColor { colors } => colors
                            .into_iter()
                            .map(|color| ContinuousModification::AddColor { color })
                            .collect(),
                        other => vec![other],
                    })
                    .collect();
            }
            if !modifications.is_empty() {
                return Some((modifications, ContinuousVerb::Unbound));
            }
        }
    }
    // CR 613.1f: denial is a distinct rule mode. It never invents removal of a
    // different denied keyword. Resolving callers can retain the ordinary
    // recipient-local mode grant; hosted publication promotes it to a sibling.
    if let Ok((keyword_text, _)) = alt((
        tag::<_, _, OracleError<'_>>("can't have or gain "),
        tag("can't have "),
    ))
    .parse(lower.as_str())
    {
        // OR in a denial names every forbidden keyword, rather than a choice.
        let (_, parts) = all_consuming(separated_list1(
            tag::<_, _, OracleError<'_>>(" or "),
            recognize(many1(preceded(
                not(tag(" or ")),
                nom::character::complete::anychar,
            ))),
        ))
        .parse(keyword_text)
        .ok()?;
        let keywords = parts
            .into_iter()
            .map(map_keyword_complete)
            .collect::<Option<Vec<_>>>()?;
        return Some((
            keywords
                .into_iter()
                .map(|keyword| ContinuousModification::AddStaticMode {
                    mode: StaticMode::CantHaveKeyword { keyword },
                })
                .collect(),
            verb,
        ));
    }
    if let Some(((), subtype_text)) = nom_on_lower(text, &lower, |input| {
        let (input, _) = alt((
            tag::<_, _, OracleError<'_>>("isn't "),
            tag("is not "),
            tag("are not "),
        ))
        .parse(input)?;
        value((), opt(nom_primitives::parse_article)).parse(input)
    }) {
        if let Some(core_type) =
            super::oracle_effect::animation::core_type_from_animation_word(subtype_text)
        {
            return Some((
                vec![ContinuousModification::RemoveType { core_type }],
                ContinuousVerb::Remove,
            ));
        }
        let (subtype, consumed) = parse_subtype(subtype_text)?;
        if consumed == subtype_text.len() {
            return Some((
                vec![ContinuousModification::RemoveSubtype { subtype }],
                verb,
            ));
        }
    }
    // These existing owners expose a complete nom production. Reuse their
    // ordinary lowering only after that production owns the entire clause.
    let complete_loss = all_consuming(parse_loss_enumeration)
        .parse(lower.as_str())
        .ok()
        .is_some_and(|(_, members)| !members.contains(&LossMember::CardTypes));
    let complete_supertype = all_consuming(preceded(
        tag::<_, _, OracleError<'_>>("is "),
        nom_target::parse_supertype_word,
    ))
    .parse(lower.as_str())
    .is_ok();
    let complete_type_set = all_consuming((
        alt((tag::<_, _, OracleError<'_>>("is "), tag("are "))),
        tag("every creature type"),
    ))
    .parse(lower.as_str())
    .is_ok();
    let complete_combat =
        all_consuming(super::evasion::parse_assigns_damage_from_toughness_predicate)
            .parse(lower.as_str())
            .is_ok();
    let complete_chosen = all_consuming(parse_last_chosen_characteristic_list)
        .parse(lower.as_str())
        .is_ok();
    if complete_loss
        || complete_supertype
        || complete_type_set
        || complete_combat
        || complete_chosen
        || parse_grant_all_abilities_clause(&lower).is_some()
    {
        let modifications = parse_continuous_modifications(text);
        return (!modifications.is_empty()).then_some((modifications, verb));
    }
    let mut keyword_mods = Vec::new();
    let mut keyword_complete = !matches!(verb, ContinuousVerb::Unbound);
    let parts = complete_keyword_list_parts(body).unwrap_or_default();
    for part in &parts {
        if matches!(verb, ContinuousVerb::Remove) {
            // Only bare keyword legs inherit this removal verb. Explicit
            // transitions are owned by the transactional connector below.
            // Removal consumes the keyword kind: the runtime removes every
            // parameterization by discriminant, including granted triggers.
            let kind = all_consuming(alt((
                value(
                    Keyword::Protection(crate::types::keywords::ProtectionTarget::Everything),
                    tag::<_, _, OracleError<'_>>("protection"),
                ),
                value(
                    Keyword::Ward(crate::types::keywords::WardCost::Mana(ManaCost::zero())),
                    tag("ward"),
                ),
                value(
                    Keyword::BandsWithOther(String::new()),
                    (tag("all \""), tag("bands with other"), tag("\" abilities")),
                ),
            )))
            .parse(part.as_ref())
            .ok()
            .map(|(_, keyword)| keyword);
            if let Some(keyword) = kind.or_else(|| map_keyword_complete(part)) {
                keyword_mods.push(ContinuousModification::RemoveKeyword { keyword });
            } else {
                keyword_complete = false;
                break;
            }
        } else {
            let part_lower = part.to_lowercase();
            let part = nom_tag_lower(part, &part_lower, "has ")
                .or_else(|| nom_tag_lower(part, &part_lower, "have "))
                .or_else(|| nom_tag_lower(part, &part_lower, "gains "))
                .or_else(|| nom_tag_lower(part, &part_lower, "gain "))
                .unwrap_or(part);
            if push_grant_clause_modifications_with_policy(
                &mut keyword_mods,
                part,
                where_x,
                super::oracle_keyword::KeywordRemainderPolicy::RequireAllConsuming,
            )
            .is_none()
            {
                keyword_complete = false;
                break;
            }
        }
    }
    // The legacy list helper intentionally removes dangling punctuation for its
    // hosts. A complete predicate may not use that recovery to admit a tail.
    let dangling = alt((tag::<_, _, OracleError<'_>>("and"), tag("or")))
        .parse(parts.last().map_or("", |p| p.as_ref()))
        .is_ok();
    if keyword_complete
        && !keyword_mods.is_empty()
        && !dangling
        // allow-noncombinator: terminal punctuation validation, not dispatch.
        && !text.trim_end().ends_with(',')
    {
        return Some((keyword_mods, verb));
    }
    if let Ok((_, inner)) = all_consuming(nom::sequence::delimited(
        tag::<_, _, OracleError<'_>>("\""),
        take_until("\""),
        tag("\""),
    ))
    .parse(body)
    {
        if !matches!(verb, ContinuousVerb::Grant) {
            return None;
        }
        let mods = classify_quoted_inner(inner);
        return (!mods.is_empty()).then_some((mods, verb));
    }
    // Resolve explicit companion predicates before a deferred quantity owner
    // can interpret their words as part of its population description.
    if let Some(sequence) = complete_modification_sequence(text, where_x, inherited_verb) {
        return Some(sequence);
    }
    let body_lower = body.to_lowercase();
    if matches!(verb, ContinuousVerb::Pump)
        || alt((tag::<_, _, OracleError<'_>>("+"), tag("-")))
            .parse(body_lower.as_str())
            .is_ok()
    {
        // CR 613.4c: "an additional" qualifies this additive pump, while the
        // base-P/T numeric production retains its separate grammar contract.
        let body_lower = body.to_lowercase();
        let body = nom_tag_lower(body, &body_lower, "an additional ").unwrap_or(body);
        let body_lower = body.to_lowercase();
        if let Ok((_, (_, power, _, toughness))) =
            all_consuming(parse_variable_pt_pattern).parse(body_lower.as_str())
        {
            // The shared axis grammar also recognizes fixed magnitudes; those
            // belong to the fixed owner below. True variables retain the
            // dynamic owner's binding checks and cannot fall back to numbers.
            if matches!(power, PtAxisMag::VarX | PtAxisMag::VarY)
                || matches!(toughness, PtAxisMag::VarX | PtAxisMag::VarY)
            {
                return parse_dynamic_pt_in_text(&format!("gets {}", body_lower), where_x)
                    .map(|modifications| (modifications, verb));
            }
        }
        if let Ok((rest, (power, toughness))) = parse_pt_mod_with_remainder(body) {
            if rest.trim().is_empty() {
                return Some((
                    vec![
                        ContinuousModification::AddPower { value: power },
                        ContinuousModification::AddToughness { value: toughness },
                    ],
                    verb,
                ));
            }
            if let Some(quantity_text) = nom_tag_lower(rest, &rest.to_lowercase(), " for each ") {
                if let Ok((tail, QuantityRef::CountersOn { .. })) =
                    super::oracle_nom::quantity::parse_for_each_clause_ref(quantity_text)
                {
                    if !tail.trim().is_empty() {
                        return None;
                    }
                }
                if let Some(quantity) =
                    super::oracle_quantity::parse_for_each_clause_expr_deferred(quantity_text)
                {
                    let mut mods = Vec::new();
                    push_dynamic_pt_modifications(&mut mods, power, toughness, quantity);
                    return (!mods.is_empty()).then_some((mods, verb));
                }
            }
        }
    }
    // CR 613.4b: setting only one base stat preserves the other axis. The
    // shared numeric owner must consume the whole value before publication.
    if let Ok((_, BasePtSide::Fixed { value })) = all_consuming(preceded(
        tag::<_, _, OracleError<'_>>("base power "),
        super::anthem::parse_base_pt_side,
    ))
    .parse(body_lower.as_str())
    {
        return Some((vec![ContinuousModification::SetPower { value }], verb));
    }
    if let Ok((_, BasePtSide::Fixed { value })) = all_consuming(preceded(
        tag::<_, _, OracleError<'_>>("base toughness "),
        super::anthem::parse_base_pt_side,
    ))
    .parse(body_lower.as_str())
    {
        return Some((vec![ContinuousModification::SetToughness { value }], verb));
    }
    if alt((
        tag::<_, _, OracleError<'_>>("base power and "),
        tag("power and "),
    ))
    .parse(body_lower.as_str())
    .is_ok()
    {
        if let Some(value) = super::anthem::parse_base_pt_each_equal_dynamic(body_lower.as_str()) {
            return Some((
                vec![
                    ContinuousModification::SetPowerDynamic {
                        value: value.clone(),
                    },
                    ContinuousModification::SetToughnessDynamic { value },
                ],
                verb,
            ));
        }
    }
    // The pair owner also consumes both values completely, including an X
    // pair whose expression is owned by the where-X clause.
    if let Ok((_, (power, _, toughness))) = all_consuming(preceded(
        tag::<_, _, OracleError<'_>>("base power and toughness "),
        (
            super::anthem::parse_base_pt_side,
            tag("/"),
            super::anthem::parse_base_pt_side,
        ),
    ))
    .parse(body_lower.as_str())
    {
        return match (power, toughness) {
            (BasePtSide::Fixed { value: power }, BasePtSide::Fixed { value: toughness }) => Some((
                vec![
                    ContinuousModification::SetPower { value: power },
                    ContinuousModification::SetToughness { value: toughness },
                ],
                verb,
            )),
            (power, toughness) => {
                let quantity = resolve_base_pt_x_ref(where_x)?;
                Some((
                    vec![
                        ContinuousModification::SetPowerDynamic {
                            value: base_pt_side_to_expr(power, &quantity),
                        },
                        ContinuousModification::SetToughnessDynamic {
                            value: base_pt_side_to_expr(toughness, &quantity),
                        },
                    ],
                    verb,
                ))
            }
        };
    }
    if let Some(modification) = parse_all_land_types_modification(text) {
        return Some((vec![modification], verb));
    }
    // CR 508.1d + CR 509.1c: a positive combat requirement is its own
    // complete member of a keyword/modifier conjunction, with the same
    // recipient and duration as the surrounding resolving instruction.
    if let Ok((_, requirement)) =
        all_consuming(parse_rule_static_predicate_nom).parse(lower.as_str())
    {
        if matches!(
            requirement,
            RuleStaticPredicate::MustAttack
                | RuleStaticPredicate::MustBlock
                | RuleStaticPredicate::MustBeBlocked
        ) {
            let definition = lower_rule_static(requirement, None, TargetFilter::SelfRef, text);
            return Some((
                vec![ContinuousModification::AddStaticMode {
                    mode: definition.mode,
                }],
                verb,
            ));
        }
    }
    // Restriction parsing already requires its complete clause.
    if let Some(modes) = parse_restriction_modes(lower.as_str()) {
        let mods: Vec<_> = modes
            .into_iter()
            .filter(static_mode_needs_grant_propagation)
            .map(|mode| ContinuousModification::AddStaticMode { mode })
            .collect();
        if !mods.is_empty() {
            return Some((mods, verb));
        }
    }
    if all_consuming(alt((
        tag::<_, _, OracleError<'_>>("can't become suspected"),
        tag("cant become suspected"),
    )))
    .parse(lower.as_str())
    .is_ok()
    {
        return Some((
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::CantBecomeSuspected,
            }],
            verb,
        ));
    }
    // A type companion ends at its modeled additive suffix. The type grammar
    // must own every word before that suffix, as well as the complete suffix.
    if let Some(type_body) = nom_tag_tp(&tp, "are ")
        .or_else(|| nom_tag_tp(&tp, "is "))
        .or_else(|| nom_tag_tp(&tp, "becomes "))
        .or_else(|| nom_tag_tp(&tp, "become "))
    {
        if let Some((types, suffix)) = type_body.split_around_outside_quotes(" in addition to ") {
            if all_consuming((
                alt((tag::<_, _, OracleError<'_>>("its "), tag("their "))),
                tag("other "),
                alt((
                    tag("colors and creature types"),
                    tag("colors and types"),
                    tag("creature types"),
                    tag("land types"),
                    tag("types"),
                    tag("colors"),
                )),
            ))
            .parse(suffix.lower)
            .is_ok()
            {
                let (filter, remainder) = parse_type_phrase_folding(types.original);
                if !matches!(filter, TargetFilter::Any) && remainder.trim().is_empty() {
                    return parse_additive_type_clause_modifications(&format!(
                        "is {} in addition to {}",
                        types.original, suffix.original
                    ))
                    .map(|modifications| (modifications, verb));
                }
            }
        }
    }
    complete_modification_sequence(text, where_x, inherited_verb)
}

fn complete_modification_sequence(
    text: &str,
    where_x: Option<&str>,
    inherited_verb: ContinuousVerb,
) -> Option<(Vec<ContinuousModification>, ContinuousVerb)> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    // Try every OUTSIDE-QUOTE connector transactionally. A failed split leaves
    // the original predicate intact, including all intervening semantic text.
    tp.scan_around_outside_quotes(
        |input| alt((tag(", and "), tag(" and "), tag(", "))).parse(input),
        |left, _, right| {
            let left = left.original.trim();
            let right = right.original.trim();
            // CR 205.1b: a final retention sentence qualifies the animation even
            // when a keyword member intervenes. Offer that same complete sentence
            // to the left transaction rather than attaching it to the keyword.
            let right_lower = right.to_lowercase();
            let retained_left =
                super::split_type_retention_clause(&right_lower).and_then(|(before, _)| {
                    let tail = &right[before.len()..];
                    let tail_lower = tail.to_lowercase();
                    all_consuming(terminated(
                        super::parse_type_retention_clause,
                        opt(tag(".")),
                    ))
                    .parse(tail_lower.as_str())
                    .ok()?;
                    Some((
                        format!("{left}. {tail}"),
                        right[..before.len()].trim().trim_end_matches('.').trim(),
                    ))
                });
            // CR 613.1f: a bare keyword keeps the preceding explicit operation.
            // Carry the left transaction's final verb into the right transaction;
            // publish neither vector if the complete right clause is unsupported.
            for (left, right) in std::iter::once((left, right)).chain(
                retained_left
                    .as_ref()
                    .map(|(left, right)| (left.as_str(), *right)),
            ) {
                if let Some((mut first, left_verb)) =
                    complete_modification_clause(left, where_x, inherited_verb)
                {
                    if let Some((mut last, final_verb)) =
                        complete_modification_clause(right, where_x, left_verb)
                    {
                        // CR 613.1f: "loses all other abilities" preserves abilities
                        // granted by this same complete instruction. Remove the older
                        // abilities before publishing the instruction's own grants.
                        // The complete right transaction above still owns every tail.
                        let right_lower = right.to_lowercase();
                        if preceded(
                            alt((tag::<_, _, OracleError<'_>>("loses "), tag("lose "))),
                            tag("all other abilities"),
                        )
                        .parse(right_lower.as_str())
                        .is_ok()
                        {
                            if let Some(index) = last.iter().position(|modification| {
                                matches!(modification, ContinuousModification::RemoveAllAbilities)
                            }) {
                                first.insert(0, last.remove(index));
                            }
                        }
                        first.extend(last);
                        return Some((first, final_verb));
                    }
                }
            }
            None
        },
    )
}

fn complete_keyword_list_parts(text: &str) -> Option<Vec<Cow<'_, str>>> {
    fn separator(input: &str) -> OracleResult<'_, &str> {
        alt((tag(", and/or "), tag(", and "), tag(" and "), tag(", "))).parse(input)
    }
    fn part(input: &str) -> OracleResult<'_, &str> {
        recognize(nom::multi::many1(preceded(
            not(separator),
            nom::character::complete::anychar,
        )))
        .parse(input)
    }
    let (_, parts) = all_consuming(separated_list1(separator, part))
        .parse(text.trim())
        .ok()?;
    let parts: Vec<_> = parts.into_iter().map(str::trim).collect();
    if parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    Some(super::oracle_keyword::expand_protection_parts(&parts))
}

/// CR 613.4c + CR 702: In a granted-keyword "where X is its `<P/T/mana value>`"
/// clause, "its" refers to the RECIPIENT of the grant — the creature that has the
/// keyword — not the grant's source object. `parse_quantity_ref_complete` maps a
/// bare "its power" / "its toughness" / "its mana value" to `Source` scope (the
/// correct default in a self-referential context); rebind those to `Recipient` so
/// the continuous layer resolves the value against each affected creature
/// (Infantry Shield: "Equipped creature has … mobilize X, where X is its power" →
/// the equipped creature's power). Self-grants (subject `~`) are unaffected — for
/// them the recipient IS the source, so both scopes resolve identically.
fn rebind_source_scope_to_recipient(
    qty: crate::types::ability::QuantityRef,
) -> crate::types::ability::QuantityRef {
    use crate::types::ability::{ObjectScope, QuantityRef};
    match qty {
        QuantityRef::Power {
            scope: ObjectScope::Source,
        } => QuantityRef::Power {
            scope: ObjectScope::Recipient,
        },
        QuantityRef::BasePower {
            scope: ObjectScope::Source,
        } => QuantityRef::BasePower {
            scope: ObjectScope::Recipient,
        },
        QuantityRef::Toughness {
            scope: ObjectScope::Source,
        } => QuantityRef::Toughness {
            scope: ObjectScope::Recipient,
        },
        QuantityRef::ObjectManaValue {
            scope: ObjectScope::Source,
        } => QuantityRef::ObjectManaValue {
            scope: ObjectScope::Recipient,
        },
        other => other,
    }
}

pub(crate) fn push_grant_clause_modifications(
    modifications: &mut Vec<ContinuousModification>,
    part: &str,
    where_x_expression: Option<&str>,
) {
    let _ = push_grant_clause_modifications_with_policy(
        modifications,
        part,
        where_x_expression,
        super::oracle_keyword::KeywordRemainderPolicy::DiscardRemainder,
    );
}

pub(crate) fn push_grant_clause_modifications_with_policy(
    modifications: &mut Vec<ContinuousModification>,
    part: &str,
    where_x_expression: Option<&str>,
    policy: super::oracle_keyword::KeywordRemainderPolicy,
) -> Option<()> {
    // CR 702.16n / CR 702.16p: complete admission permits only a modeled
    // protection exemption after the keyword sentence. Deliberately permissive
    // host-owned grants retain their existing remainder policy; the complete
    // predicate never treats that policy as a consumption proof.
    let part =
        match super::oracle_nom::bridge::split_once_on_lower(part, &part.to_lowercase(), ". ") {
            Some((first, rider)) => {
                if matches!(
                    policy,
                    super::oracle_keyword::KeywordRemainderPolicy::RequireAllConsuming
                ) && (complete_protection_rider(rider).is_none()
                    || !matches!(map_keyword_complete(first), Some(Keyword::Protection(_))))
                {
                    return None;
                }
                first
            }
            None => part,
        };

    let part_trimmed = part.trim().trim_end_matches('.');
    let (part_without_duration, _) = strip_trailing_duration(part_trimmed);
    let part_trimmed = part_without_duration.trim().trim_end_matches('.');
    let part_lower = part_trimmed.to_lowercase();

    // CR 509.1b: A compound equipped/enchanted-creature grant lists restriction
    // conjuncts with an anaphoric subject ("…, it can't be blocked, …" — Psychic
    // Paper). Strip a leading subject-anaphor so the bare predicate reaches the
    // single restriction authority (`parse_restriction_modes`) already called at
    // this fn's tail — no second `CantBeBlocked` detector. `tag("it ")` is
    // word-boundary-safe (it never matches "its …"). Keywords / "can't be the
    // target" grants never begin with these anaphors, so the strip leaves
    // `map_keyword` / `classify_cant_be_targeted` unaffected. Mirrors the
    // anaphor-strip idiom in oracle_static/shared.rs.
    let part_trimmed = nom_tag_lower(part_trimmed, &part_lower, "it ")
        .or_else(|| nom_tag_lower(part_trimmed, &part_lower, "this creature "))
        .or_else(|| nom_tag_lower(part_trimmed, &part_lower, "they "))
        .unwrap_or(part_trimmed);
    let part_lower = part_trimmed.to_lowercase();

    // CR 702: Check for dynamic "keyword X" with "where X is [qty]"
    if let Some(where_expr) = where_x_expression {
        if let Ok((_, kw_name)) = all_consuming(terminated(
            alpha1::<_, OracleError<'_>>,
            preceded(space1, tag_no_case("x")),
        ))
        .parse(part_lower.as_str())
        {
            if let Some(kind) = crate::types::keywords::DynamicKeywordKind::from_name(kw_name) {
                if let Ok((_, qty_ref)) =
                    super::oracle_nom::quantity::parse_quantity_ref_complete(where_expr)
                {
                    modifications.push(ContinuousModification::AddDynamicKeyword {
                        kind,
                        value: QuantityExpr::Ref {
                            qty: rebind_source_scope_to_recipient(qty_ref),
                        },
                    });
                    return Some(());
                }
            }
        }
    }

    // CR 608.2d + CR 613.1f: chosen-keyword anaphor — "that ability" / "the
    // chosen ability" / "the chosen keyword" refers back to a preceding
    // `Effect::Choose { ChoiceType::Keyword, persist }` clause (Angelic
    // Skirmisher: "choose first strike, vigilance, or lifelink. Creatures you
    // control gain that ability ..."; Linvala, Shield of Sea Gate: "choose
    // hexproof or indestructible. Creatures you control gain that ability
    // ..."). The plural forms — "each of the chosen abilities" / "the chosen
    // abilities" — refer back to a multi-keyword choice (Greymond, Avacyn's
    // Stalwart: "choose two abilities ... Humans you control have each of the
    // chosen abilities"); the same `AddChosenKeyword` reads ALL persisted
    // `ChosenAttribute::Keyword` entries at layer evaluation. Emits
    // `AddChosenKeyword`, the additive mirror of `RemoveChosenKeyword` (Urborg /
    // Walking Sponge). Checked before `map_keyword` so the anaphor is never
    // mis-classified as an unknown keyword. Builds for the whole "gain the
    // chosen keyword(s)" class.
    if all_consuming(alt((
        tag::<_, _, OracleError<'_>>("that ability"),
        tag("each of the chosen abilities"),
        tag("the chosen abilities"),
        tag("the chosen ability"),
        tag("the chosen keyword"),
    )))
    .parse(part_lower.as_str())
    .is_ok()
    {
        modifications.push(ContinuousModification::AddChosenKeyword);
        return Some(());
    }

    // CR 702.6a: bare "equip {N}" in a keyword list ("has indestructible and
    // equip {0}") is the equip activated ability — not an inert AddKeyword.
    // Mirrors `classify_quoted_inner`'s pre-keyword equip dispatch.
    if nom_tag_lower(&part_lower, &part_lower, "equip").is_some() {
        if let Some(ability) = super::oracle::try_parse_equip_lowered(part_trimmed) {
            modifications.push(ContinuousModification::GrantAbility {
                definition: Box::new(ability),
            });
            return Some(());
        }
    }

    if let Some(kw) = match policy {
        super::oracle_keyword::KeywordRemainderPolicy::DiscardRemainder => {
            map_keyword(part_trimmed)
        }
        super::oracle_keyword::KeywordRemainderPolicy::RequireAllConsuming => {
            map_keyword_complete(part_trimmed)
        }
    } {
        modifications.push(ContinuousModification::AddKeyword { keyword: kw });
        return Some(());
    }

    // CR 702.18a / 702.11a: a descriptive "can't be the target [of ...]" grant is
    // Shroud (blanket) or Hexproof (opponents only). Emit the keyword so the
    // existing targeting checks apply the correct controller scope, rather than a
    // scope-less rule static.
    if let Some(scope) =
        crate::parser::oracle_keyword::classify_cant_be_targeted(part_lower.as_str()).filter(|_| {
            matches!(
                policy,
                super::oracle_keyword::KeywordRemainderPolicy::DiscardRemainder
            )
        })
    {
        let keyword = match scope {
            crate::parser::oracle_keyword::CantBeTargetedScope::AnyPlayer => Keyword::Shroud,
            crate::parser::oracle_keyword::CantBeTargetedScope::OpponentsOnly => Keyword::Hexproof,
        };
        modifications.push(ContinuousModification::AddKeyword { keyword });
        return Some(());
    }

    if let Some(modes) = parse_restriction_modes(part_lower.as_str()) {
        let before = modifications.len();
        for mode in modes {
            if static_mode_needs_grant_propagation(&mode) {
                modifications.push(ContinuousModification::AddStaticMode { mode });
            }
        }
        return (modifications.len() > before).then_some(());
    }
    None
}

/// Extract quoted ability text from Oracle text and parse each into a typed AbilityDefinition.
///
/// Quoted abilities like `"{T}: Add two mana of any one color."` are parsed by splitting
/// at the cost separator (`:` after mana/tap symbols) and reusing `parse_oracle_cost` +
/// `parse_effect_chain`. Non-activated quoted text is parsed as a spell-like effect chain.
/// Parse quoted abilities and return the appropriate ContinuousModification.
/// CR 604.1: Trigger-prefix quoted text (when/whenever/at the beginning) becomes
/// GrantTrigger to preserve trigger metadata; all others become GrantAbility.
pub(crate) fn parse_quoted_ability_modifications(text: &str) -> Vec<ContinuousModification> {
    let mut modifications = Vec::new();
    let mut start = None;

    for (idx, ch) in text.char_indices() {
        if ch == '"' {
            if let Some(open) = start.take() {
                let ability_text = text[open + 1..idx].trim();
                modifications.extend(classify_quoted_inner(ability_text));
            } else {
                start = Some(idx);
            }
        }
    }

    modifications
}

/// CR 604.1: Classify already-stripped inner-quote text into the appropriate
/// `ContinuousModification` variant. Extracted from
/// `parse_quoted_ability_modifications` so callers that already have the
/// inner-quote slice (e.g., `parser::oracle_nom::return_as_aura::try_parse`)
/// can dispatch directly without re-walking for `"..."` pairs.
///
/// Dispatch ladder (single authority — DO NOT duplicate elsewhere):
///   1. CR 603.1: trigger prefix ("when "/"whenever "/"at the beginning of "/
///      "at the end of ") → `ContinuousModification::GrantTrigger`.
///   2. CR 702: keyword text ("flying", "ward—pay 2 life", etc.) →
///      `ContinuousModification::AddKeyword`.
///   3. CR 113.3d + CR 604.1: static-line text ("enchanted creature gets +N/+M",
///      "creatures you control have ...") → one or more
///      `ContinuousModification::GrantStaticAbility` / `AddStaticMode`.
///   4. Fallback (no static/trigger/keyword/replacement shape matched):
///      parse the remaining text as an ability body → `GrantAbility`
///      wrapping the parsed `AbilityDefinition`.
///
/// Visibility: `pub(crate)` so external crate-local callers can reuse the
/// canonical inner classifier without exposing the private
/// `parse_quoted_ability` / `parse_quoted_rule_static_modifications` helpers.
pub(crate) fn classify_quoted_inner(ability_text: &str) -> Vec<ContinuousModification> {
    // Oracle's punctuation convention carries the *enclosing* sentence's comma
    // INSIDE the closing quote of a granted ability when a clause follows — e.g.
    // Bronzehide Lion's `..."{G}{W}: Enchanted creature gains indestructible until
    // end of turn," and it loses all other abilities.` That trailing `,` is
    // sentence punctuation, not part of the ability; left attached it defeats the
    // inner duration combinator ("until end of turn," ≠ "until end of turn") and
    // the phrase falls through to prose, silently dropping the UntilEndOfTurn
    // duration. Strip it here, at the single boundary every quoted-grant caller
    // funnels through (aura grants, token grants, keyword-list grants), so a
    // quoted ability parses identically to its unquoted form.
    //
    // Only the comma is stripped — a trailing PERIOD is the ability's own terminal
    // punctuation ("{T}: Add {G}." / "...equal to the difference.") that must be
    // preserved in the serialized `description` (#5599), and it does not defeat
    // any inner combinator. `split_keyword_list` strips `['.', ',']` because there
    // the text is consumed structurally, not surfaced as a description.
    let ability_text = ability_text.trim().trim_end_matches(',').trim();
    if ability_text.is_empty() {
        return Vec::new();
    }

    // CR 114.1 + CR 113.3d: Nested single-quoted granted abilities inside a
    // double-quoted grant body must be promoted to double quotes before
    // dispatch so static-line and activated parsers recognise the inner ability
    // boundary (Koth emblem, Roar of the Fifth People chapter II — #5978).
    let ability_text = super::grammar::promote_nested_ability_quotes(ability_text);

    // CR 207.2c: A granted ability's text may carry an italicized ability-word
    // prefix ("Landfall — Whenever a land you control enters, ..."). Ability
    // words have no rules meaning, so the body parses through ordinary
    // trigger/keyword/static machinery. Strip a recognized ability-word prefix
    // and re-classify the remainder so the inner trigger/static is detected
    // (otherwise the ability-word prefix masks the trigger keyword and the line
    // falls through to the GrantAbility catch-all as an unimplemented effect).
    // Gated on a known ability word so a legitimate em-dash body is untouched.
    if let Some((aw_name, body)) = super::oracle_modal::strip_ability_word_with_name(&ability_text)
    {
        if super::oracle_modal::is_known_ability_word(&aw_name) {
            return classify_quoted_inner(&body);
        }
    }

    let lower = ability_text.to_lowercase();

    // CR 603.1: Detect trigger prefixes to route to GrantTrigger.
    if nom_tag_lower(&lower, &lower, "when ").is_some()
        || nom_tag_lower(&lower, &lower, "whenever ").is_some()
        || nom_tag_lower(&lower, &lower, "at the beginning of ").is_some()
        || nom_tag_lower(&lower, &lower, "at the end of ").is_some()
    {
        return super::oracle_trigger::parse_trigger_lines(&ability_text, "~")
            .into_iter()
            .map(|trigger| ContinuousModification::GrantTrigger {
                trigger: Box::new(trigger),
            })
            .collect();
    }

    // CR 702.6a: a standalone "Equip {N}" line is the equip activated ability —
    // detect it BEFORE keyword extraction. An MTGJSON keyword name can match the
    // printed equip cost, so parse_granted_keyword_fragment("equip {2}") would otherwise
    // land an inert AddKeyword{Equip}; but equip is an activated ability that needs
    // its Effect::Attach body. Mirrors oracle.rs's "Pre-keyword activated ability"
    // ordering. `try_parse_equip` assumes its caller has already confirmed the
    // "equip" prefix (it strips the first 5 bytes unconditionally), so the
    // `starts_with` guard is required — without it any quoted line would be
    // mis-parsed as equip.
    if nom_tag_lower(&lower, &lower, "equip").is_some() {
        if let Some(ability) = super::oracle::try_parse_equip_lowered(&ability_text) {
            return vec![ContinuousModification::GrantAbility {
                definition: Box::new(ability),
            }];
        }
    }

    // CR 702: Quoted text that is a keyword (e.g. "Ward—Pay 2 life") should be
    // granted as AddKeyword, not wrapped in an AbilityDefinition.
    if let Some(keyword) = super::oracle_keyword::parse_granted_keyword_fragment(&lower) {
        return vec![ContinuousModification::AddKeyword { keyword }];
    }

    // CR 113.3d + CR 604.1: Static-line text → GrantStaticAbility / AddStaticMode.
    if let Some(static_modifications) = parse_quoted_rule_static_modifications(&ability_text) {
        return static_modifications;
    }

    // CR 614.1a + CR 614.6 + CR 201.5b: Object-hosted replacement rider — "If ~
    // would leave the battlefield, exile it instead of putting it anywhere else."
    // (`~` because `normalize_card_name_refs` rewrote the "this creature/permanent/
    // land" self-reference card-wide). The parser IS the detector: route through
    // the shared `try_parse_leave_battlefield_exile_replacement` combinator and,
    // on a hit, grant the replacement to the recipient via `GrantReplacement` so
    // the `~` binds to the object that gains the ability (the reanimated
    // permanent), not the granting source.
    //
    // CR 611.2a + CR 613.1f: the granted definition is built from the UNSTAMPED
    // `leave_battlefield_exile_replacement` constructor, never from the detector's
    // `AddTargetReplacement` payload — that standalone payload carries
    // `RestrictionExpiry::UntilHostLeavesPlay` (#6538). A granted replacement's
    // lifetime is governed by the granting continuous effect's duration and is
    // re-derived every layer pass, so it must not carry a host-lifetime stamp:
    // #6538's `is_runtime_host_lifetime_replacement` keys base-install /
    // non-copiable / host-exit-prune off exactly that stamp, and a base-installed
    // granted rider would survive the granting effect lapsing (Elemental
    // Expressionist's "until end of turn" grant would become permanent).
    if super::oracle_effect::try_parse_leave_battlefield_exile_replacement(&lower).is_some() {
        return vec![ContinuousModification::GrantReplacement {
            replacement: Box::new(super::oracle_effect::leave_battlefield_exile_replacement()),
        }];
    }

    // Fallback: no static/trigger/keyword/replacement shape matched above —
    // parse the remaining text as an ability body and wrap it in GrantAbility.
    vec![ContinuousModification::GrantAbility {
        definition: Box::new(parse_quoted_ability(&ability_text)),
    }]
}

/// CR 702: Split a keyword list like "flying and first strike" into individual keywords.
pub(crate) fn split_keyword_list(text: &str) -> Vec<Cow<'_, str>> {
    // Strip both trailing periods and trailing commas. A comma tail arises when
    // `strip_quoted_segments` removes `and "Whenever..."` from the end of a list
    // like "has first strike, trample, haste, and \"Whenever...\""— the connector
    // `, and` is dropped but the comma after the last bare keyword remains.
    let text = text.trim().trim_end_matches(['.', ',']).trim();
    // Split on ", and/or ", ", and ", " and ", or ", " — longest-match-first
    // ordering prevents ", and " from consuming the prefix of ", and/or ".
    let mut parts: Vec<&str> = Vec::new();
    for chunk in text.split(", and/or ") {
        for sub_chunk in chunk.split(", and ") {
            for sub in sub_chunk.split(" and ") {
                for item in sub.split(", ") {
                    let trimmed = item.trim();
                    if !trimmed.is_empty() {
                        parts.push(trimmed);
                    }
                }
            }
        }
    }
    // CR 702.16: Expand "protection from X and from Y" into separate entries.
    // Reuses the building block from oracle_keyword.rs which handles inline,
    // comma-continuation, and Oxford comma protection patterns.
    super::oracle_keyword::expand_protection_parts(&parts)
}

/// CR 702.16n / CR 702.16p: Parse the trailing "This effect doesn't remove …"
/// rider that accompanies a protection grant (Flickering Ward, Ward cycle,
/// Spectra Ward, Benevolent Blessing). Returns `None` when the rider is absent.
///
/// Combinator-based word-boundary scan (parser-combinator gate): tries the
/// fixed rider prefix at each word start so the sentence may follow any
/// protection grant phrasing.
pub(crate) fn parse_protection_does_not_remove(
    text: &str,
) -> Option<crate::types::ability::ProtectionDoesNotRemove> {
    use nom::branch::alt;
    use nom::bytes::complete::tag;
    use nom::combinator::value;
    use nom::Parser;

    let lower = text.to_lowercase();
    let mut remaining = lower.as_str();
    while !remaining.is_empty() {
        if let Ok((rest, ())) = value(
            (),
            alt((
                tag::<_, _, nom::error::Error<&str>>("this effect doesn't remove "),
                tag("this effect does not remove "),
            )),
        )
        .parse(remaining)
        {
            let rest = rest.trim().trim_end_matches('.').trim();
            return parse_does_not_remove_object(rest);
        }
        remaining = remaining
            .find(' ')
            .map_or("", |i| remaining[i + 1..].trim_start());
    }
    None
}

/// CR 702.16n / CR 702.16p: Object phrase after "doesn't remove ".
fn parse_does_not_remove_object(
    rest: &str,
) -> Option<crate::types::ability::ProtectionDoesNotRemove> {
    use crate::types::ability::ProtectionDoesNotRemove;
    use nom::branch::alt;
    use nom::bytes::complete::tag;
    use nom::combinator::value;
    use nom::Parser;

    // Longest matches first so Benevolent Blessing doesn't collapse to Auras.
    // `~` is the post-normalization form of "this Aura" (SELF_REF_TYPE_PHRASES):
    // `parse_oracle_text` rewrites self-refs before static dispatch, so Source
    // must match both the raw Oracle phrase and the tilde form.
    alt((
        value(
            ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached,
            alt((
                tag::<_, _, nom::error::Error<&str>>(
                    "auras and equipment you control that are already attached to it",
                ),
                tag("auras and equipment you control that are already attached to them"),
            )),
        ),
        value(
            ProtectionDoesNotRemove::Source,
            alt((tag("this aura"), tag("~"))),
        ),
        value(ProtectionDoesNotRemove::Auras, tag("auras")),
    ))
    .parse(rest)
    .ok()
    .and_then(|(leftover, exemption)| leftover.trim().is_empty().then_some(exemption))
}

/// CR 702.16n / CR 702.16p: Stamp a parsed protection SBA-exemption rider onto
/// a continuous static when the Oracle text carries one.
pub(crate) fn with_protection_does_not_remove(
    def: crate::types::ability::StaticDefinition,
    text: &str,
) -> crate::types::ability::StaticDefinition {
    match parse_protection_does_not_remove(text) {
        Some(exemption) => def.protection_does_not_remove(exemption),
        None => def,
    }
}

#[cfg(test)]
mod quote_authority_tests {
    use super::*;

    #[test]
    fn complete_pumps_distinguish_fixed_axes_from_bound_variable_axes() {
        // Synthetic grammar discriminators complement the full printed-card
        // runtime tests; fixed zero axes still belong to the numeric owner.
        for (text, power, toughness) in [
            ("gets -1/-0", -1, 0),
            ("gets +0/+2", 0, 2),
            ("gets +3/+3", 3, 3),
        ] {
            let (mods, verb) = complete_modification_clause(text, None, ContinuousVerb::Unbound)
                .expect("fixed axes must reach the existing numeric owner");
            assert!(matches!(verb, ContinuousVerb::Pump));
            assert_eq!(
                mods,
                vec![
                    ContinuousModification::AddPower { value: power },
                    ContinuousModification::AddToughness { value: toughness },
                ],
                "{text}"
            );
        }
        let binding = Some("the number of creatures you control");
        let (mods, _) =
            complete_modification_clause("gets +X/+1", binding, ContinuousVerb::Unbound)
                .expect("a bound variable retains the dynamic owner");
        assert!(matches!(
            mods.as_slice(),
            [
                ContinuousModification::AddDynamicPower { .. },
                ContinuousModification::AddToughness { value: 1 },
            ]
        ));
        for (text, binding) in [
            ("gets +X/+1", Some("an unknown quantity")),
            ("gets +X/+Y", None),
            ("gets +Y/+X", binding),
            ("gets +1/+1 and unsupported tail", None),
        ] {
            assert!(
                complete_modification_clause(text, binding, ContinuousVerb::Unbound).is_none(),
                "an unowned variable binding or tail must remain refused: {text}"
            );
        }
    }

    #[test]
    fn complete_members_skip_inner_condition_markers_and_keep_outer_duration() {
        // Synthetic grammar compositions, not purported printed card text.
        // Each quoted ability owns the first marker; the later marker gates
        // the complete grant, including its independent duration.
        for (marker, quoted, separator) in [
            (
                "if",
                "This creature has flying if you control a Swamp.",
                ContinuousConditionSeparator::If,
            ),
            (
                "as long as",
                "This creature has flying as long as it's attacking.",
                ContinuousConditionSeparator::AsLongAs,
            ),
            (
                "unless",
                "This creature can't attack unless you control a Swamp.",
                ContinuousConditionSeparator::Unless,
            ),
        ] {
            let grant = format!("has \"{quoted}\" until end of turn");
            let ungated = parse_complete_continuous_members(&grant)
                .expect("the complete quoted grant reaches the ordinary member owner");
            assert_eq!(ungated.len(), 1);
            assert!(!ungated[0].0.is_empty());
            assert!(ungated[0].1.is_none());
            let gated = parse_complete_continuous_members(&format!(
                "{grant} {marker} you control an Island"
            ))
            .expect("a quoted first occurrence cannot hide the complete outer gate");
            assert_eq!(gated.len(), 1);
            assert_eq!(gated[0].0, ungated[0].0);
            assert_eq!(
                gated[0].1,
                Some(separator.apply(parse_static_condition("you control an Island").unwrap()))
            );
            assert!(gated[0].2.is_none());
            assert_eq!(
                gated[0].3,
                Some(crate::types::ability::Duration::UntilEndOfTurn)
            );
        }
    }

    #[test]
    fn complete_member_condition_scan_refuses_unowned_full_tails() {
        let quoted = "This creature has flying if you control a Swamp.";
        let complete = format!("has \"{quoted}\" if you control an Island");
        assert!(parse_complete_continuous_members(&complete).is_some());
        for incomplete in [
            format!("{complete} and unsupported tail"),
            format!("has \"{quoted}\" if unsupported tail"),
            "has \"This creature has flying if you control a Swamp if you control an Island"
                .to_owned(),
        ] {
            assert!(
                parse_complete_continuous_members(&incomplete).is_none(),
                "no complete prefix may publish from {incomplete:?}"
            );
        }
    }

    #[test]
    fn complete_member_condition_scan_preserves_explicit_member_ownership() {
        // The quoted first `if` must not obscure the later member's own gate,
        // or cause that gate or duration to attach to the preceding pump.
        let members = parse_complete_continuous_members(
            "gets +1/+1 and has \"This creature has flying if you control a Swamp.\" until end of turn if you control an Island",
        )
        .expect("both complete members retain their separate ownership");
        assert_eq!(members.len(), 2);
        assert!(matches!(
            members[0].0.as_slice(),
            [
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 }
            ]
        ));
        assert!(members[0].1.is_none());
        assert!(members[0].3.is_none());
        assert!(!members[1].0.is_empty());
        assert_eq!(
            members[1].1,
            parse_static_condition("you control an Island")
        );
        assert_eq!(
            members[1].3,
            Some(crate::types::ability::Duration::UntilEndOfTurn)
        );
    }

    #[test]
    fn complete_sequence_retries_after_a_failed_right_transaction() {
        let left =
            complete_modification_clause("has protection from Red", None, ContinuousVerb::Unbound)
                .expect("the first candidate has a valid left clause");
        assert!(complete_modification_clause("from Blue and gets +1/+1", None, left.1).is_none());
        let (mods, verb) = complete_modification_sequence(
            "has protection from Red and from Blue and gets +1/+1",
            None,
            ContinuousVerb::Unbound,
        )
        .expect("the later split preserves the complete protection list");
        assert!(matches!(verb, ContinuousVerb::Pump));
        assert_eq!(mods.len(), 4);
        assert!(matches!(
            &mods[..],
            [
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Protection(_)
                },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Protection(_)
                },
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ]
        ));
    }

    #[test]
    fn complete_sequence_keeps_quoted_mana_body_and_refuses_unowned_tails() {
        let quoted = "{1}, {T}: Add {G}.";
        let (mods, _) = complete_modification_sequence(
            &format!("has \"{quoted}\" and gets +1/+1"),
            None,
            ContinuousVerb::Unbound,
        )
        .expect("the quoted cost comma is not a member boundary");
        let expected = classify_quoted_inner(quoted);
        assert!(!expected.is_empty());
        assert_eq!(&mods[..expected.len()], expected.as_slice());
        assert_eq!(mods.len(), expected.len() + 2);
        for tail in [
            "has \"{T}: Add {G} and gets +1/+1",
            "has flying and unsupported tail",
        ] {
            assert!(complete_modification_sequence(tail, None, ContinuousVerb::Unbound).is_none());
            assert!(complete_continuous_members(tail, None, ContinuousVerb::Unbound).is_none());
        }
    }

    #[test]
    fn complete_members_keep_inner_verbs_and_outer_member_gates_separate() {
        let single = parse_complete_continuous_members(
            "has \"This creature has flying.\" if you control a Swamp",
        )
        .expect("the inner has does not split the outer gate");
        assert_eq!(single.len(), 1);
        assert!(single[0].1.is_some());
        let multiple = parse_complete_continuous_members(
            "gets +1/+1 and has \"This creature has flying.\" if you control a Swamp",
        )
        .expect("the explicit later member owns its gate");
        assert_eq!(multiple.len(), 2);
        assert!(multiple[0].1.is_none());
        assert!(multiple[1].1.is_some());
    }
}
