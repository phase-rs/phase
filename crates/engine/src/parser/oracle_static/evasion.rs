// CR 509.1b — combat restriction / evasion statics.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

/// CR 509.1b / CR 702.111b: "<N> or more creatures" minimum-blocker phrase.
/// Composed from `parse_number` + `tag(" or more creatures")`.
pub(crate) fn parse_min_blockers_phrase(input: &str) -> OracleResult<'_, u32> {
    let (rest, n) = nom_primitives::parse_number(input)?;
    let (rest, _) = tag(" or more creatures").parse(rest)?;
    Ok((rest, n))
}

pub(crate) fn parse_source_power_block_restriction(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let (rest, _) = tag::<_, _, OracleError<'_>>("creatures with power less than ")
        .parse(lower.as_str())
        .ok()?;
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("~'s power"),
        tag("this creature's power"),
    ))
    .parse(rest)
    .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" can't block ")
        .parse(rest)
        .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("creatures you control")
        .parse(rest)
        .ok()?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    Some(
        StaticDefinition::new(StaticMode::CantBeBlockedBy {
            filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::LT,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: ObjectScope::Source,
                        },
                    },
                },
            ])),
        })
        .affected(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ))
        .description(text.to_string()),
    )
}

/// CR 509.1b: classify the remainder after "can't be blocked except by " into a
/// typed `BlockExceptionKind`. A leading count phrase ("N or more creatures")
/// is a minimum-blocker constraint; everything else is a per-blocker quality
/// filter. The parser IS the count-vs-quality detector — combat never re-parses.
pub(crate) fn classify_block_exception(filter_text: &str) -> BlockExceptionKind {
    let trimmed = filter_text.trim_end_matches('.').trim();
    if let Ok((_, min)) = parse_min_blockers_phrase(trimmed) {
        BlockExceptionKind::MinBlockers { min }
    } else {
        let normalized = strip_redundant_block_exception_by(trimmed);
        BlockExceptionKind::Quality(parse_target(&normalized).0)
    }
}

/// CR 509.1b: The "except by <filter>" evasion grammar repeats the "by"
/// preposition before each disjunct — "except by Vehicles or by creatures with
/// haste" (Fast // Furious), mirroring the CR's own "and/or" exception wording.
/// `parse_target`'s disjunction recursion expects a bare type word after the
/// connector ("or creatures"), not a second "by", so the repeated preposition
/// truncates the union to its first disjunct. Strip the redundant "by " that
/// immediately follows a disjunction connector ("or by", "and by", "and/or by")
/// so the full union parses. Combinator-scanned, not string-replaced: the "by "
/// is only removed when it sits right after a recognized connector, never inside
/// a filter word.
fn strip_redundant_block_exception_by(filter_text: &str) -> Cow<'_, str> {
    type VE<'a> = OracleError<'a>;

    // Scan for "<connector> by " at any word boundary; the combinator emits the
    // connector span so it can be re-inserted while only the redundant "by " is
    // dropped. `before` is the prefix up to (but not including) the connector.
    let scan = nom_primitives::scan_preceded(filter_text, |i: &str| {
        let (after_conn, connector) = alt((
            tag::<_, _, VE<'_>>("and/or "),
            tag::<_, _, VE<'_>>("or "),
            tag::<_, _, VE<'_>>("and "),
        ))
        .parse(i)?;
        let (after_by, _) = tag::<_, _, VE<'_>>("by ").parse(after_conn)?;
        Ok((after_by, connector))
    });
    let Some((before, connector, after)) = scan else {
        return Cow::Borrowed(filter_text);
    };
    // Re-join with the connector preserved but the redundant "by " removed, then
    // recurse to handle any further "or by" repetitions.
    let joined = format!("{before}{connector}{after}");
    Cow::Owned(strip_redundant_block_exception_by(&joined).into_owned())
}

/// CR 603.2d: Extract the source-restriction filter from a trigger-doubler's
/// Oracle text. Trigger doublers name the doubled ability's source as
/// "a triggered ability of <SOURCE>" — e.g. "a Ninja creature you control"
/// (Splinter), "another creature you control of the chosen type" (Roaming
/// Throne), or the unrestricted "a permanent you control" (Panharmonicon-class).
///
/// Returns `Some(filter)` only when `<SOURCE>` narrows beyond a bare controlled
/// permanent (a subtype, a specific core type, or a property such as "another"
/// / "of the chosen type"). A bare "permanent you control" needs no filter —
/// `apply_trigger_doubling`'s controller match already enforces control — so
/// this returns `None`, leaving `affected` unset (Panharmonicon/Isshin/Drivnod).
///
/// CR 603.2d: The source may itself be a flat disjunction of typed clauses
/// sharing one trailing controller scope — "a Shaman or another Wizard you
/// control" (Harmonic Prodigy). Such sources are composed into a
/// controller-scoped `Or`, one disjunct per [`doubler_disjunct_connector`].
pub(crate) fn parse_doubler_source_filter(lower: &str) -> Option<TargetFilter> {
    // The source phrase sits between "a triggered ability of " and the trigger
    // verb: " to trigger" (cause-form: "...causes a triggered ability of X to
    // trigger") or " triggers" (source-form: "a triggered ability of X
    // triggers"). Try " to trigger" first so the cause-form's later " triggers"
    // ("that ability triggers an additional time") is not mistaken for the
    // delimiter.
    let (_, source_phrase, _) = nom_primitives::scan_preceded(lower, |i| {
        preceded(
            tag::<_, _, OracleError<'_>>("a triggered ability of "),
            alt((take_until(" to trigger"), take_until(" triggers"))),
        )
        .parse(i)
    })?;

    // Parse the leading typed clause. A bare controlled permanent
    // ("a permanent you control", Panharmonicon) adds nothing the controller
    // match doesn't already enforce, so an unrestrictive clause yields `None`.
    let (first, remainder) = parse_doubler_disjunct(source_phrase);
    if !doubler_source_is_restrictive(&first) {
        return None;
    }

    // CR 603.2d: The source may be a flat type union sharing one trailing
    // controller scope — "a Shaman or another Wizard you control" (Harmonic
    // Prodigy). `parse_type_phrase`'s own disjunction recursion only fires when
    // the trailing disjunct opens with a bare type word, not an article or an
    // "another"/"other" designation ("another Wizard"), so it stops after the
    // first disjunct and leaves the connector in the remainder. Dispatch on that
    // connector here: no connector means a single clause (the remainder is
    // informational and ignored, preserving the prior single-clause behavior);
    // a connector means a union, parsed disjunct-by-disjunct below.
    let Ok((mut rest, ())) = doubler_disjunct_connector(remainder.trim_start()) else {
        return Some(first);
    };

    let mut branches = vec![first];
    loop {
        let (filter, remainder) = parse_doubler_disjunct(rest);
        // Each disjunct must independently narrow to a restrictive typed clause.
        // If one does not — e.g. a stray "or" inside an unrelated suffix
        // ("power 4 or greater") split the phrase mid-clause — bail so the
        // doubler falls back to its conservative controller-only scope. This
        // keeps the fallback strictly safe: a mis-parse can only widen back to
        // "all your triggers", never narrow to a wrong subset.
        if !doubler_source_is_restrictive(&filter) {
            return None;
        }
        branches.push(filter);
        match doubler_disjunct_connector(remainder.trim_start()) {
            Ok((next, ())) => rest = next,
            Err(_) => break,
        }
    }

    // The shared "you control" scope is stated once, on the final disjunct;
    // distribute it to every branch so the union never doubles an opponent's
    // matching permanent.
    Some(distribute_controller_to_or(TargetFilter::Or {
        filters: branches,
    }))
}

/// CR 603.2d: Match the connector between two typed disjuncts in a flat union —
/// "or", the Oxford-comma "`, or`", or a bare list comma "`, `"
/// ("a Shaman, a Wizard, or a Cleric"). Longest-match-first so "`, or`" wins
/// over the bare "`, `". Combinator-based so the union is parsed, not
/// string-split.
fn doubler_disjunct_connector(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((tag::<_, _, OracleError<'_>>(", or "), tag(", "), tag("or "))),
    )
    .parse(input)
}

/// CR 603.2d: A doubler `affected` filter must narrow beyond a bare controlled
/// permanent — `apply_trigger_doubling`'s controller match already enforces
/// control, so `Permanent`/`Card`/`Any` core types add nothing. A clause is
/// restrictive when it carries a concrete type/subtype restriction or any
/// property (subtype designations, "another", "of the chosen type", etc.).
fn doubler_source_is_restrictive(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => {
            tf.type_filters.iter().any(|t| {
                !matches!(
                    t,
                    TypeFilter::Permanent | TypeFilter::Card | TypeFilter::Any
                )
            }) || !tf.properties.is_empty()
        }
        TargetFilter::Or { filters } => filters.iter().all(doubler_source_is_restrictive),
        // CR 603.2d: "a triggered ability of ~" names the doubler's own source
        // object — a self-reference that narrows to exactly one permanent, so it
        // is restrictive (Cloud, Midgar Mercenary).
        TargetFilter::SelfRef => true,
        _ => false,
    }
}

/// CR 603.2d + CR 301.5a: Parse one disjunct of a trigger-doubler's source
/// phrase, handling the two source-relative referents `parse_type_phrase` cannot
/// express before falling back to it for ordinary typed clauses:
/// - `~` — the normalized source name → [`TargetFilter::SelfRef`] (Cloud doubling
///   "a triggered ability of ~").
/// - "an Equipment attached to it" — here "it" is anaphoric on the doubler's own
///   source, so it is the source-relative [`FilterProp::AttachedToSource`] set.
///   `parse_type_phrase` maps "attached to it" to `AttachedToRecipient` (an
///   enchanted-creature host), which is the wrong referent in a doubler, so this
///   clause is hand-built.
fn parse_doubler_disjunct(phrase: &str) -> (TargetFilter, &str) {
    if let Ok((rest, filter)) = alt((
        value(TargetFilter::SelfRef, tag::<_, _, OracleError<'_>>("~")),
        value(
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Equipment".to_string())
                    .properties(vec![FilterProp::AttachedToSource]),
            ),
            alt((
                tag("an equipment attached to it"),
                tag("a equipment attached to it"),
            )),
        ),
    ))
    .parse(phrase)
    {
        return (filter, rest);
    }
    parse_type_phrase(phrase)
}

pub(crate) fn parse_max_combat_creatures_static(lower: &str) -> Option<StaticMode> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("no more than ")
        .parse(lower)
        .ok()?;
    let (max, rest) = parse_number(rest)?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("creature").parse(rest).ok()?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>("s")).parse(rest).ok()?;
    let (rest, mode) = alt((
        // CR 508.5 + CR 802.1: "...can attack you each combat" is a
        // defending-player-scoped cap (Judoon Enforcers) — only attacks
        // declared against this static's controller are limited. Must precede
        // the bare " can attack each combat" arm (longest match first).
        value(
            StaticMode::MaxAttackersEachCombat {
                max,
                defender: Some(AttackDefenderScope::Controller),
            },
            tag::<_, _, OracleError<'_>>(" can attack you each combat"),
        ),
        value(
            StaticMode::MaxAttackersEachCombat {
                max,
                defender: None,
            },
            tag(" can attack each combat"),
        ),
        value(
            StaticMode::MaxBlockersEachCombat { max },
            tag(" can block each combat"),
        ),
    ))
    .parse(rest)
    .ok()?;
    let (_, _) = all_consuming(opt(tag::<_, _, OracleError<'_>>(".")))
        .parse(rest)
        .ok()?;
    Some(mode)
}

/// CR 508.1c: The directional attack restriction (Pramikon, Sky Rampart;
/// Mystic Barrier; Teyo, Geometric Tactician): "Each player may attack only the
/// nearest opponent in the [last] chosen direction and planeswalkers controlled
/// by that opponent." The `opt(tag("last "))` tolerates both the base wording
/// ("the chosen direction") and Mystic Barrier's re-choosable phrasing ("the
/// last chosen direction"). The chosen direction is bound separately by the
/// linked "choose left or right" ability (CR 607.2d); this static is the nullary
/// marker read by the CR 508.1c attacker-declaration gate in `combat.rs`.
pub(crate) fn parse_attack_only_neighbor_static(lower: &str) -> Option<StaticMode> {
    let (rest, _) =
        tag::<_, _, OracleError<'_>>("each player may attack only the nearest opponent in the ")
            .parse(lower)
            .ok()?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>("last "))
        .parse(rest)
        .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(
        "chosen direction and planeswalkers controlled by that opponent",
    )
    .parse(rest)
    .ok()?;
    let (_, _) = all_consuming(opt(tag::<_, _, OracleError<'_>>(".")))
        .parse(rest)
        .ok()?;
    Some(StaticMode::AttackOnlyNeighbor)
}

pub(crate) fn parse_compound_subject_rule_static(
    text: &str,
    lower: &str,
) -> Option<Vec<StaticDefinition>> {
    let (subject_lower, first, after_first) =
        nom_primitives::scan_preceded(lower, parse_rule_static_predicate_nom)?;
    let (rest, mut predicates) = many0(preceded(
        parse_rule_static_separator_nom,
        parse_rule_static_tail_predicate_nom,
    ))
    .parse(after_first)
    .ok()?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    if predicates.is_empty() {
        return None;
    }
    let subject = text[..subject_lower.len()].trim();
    let affected = parse_rule_static_subject_filter(subject)?;
    predicates.insert(0, (first, None));
    Some(
        predicates
            .into_iter()
            .map(|(predicate, defended)| {
                lower_rule_static(predicate, affected.clone(), text).attack_defended(defended)
            })
            .collect(),
    )
}

/// CR 702.11 + CR 702.16 + CR 702.18 + CR 611.3a: Compound-subject keyword-grant
/// statics of the form `"You and <object subject> have <keyword>"` — a single
/// keyword grant bound to a player plus an object subset.
///
/// A single `StaticDefinition` cannot carry both a player scope and an object
/// scope, so decompose into two:
///   - an object-half `Continuous` def whose `affected` is the object subset;
///   - a player-half def whose mode is the player-applicable keyword mode
///     (`PlayerProtection` / `Hexproof` / `Shroud`) and whose `affected` is the
///     controller.
///
/// Object subjects reuse [`parse_rule_static_subject_filter`] so subtype scopes
/// ("Humans you control"), self refs ("this creature"), and "other <subtype>
/// you control" all resolve — not a hard-coded alt of three controller phrases.
///
/// Only player-applicable keywords claim this pattern (a player cannot
/// meaningfully "have flying"). Leading `"During your turn, "` gates both
/// halves with `DuringYourTurn` (Gruul Spellbreaker). Trailing `" as long as
/// <cond>"` is applied by `parse_continuous_gets_has` on the object half and
/// then copied onto the player half; inverted `"As long as <cond>, …"` forms
/// are rewritten to that trailing shape by the multi-dispatch path before
/// reaching here.
pub(crate) fn parse_compound_subject_keyword_static(
    text: &str,
    lower: &str,
) -> Option<Vec<StaticDefinition>> {
    let input = TextPair::new(text, lower);

    // Optional leading turn window (Gruul Spellbreaker class).
    let (body, turn_condition) = if let Some(rest) = nom_tag_tp(&input, "during your turn, ") {
        (rest, Some(StaticCondition::DuringYourTurn))
    } else {
        (input, None)
    };

    // Subject: "you and <object subject phrase>".
    let after_you = nom_tag_tp(&body, "you and ")?;

    // Locate the continuous predicate verb ("have"/"has"/"gain"/"gains"/…) so
    // the object subject can be any phrase `parse_rule_static_subject_filter`
    // understands — not a hard-coded controller-phrase alt list.
    let subject_end = find_continuous_predicate_start(after_you.lower)?;
    let (object_subject, predicate) = after_you.split_at(subject_end);
    let object_subject = object_subject.trim_start().trim_end();
    let predicate = predicate.trim_start().trim_end();
    if object_subject.is_empty() || predicate.is_empty() {
        return None;
    }

    let affected = parse_rule_static_subject_filter(object_subject.original)?;
    // Player half is reserved for the controller; refuse a second player scope
    // ("you and each player have …") so we never emit two player defs.
    if rule_static_affected_is_player_scope(&affected) {
        return None;
    }

    // Object-half: delegate the predicate to the shared keyword-grant builder
    // (also peels trailing " as long as <cond>" onto `object_def.condition`).
    let mut object_def = parse_continuous_gets_has(predicate.original, affected, text)?;

    // Derive the player-half mode from the granted keyword. Only player-
    // applicable keyword modes claim this pattern.
    let player_mode = object_def.modifications.iter().find_map(|m| match m {
        ContinuousModification::AddKeyword {
            keyword: crate::types::keywords::Keyword::Protection(pt),
        } => Some(StaticMode::PlayerProtection(pt.clone())),
        ContinuousModification::AddKeyword {
            keyword: crate::types::keywords::Keyword::Hexproof,
        } => Some(StaticMode::Hexproof),
        ContinuousModification::AddKeyword {
            keyword: crate::types::keywords::Keyword::Shroud,
        } => Some(StaticMode::Shroud),
        _ => None,
    })?;

    // Propagate leading turn-window / trailing as-long-as gates onto both halves
    // so the compound grant stays time-locked as one continuous effect (CR 611.3a).
    object_def.condition = match (turn_condition, object_def.condition.take()) {
        (Some(turn), Some(trailing)) => Some(StaticCondition::And {
            conditions: vec![turn, trailing],
        }),
        (Some(turn), None) => Some(turn),
        (None, Some(trailing)) => Some(trailing),
        (None, None) => None,
    };

    let mut player_def = StaticDefinition::new(player_mode)
        .affected(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ))
        .description(text.to_string());
    player_def.condition = object_def.condition.clone();

    Some(vec![object_def, player_def])
}

/// CR 702.16 + CR 702.16k + CR 702.16i: Player-SUBJECT protection of the form
/// `"You have protection from <quality>."` — the PLAYER gains the protection,
/// distinct from `"creatures you control have protection from <quality>"`
/// (which grants the keyword to permanents). A `StaticDefinition` cannot carry
/// the keyword on a player, so this emits `StaticMode::PlayerProtection` with
/// `affected = the controller (Typed{controller: You})`, mirroring the
/// player-half produced by `parse_compound_subject_keyword_static` and consumed
/// by `player_protection_from`.
///
/// Quality classification is delegated to the single authority
/// `parse_protection_target`, so every quality form already understood for
/// permanent protection (color, everything, each of your opponents, card type,
/// mana-value filter) is unlocked for the player subject in one stroke — this
/// builds the player-subject protection class, not one card (Absolute Virtue).
pub(crate) fn parse_player_protection_static(text: &str, lower: &str) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;

    // Subject + verb prefix: "you have protection from " (compose apostrophe /
    // contracted variants via `alt` only as real Oracle text requires them).
    let (rest_lower, _) = alt((
        tag::<_, _, VE<'_>>("you have protection from "),
        tag("you've got protection from "),
    ))
    .parse(lower)
    .ok()?;

    // Recover the original-case quality slice (TextPair-equivalent offset idiom),
    // then strip the sentence terminator. The quality is classified by the typed
    // `parse_protection_target` lookup — never an Oracle-text dispatch here.
    let quality = text[text.len() - rest_lower.len()..]
        .trim()
        .trim_end_matches('.')
        .trim();
    if quality.is_empty() {
        return None;
    }

    let target = crate::types::keywords::parse_protection_target(quality);

    Some(
        StaticDefinition::new(StaticMode::PlayerProtection(target))
            .affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
            .description(text.to_string()),
    )
}

pub(crate) fn parse_rule_static_separator_nom(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>(", or "),
            tag::<_, _, OracleError<'_>>(", and "),
            tag(", "),
            tag(" or "),
            tag(" and "),
        )),
    )
    .parse(input)
}

fn parse_extra_blockers_creature_noun(input: &str) -> OracleResult<'_, ()> {
    value((), (tag(" creature"), opt(tag("s")))).parse(input)
}

pub(crate) fn parse_extra_blockers_count_phrase(input: &str) -> OracleResult<'_, Option<u32>> {
    alt((
        value(None, tag("any number of creatures")),
        map(
            preceded(
                tag("an additional "),
                terminated(
                    nom_primitives::parse_number,
                    parse_extra_blockers_creature_noun,
                ),
            ),
            Some,
        ),
        value(
            Some(1),
            (
                tag("additional creature"),
                opt(tag::<_, _, OracleError<'_>>("s")),
            ),
        ),
        map(
            terminated(
                nom_primitives::parse_number,
                (
                    tag(" additional creature"),
                    opt(tag::<_, _, OracleError<'_>>("s")),
                ),
            ),
            Some,
        ),
    ))
    .parse(input)
}

fn parse_extra_blockers_tail(input: &str) -> OracleResult<'_, Option<u32>> {
    let (input, count) = parse_extra_blockers_count_phrase(input)?;
    let (input, _) = opt(alt((
        tag::<_, _, OracleError<'_>>(" each combat"),
        tag(" this combat"),
        tag(" this turn"),
    )))
    .parse(input)?;
    Ok((input, count))
}

fn parse_can_block_extra_blockers_predicate(input: &str) -> OracleResult<'_, Option<u32>> {
    preceded(tag("can block "), parse_extra_blockers_tail).parse(input)
}

fn parse_and_can_block_extra_blockers_predicate(input: &str) -> OracleResult<'_, Option<u32>> {
    preceded(tag("and "), parse_can_block_extra_blockers_predicate).parse(input)
}

fn extra_blockers_static_definition(
    affected: TargetFilter,
    mode: StaticMode,
    text: &str,
) -> StaticDefinition {
    if matches!(affected, TargetFilter::SelfRef) {
        StaticDefinition::new(mode)
            .affected(affected)
            .description(text.to_string())
    } else {
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::AddStaticMode { mode }])
            .description(text.to_string())
    }
}

fn parse_subject_trailing_conjunction(input: &str) -> OracleResult<'_, ()> {
    value((), all_consuming((take_until(" and"), tag(" and")))).parse(input)
}

pub(crate) fn parse_extra_blockers_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let (before, count, rest) =
        nom_primitives::scan_preceded(&lower, parse_can_block_extra_blockers_predicate)?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    let subject_len = before
        .trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .len();
    let subject = text[..subject_len].trim();
    if subject.is_empty() {
        return None;
    }
    let subject_lower = lower[..subject_len].trim();
    if parse_subject_trailing_conjunction(subject_lower).is_ok() {
        return None;
    }
    let affected = parse_rule_static_subject_filter(subject).unwrap_or(TargetFilter::SelfRef);
    Some(extra_blockers_static_definition(
        affected,
        StaticMode::ExtraBlockers { count },
        text,
    ))
}

pub(crate) fn is_extra_blockers_static_candidate(lower: &str) -> bool {
    parse_extra_blockers_static(lower).is_some()
}

/// CR 509.1c + CR 611.3a: A printed permanent forced-block ("lure") static —
/// "All creatures able to block `<subject>` do so" — where `<subject>` is a
/// rule-static subject (a self-reference `~`, or "enchanted creature" for the
/// Aura form). This is the PERMANENT static class (Ochran Assassin, Breaker of
/// Armies, Prized Unicorn, Lure), distinct from the one-shot spell/activated
/// form "… target creature this turn do so" (Alluring Scent), which
/// `try_parse_mass_forced_block` lowers to a duration-bounded `GenericEffect`.
/// Misclassifying the printed static as that one-shot effect leaves it as a
/// never-resolving ability, so the lure never applies (issue #4949). Emitting a
/// permanent `StaticMode::MustBeBlockedByAll` static routes it through the combat
/// enforcement that already exists (`game/combat.rs`, CR 509.1c). The subject is
/// resolved by `parse_rule_static_subject_filter`, which returns `None` for a
/// `target …` subject so a genuine spell/effect form still falls through to the
/// effect parser.
/// CR 509.1c: the blocker slot of the "All <blockers> able to block <subject> do
/// so" grammar, consumed after the leading `tag("all ")`. Returns the remainder
/// (positioned at the subject) and the optional blocker filter:
/// - Slot A — the bare "creature(s) able to block " form → `None` (every idle
///   able creature is compelled: Lure, Ochran Assassin). Byte-identical to the
///   old single `tag`, so unfiltered lines are unchanged.
/// - Slot B — "<type-phrase> able to block " (Talruum Piper "creatures with
///   flying", Marble Priest "Walls") → `Some(filter)`. The type slot is parsed by
///   the shared `parse_type_phrase` building block; the phrase must fully consume
///   up to the literal " able to block " (else the Some form is rejected as
///   mis-scoped and this returns `None`, letting the line fall through).
///
/// A runs before B, and B requires the " able to block " tag on its remainder, so
/// a bare "creatures able to block " is always resolved to `None` by A.
pub(crate) fn parse_forced_block_blocker_slot(input: &str) -> Option<(&str, Option<TargetFilter>)> {
    // Slot A: bare "creature(s) able to block " → None (unfiltered lure).
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("creatures able to block "),
        tag("creature able to block "),
    ))
    .parse(input)
    {
        return Some((rest, None));
    }
    // Slot B: "<type-phrase> able to block " → Some(filter). Scope the type phrase
    // to the text before the literal " able to block " so `parse_type_phrase`
    // cannot over-consume into the subject.
    let (rest, type_text) = take_until::<_, _, OracleError<'_>>(" able to block ")
        .parse(input)
        .ok()?;
    let (filter, filter_remainder) = parse_type_phrase(type_text);
    if !filter_remainder.trim().is_empty() {
        return None; // mis-scoped Some — reject rather than accept a partial filter.
    }
    let (rest, _) = tag::<_, _, OracleError<'_>>(" able to block ")
        .parse(rest)
        .ok()?;
    Some((rest, Some(filter)))
}

pub(crate) fn parse_forced_block_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    // Grammar: "all creatures able to block <subject> do so[.]". The subject is
    // taken up to the " do so" imperative, then classified by
    // `parse_rule_static_subject_filter`. A one-shot spell form ("… target
    // creature this turn do so", Alluring Scent) has a `target …` subject, which
    // is not a rule-static subject, so this returns `None` and the line falls
    // through to `try_parse_mass_forced_block` — no separate duration/target check
    // is needed.
    // Two-slot blocker grammar after "all ": slot A (the bare "creatures able to
    // block " form) yields `blockers = None` (every idle able creature — Lure,
    // Ochran Assassin); slot B ("<type-phrase> able to block ", e.g. "creatures
    // with flying" for Talruum Piper, "Walls" for Marble Priest) yields
    // `blockers = Some(filter)` (only matching creatures compelled). A runs
    // before B and is byte-identical to the old literal, so unfiltered lines are
    // unchanged. CR 509.1c.
    let (rest, _) = tag::<_, _, OracleError<'_>>("all ")
        .parse(lower.as_str())
        .ok()?;
    let (rest, blockers) = parse_forced_block_blocker_slot(rest)?;
    let (_, subject_lower) = all_consuming(terminated(
        take_until::<_, _, OracleError<'_>>(" do so"),
        (tag(" do so"), opt(tag(".")), space0),
    ))
    .parse(rest)
    .ok()?;
    // The lowercasing is ASCII-length-preserving, so the subject occupies the same
    // byte span in the original-cased `text`.
    let start = lower.len() - rest.len();
    let subject = text.get(start..start + subject_lower.len())?.trim();
    let affected = parse_rule_static_subject_filter(subject)?;
    Some(
        StaticDefinition::new(StaticMode::MustBeBlockedByAll { blockers })
            .affected(affected)
            .description(text.to_string()),
    )
}

pub(crate) fn is_forced_block_static_candidate(lower: &str) -> bool {
    parse_forced_block_static(lower).is_some()
}

/// CR 702.3b + CR 611.3a + CR 613: Decompose `"<predicate_1> and can attack
/// as though <pronoun> didn't have defender[ as long as <cond>]"` into two
/// independent `StaticDefinition`s sharing the same `affected` + `condition`.
///
/// Strategy: locate the conjunction phrase at a word boundary via
/// `scan_preceded`, splice it out of the text, and re-parse the remainder
/// via `parse_static_line_multi`. Recursion is safe — the spliced text no
/// longer contains the conjunction marker. The first conjunct's `affected`
/// and `condition` are cloned onto a companion `CanAttackWithDefender`
/// definition. All emitted definitions share the original full-line
/// description, matching the convention used by other compound handlers
/// (e.g., `CantBeEquipped` + `CantBeEnchanted`).
pub(crate) fn try_split_and_can_attack_despite_defender(
    text: &str,
) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;
    let lower = text.to_lowercase();

    // `scan_preceded` advances past each space so `remaining` always starts on
    // a word — so the tag begins at "and", not at the leading space. We then
    // strip the trailing space of `before` to produce clean Line A text.
    let (before, matched, _rest) = nom_primitives::scan_preceded(&lower, |i: &str| {
        alt((
            tag::<_, _, VE>("and can attack as though it didn't have defender"),
            tag::<_, _, VE>("and can attack as though they didn't have defender"),
        ))
        .parse(i)
    })?;

    // ASCII lowercasing preserves byte lengths, so `before`/`matched` byte
    // offsets into `lower` also index into the original-case `text`.
    let before_len = before.len();
    let matched_len = matched.len();
    // Drop the trailing space that precedes the "and" marker so Line A doesn't
    // end up with " ." before its terminating period.
    let cut_end = if before.ends_with(' ') {
        before_len - 1
    } else {
        before_len
    };
    let line_a = format!("{}{}", &text[..cut_end], &text[before_len + matched_len..]);

    let mut defs = parse_static_line_multi(&line_a);
    if defs.is_empty() {
        return None;
    }

    // Restore descriptions to the original full-line text on every conjunct.
    for def in &mut defs {
        def.description = Some(text.to_string());
    }

    let template = &defs[0];
    let mut companion =
        StaticDefinition::new(StaticMode::CanAttackWithDefender).description(text.to_string());
    if let Some(affected) = template.affected.clone() {
        companion = companion.affected(affected);
    }
    if let Some(cond) = template.condition.clone() {
        companion = companion.condition(cond);
    }
    defs.push(companion);
    Some(defs)
}

pub(crate) fn try_split_and_must_attack_block(text: &str) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;
    let lower = text.to_lowercase();

    let (before, modes, rest) = nom_primitives::scan_preceded(&lower, |i: &str| {
        let (i, _) = opt(tag::<_, _, VE>("and ")).parse(i)?;
        alt((
            value(
                vec![StaticMode::MustAttack, StaticMode::MustBlock],
                alt((
                    tag::<_, _, VE>("attacks or blocks each combat if able"),
                    tag("attack or block each combat if able"),
                )),
            ),
            value(
                vec![StaticMode::MustAttack],
                alt((
                    tag::<_, _, VE>("attacks each combat if able"),
                    tag("attack each combat if able"),
                    tag("attacks each turn if able"),
                    tag("attack each turn if able"),
                    tag("must attack each combat if able"),
                    tag("must attack if able"),
                )),
            ),
            value(
                vec![StaticMode::MustBlock],
                alt((
                    tag::<_, _, VE>("blocks each combat if able"),
                    tag("block each combat if able"),
                    tag("blocks each turn if able"),
                    tag("block each turn if able"),
                    tag("must block each combat if able"),
                    tag("must block if able"),
                )),
            ),
            value(
                vec![StaticMode::MustBeBlocked { by: None }],
                alt((
                    tag::<_, _, VE>("must be blocked each combat if able"),
                    tag("must be blocked if able"),
                )),
            ),
            value(
                vec![StaticMode::Goaded],
                alt((tag::<_, _, VE>("is goaded"), tag("are goaded"))),
            ),
        ))
        .parse(i)
    })?;
    let tail_predicates = parse_rule_static_tail_predicates(rest)?;
    let cut_end = before
        .trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .len();
    let line_a = format!("{}.", text[..cut_end].trim_end_matches('.'));
    let mut defs = parse_static_line_multi(&line_a);
    if defs.is_empty() {
        return None;
    }
    for def in &mut defs {
        def.description = Some(text.to_string());
    }

    let template = &defs[0];
    let affected = template.affected.clone()?;
    let condition = template.condition.clone();
    for mode in modes {
        let mut companion = StaticDefinition::new(mode)
            .affected(affected.clone())
            .description(text.to_string());
        if let Some(condition) = condition.clone() {
            companion = companion.condition(condition);
        }
        defs.push(companion);
    }
    for (predicate, defended) in tail_predicates {
        let mut companion =
            lower_rule_static(predicate, affected.clone(), text).attack_defended(defended);
        if let Some(condition) = condition.clone() {
            companion = companion.condition(condition);
        }
        defs.push(companion);
    }
    Some(defs)
}

/// CR 509.1b: Decompose `"<predicate> and can block an additional N creatures
/// [each combat]"` (or `"… any number of creatures"`) into the first conjunct's
/// static(s) plus an `ExtraBlockers` static sharing the same `affected` set.
///
/// Without this split the trailing extra-block grant was dropped: Brave the
/// Sands ("Creatures you control have vigilance and can block an additional
/// creature each combat.") parsed to only the vigilance grant, so its
/// extra-block clause did nothing. Mirrors `try_split_and_can_attack_despite_defender`
/// and `try_split_and_must_attack_block`: splice the conjunction out, re-parse
/// the remainder for the first conjunct, then clone its `affected`/`condition`
/// onto the companion `ExtraBlockers` definition.
pub(crate) fn try_split_and_can_block_additional(text: &str) -> Option<Vec<StaticDefinition>> {
    let lower = text.to_lowercase();

    let (before, count, rest) =
        nom_primitives::scan_preceded(&lower, parse_and_can_block_extra_blockers_predicate)?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    let cut_end = before
        .trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .len();
    let line_a = format!("{}.", text[..cut_end].trim_end_matches('.'));
    let mut defs = parse_static_line_multi(&line_a);
    if defs.is_empty() {
        return None;
    }
    for def in &mut defs {
        def.description = Some(text.to_string());
    }

    let affected = defs[0].affected.clone()?;
    let condition = defs[0].condition.clone();
    let mut companion =
        extra_blockers_static_definition(affected, StaticMode::ExtraBlockers { count }, text);
    if let Some(condition) = condition {
        companion = companion.condition(condition);
    }
    defs.push(companion);
    Some(defs)
}

/// CR 509.1b: Decompose `"<continuous grant> and can't block"` into the first
/// conjunct's static(s) plus a `CantBlock` static sharing the same `affected`
/// (and any `condition`).
///
/// Without this split the trailing blocking restriction was dropped: downside
/// pumps like Copper Carapace ("Equipped creature gets +2/+2 and can't block."),
/// Maniacal Rage / Undying Rage, and Threshold creatures ("this creature gets
/// +2/+2 and can't block.") parsed to only the P/T grant, so the equipped/
/// enchanted creature could still block — the card's entire drawback vanished.
/// Mirrors `try_split_and_can_block_additional`. A terminal-phrase guard keeps
/// this disjoint from the already-handled "can't block alone", "can't block
/// <filter>", and "can't block unless …" shapes.
pub(crate) fn try_split_and_cant_block(text: &str) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;
    let lower = text.to_lowercase();

    let (before, _matched, rest) = nom_primitives::scan_preceded(&lower, |i: &str| {
        // Match both the ASCII and typographic U+2019 apostrophe.
        let (i, _) = alt((
            tag::<_, _, VE>("and can't block"),
            tag::<_, _, VE>("and can\u{2019}t block"),
        ))
        .parse(i)?;
        // Optional trailing duration phrase.
        let (i, _) = opt(alt((
            tag::<_, _, VE>(" each combat"),
            tag::<_, _, VE>(" this combat"),
            tag::<_, _, VE>(" this turn"),
        )))
        .parse(i)?;
        Ok((i, ()))
    })?;

    // CR 509.1b: only the bare, terminal "can't block" is a plain CantBlock. A
    // remaining tail ("alone", "<filter>", "unless …") is a different restriction
    // owned by another branch — decline so we don't mis-split it.
    if !rest.trim_start().trim_end_matches('.').trim().is_empty() {
        return None;
    }

    let cut_end = before
        .trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .len();
    let line_a = format!("{}.", text[..cut_end].trim_end_matches('.'));
    let mut defs = parse_static_line_multi(&line_a);
    if defs.is_empty() {
        return None;
    }
    for def in &mut defs {
        def.description = Some(text.to_string());
    }

    let affected = defs[0].affected.clone()?;
    let condition = defs[0].condition.clone();
    let mut companion = StaticDefinition::new(StaticMode::CantBlock)
        .affected(affected)
        .description(text.to_string());
    if let Some(condition) = condition {
        companion = companion.condition(condition);
    }
    defs.push(companion);
    Some(defs)
}

/// CR 502.3: Decompose `"<continuous grant> and doesn't untap during [its
/// controller's] untap step"` into the first conjunct's static(s) plus a
/// `CantUntap` static sharing the same `affected` (and any trailing "as long
/// as …" condition).
///
/// Without this split the trailing untap restriction was dropped: Flood the
/// Engine ("Enchanted permanent loses all abilities and doesn't untap during
/// its controller's untap step.") parsed to only the loses-all-abilities def,
/// so the enchanted permanent untapped normally — the lock vanished. Mirrors
/// `try_split_and_cant_block`. Requiring a recognized untap-step phrase keeps
/// this disjoint from the one-time "during their next untap step" effect, and
/// the `defs.is_empty()` guard leaves the "enters tapped and doesn't untap"
/// replacement+static compound (issue #292) to its own earlier carve-out.
pub(crate) fn try_split_and_doesnt_untap(text: &str) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;
    let lower = text.to_lowercase();

    let (before, _matched, rest) = nom_primitives::scan_preceded(&lower, |i: &str| {
        // Match both the ASCII and typographic U+2019 apostrophe.
        let (i, _) = alt((
            tag::<_, _, VE>("and doesn't untap during"),
            tag::<_, _, VE>("and doesn\u{2019}t untap during"),
        ))
        .parse(i)?;
        // Require a recognized permanent-static untap-step phrase to follow, so
        // we only split the standing form (not a one-time "during their next
        // untap step", which is an effect, not a CantUntap static).
        let (i, _) = preceded(
            space0,
            alt((
                tag::<_, _, VE>("its controller's untap step"),
                tag::<_, _, VE>("its controller\u{2019}s untap step"),
                tag::<_, _, VE>("their controllers' untap steps"),
                tag::<_, _, VE>("their controllers\u{2019} untap steps"),
                tag::<_, _, VE>("your untap step"),
            )),
        )
        .parse(i)?;
        Ok((i, ()))
    })?;

    // CR 502.3: only split when the untap clause is terminal or carries a
    // recognized "as long as …"/"if …" rider (routed to the companion below).
    // Decline any other trailing clause ("… untap step, then …") rather than
    // silently dropping it — parity with the sibling `try_split_and_cant_block`
    // terminal guard.
    let tail = rest.trim_start().trim_end_matches('.').trim();
    let recognized_rider = tail.is_empty()
        || alt((tag::<_, _, VE>("as long as "), tag::<_, _, VE>("if ")))
            .parse(tail)
            .is_ok();
    if !recognized_rider {
        return None;
    }

    let cut_end = before
        .trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .len();
    let line_a = format!("{}.", text[..cut_end].trim_end_matches('.'));
    let mut defs = parse_static_line_multi(&line_a);
    if defs.is_empty() {
        return None;
    }
    for def in &mut defs {
        def.description = Some(text.to_string());
    }

    let affected = defs[0].affected.clone()?;
    // CR 502.3: a trailing "as long as …"/"if …" rider on the untap clause
    // belongs on the CantUntap companion; otherwise inherit the grant's gate.
    let condition = extract_cant_untap_condition(&lower).or_else(|| defs[0].condition.clone());
    let mut companion = StaticDefinition::new(StaticMode::CantUntap)
        .affected(affected)
        .description(text.to_string());
    if let Some(condition) = condition {
        companion = companion.condition(condition);
    }
    defs.push(companion);
    Some(defs)
}

/// CR 508.1c: Decompose `"<continuous grant> and can't attack"` into the first
/// conjunct's static(s) plus a `CantAttack` static sharing the same `affected`
/// (and any `condition`).
///
/// CR 508.1c / CR 509.1b: Decompose `"<continuous grant or restriction> and
/// can't attack or block"` into the first conjunct's static(s) plus a
/// `CantAttackOrBlock` static sharing the same `affected` set (and any
/// shared condition).
///
/// Without this split the trailing combat lockout was dropped: Immovable Rod
/// ("another target permanent loses all abilities and can't attack or block")
/// and Fog on the Barrow-Downs parsed to only the leading clause, so the
/// affected creature could still attack and block — the defining lockout
/// effect was silently inert. Mirrors `try_split_and_cant_block`.
///
/// Registered before `try_split_and_cant_attack` so the combined "attack or
/// block" phrase is consumed first; the bare-attack splitter's terminal guard
/// would decline the "or block" tail anyway, but ordering is belt-and-suspenders.
pub(crate) fn try_split_and_cant_attack_or_block(text: &str) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;
    let lower = text.to_lowercase();

    let (before, _matched, rest) = nom_primitives::scan_preceded(&lower, |i: &str| {
        // Match both the ASCII and typographic U+2019 apostrophe.
        let (i, _) = alt((
            tag::<_, _, VE>("and can't attack or block"),
            tag::<_, _, VE>("and can\u{2019}t attack or block"),
        ))
        .parse(i)?;
        // Optional trailing duration phrase.
        let (i, _) = opt(alt((
            tag::<_, _, VE>(" each combat"),
            tag::<_, _, VE>(" this combat"),
            tag::<_, _, VE>(" this turn"),
        )))
        .parse(i)?;
        Ok((i, ()))
    })?;

    // Only the bare, terminal "can't attack or block" maps to CantAttackOrBlock.
    // A remaining tail is a different restriction — decline so we don't mis-split.
    if !rest.trim_start().trim_end_matches('.').trim().is_empty() {
        return None;
    }

    let cut_end = before
        .trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .len();
    let line_a = format!("{}.", text[..cut_end].trim_end_matches('.'));
    let mut defs = parse_static_line_multi(&line_a);
    if defs.is_empty() {
        return None;
    }
    for def in &mut defs {
        def.description = Some(text.to_string());
    }

    let affected = defs[0].affected.clone()?;
    let condition = defs[0].condition.clone();
    let mut companion = StaticDefinition::new(StaticMode::CantAttackOrBlock)
        .affected(affected)
        .description(text.to_string());
    if let Some(condition) = condition {
        companion = companion.condition(condition);
    }
    defs.push(companion);
    Some(defs)
}

/// Without this split the trailing attacking restriction was dropped: Cagemail
/// ("Enchanted creature gets +2/+2 and can't attack.") parsed to only the +2/+2
/// grant, so the enchanted creature could still attack — the Aura's drawback
/// vanished, making it a strictly-better-than-printed pure pump. Mirrors
/// `try_split_and_cant_block`. A terminal-phrase guard keeps this disjoint from
/// the already-handled "can't attack alone" shape and from the scoped
/// "can't attack you / planeswalkers / its owner …" restrictions, which are a
/// different `StaticMode`.
pub(crate) fn try_split_and_cant_attack(text: &str) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;

    let (before, _matched, rest) = nom_primitives::scan_preceded(text, |i: &str| {
        // Match both the ASCII and typographic U+2019 apostrophe.
        let (i, _) = alt((
            tag_no_case::<_, _, VE>("and can't attack"),
            tag_no_case::<_, _, VE>("and can\u{2019}t attack"),
        ))
        .parse(i)?;
        // Optional trailing duration phrase.
        let (i, _) = opt(alt((
            tag_no_case::<_, _, VE>(" each combat"),
            tag_no_case::<_, _, VE>(" this combat"),
            tag_no_case::<_, _, VE>(" this turn"),
        )))
        .parse(i)?;
        Ok((i, ()))
    })?;

    // CR 508.1c: only the bare, terminal "can't attack" is a plain CantAttack. A
    // remaining tail ("alone", "you or planeswalkers …", "its owner …", "unless
    // …") is a different restriction owned by another branch — decline so we
    // don't mis-split it.
    if !rest.trim_start().trim_end_matches('.').trim().is_empty() {
        return None;
    }

    let cut_end = before
        .trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .len();
    let line_a = format!("{}.", text[..cut_end].trim_end_matches('.'));
    let mut defs = parse_static_line_multi(&line_a);
    if defs.is_empty() {
        return None;
    }
    for def in &mut defs {
        def.description = Some(text.to_string());
    }

    let affected = defs[0].affected.clone()?;
    let condition = defs[0].condition.clone();
    let mut companion = StaticDefinition::new(StaticMode::CantAttack)
        .affected(affected)
        .description(text.to_string());
    if let Some(condition) = condition {
        companion = companion.condition(condition);
    }
    defs.push(companion);
    Some(defs)
}

/// CR 508.1b + CR 508.1c: Decompose `"<grant or restriction>[,] and can't
/// attack you [or planeswalkers you control]"` (the Vow cycle — Vow of
/// Lightning, Duty, Flight, Torment, Wildness) into the first conjunct's
/// static(s) plus a companion `CantAttack` static scoped to the Aura
/// controller's side of the board, sharing the same `affected` set.
///
/// Without this split the trailing attack restriction was silently dropped:
/// Vow of Lightning ("Enchanted creature gets +2/+2, has first strike, and
/// can't attack you or planeswalkers you control.") parsed to only the +2/+2
/// grant and first-strike keyword — the lockout that defines the Vow cycle
/// was completely inert and the enchanted creature could freely attack its
/// Aura's controller.
///
/// Registered before `try_split_and_cant_attack` so the more specific scoped
/// phrase is consumed first; the bare-attack splitter's terminal guard would
/// decline the " you …" tail anyway, but ordering is belt-and-suspenders.
///
/// Handles two scoped forms:
/// - `"and can't attack you"` → `CantAttack` with `defended = Player`
/// - `"and can't attack you or planeswalkers you control"` → `CantAttack`
///   with `defended = PlayerOrPlaneswalker`
pub(crate) fn try_split_and_cant_attack_scoped(text: &str) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;
    let lower = text.to_ascii_lowercase();

    let (before, defended, rest) = nom_primitives::scan_preceded(&lower, |i: &str| {
        let (i, _) = alt((
            tag::<_, _, VE>("and can't attack"),
            tag::<_, _, VE>("and can\u{2019}t attack"),
        ))
        .parse(i)?;
        let (i, defended) = parse_cant_attack_defended_scope_nom(i)?;
        let Some(defended) = defended else {
            return Err(nom::Err::Error(OracleError::new(
                i,
                nom::error::ErrorKind::Tag,
            )));
        };
        // Optional trailing duration phrase.
        let (i, _) = opt(alt((
            tag::<_, _, VE>(" each combat"),
            tag::<_, _, VE>(" this combat"),
            tag::<_, _, VE>(" this turn"),
        )))
        .parse(i)?;
        Ok((i, defended))
    })?;

    // Terminal guard: decline unless the tail is empty (punctuation only).
    if !rest.trim_start().trim_end_matches('.').trim().is_empty() {
        return None;
    }

    let cut_end = before
        .trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .len();
    let line_a = format!("{}.", text[..cut_end].trim_end_matches('.'));
    let mut defs = parse_static_line_multi(&line_a);
    if defs.is_empty() {
        return None;
    }
    for def in &mut defs {
        def.description = Some(text.to_string());
    }

    let affected = defs[0].affected.clone()?;
    let condition = defs[0].condition.clone();
    let mut companion = StaticDefinition::new(StaticMode::CantAttack)
        .affected(affected)
        .attack_defended(Some(defended))
        .description(text.to_string());
    if let Some(condition) = condition {
        companion = companion.condition(condition);
    }
    defs.push(companion);
    Some(defs)
}

/// CR 702.5 / CR 702.6: Decompose `"<grant or restriction> and can't be
/// enchanted [or equipped] [by other Auras]"` (and the "equipped" lead-in) into
/// the first conjunct's static(s) plus the matching attach-prohibition
/// static(s) — `Other("CantBeEquipped")` / `Other("CantBeEnchanted")` — sharing
/// the same `affected` set.
///
/// Without this split the trailing attach prohibition was dropped: Anti-Magic
/// Aura ("Enchanted creature can't be the target of spells and can't be
/// enchanted by other Auras.") and Consecrate Land ("Enchanted land has
/// indestructible and can't be enchanted by other Auras.") parsed to only the
/// first clause, so other Auras could still be attached — half the card
/// vanished. Mirrors `try_split_and_cant_block`; the classifier matches the
/// standalone attach-prohibition dispatch (equipped-first ordering) so a
/// compound "equipped or enchanted" yields both prohibitions.
pub(crate) fn try_split_and_cant_be_attached(text: &str) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;
    let lower = text.to_lowercase();

    let (before, _matched, _rest) = nom_primitives::scan_preceded(&lower, |i: &str| {
        // Match both the ASCII and typographic U+2019 apostrophe.
        let (i, _) = alt((
            tag::<_, _, VE>("and can't be "),
            tag::<_, _, VE>("and can\u{2019}t be "),
        ))
        .parse(i)?;
        let (i, _) = alt((tag::<_, _, VE>("enchanted"), tag::<_, _, VE>("equipped"))).parse(i)?;
        Ok((i, ()))
    })?;

    // Classify the attach prohibition(s) from the full second clause, mirroring
    // the standalone dispatch (`dispatch.rs` / `shared.rs`): "equipped" → host
    // can't be equipped (CR 702.6), "enchanted" → can't be enchanted (CR 702.5);
    // a compound "equipped or enchanted" yields both, equipped-first.
    let attach_clause = &lower[before.len()..];
    let mut modes: Vec<StaticMode> = Vec::new();
    if nom_primitives::scan_contains(attach_clause, "equipped") {
        modes.push(StaticMode::Other("CantBeEquipped".to_string()));
    }
    if nom_primitives::scan_contains(attach_clause, "enchanted") {
        modes.push(StaticMode::Other("CantBeEnchanted".to_string()));
    }
    if modes.is_empty() {
        return None;
    }

    let cut_end = before
        .trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .len();
    let line_a = format!("{}.", text[..cut_end].trim_end_matches('.'));
    let mut defs = parse_static_line_multi(&line_a);
    if defs.is_empty() {
        return None;
    }
    for def in &mut defs {
        def.description = Some(text.to_string());
    }

    let affected = defs[0].affected.clone()?;
    for mode in modes {
        defs.push(
            StaticDefinition::new(mode)
                .affected(affected.clone())
                .description(text.to_string()),
        );
    }
    Some(defs)
}

/// CR 602.5 + CR 603.2a: Decompose `"<grant or restriction> and [its] activated
/// abilities can't be activated"` into the first conjunct's static(s) plus a
/// `CantBeActivated` static. The companion's `source_filter` is the first
/// conjunct's host filter (e.g. `EnchantedBy`) — see the inline note below.
///
/// Without this split the trailing activation prohibition was dropped: Viper's
/// Kiss ("Enchanted creature gets -1/-1, and its activated abilities can't be
/// activated.") parsed to only the -1/-1 grant, so the enchanted creature's
/// activated abilities still worked. Mirrors `try_split_and_cant_block`.
/// The "can't attack/block, and activated abilities can't be activated" compound
/// (Arrest, Faith's Fetters) is handled by its own earlier branch.
pub(crate) fn try_split_and_cant_activate_abilities(text: &str) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;
    let lower = text.to_lowercase();

    let (before, _matched, _rest) = nom_primitives::scan_preceded(&lower, |i: &str| {
        // Compose the two independent axes rather than enumerating the product:
        // an optional possessive "its " and the ASCII / U+2019 apostrophe form.
        let (i, _) = tag::<_, _, VE>("and ").parse(i)?;
        let (i, _) = opt(tag::<_, _, VE>("its ")).parse(i)?;
        let (i, _) = alt((
            tag::<_, _, VE>("activated abilities can't be activated"),
            tag::<_, _, VE>("activated abilities can\u{2019}t be activated"),
        ))
        .parse(i)?;
        Ok((i, ()))
    })?;

    let cut_end = before
        .trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .len();
    let line_a = format!("{}.", text[..cut_end].trim_end_matches('.'));
    let mut defs = parse_static_line_multi(&line_a);
    if defs.is_empty() {
        return None;
    }
    for def in &mut defs {
        def.description = Some(text.to_string());
    }

    // CR 602.5 + CR 603.2a: the prohibition applies to the same subject as the
    // grant (the enchanted/equipped creature). `CantBeActivated` is a
    // data-carrying static with no layer-pipeline handler — it is NOT re-homed
    // onto the host the way `Continuous`/`GrantStaticAbility` modifications are.
    // `is_blocked_by_cant_be_activated` (game/casting.rs) matches `source_filter`
    // against the activating permanent from the static SOURCE's perspective
    // (`FilterContext::from_source(static_owner)`), ignoring `affected`. The
    // static lives on the Aura/Equipment, so `source_filter` must be the host
    // filter (e.g. `EnchantedBy`) to resolve to the enchanted/equipped creature.
    // A `SelfRef` `source_filter` would resolve to the Aura/Equipment itself and
    // silently block nothing. For a self-referential grant ("this creature gets
    // … and its activated abilities …") the first conjunct's filter is already
    // `SelfRef`, so threading it through is correct in every case.
    let affected = defs[0].affected.clone()?;
    defs.push(
        StaticDefinition::new(StaticMode::CantBeActivated {
            who: ProhibitionScope::AllPlayers,
            source_filter: affected.clone(),
            exemption: parse_cant_be_activated_exemption_in_text(&lower),
        })
        .affected(affected)
        .description(text.to_string()),
    );
    Some(defs)
}

/// CR 701.21: Decompose `"<grant or restriction> and can't be sacrificed"` into
/// the first conjunct's static(s) plus an `Other("CantBeSacrificed")` static
/// sharing the same `affected` set.
///
/// Without this split the trailing sacrifice prohibition was dropped: Assault
/// Suit ("Equipped creature gets +2/+2, has haste, can't attack you or
/// planeswalkers you control, and can't be sacrificed.") parsed without the
/// `CantBeSacrificed` static, so the equipped creature could still be
/// sacrificed — defeating the Equipment's political lock. Mirrors
/// `try_split_and_cant_block`; `CantBeSacrificed` is a `StaticMode::Other(..)`
/// host-prohibition (runtime-enforced in `game::sacrifice`), not a
/// `ContinuousModification`, so the continuous-grant default drops it.
pub(crate) fn try_split_and_cant_be_sacrificed(text: &str) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;
    let lower = text.to_lowercase();

    let (before, _matched, rest) = nom_primitives::scan_preceded(&lower, |i: &str| {
        // Match both the ASCII and typographic U+2019 apostrophe.
        alt((
            tag::<_, _, VE>("and can't be sacrificed"),
            tag::<_, _, VE>("and can\u{2019}t be sacrificed"),
        ))
        .parse(i)
    })?;

    // Only the bare, terminal "can't be sacrificed" is a plain prohibition. A
    // remaining tail ("unless …", "to …") is a qualified restriction owned by
    // another branch — decline so we don't mis-split it.
    if !rest.trim_start().trim_end_matches('.').trim().is_empty() {
        return None;
    }

    let cut_end = before
        .trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .len();
    let line_a = format!("{}.", text[..cut_end].trim_end_matches('.'));
    let mut defs = parse_static_line_multi(&line_a);
    if defs.is_empty() {
        return None;
    }
    for def in &mut defs {
        def.description = Some(text.to_string());
    }

    let affected = defs[0].affected.clone()?;
    defs.push(
        StaticDefinition::new(StaticMode::Other("CantBeSacrificed".to_string()))
            .affected(affected)
            .description(text.to_string()),
    );
    Some(defs)
}

/// CR 702.18a / CR 702.11a: Decompose `"<grant or restriction> and can't be the
/// target of …"` into the first conjunct's static(s) plus the targeting
/// restriction, sharing the same `affected` set.
///
/// Without this split the trailing targeting prohibition was dropped: Spectral
/// Shield ("Enchanted creature gets +0/+2 and can't be the target of spells.")
/// parsed to only the +0/+2 grant, so the enchanted creature could still be
/// targeted — the Aura's entire protection was lost. Mirrors
/// `try_split_and_cant_be_attached`; the descriptive "can't be the target …"
/// form is a `CantBeTargeted` `StaticMode` (or Hexproof for the opponents-only
/// scope — CR 702.11a), not a `ContinuousModification`, so the continuous-grant
/// default drops it. Scope classification reuses `classify_cant_be_targeted`,
/// matching the standalone dispatch so the "your opponents control" qualifier is
/// preserved rather than collapsed into blanket Shroud.
pub(crate) fn try_split_and_cant_be_targeted(text: &str) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;
    let lower = text.to_ascii_lowercase();

    let (before, _matched, _rest) = nom_primitives::scan_preceded(&lower, |i: &str| {
        // Match both the ASCII and typographic U+2019 apostrophe, and both the
        // "target of …" and bare "targeted" phrasings.
        alt((
            tag::<_, _, VE>("and can't be the target"),
            tag::<_, _, VE>("and can\u{2019}t be the target"),
            tag::<_, _, VE>("and can't be targeted"),
            tag::<_, _, VE>("and can\u{2019}t be targeted"),
        ))
        .parse(i)
    })?;

    // Classify the whole trailing clause exactly as the standalone dispatch does
    // (`dispatch.rs`), so "… your opponents control" → Hexproof (CR 702.11a) and
    // the unqualified form → blanket Shroud (CR 702.18a). Decline if the tail is
    // not a recognized targeting restriction.
    let targeting_clause = &lower[before.len()..];
    let scope = crate::parser::oracle_keyword::classify_cant_be_targeted(targeting_clause)?;

    let cut_end = before
        .trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .len();
    let line_a = format!("{}.", text[..cut_end].trim_end_matches('.'));
    let mut defs = parse_static_line_multi(&line_a);
    if defs.is_empty() {
        return None;
    }
    for def in &mut defs {
        def.description = Some(text.to_string());
    }

    let affected = defs[0].affected.clone()?;
    let companion = match scope {
        // CR 702.11a: "… your opponents control" grants Hexproof so the
        // permanent's own controller can still target it.
        crate::parser::oracle_keyword::CantBeTargetedScope::OpponentsOnly => {
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: crate::types::keywords::Keyword::Hexproof,
                }])
                .description(text.to_string())
        }
        // CR 702.18a: blanket — can't be targeted by any player. Enforced in
        // `targeting.rs::can_target` via the object's active static definitions.
        crate::parser::oracle_keyword::CantBeTargetedScope::AnyPlayer => {
            StaticDefinition::new(StaticMode::CantBeTargeted)
                .affected(affected)
                .description(text.to_string())
        }
    };
    defs.push(companion);
    Some(defs)
}

/// CR 509.1b: Classify a "can't be blocked …" evasion predicate (lowercased,
/// starting with "can't be blocked") into the corresponding `StaticMode` and
/// optional evasion condition, composing the same building blocks the standalone
/// branches use. Returns `None` when the tail is not a recognized evasion shape.
pub(crate) fn cant_be_blocked_mode(clause: &str) -> Option<(StaticMode, Option<StaticCondition>)> {
    type VE<'a> = OracleError<'a>;
    let clause = clause.replace('\u{2019}', "'");
    let rest = nom_tag_lower(&clause, &clause, "can't be blocked")?;
    let rest = rest.trim_end_matches('.').trim_end();
    // "except by <filter>" → CantBeBlockedExceptBy (quality or min-blockers).
    if let Some(filter) = nom_tag_lower(rest, rest, " except by ") {
        return Some((
            StaticMode::CantBeBlockedExceptBy {
                kind: classify_block_exception(filter),
            },
            None,
        ));
    }
    // "by more than N creature(s)" → per-creature blocker maximum. Must precede
    // the generic "by <filter>" branch, which would read "more than …" as a
    // quality filter.
    if let Some(after) = nom_tag_lower(rest, rest, " by more than ") {
        if let Ok((after, max)) = nom_primitives::parse_number(after) {
            if let Ok((after, _)) =
                alt((tag::<_, _, VE>(" creatures"), tag(" creature"))).parse(after)
            {
                if after.trim().is_empty() {
                    return Some((StaticMode::CantBeBlockedByMoreThan { max }, None));
                }
            }
        }
        return None;
    }
    // "by <filter>" → CantBeBlockedBy.
    if let Some(filter_text) = nom_tag_lower(rest, rest, " by ") {
        let filter_tp = TextPair::new(filter_text, filter_text);
        let (filter, remainder) = if let Some(filter) = parse_chosen_qualifier_subject(&filter_tp) {
            (filter, "")
        } else {
            parse_type_phrase(filter_text)
        };
        if !matches!(filter, TargetFilter::Any) {
            let condition = parse_compound_cant_be_blocked_condition(remainder);
            return Some((StaticMode::CantBeBlockedBy { filter }, condition));
        }
        return None;
    }
    // CR 509.1b: "can't be blocked unless it's attacking its owner [or a
    // permanent its owner controls]" — conditional evasion gated on the
    // recipient's attack target relative to its OWNER (CR 108.3). Express as
    // CantBeBlocked + Not(RecipientAttackingOwnerTarget): unblockable EXCEPT when
    // attacking owner / owner-controlled permanent. Must precede the generic
    // "as long as …" condition fallthrough so the "unless" form is classified
    // explicitly rather than mis-handled by the generic condition parser.
    if let Some(after) = nom_tag_lower(rest, rest, " unless ") {
        if let Some(target) = parse_block_unless_attacking_owner_nom(after) {
            return Some((
                StaticMode::CantBeBlocked,
                Some(StaticCondition::Not {
                    condition: Box::new(StaticCondition::RecipientAttackingOwnerTarget { target }),
                }),
            ));
        }
    }
    // Bare "can't be blocked".
    if rest.is_empty() {
        return Some((StaticMode::CantBeBlocked, None));
    }
    if let Some(condition) = parse_compound_cant_be_blocked_condition(rest) {
        return Some((StaticMode::CantBeBlocked, Some(condition)));
    }
    None
}

/// CR 509.1b + CR 506.2 + CR 108.3: classify the "unless it's attacking its
/// owner [or a permanent its owner controls]" exception following
/// "can't be blocked". Mirrors the attack-side owner-relative axis
/// (`parse_cant_attack_rule_static_predicate_nom`). The longer
/// `OwnerOrPlaneswalker` phrase is ordered before `Owner` (nom `alt` is
/// leftmost-match). `tag_no_case` handles casing; the split path reconstructs
/// the clause with an ASCII apostrophe (`try_split_and_cant_be_blocked`), so a
/// single ASCII-apostrophe arm suffices. Returns `Some` only when the combinator
/// consumes the whole tail — the parser IS the detector.
fn parse_block_unless_attacking_owner_nom(
    input: &str,
) -> Option<crate::types::triggers::AttackTargetFilter> {
    use crate::types::triggers::AttackTargetFilter;
    let (rest, target) = alt((
        value(
            AttackTargetFilter::OwnerOrPlaneswalker,
            tag_no_case::<_, _, OracleError<'_>>(
                "it's attacking its owner or a permanent its owner controls",
            ),
        ),
        value(
            AttackTargetFilter::Owner,
            tag_no_case::<_, _, OracleError<'_>>("it's attacking its owner"),
        ),
    ))
    .parse(input)
    .ok()?;
    rest.trim().is_empty().then_some(target)
}

/// CR 509.1b: Attach a trailing "as long as …" condition to the evasion
/// restriction produced by the compound split.
fn parse_compound_cant_be_blocked_condition(text: &str) -> Option<StaticCondition> {
    let condition_text = text.trim().trim_end_matches('.');
    if condition_text.is_empty() {
        return None;
    }
    nom_condition::parse_condition(condition_text)
        .ok()
        .and_then(|(rest, condition)| rest.trim().is_empty().then_some(condition))
}

/// CR 509.1b: Decompose `"<predicate> and can't be blocked[ by/except by … | by
/// more than N creatures]"` into the first conjunct's static(s) plus the
/// matching `CantBeBlocked*` static, both sharing the same `affected` set.
///
/// Without this split the trailing evasion clause was dropped: Madcap Skills
/// ("Enchanted creature gets +3/+0 and can't be blocked by more than one
/// creature.") parsed to only the +3/+0 grant. Mirrors
/// `try_split_and_can_block_additional`. Standalone "can't be blocked …" lines
/// (no preceding "and") are handled by the existing branches, so this requires
/// the conjunction.
pub(crate) fn try_split_and_cant_be_blocked(text: &str) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;
    let lower = text.to_lowercase();

    // Match both the ASCII apostrophe and the typographic U+2019 form, mirroring
    // the standalone evasion branches (`shared.rs` / the dispatch path); the
    // static parse path does not universally normalize apostrophes. The matched
    // tail (`rest`) carries no apostrophe, so the clause is reconstructed with an
    // ASCII "can't be blocked" and `cant_be_blocked_mode` needs no apostrophe arm.
    let (before, _matched, rest) = nom_primitives::scan_preceded(&lower, |i: &str| {
        alt((
            tag::<_, _, VE>("and can't be blocked"),
            tag::<_, _, VE>("and can\u{2019}t be blocked"),
        ))
        .parse(i)
    })?;
    let clause = format!("can't be blocked{rest}");
    let (mode, evasion_condition) = cant_be_blocked_mode(&clause)?;

    let cut_end = before
        .trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .len();
    let line_a = format!("{}.", text[..cut_end].trim_end_matches('.'));
    let mut defs = parse_static_line_multi(&line_a);
    if defs.is_empty() {
        return None;
    }
    for def in &mut defs {
        def.description = Some(text.to_string());
    }

    let affected = defs[0].affected.clone()?;
    let condition = evasion_condition.or_else(|| defs[0].condition.clone());
    let mut companion = StaticDefinition::new(mode)
        .affected(affected)
        .description(text.to_string());
    if let Some(condition) = condition {
        companion = companion.condition(condition);
    }
    defs.push(companion);
    Some(defs)
}

/// CR 105.2c / CR 205.4a: Parse property-based creature descriptors that are not subtypes.
/// Handles "colorless", "multicolored", "snow", and "snow and [Subtype]" patterns.
/// Returns a fully constructed `TargetFilter` with the appropriate properties.
pub(crate) fn parse_property_descriptor(
    desc_lower: &str,
    desc_remaining: &str,
    extra_props: &[FilterProp],
    is_other: bool,
) -> Option<TargetFilter> {
    let mut props = extra_props.to_vec();
    if is_other {
        props.push(FilterProp::Another);
    }

    // CR 105.2c: "colorless creatures" — zero colors
    if desc_lower == "colorless" {
        props.push(FilterProp::ColorCount {
            comparator: Comparator::EQ,
            count: 0,
        });
        return Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(props),
        ));
    }

    // CR 105.2a: "monocolored creatures" — exactly one color
    if desc_lower == "monocolored" {
        props.push(FilterProp::ColorCount {
            comparator: Comparator::EQ,
            count: 1,
        });
        return Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(props),
        ));
    }

    // CR 105.2: "multicolored creatures" — two or more colors
    if desc_lower == "multicolored" {
        props.push(FilterProp::ColorCount {
            comparator: Comparator::GE,
            count: 2,
        });
        return Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(props),
        ));
    }

    // CR 205.4a: "snow and [Subtype]" — supertype + subtype compound
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    if let Some(rest) = desc_lower.strip_prefix("snow and ") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        props.push(FilterProp::HasSupertype {
            value: Supertype::Snow,
        });
        // Remainder should be a capitalized subtype word
        let subtype_part = &desc_remaining[desc_remaining.len() - rest.len()..];
        if is_capitalized_words(subtype_part) {
            return Some(TargetFilter::Typed(
                typed_filter_for_subtype(subtype_part)
                    .controller(ControllerRef::You)
                    .properties(props),
            ));
        }
    }

    // CR 205.4a: "snow creatures" — just the supertype
    if desc_lower == "snow" {
        props.push(FilterProp::HasSupertype {
            value: Supertype::Snow,
        });
        return Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(props),
        ));
    }

    None
}

/// CR 205.3m: Try to parse a compound subtype descriptor like "Ninja and Rogue" or "Elf or Warrior"
/// into an `Or` filter with one creature+subtype+controller per part.
/// Returns `None` if the descriptor is not a compound subtype pattern.
pub(crate) fn try_parse_compound_subtypes(
    descriptor: &str,
    extra_props: &[FilterProp],
    is_other: bool,
) -> Option<TargetFilter> {
    let (left, right) = descriptor
        .split_once(" and ") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        .or_else(|| descriptor.split_once(" or "))?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    let left_trimmed = left.trim();
    let right_trimmed = right.trim();
    if !is_capitalized_words(left_trimmed) || !is_capitalized_words(right_trimmed) {
        return None;
    }
    let left_sub = parse_subtype(left_trimmed)
        .map(|(c, _)| c)
        .unwrap_or_else(|| left_trimmed.to_string());
    let right_sub = parse_subtype(right_trimmed)
        .map(|(c, _)| c)
        .unwrap_or_else(|| right_trimmed.to_string());
    // Inject extra_props and Another into each inner filter at construction time,
    // because add_property does not recurse into TargetFilter::Or.
    let mut all_props = extra_props.to_vec();
    if is_other {
        all_props.push(FilterProp::Another);
    }
    let filters = vec![
        TargetFilter::Typed(
            typed_filter_for_subtype(&left_sub)
                .controller(ControllerRef::You)
                .properties(all_props.clone()),
        ),
        TargetFilter::Typed(
            typed_filter_for_subtype(&right_sub)
                .controller(ControllerRef::You)
                .properties(all_props),
        ),
    ];
    Some(TargetFilter::Or { filters })
}

/// CR 510.1a + CR 613.11: The "assign[s] combat damage equal to <poss> toughness
/// rather than <poss> power" predicate. CR 510.1a is the default ("assigns combat
/// damage equal to its power"); this is a continuous rule-modification effect
/// (CR 613.11) that substitutes toughness for power. All surface forms map to the
/// same [`ContinuousModification::AssignDamageFromToughness`] rule.
///
/// The phrase is decoupled into two independent axes rather than enumerated as
/// whole-string permutations:
///   * verb axis — `alt(("assigns", "assign"))`: the intact static form keeps the
///     inflected "assigns", while the one-shot EFFECT pipeline deconjugates the
///     verb via `normalize_verb_token` ("assigns" → "assign") *before* reaching
///     this predicate, so the deconjugated-singular form "assign … its … its"
///     must be accepted alongside the intact static forms.
///   * possessive-agreement axis — `alt(("its … its", "their … their"))`: singular
///     ("each creature … its …") vs plural ("creatures you control … their …").
///
/// Because the axes are composed independently, all four combinations parse,
/// which is a strict superset of the two intact static forms this previously
/// accepted. Centralizing the phrase here keeps the static-line parser and the
/// one-shot continuous-effect parser (`parse_continuous_modifications`) in
/// lockstep so a new subject scope never silently drops a surface form. Returns
/// the post-phrase remainder.
pub(crate) fn parse_assigns_damage_from_toughness_predicate(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            alt((tag("assigns"), tag("assign"))),
            tag(" combat damage equal to "),
            alt((
                tag("its toughness rather than its power"),
                tag("their toughness rather than their power"),
            )),
        ),
    )
    .parse(input)
}

/// CR 510.1c: Parse "each creature [you control] [with condition] assigns combat damage
/// equal to its toughness rather than its power" patterns.
///
/// Supports Oracle patterns:
/// - "each creature you control assigns combat damage equal to its toughness..."
/// - "each creature you control with defender assigns combat damage equal to its toughness..."
/// - "each creature you control with toughness greater than its power assigns combat damage..."
/// - "each creature assigns combat damage equal to its toughness..." (global, no controller)
/// - "this creature assigns combat damage equal to its toughness..." (self-referential)
pub(crate) fn parse_assigns_damage_from_toughness(
    lower: &str,
    text: &str,
) -> Option<StaticDefinition> {
    let suffix = "assigns combat damage equal to its toughness rather than its power";
    let suffix_alt = "assign combat damage equal to their toughness rather than their power";

    // CR 510.1c: Self-referential variant — "This creature assigns..." or
    // the canonical "~ assigns..." form (post-self-noun normalization).
    if let Some(rest) =
        nom_tag_lower(lower, lower, "this creature ").or_else(|| nom_tag_lower(lower, lower, "~ "))
    {
        let cleaned = rest.trim_end_matches('.').trim();
        if nom_tag_lower(cleaned, cleaned, suffix).is_some_and(|r| r.is_empty()) {
            return Some(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::AssignDamageFromToughness])
                    .description(text.to_string()),
            );
        }
        return None;
    }

    // Determine controller scope: "each creature you control " vs "each creature "
    let (rest, has_controller) =
        if let Some(r) = nom_tag_lower(lower, lower, "each creature you control ") {
            (r, true)
        } else {
            let r = nom_tag_lower(lower, lower, "each creature ")?;
            (r, false)
        };

    let (condition_text, _) =
        if let Ok((_, (before, _))) = nom_primitives::split_once_on(rest, suffix) {
            (before, "")
        } else if let Ok((_, (before, _))) = nom_primitives::split_once_on(rest, suffix_alt) {
            (before, "")
        } else {
            return None;
        };

    let condition_text = condition_text.trim();

    let mut filter = if has_controller {
        TypedFilter::creature().controller(ControllerRef::You)
    } else {
        TypedFilter::creature()
    };

    if !condition_text.is_empty() {
        // Parse "with [condition]" clause
        let with_clause = nom_tag_lower(condition_text, condition_text, "with ")?;
        let with_clause = with_clause.trim();

        if with_clause == "toughness greater than its power" {
            filter = filter.properties(vec![FilterProp::ToughnessGTPower]);
        } else {
            // Treat as keyword condition: "with defender", "with flying", etc.
            let keyword: Keyword = with_clause.parse().ok()?;
            filter = filter.properties(vec![FilterProp::WithKeyword { value: keyword }]);
        }
    }

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(filter))
            .modifications(vec![ContinuousModification::AssignDamageFromToughness])
            .description(text.to_string()),
    )
}

pub(crate) fn parse_attached_assigns_damage_from_toughness(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;

    #[derive(Clone, Copy)]
    enum AttachedSubject {
        Enchanted,
        Equipped,
    }

    let lower = tp.lower.trim_end_matches('.');
    let (rest, subject) = preceded(
        tag::<_, _, VE<'_>>("as long as "),
        alt((
            value(AttachedSubject::Enchanted, tag("enchanted creature")),
            value(AttachedSubject::Equipped, tag("equipped creature")),
        )),
    )
    .parse(lower)
    .ok()?;

    let (rest, condition_prop) = if let Ok((rest, _)) =
        tag::<_, _, VE<'_>>("'s toughness is greater than its power").parse(rest)
    {
        (rest, FilterProp::ToughnessGTPower)
    } else {
        let (after_has, _) = tag::<_, _, VE<'_>>(" has ").parse(rest).ok()?;
        let (rest, keyword_text) = take_until::<_, _, VE<'_>>(", it assigns")
            .parse(after_has)
            .ok()?;
        let keyword = map_keyword(keyword_text.trim())?;
        (rest, FilterProp::WithKeyword { value: keyword })
    };
    let (rest, _) = tag::<_, _, VE<'_>>(
        ", it assigns combat damage equal to its toughness rather than its power",
    )
    .parse(rest)
    .ok()?;
    if !rest.is_empty() {
        return None;
    }

    let attachment_prop = match subject {
        AttachedSubject::Enchanted => FilterProp::EnchantedBy,
        AttachedSubject::Equipped => FilterProp::EquippedBy,
    };

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![attachment_prop, condition_prop]),
            ))
            .modifications(vec![ContinuousModification::AssignDamageFromToughness])
            .description(text.to_string()),
    )
}

/// Possessive pronoun for any character's combat damage ("its"/"his"/"her"/"their").
/// Widens the neuter-only assumption so gendered-character cards (e.g. Wolverine)
/// parse the same combat-damage-assignment static as neuter creatures.
fn parse_possessive_pronoun(input: &str) -> OracleResult<'_, &str> {
    alt((tag("its"), tag("his"), tag("her"), tag("their"))).parse(input)
}

/// Nominative pronoun for any character ("it"/"he"/"she"/"they").
fn parse_nominative_pronoun(input: &str) -> OracleResult<'_, &str> {
    alt((tag("it"), tag("he"), tag("she"), tag("they"))).parse(input)
}

/// CR 510.1c: Parse "you may have this creature assign its combat damage as though it
/// weren't blocked" self-referential static. Accepts gendered pronouns
/// (his/her/he/she/they) so named characters parse the same as neuter creatures.
pub(crate) fn parse_assign_damage_as_though_unblocked(
    lower: &str,
    text: &str,
) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;

    let clean = lower.trim_end_matches('.');
    let result = preceded(
        tag::<_, _, VE<'_>>("you may have "),
        alt((tag("this creature"), tag("~"), tag("it"))),
    )
    .parse(clean)
    .ok()?;
    let (rest, _) = result;
    let (rest, _) = tag::<_, _, VE<'_>>(" assign ").parse(rest).ok()?;
    let (rest, _) = parse_possessive_pronoun(rest).ok()?;
    let (rest, _) = tag::<_, _, VE<'_>>(" combat damage as though ")
        .parse(rest)
        .ok()?;
    let (rest, _) = parse_nominative_pronoun(rest).ok()?;
    let (rest, _) = tag::<_, _, VE<'_>>(" weren't blocked").parse(rest).ok()?;
    if !rest.is_empty() {
        return None;
    }

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AssignDamageAsThoughUnblocked])
            .description(text.to_string()),
    )
}

/// CR 510.1c: Parse attached-creature controller wording:
/// - "Enchanted creature's controller may have it assign its combat damage as though it weren't blocked."
/// - "Equipped creature's controller may have it assign its combat damage as though it weren't blocked."
pub(crate) fn parse_attached_creature_assign_damage_as_though_unblocked(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;

    let clean = TextPair::new(
        tp.original.trim_end_matches('.'),
        tp.lower.trim_end_matches('.'),
    );
    let (rest, affected) = if let Some(rest) = nom_tag_tp(&clean, "enchanted creature") {
        (
            rest,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy])),
        )
    } else {
        let rest = nom_tag_tp(&clean, "equipped creature")?;
        (
            rest,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy])),
        )
    };

    let (after, _) = tag::<_, _, VE<'_>>("'s controller may have ")
        .parse(rest.lower)
        .ok()?;
    let (after, _) = parse_nominative_pronoun(after).ok()?;
    let (after, _) = tag::<_, _, VE<'_>>(" assign ").parse(after).ok()?;
    let (after, _) = parse_possessive_pronoun(after).ok()?;
    let (after, _) = tag::<_, _, VE<'_>>(" combat damage as though ")
        .parse(after)
        .ok()?;
    let (after, _) = parse_nominative_pronoun(after).ok()?;
    let (_, _) = tag::<_, _, VE<'_>>(" weren't blocked").parse(after).ok()?;

    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::AssignDamageAsThoughUnblocked])
            .description(text.to_string()),
    )
}

pub(crate) fn parse_subject_rule_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let (affected, predicate_text) = strip_rule_static_subject(tp.original, tp.lower)?;

    // CR 509.1b: Evasion ability — "<self/typed subject> can't be blocked except by
    // <filter>" is a static ability restricting blockers; must land as a top-level
    // continuous static (CR 604.1), not a spell-resolution GenericEffect. Reuses
    // classify_block_exception for the count-vs-quality BlockExceptionKind. Handled
    // here before the generic predicate parse so it cannot fall through to
    // dispatch_line_nom. The dispatch.rs CantBeBlockedExceptBy arm is guarded
    // `!except by`, so the two paths are disjoint.
    let pred_lower = predicate_text.to_lowercase();
    if let Some(rest) = nom_tag_lower(predicate_text, &pred_lower, "can't be blocked except by ")
        .or_else(|| {
            nom_tag_lower(
                predicate_text,
                &pred_lower,
                "can\u{2019}t be blocked except by ",
            )
        })
    {
        let def = StaticDefinition::new(StaticMode::CantBeBlockedExceptBy {
            kind: classify_block_exception(rest),
        })
        .affected(affected.clone())
        .description(text.to_string());
        // A "can't be blocked except by <filter>" predicate never carries a
        // trailing granted-keyword companion (the " has "/" gains " needles
        // can't appear in it), so the evasion static is complete on its own.
        return Some(def);
    }

    // CR 509.1b: "<subject> can't be blocked [by filter / unless / as long as …]"
    // (Tetsuko Umezawa, Fugitive). Reuses cant_be_blocked_mode for tail classification.
    // `strip_rule_static_subject` already matched the bare evasion marker.
    if !nom_primitives::scan_contains(&pred_lower, "except by") {
        let clause = pred_lower.trim().trim_end_matches('.');
        if let Some((mode, condition)) = cant_be_blocked_mode(clause) {
            let mut def = StaticDefinition::new(mode)
                .affected(affected.clone())
                .description(text.to_string());
            if let Some(c) = condition {
                def.condition = Some(c);
            }
            return Some(def);
        }
    }

    // CR 604.1 + CR 508.1d: a trailing "unless you control <X>" clause makes a
    // rule-static (e.g. "attacks each combat if able") conditional — the
    // requirement/restriction applies only while the controller does NOT control
    // <X>. Class: Reckless Cohort ("…unless you control another Ally"), Marauding
    // Maulhorn, and any rule-static with the same "unless you control" rider.
    // Strip the clause, classify the base predicate, and attach the negated
    // control presence via the shared `parse_control_conditions` building block.
    let pred_tp = TextPair::new(predicate_text, &pred_lower);
    if let Some((base, unless)) = pred_tp.split_around(" unless ") {
        if let Ok(("", control)) = crate::parser::oracle_nom::condition::parse_control_conditions(
            unless.lower.trim_end_matches('.'),
        ) {
            let predicate = parse_rule_static_predicate(base.original)?;
            let mut def = lower_rule_static(predicate, affected, text);
            def.condition = Some(StaticCondition::Not {
                condition: Box::new(control),
            });
            return Some(def);
        }
    }

    if let Ok((rest, (predicate, defended))) =
        parse_combat_rule_static_predicate_with_defended_nom(predicate_text)
    {
        if rest.trim().is_empty() {
            return Some(lower_rule_static(predicate, affected, text).attack_defended(defended));
        }
    }

    let predicate = parse_rule_static_predicate(predicate_text)?;
    // CR 502.3: Extract trailing condition for CantUntap statics (e.g., "as long as [condition]")
    if matches!(predicate, RuleStaticPredicate::CantUntap) {
        let pred_lower = predicate_text.to_lowercase();
        if let Some(condition) = extract_cant_untap_condition(&pred_lower) {
            let mut def = lower_rule_static(predicate, affected, text);
            def.condition = Some(condition);
            return Some(def);
        }
    }
    Some(lower_rule_static(predicate, affected, text))
}

/// CR 509.1b + CR 609.4 + CR 702.14c + CR 702.14d:
/// "Creatures with <X>walk can be blocked as though they didn't have <X>walk."
/// Both qualifier tokens MUST agree (printed cards always reference the same
/// qualifier; cross-qualifier sentences are guarded out per CR 702.14d).
///
/// Class: the Portal/Legends "creatures with Xwalk can be blocked as though
/// they didn't have Xwalk" cycle (Ur-Drago and four siblings — one per basic
/// land subtype). Produces a `StaticMode::IgnoreLandwalkForBlocking` global
/// rule-modification static.
pub(crate) fn try_parse_ignore_landwalk_for_blocking(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    let ((q1, q2), rest) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, _) = tag::<_, _, OracleError<'_>>("creatures with ").parse(i)?;
        let (i, q1) = parse_basic_landwalk_qualifier(i)?;
        let (i, _) = tag(" can be blocked as though they didn't have ").parse(i)?;
        let (i, q2) = parse_basic_landwalk_qualifier(i)?;
        let (i, _) = opt(tag(".")).parse(i)?;
        Ok((i, (q1, q2)))
    })?;
    if !rest.trim().is_empty() {
        return None;
    }
    // CR 702.14d: qualifiers don't cancel cross-type. Printed cards always
    // reference the same qualifier on both sides; guard against false matches.
    if q1 != q2 {
        return None;
    }
    Some(
        StaticDefinition::new(StaticMode::IgnoreLandwalkForBlocking {
            qualifier: Some(q1.to_string()),
        })
        .description(text.to_string()),
    )
}

/// CR 508.1d + CR 508.1h + CR 509.1c + CR 118.12a: Parse the combat-tax static family:
///
/// - "Creatures can't attack [you | you or planeswalkers you control] unless their
///   controller pays {N} [for each of those creatures][, where X is the number of
///   <filter>][.]"
/// - "Creatures can't block unless their controller pays {N} [for each of those
///   creatures]."
///
/// Nom-driven: every detection and dispatch step is a typed combinator, no
/// `contains()`/`starts_with()` substring heuristics. Produces a
/// `StaticDefinition` with the typed `UnlessPayScaling` variant matching the
/// Oracle text's scaling hint.
///
/// Returns `None` if the text does not match this family. Callers fall through
/// to the general "~ can't attack/block" handlers below.
pub(crate) fn parse_combat_tax_static(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    // Run on the ORIGINAL-case text so `{X}` mana shards and `X` in the dynamic
    // clause are preserved for nom's `parse_mana_cost` (which is case-sensitive
    // on X). All structural tags use `tag_no_case` to remain robust to
    // capitalization at the start of the line.
    let original = tp.original.trim_end_matches('.');
    let (rest, outcome) = parse_combat_tax_body(original).ok()?;
    if !rest.is_empty() {
        return None;
    }
    let CombatTaxParse {
        mode,
        affected,
        base_cost,
        scaling,
        defended,
    } = outcome;
    let mut def = StaticDefinition::new(mode)
        .affected(affected)
        .description(text.to_string());
    def.condition = Some(StaticCondition::UnlessPay {
        cost: base_cost,
        scaling,
        defended,
    });
    Some(def)
}

pub(crate) fn parse_subject_combat_rule_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let (subject_lower, (predicate, defended), rest) = nom_primitives::scan_preceded(
        &lower,
        parse_combat_rule_static_predicate_with_defended_nom,
    )?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(rest).ok()?;
    let subject = text[..subject_lower.len()].trim();
    let affected = parse_rule_static_subject_filter(subject)?;
    let mut def = lower_rule_static(predicate, affected, text).attack_defended(defended);
    let trailing = rest.trim();
    if trailing.is_empty() {
        return Some(def);
    }
    if let Some(unless_cond) = {
        let tp = TextPair::new(text, &lower);
        super::shared::parse_unless_static_condition(&tp)
    } {
        def.condition = Some(unless_cond);
        return Some(def);
    }
    None
}

/// CR 702.122a / 702.171a / 702.184c: nom parser for the crew/saddle/station
/// power-contribution modifier predicate. Composes the named action-list prefix
/// (which records the affected keyword actions) with the modifier tail.
fn parse_crew_contribution_predicate_nom(
    input: &str,
) -> OracleResult<'_, (CrewContributionKind, Vec<CrewAction>)> {
    let (input, actions) = alt((
        value(
            vec![CrewAction::Saddle, CrewAction::Crew],
            tag::<_, _, OracleError<'_>>("saddles mounts and crews vehicles"),
        ),
        value(
            vec![CrewAction::Crew, CrewAction::Station],
            tag("crews vehicles and stations permanents"),
        ),
        value(vec![CrewAction::Crew], tag("crews vehicles")),
        // CR 702.184a: bare "stations permanents" — station-only contribution
        // modifier (Tapestry Warden: "… stations permanents using its toughness
        // rather than its power").
        value(vec![CrewAction::Station], tag("stations permanents")),
    ))
    .parse(input)?;
    let (input, _) = space1.parse(input)?;
    let (input, kind) = alt((
        map(
            (
                tag::<_, _, OracleError<'_>>("as though its power were "),
                nom_primitives::parse_number,
                tag(" greater"),
            ),
            |(_, n, _)| CrewContributionKind::PowerDelta { delta: n as i32 },
        ),
        value(
            CrewContributionKind::ToughnessInsteadOfPower,
            tag("using its toughness rather than its power"),
        ),
    ))
    .parse(input)?;
    Ok((input, (kind, actions)))
}

/// CR 702.122a / 702.171a / 702.184c: "<subject> crews Vehicles [/ saddles
/// Mounts / stations permanents] as though its power were N greater" or "…
/// using its toughness rather than its power" — a continuous static that
/// modifies the creature's contributed power when paying a crew/saddle/station
/// cost (Reckoner Bankbuster, the "Roads" cycle, Giant Ox, Stoic Star-Captain).
pub(crate) fn parse_crew_contribution_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let (subject_lower, (kind, actions), rest) =
        nom_primitives::scan_preceded(&lower, parse_crew_contribution_predicate_nom)?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    let subject = text[..subject_lower.len()].trim();
    let affected = parse_rule_static_subject_filter(subject)?;
    let mode = StaticMode::CrewContribution { kind, actions };
    // CR 613.1: a self-referential modifier lives directly on the creature's own
    // `static_definitions` (read by `active_static_definitions`). A modifier
    // granted to a group ("Each creature you control crews … as though its power
    // were 2 greater", Stoic Star-Captain) must be propagated onto each affected
    // creature via `AddStaticMode` so the same lookup observes it — mirroring how
    // a granted `CantCrew` propagates.
    let def = if matches!(affected, TargetFilter::SelfRef) {
        StaticDefinition::new(mode).affected(affected)
    } else {
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::AddStaticMode { mode }])
    };
    Some(def.description(text.to_string()))
}

/// Nom 8.0 parser for the combat-tax body.
///
/// Grammar (case-insensitive):
///   body      := subject restriction scope? " unless " payer mana_cost suffix?
///   subject   := color? "creatures " | "enchanted creature "
///              | "each creature with one or more counters on it " | "~ "
///   color     := ("non")? ("white"|"blue"|"black"|"red"|"green")
///   restriction := "can't attack" | "can't block" | "can't attack or block"
///   scope     := " you" | " you or planeswalkers you control"
///   payer     := "their controller pays " | "its controller pays " | "you pay "
///   suffix    := " for each ..." dynamic_x?
///   dynamic_x := ", where x is the number of " <filter-phrase>
pub(crate) fn parse_combat_tax_body(input: &str) -> OracleResult<'_, CombatTaxParse> {
    use crate::parser::oracle_nom::error::OracleError;
    use crate::types::ability::UnlessPayScaling;

    // Subject: "[color] creatures " (opponents' creatures — the prison family,
    // optionally narrowed by a color predicate), "enchanted creature " (aura
    // form — Brainwash), "each creature with one or more counters on it "
    // (counter-gated form — Nils, Discipline Enforcer), or "~ " (self-referential
    // tax — Myr Prototype, Phyrexian Marauder). Each subject type drives the
    // affected-filter shape independently.
    //
    // Order matters: the counter-gated form must be tried before the bare
    // "creatures " tag because the counter phrasing starts with "each" rather
    // than "creatures" and so does not conflict with the primary alt branch;
    // it is listed first for clarity of grammar.
    let (input, subject) = alt((
        value(
            CombatTaxSubject::EachCreatureWithCounters,
            tag_no_case::<_, _, OracleError<'_>>("each creature with one or more counters on it "),
        ),
        // CR 105.2: optional leading color predicate composed as a
        // single axis before the bare "creatures " tag — "Nonblack creatures"
        // (Elephant Grass) → NotColor, "Red creatures" → HasColor.
        map(
            (
                opt((
                    alt((
                        map(
                            preceded(
                                tag_no_case::<_, _, OracleError<'_>>("non"),
                                nom_primitives::parse_color,
                            ),
                            |color| FilterProp::NotColor { color },
                        ),
                        map(nom_primitives::parse_color, |color| FilterProp::HasColor {
                            color,
                        }),
                    )),
                    space1,
                )),
                tag_no_case::<_, _, OracleError<'_>>("creatures "),
            ),
            |(color, _)| CombatTaxSubject::Creatures(color.map(|(prop, _)| prop)),
        ),
        value(
            CombatTaxSubject::EnchantedCreature,
            tag_no_case::<_, _, OracleError<'_>>("enchanted creature "),
        ),
        // CR 508.1d / CR 509.1c: self-referential combat tax — "~ can't attack
        // [or block] unless you pay ..." (Myr Prototype, Phyrexian Marauder).
        value(
            CombatTaxSubject::SourcePermanent,
            tag::<_, _, OracleError<'_>>("~ "),
        ),
    ))
    .parse(input)?;

    let (input, mode) = alt((
        value(
            StaticMode::CantAttackOrBlock,
            tag_no_case::<_, _, OracleError<'_>>("can't attack or block"),
        ),
        value(
            StaticMode::CantAttack,
            tag_no_case::<_, _, OracleError<'_>>("can't attack"),
        ),
        value(
            StaticMode::CantBlock,
            tag_no_case::<_, _, OracleError<'_>>("can't block"),
        ),
    ))
    .parse(input)?;

    // CR 506.3 + CR 508.1d: Optional attack-target scope captured as typed
    // `AttackTargetFilter` so the runtime can filter taxed attackers by their
    // declared `AttackTarget`. Block-side restrictions have no defender scope
    // (the defender is implicit), so `defended` stays `None` for `CantBlock`.
    // Order matters: " you or planeswalkers you control" must precede " you"
    // so the longer phrase wins (nom `alt` is leftmost-match).
    use crate::types::triggers::AttackTargetFilter;
    let (input, defended) = opt(alt((
        // CR 508.1c + CR 310.5: " you or permanents you control" also defends
        // battles — a distinct filter from the planeswalker-only phrase.
        value(
            AttackTargetFilter::PlayerOrPermanents,
            tag_no_case::<_, _, OracleError<'_>>(" you or permanents you control"),
        ),
        value(
            AttackTargetFilter::PlayerOrPlaneswalker,
            tag_no_case::<_, _, OracleError<'_>>(" you or planeswalkers you control"),
        ),
        value(
            AttackTargetFilter::Player,
            tag_no_case::<_, _, OracleError<'_>>(" you"),
        ),
    )))
    .parse(input)?;

    let (input, _) = tag_no_case::<_, _, OracleError<'_>>(" unless ").parse(input)?;
    let (input, _) = alt((
        tag_no_case::<_, _, OracleError<'_>>("their controller pays "),
        tag_no_case::<_, _, OracleError<'_>>("its controller pays "),
        // CR 508.1d / CR 509.1c: "~ can't attack unless you pay ..." — the
        // source permanent's controller is the payer (Myr Prototype).
        tag_no_case::<_, _, OracleError<'_>>("you pay "),
    ))
    .parse(input)?;

    let (input, base_cost) = nom_primitives::parse_mana_cost(input)?;

    // Optional "for each ..." tail → PerAffectedCreature scaling. Attested
    // phrasings in the live catalog:
    //   - " for each of those creatures" (Sphere of Safety, Archangel of Tithes)
    //   - " for each creature they control that's attacking you" (Ghostly Prison,
    //     Propaganda, Windborn Muse, Baird). This phrasing further filters the
    //     tax to "attacking-you" creatures — already implicit in the affected
    //     filter for the attack side.
    let (input, per_affected) = opt(alt((
        tag_no_case::<_, _, OracleError<'_>>(" for each of those creatures"),
        tag_no_case::<_, _, OracleError<'_>>(
            " for each creature they control that's attacking you or a planeswalker you control",
        ),
        tag_no_case::<_, _, OracleError<'_>>(
            " for each creature they control that's attacking you",
        ),
        tag_no_case::<_, _, OracleError<'_>>(" for each attacking creature they control"),
    )))
    .parse(input)?;

    // Optional ", where X is the number of <filter>" — only valid when the base
    // cost carried an {X} shard. Used by Sphere of Safety.
    let (input, dynamic_qty) = opt(parse_dynamic_x_clause).parse(input)?;
    let (input, for_each_qty) = if per_affected.is_none() {
        opt(parse_for_each_cost_quantity).parse(input)?
    } else {
        (input, None)
    };
    let dynamic_qty = dynamic_qty.or(for_each_qty);

    // Subject-driven affected filter:
    //   - `Creatures` (Ghostly Prison family): opponents' creatures. `ControllerRef::Opponent`
    //     resolves against the static's controller (the player benefiting from the tax).
    //   - `EnchantedCreature` (Brainwash): the attached-to creature — property `EnchantedBy`
    //     matches the aura's enchant target at runtime.
    //   - `EachCreatureWithCounters` (Nils): any creature carrying one or more counters of
    //     any type (CR 122.1). Note that the Nils static applies to creatures controlled by
    //     any player, not just opponents — the official ruling confirms "Your opponents can
    //     choose not to pay..." implying the static targets opponents in practice, but the
    //     rules text is controller-agnostic ("Each creature with one or more counters...").
    let affected = match subject {
        // CR 105.2: opponents' creatures, optionally narrowed by a
        // color predicate ("Nonblack creatures" → NotColor, etc.).
        CombatTaxSubject::Creatures(color_prop) => TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: Some(ControllerRef::Opponent),
            properties: color_prop.into_iter().collect(),
        }),
        CombatTaxSubject::EnchantedCreature => TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![FilterProp::EnchantedBy],
        }),
        CombatTaxSubject::EachCreatureWithCounters => TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![FilterProp::Counters {
                counters: CounterMatch::Any,
                comparator: Comparator::GE,
                count: QuantityExpr::Fixed { value: 1 },
            }],
        }),
        // CR 508.1d / CR 509.1c: the source permanent itself (Myr Prototype).
        CombatTaxSubject::SourcePermanent => TargetFilter::SelfRef,
    };

    // CR 118.12a: Scaling selection.
    //   - `PerAffectedWithRef`: dynamic quantity (currently only `AnyCountersOnTarget`)
    //     that must be resolved PER affected creature using that creature as the target
    //     (Nils, Discipline Enforcer — "pays {X}, where X is the number of counters on
    //     that creature"). Detected by the typed QuantityRef.
    //   - Otherwise falls through to the canonical (per_affected, dynamic_qty) lattice.
    let scaling = match (per_affected.is_some(), dynamic_qty) {
        (
            _,
            Some(QuantityRef::CountersOn {
                scope: ObjectScope::Target,
                counter_type: None,
            }),
        ) => UnlessPayScaling::PerAffectedWithRef {
            quantity: QuantityRef::CountersOn {
                scope: ObjectScope::Target,
                counter_type: None,
            },
        },
        (true, Some(qty)) => UnlessPayScaling::PerAffectedAndQuantityRef { quantity: qty },
        (true, None) => UnlessPayScaling::PerAffectedCreature,
        (false, Some(qty)) => UnlessPayScaling::PerQuantityRef { quantity: qty },
        (false, None) => UnlessPayScaling::Flat,
    };

    // CR 509.1c: Block-side taxes never carry a defender scope (the "defender"
    // for a CantBlock restriction is implicit — it's the static's controller
    // who is being attacked, but the restriction governs blockers). Drop any
    // scope that snuck in to keep the AST faithful to the rules.
    let defended = match mode {
        StaticMode::CantBlock => None,
        _ => defended,
    };

    Ok((
        input,
        CombatTaxParse {
            mode,
            affected,
            base_cost,
            scaling,
            defended,
        },
    ))
}

/// CR 702.3b + CR 611.3a: parse "<subject> can attack as though <pronoun>
/// didn't have defender [as long as <condition>]" into a StaticMode::
/// CanAttackWithDefender on `affected` with an optional condition.
///
/// Uses `scan_split_at_phrase(tag("can attack as though"))` to locate the
/// phrase at a word boundary (unlike the old ` can attack` form which
/// required a leading space and silently failed when the subject was `~`).
/// Fails gracefully (returns `None`) when the phrase is missing, the tail
/// doesn't match either pronoun form, or the subject cannot be resolved
/// to a known filter — letting subsequent dispatch branches try.
pub(crate) fn parse_can_attack_despite_defender(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    // Split trailing " as long as <condition>" first so the subject-prefix
    // extraction sees only "<subject> can attack as though <pronoun>
    // didn't have defender".
    let (body_tp, condition_tp) = match tp.split_around(" as long as ") {
        Some((before, after)) => (before, Some(after)),
        None => (*tp, None),
    };

    let (subject_prefix, _) = nom_primitives::scan_split_at_phrase(body_tp.lower, |i| {
        tag::<_, _, OracleError<'_>>("can attack as though").parse(i)
    })?;

    // Verify the rest of the phrase: " it didn't have defender" or
    // " they didn't have defender". Guards against "can attack as though
    // it had haste" reaching subject dispatch.
    type VE<'a> = OracleError<'a>;
    let after_phrase = &body_tp.lower[subject_prefix.len() + "can attack as though".len()..];
    let tail_ok = alt((
        tag::<_, _, VE>(" it didn't have defender"),
        tag::<_, _, VE>(" they didn't have defender"),
    ))
    .parse(after_phrase)
    .is_ok();
    if !tail_ok {
        return None;
    }

    // Subject text = original slice for correct case preservation.
    let subject_original = body_tp.original[..subject_prefix.len()].trim();
    let subject_lower = body_tp.lower[..subject_prefix.len()].trim();

    // Dispatch subject: SelfRef for ~/this creature (and other self-ref
    // phrases); parse_continuous_subject_filter for filter subjects
    // (handles "each", "other", modified-creature, subtype, and
    // core-type subjects with consistent semantics). Defer to other
    // branches when the subject is not recognized.
    // structural: not dispatch — slice-contains over a finite constant list
    let affected = if subject_original == "~" || SELF_REF_TYPE_PHRASES.contains(&subject_lower) {
        TargetFilter::SelfRef
    } else {
        parse_continuous_subject_filter(subject_original)?
    };

    let mut def = StaticDefinition::new(StaticMode::CanAttackWithDefender)
        .affected(affected)
        .description(description.to_string());
    if let Some(cond_tp) = condition_tp {
        let cond_text = cond_tp.original.trim().trim_end_matches('.');
        let condition =
            parse_static_condition(cond_text).unwrap_or(StaticCondition::Unrecognized {
                text: cond_text.to_string(),
            });
        def = def.condition(condition);
    }
    Some(def)
}

/// CR 602.5a: parse "[You may ]activate abilities of <subject> as though
/// those creatures had haste" (or "as though that creature had haste") into a
/// `StaticMode::CanActivateAbilitiesAsThoughHaste` on `affected`.
///
/// This bypasses ONLY the summoning-sickness gate on `{T}`/`{Q}` activated
/// abilities — it is NOT `AddKeyword(Haste)` (combat attacker validation
/// CR 508.1a is untouched). Canonical card: Tyvar, Jubilant Brawler.
///
/// Uses `scan_split_at_phrase(tag("activate abilities of "))` to locate the
/// phrase at a word boundary, verifies the tail matches one of the haste
/// forms, and resolves the subject via `parse_continuous_subject_filter`.
/// Returns `None` (graceful fall-through) when the phrase is absent, the tail
/// doesn't match, or the subject cannot be resolved — so unrelated lines like
/// "can attack as though it had haste" never match here.
pub(crate) fn parse_activate_abilities_as_though_haste(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;

    // Consume an optional leading "you may " so the subject extraction sees
    // only the "activate abilities of <subject> as though ..." body.
    let body_tp = nom_tag_tp(tp, "you may ").unwrap_or(*tp);

    let (_prefix, rest) = nom_primitives::scan_split_at_phrase(body_tp.lower, |i| {
        tag::<_, _, VE>("activate abilities of ").parse(i)
    })?;

    // `rest` begins at "activate abilities of "; the subject is everything
    // between that phrase and the trailing haste clause.
    let after_phrase_offset = body_tp.lower.len() - rest.len() + "activate abilities of ".len();
    let subject_and_tail_lower = &body_tp.lower[after_phrase_offset..];

    // Locate the haste tail at a word boundary. Either plural ("those
    // creatures") or singular ("that creature") form is accepted.
    let (subject_lower, _tail) =
        nom_primitives::scan_split_at_phrase(subject_and_tail_lower, |i| {
            alt((
                tag::<_, _, VE>("as though those creatures had haste"),
                tag::<_, _, VE>("as though that creature had haste"),
            ))
            .parse(i)
        })?;

    // Subject text = original slice for correct case preservation.
    let subject_start = after_phrase_offset;
    let subject_end = after_phrase_offset + subject_lower.len();
    let subject_original = body_tp.original[subject_start..subject_end].trim();

    let affected = parse_continuous_subject_filter(subject_original)?;

    Some(
        StaticDefinition::new(StaticMode::CanActivateAbilitiesAsThoughHaste)
            .affected(affected)
            .description(description.to_string()),
    )
}

/// CR 509.1b + CR 609.4 + CR 702.28b: parse "<subject> can block creatures with
/// shadow as though <they didn't have shadow | it had shadow>" into a
/// `StaticMode::CanBlockShadow` on `affected`.
///
/// Captures both printed phrasings of the same block-legality outcome — Heartwood
/// Dryad ("... as though they didn't have shadow") and Wall of Diffusion ("... as
/// though it had shadow"). Mirrors `parse_can_attack_despite_defender`: locate the
/// `"can block creatures with shadow as though"` phrase at a word boundary with
/// `scan_split_at_phrase`, verify the tail with an `alt()` of the two forms, then
/// resolve the subject via `parse_continuous_subject_filter`. Returns `None`
/// (graceful fall-through) when the phrase is absent, the tail doesn't match, or
/// the subject can't be resolved — so unrelated shadow lines never match here.
pub(crate) fn parse_block_shadow_as_though(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;

    let (subject_prefix, _) = nom_primitives::scan_split_at_phrase(tp.lower, |i| {
        tag::<_, _, VE>("can block creatures with shadow as though").parse(i)
    })?;

    // Verify the trailing clause: " they didn't have shadow" (Heartwood Dryad)
    // or " it had shadow" (Wall of Diffusion). Both lift the same CR 702.28b
    // blocker-side restriction; the `alt()` keeps the two phrasings on one axis.
    let after_phrase =
        &tp.lower[subject_prefix.len() + "can block creatures with shadow as though".len()..];
    let (rest, _) = alt((
        tag::<_, _, VE>(" they didn't have shadow"),
        tag::<_, _, VE>(" it had shadow"),
    ))
    .parse(after_phrase)
    .ok()?;
    let (rest, _) = opt(tag::<_, _, VE>(".")).parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    // Subject text = original slice for correct case preservation.
    let subject_original = tp.original[..subject_prefix.len()].trim();
    let affected = parse_continuous_subject_filter(subject_original)?;

    Some(
        StaticDefinition::new(StaticMode::CanBlockShadow)
            .affected(affected)
            .description(description.to_string()),
    )
}

/// CR 508.1d / CR 509.1c: Parse subject-scoped "attack/block each combat if able" patterns.
///
/// Handles "All creatures attack each combat if able", "Creatures you control attack each
/// combat if able", "Creatures your opponents control attack each combat if able", and the
/// combined "attacks or blocks each combat if able" variant.
pub(crate) fn try_parse_scoped_must_attack_block(
    lower: &str,
    text: &str,
) -> Option<Vec<StaticDefinition>> {
    // Strip trailing period for matching.
    let clean = lower.trim_end_matches('.');
    let clean_text = text.trim_end_matches('.');

    // Try to extract the verb phrase suffix and determine the mode(s).
    let (_, (subject_lower, modes)) = all_consuming(alt((
        map(
            terminated(
                take_until(" attacks or blocks each combat if able"),
                tag::<_, _, OracleError<'_>>(" attacks or blocks each combat if able"),
            ),
            |subj| (subj, vec![StaticMode::MustAttack, StaticMode::MustBlock]),
        ),
        map(
            terminated(
                take_until(" attack or block each combat if able"),
                tag(" attack or block each combat if able"),
            ),
            |subj| (subj, vec![StaticMode::MustAttack, StaticMode::MustBlock]),
        ),
        map(
            terminated(
                take_until(" attack each combat if able"),
                tag(" attack each combat if able"),
            ),
            |subj| (subj, vec![StaticMode::MustAttack]),
        ),
        map(
            terminated(
                take_until(" attacks each combat if able"),
                tag(" attacks each combat if able"),
            ),
            |subj| (subj, vec![StaticMode::MustAttack]),
        ),
        map(
            terminated(
                take_until(" attack each turn if able"),
                tag(" attack each turn if able"),
            ),
            |subj| (subj, vec![StaticMode::MustAttack]),
        ),
        map(
            terminated(
                take_until(" block each combat if able"),
                tag(" block each combat if able"),
            ),
            |subj| (subj, vec![StaticMode::MustBlock]),
        ),
        map(
            terminated(
                take_until(" blocks each combat if able"),
                tag(" blocks each combat if able"),
            ),
            |subj| (subj, vec![StaticMode::MustBlock]),
        ),
        map(
            terminated(
                take_until(" block each turn if able"),
                tag(" block each turn if able"),
            ),
            |subj| (subj, vec![StaticMode::MustBlock]),
        ),
    )))
    .parse(clean)
    .ok()?;
    let subject = &clean_text[..subject_lower.len()];

    // Determine the affected filter from the subject phrase.
    let affected = match subject_lower {
        "all creatures" | "each creature" => TargetFilter::Typed(TypedFilter::creature()),
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "creatures you control" => {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
        }
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "creatures your opponents control" => {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent))
        }
        "~" | "this creature" => TargetFilter::SelfRef,
        _ => parse_creature_subject_filter(subject)
            .or_else(|| parse_continuous_subject_filter(subject))?,
    };

    // Emit one StaticDefinition per mode. For compound "attacks or blocks each
    // combat if able", this produces both MustAttack and MustBlock statics.
    Some(
        modes
            .into_iter()
            .map(|mode| {
                StaticDefinition::new(mode)
                    .affected(affected.clone())
                    .description(text.to_string())
            })
            .collect(),
    )
}

/// CR 611.3a + CR 613.1f: Detect and split
/// `"PRIMARY and FOREIGN_SUBJECT have/has/gains/gain KEYWORD [as long as COND]"`
/// (including the inverted form `"As long as COND, PRIMARY and FOREIGN_SUBJECT …"`).
///
/// A "foreign subject" is any noun phrase parseable by `parse_continuous_subject_filter`
/// that does NOT resolve to `SelfRef`. Example: "creatures you control have vigilance"
/// after "~ gets +2/+2 and" — Angelic Field Marshal's Lieutenant ability.
///
/// Returns two `StaticDefinition`s: one for the primary (existing `affected`) plus a
/// companion `Continuous` def for the foreign-subject keyword grant. Both inherit the
/// same `StaticCondition` when present so the gate applies to both effects.
///
/// CR 109.5 + CR 611.3a: the condition binds each effect independently (CR 611.3a),
/// but MTG print convention always states one condition for the whole clause, so both
/// defs receive the same condition object.
pub(crate) fn try_split_and_foreign_keyword_grant(text: &str) -> Option<Vec<StaticDefinition>> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Normalize the inverted "As long as COND, EFFECT" orientation so the rest
    // of the logic always operates on EFFECT with an optional separate COND.
    let (effect_original, condition_text): (String, Option<String>) =
        if let Some(split) = try_split_inverted_as_long_as(&tp) {
            (
                split.effect_text.clone(),
                Some(split.condition_text.clone()),
            )
        } else if let Some((before, after)) = tp.split_around(" as long as ") {
            (
                before.original.trim().to_string(),
                Some(after.original.trim().trim_end_matches('.').to_string()),
            )
        } else {
            (text.to_string(), None)
        };

    let effect_lower = effect_original.to_lowercase();

    // Scan for "and FOREIGN_SUBJECT verb KEYWORD" in the effect text.
    // We try each grant verb and check every " and " position.
    for verb in [" have ", " has ", " gains ", " gain "] {
        let mut search_lower = effect_lower.as_str();
        let mut search_offset = 0;
        while let Some((before_and, subject_lower, keyword_lower)) =
            nom_primitives::scan_preceded(search_lower, |input| {
                let (after_and, _) = tag::<_, _, OracleError<'_>>("and ").parse(input)?;
                let (after_subject, subject) = take_until(verb).parse(after_and)?;
                let (after_verb, _) = tag::<_, _, OracleError<'_>>(verb).parse(after_subject)?;
                Ok((after_verb, subject))
            })
        {
            let and_pos = search_offset + before_and.len();

            let subject_lower = subject_lower.trim();
            if subject_lower.is_empty() {
                search_offset = and_pos + "and ".len();
                search_lower = &effect_lower[search_offset..];
                continue;
            }

            // Subject must resolve to a recognised non-SelfRef filter.
            let companion_filter = match parse_continuous_subject_filter(subject_lower) {
                Some(f) if !matches!(f, TargetFilter::SelfRef) => f,
                _ => {
                    search_offset = and_pos + "and ".len();
                    search_lower = &effect_lower[search_offset..];
                    continue;
                }
            };

            // Keyword text is everything after the verb.
            let kw_start = effect_lower.len() - keyword_lower.len();
            if kw_start >= effect_original.len() {
                search_offset = and_pos + "and ".len();
                search_lower = &effect_lower[search_offset..];
                continue;
            }
            let keyword_text = effect_original[kw_start..].trim().trim_end_matches('.');
            if keyword_text.is_empty() {
                search_offset = and_pos + "and ".len();
                search_lower = &effect_lower[search_offset..];
                continue;
            }

            // Parse keyword list into companion modifications.
            let mut companion_mods = Vec::new();
            for part in split_keyword_list(keyword_text) {
                push_grant_clause_modifications(&mut companion_mods, part.as_ref(), None);
            }
            if companion_mods.is_empty() {
                search_offset = and_pos + "and ".len();
                search_lower = &effect_lower[search_offset..];
                continue;
            }

            // Primary text is everything before " and FOREIGN_SUBJECT".
            let primary_text = effect_original[..and_pos].trim_end_matches(',').trim();
            if primary_text.is_empty() {
                search_offset = and_pos + "and ".len();
                search_lower = &effect_lower[search_offset..];
                continue;
            }

            // Re-parse the primary with the condition included so the primary def
            // already carries the condition object.
            let primary_full = if let Some(ref cond) = condition_text {
                format!("{primary_text} as long as {cond}.")
            } else {
                format!("{primary_text}.")
            };
            let mut primary_defs = parse_static_line_multi(&primary_full);
            if primary_defs.is_empty() {
                search_offset = and_pos + "and ".len();
                search_lower = &effect_lower[search_offset..];
                continue;
            }

            for def in &mut primary_defs {
                def.description = Some(text.to_string());
            }

            // Resolve the condition object for the companion.
            let condition = condition_text.as_deref().and_then(|ct| {
                parse_static_condition(ct).or(Some(StaticCondition::Unrecognized {
                    text: ct.to_string(),
                }))
            });
            let effective_condition =
                condition.or_else(|| primary_defs.first().and_then(|d| d.condition.clone()));

            let mut companion = StaticDefinition::continuous()
                .affected(companion_filter)
                .modifications(companion_mods)
                .description(text.to_string());
            if let Some(cond) = effective_condition {
                companion.condition = Some(cond);
            }

            primary_defs.push(companion);
            return Some(primary_defs);
        }
    }

    None
}

#[cfg(test)]
mod assign_damage_pronoun_tests {
    use super::*;

    fn assert_unblocked_self(lower: &str) {
        let def = parse_assign_damage_as_though_unblocked(lower, lower)
            .unwrap_or_else(|| panic!("expected Some for {lower:?}"));
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AssignDamageAsThoughUnblocked]
        );
    }

    #[test]
    fn parses_neuter_pronouns() {
        // Regression: neuter "its"/"it" must still parse (Thorn Elemental class).
        assert_unblocked_self(
            "you may have ~ assign its combat damage as though it weren't blocked",
        );
    }

    #[test]
    fn parses_masculine_pronouns() {
        // Wolverine, Claws Out: "his"/"he".
        assert_unblocked_self(
            "you may have ~ assign his combat damage as though he weren't blocked",
        );
    }

    #[test]
    fn parses_feminine_pronouns() {
        assert_unblocked_self(
            "you may have ~ assign her combat damage as though she weren't blocked",
        );
    }

    #[test]
    fn rejects_non_matching() {
        assert!(parse_assign_damage_as_though_unblocked(
            "you may have ~ assign its combat damage to any target",
            "you may have ~ assign its combat damage to any target",
        )
        .is_none());
    }
}
