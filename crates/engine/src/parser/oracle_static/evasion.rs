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
        BlockExceptionKind::Quality(parse_target(trimmed).0)
    }
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
    let (first, remainder) = parse_type_phrase(source_phrase);
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
        let (filter, remainder) = parse_type_phrase(rest);
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
    let TargetFilter::Typed(tf) = filter else {
        return false;
    };
    tf.type_filters.iter().any(|t| {
        !matches!(
            t,
            TypeFilter::Permanent | TypeFilter::Card | TypeFilter::Any
        )
    }) || !tf.properties.is_empty()
}

pub(crate) fn parse_max_combat_creatures_static(lower: &str) -> Option<StaticMode> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("no more than ")
        .parse(lower)
        .ok()?;
    let (max, rest) = parse_number(rest)?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("creature").parse(rest).ok()?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>("s")).parse(rest).ok()?;
    let (rest, mode) = alt((
        value(
            StaticMode::MaxAttackersEachCombat { max },
            tag::<_, _, OracleError<'_>>(" can attack each combat"),
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
    predicates.insert(0, first);
    Some(
        predicates
            .into_iter()
            .map(|predicate| lower_rule_static(predicate, affected.clone(), text))
            .collect(),
    )
}

/// CR 702.16 + CR 609.6: Compound-subject keyword-grant statics of the form
/// `"You and creatures you control have <keyword>"` — a single keyword grant
/// bound to a player plus an object subset. A single `StaticDefinition` cannot
/// carry both a player scope and an object scope, so decompose into two:
///   - an object-half `Continuous` def whose `affected` is the object subset;
///   - a player-half `PlayerProtection` def whose `affected` is the controller.
///
/// Restricted to `Protection(_)` grants — the only player-applicable keyword
/// with a runtime-implemented `PlayerProtection` mode. Returns `None` for any
/// other granted keyword (a player cannot meaningfully "have flying").
pub(crate) fn parse_compound_subject_keyword_static(
    text: &str,
    lower: &str,
) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;

    // Subject: "you and <object subject phrase> ".
    let (after_you, _) = tag::<_, _, VE<'_>>("you and ").parse(lower).ok()?;
    let (predicate_lower, _) = alt((
        tag::<_, _, VE<'_>>("creatures you control "),
        tag("other creatures you control "),
        tag("permanents you control "),
    ))
    .parse(after_you)
    .ok()?;

    // Map the matched lowercase spans back onto the original-case text so the
    // object-subject filter and predicate retain their original casing.
    let object_subject = text[text.len() - after_you.len()..text.len() - predicate_lower.len()]
        .trim()
        .trim_end_matches(' ');
    let predicate = text[text.len() - predicate_lower.len()..].trim();

    let affected = parse_rule_static_subject_filter(object_subject)?;

    // Object-half: delegate the predicate to the shared keyword-grant builder.
    let object_def = parse_continuous_gets_has(predicate, affected, text)?;

    // Extract the granted protection target — only `Protection(_)` grants get a
    // player-half. Any other keyword (or no keyword) → not this pattern.
    let protection_target = object_def.modifications.iter().find_map(|m| match m {
        ContinuousModification::AddKeyword {
            keyword: crate::types::keywords::Keyword::Protection(pt),
        } => Some(pt.clone()),
        _ => None,
    })?;

    let player_def = StaticDefinition::new(StaticMode::PlayerProtection(protection_target))
        .affected(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ))
        .description(text.to_string());

    Some(vec![object_def, player_def])
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
                vec![StaticMode::MustBeBlocked],
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
    for predicate in tail_predicates {
        let mut companion = lower_rule_static(predicate, affected.clone(), text);
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
    type VE<'a> = OracleError<'a>;
    let lower = text.to_lowercase();

    let (before, count, _rest) = nom_primitives::scan_preceded(&lower, |i: &str| {
        let (i, _) = tag::<_, _, VE>("and can block ").parse(i)?;
        // CR 107.1c: "any number of creatures" → unbounded (None); otherwise a
        // numeric count of additional creatures ("an"/"a" → 1, "two" → 2, …).
        let (i, count): (&str, Option<u32>) = if let Ok((after, _)) = (
            tag::<_, _, VE>("any"),
            tag::<_, _, VE>(" number of creatures"),
        )
            .parse(i)
        {
            (after, None)
        } else {
            let (after_n, n) = nom_primitives::parse_number(i)?;
            let (after_kw, _) = tag::<_, _, VE>(" additional creature").parse(after_n)?;
            let (after_s, _) = opt(tag::<_, _, VE>("s")).parse(after_kw)?;
            (after_s, Some(n))
        };
        // Optional trailing duration phrase ("each combat", "this combat", …).
        let (i, _) = opt(alt((
            tag::<_, _, VE>(" each combat"),
            tag::<_, _, VE>(" this combat"),
            tag::<_, _, VE>(" this turn"),
        )))
        .parse(i)?;
        Ok((i, count))
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

    let affected = defs[0].affected.clone()?;
    let condition = defs[0].condition.clone();
    let mut companion = StaticDefinition::new(StaticMode::ExtraBlockers { count })
        .affected(affected)
        .description(text.to_string());
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

/// CR 508.1c: Decompose `"<continuous grant> and can't attack"` into the first
/// conjunct's static(s) plus a `CantAttack` static sharing the same `affected`
/// (and any `condition`).
///
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

/// CR 509.1b: Classify a "can't be blocked …" evasion predicate (lowercased,
/// starting with "can't be blocked") into the corresponding `StaticMode` and
/// optional evasion condition, composing the same building blocks the standalone
/// branches use. Returns `None` when the tail is not a recognized evasion shape.
fn cant_be_blocked_mode(clause: &str) -> Option<(StaticMode, Option<StaticCondition>)> {
    type VE<'a> = OracleError<'a>;
    let rest = nom_tag_lower(clause, clause, "can't be blocked")?;
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
    // Bare "can't be blocked".
    if rest.is_empty() {
        return Some((StaticMode::CantBeBlocked, None));
    }
    if let Some(condition) = parse_compound_cant_be_blocked_condition(rest) {
        return Some((StaticMode::CantBeBlocked, Some(condition)));
    }
    None
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

/// CR 510.1c: Parse "you may have this creature assign its combat damage as though it
/// weren't blocked" self-referential static.
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
    let (rest, _) = tag::<_, _, VE<'_>>(" assign its combat damage as though it weren't blocked")
        .parse(rest)
        .ok()?;
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

    let (_, _) = tag::<_, _, VE<'_>>(
        "'s controller may have it assign its combat damage as though it weren't blocked",
    )
    .parse(rest.lower)
    .ok()?;

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
    if !rest.trim().is_empty() {
        return None;
    }
    let subject = text[..subject_lower.len()].trim();
    let affected = parse_rule_static_subject_filter(subject)?;
    Some(lower_rule_static(predicate, affected, text).attack_defended(defended))
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
