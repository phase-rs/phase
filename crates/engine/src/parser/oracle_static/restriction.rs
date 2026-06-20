// CR 601.3 — casting/activation restriction statics.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

/// CR 601.2 + CR 602.5 + CR 117.1a + CR 117.1b: Parse "[subject] can cast spells
/// and activate abilities only during {your | their own} turn(s)" — City of
/// Solitude class. Emits TWO statics (cast-half + activate-half) so the
/// runtime gates dispatch independently.
///
/// Subject → scope via the shared `strip_casting_prohibition_subject` helper.
/// Trailing "only during X turn(s)" → typed `WhenKind` via the same shared
/// `parse_when_clause` combinator that the cast-only branch uses.
///
/// Grammar:
///   <SUBJECT> "can cast spells and activate abilities " parse_when_clause
pub(crate) fn parse_cast_and_activate_only_during(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<Vec<StaticDefinition>> {
    let lower = tp.lower;
    if !nom_primitives::scan_contains(lower, "can cast spells and activate abilities only during") {
        return None;
    }
    // Subject → scope.
    let (who, after_subject) = strip_casting_prohibition_subject(lower)?;
    // Verb phrase + shared when-clause combinator.
    fn parse_predicate(i: &str) -> OracleResult<'_, WhenKind> {
        let (i, _) =
            tag::<_, _, OracleError<'_>>("can cast spells and activate abilities ").parse(i)?;
        let (i, kind) = parse_when_clause(i)?;
        Ok((i, kind))
    }
    let (rest, kind) = parse_predicate(after_subject).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    let when = when_kind_to_condition(kind);

    // Preserve full Oracle text on both emitted statics' `description`.
    // CR 605.1a: City of Solitude per its 2009-10-01 ruling blocks mana
    // abilities — emit `ActivationExemption::None`. Future printings that
    // carve out mana abilities ("...except mana abilities") may extend the
    // parser to detect the exemption suffix; today no printed card uses that
    // shape.
    Some(vec![
        StaticDefinition::new(StaticMode::CantCastDuring {
            who: who.clone(),
            when: when.clone(),
        })
        .description(text.to_string()),
        StaticDefinition::new(StaticMode::CantActivateDuring {
            who,
            when,
            exemption: ActivationExemption::None,
        })
        .description(text.to_string()),
    ])
}

/// CR 704.5j: Parse a "the \"legend rule\" doesn't apply [to <scope> you control]"
/// static-ability line into a `LegendRuleDoesntApply` definition.
///
/// - Global form ("the legend rule doesn't apply.") → `affected: None`
///   (Mirror Gallery).
/// - Scoped form ("... doesn't apply to <scope> you control.") → a
///   controller-scoped `affected` filter derived from `<scope>`.
///
/// Anchored on the canonical opening, so conditional / compound forms that do
/// not begin with the exemption clause — "If there are exactly two permanents
/// named …" (Brothers Yamazaki), "As long as you control …" (Mothers Yamazaki),
/// "Numot … have vigilance and haste, and the legend rule doesn't apply to
/// them" (The Herald of Numot) — fall through and remain Unimplemented rather
/// than being misparsed.
pub(crate) fn parse_legend_rule_exemption(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    let rest = nom_tag_tp(tp, "the \"legend rule\" doesn't apply")?;

    // Global form: nothing (or just the sentence terminator) follows.
    if rest.lower.trim().trim_end_matches('.').trim().is_empty() {
        return Some(
            StaticDefinition::new(StaticMode::LegendRuleDoesntApply).description(text.to_string()),
        );
    }

    // Scoped form: "... to <scope> you control."
    let scope = nom_tag_tp(&rest, " to ")?;
    let affected = parse_legend_rule_scope(&scope)?;
    Some(
        StaticDefinition::new(StaticMode::LegendRuleDoesntApply)
            .affected(affected)
            .description(text.to_string()),
    )
}

/// CR 704.5j: Resolve the `<scope>` noun phrase of a legend-rule exemption
/// ("permanents you control", "creatures you control", "tokens you control",
/// "commanders you control", "Slivers you control", ...) into a
/// controller-scoped `affected` filter. The legend rule applies only to
/// permanents (CR 704.5j: "legendary permanents"), so only permanent card
/// types are accepted as bare-type scopes. Returns `None` for scopes this
/// parser cannot resolve precisely (e.g. the bare pronoun "them"), so those
/// cards are deferred rather than given a filter that silently matches nothing.
/// "creature tokens you control" is handled explicitly (The Master, Multiplied).
/// CR 109.5: "you control" resolves to the source's controller.
pub(crate) fn parse_legend_rule_scope(scope: &TextPair<'_>) -> Option<TargetFilter> {
    // Drop the trailing sentence terminator so the combinator suffix split sees
    // a clean "<descriptor> you control" phrase. allow-noncombinator: punctuation
    // cleanup on a pre-tokenized chunk, not parsing dispatch.
    let lower = scope.lower.trim_end().trim_end_matches('.').trim_end();
    let cleaned = TextPair::new(&scope.original[..lower.len()], lower);
    let base = parse_subject_suffix(&cleaned, " you control")?;

    // "permanents you control" — every permanent the controller controls
    // (Sakashima of a Thousand Faces, Mirror Box).
    if base.lower == "permanents" {
        return Some(TargetFilter::Typed(
            TypedFilter::permanent().controller(ControllerRef::You),
        ));
    }

    // "<Subtype>s you control" — permanents of a single subtype (Sliver
    // Gravemother, Spider-Verse). Require the subtype to consume the whole base
    // so multi-word scopes ("creature tokens") are deferred, not truncated.
    if base.lower == "creature tokens" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature()
                .properties(vec![FilterProp::Token])
                .controller(ControllerRef::You),
        ));
    }

    // CR 704.5j: bare permanent card-type scopes ("creatures you control",
    // "artifacts you control", ...). Generalizes the "creatures" case (Council
    // of Reeds) to the whole "<permanent type>s you control" class. Only
    // permanent types map — the legend rule applies solely to permanents.
    if let Some(card_type) = legend_rule_permanent_type(base.lower) {
        return Some(TargetFilter::Typed(
            TypedFilter::new(card_type).controller(ControllerRef::You),
        ));
    }

    // CR 111.1 + CR 704.5j: "tokens you control" — any token permanent (Cadric,
    // Soul Kindler). Token-ness is a property, not a card type.
    if base.lower == "tokens" {
        return Some(TargetFilter::Typed(
            TypedFilter::permanent()
                .properties(vec![FilterProp::Token])
                .controller(ControllerRef::You),
        ));
    }

    // CR 903.3 + CR 704.5j: "commanders you control" — permanents that are
    // commanders. Commander designation is a card attribute, modeled as a
    // permanent property.
    if base.lower == "commanders" {
        return Some(TargetFilter::Typed(
            TypedFilter::permanent()
                .properties(vec![FilterProp::IsCommander])
                .controller(ControllerRef::You),
        ));
    }

    if let Some((canonical, consumed)) = parse_subtype(base.original) {
        if consumed == base.original.len() {
            return Some(TargetFilter::Typed(
                TypedFilter::permanent()
                    .subtype(canonical)
                    .controller(ControllerRef::You),
            ));
        }
    }

    None
}

/// CR 704.5j: Map a plural permanent card-type word ("creatures", "artifacts",
/// …) to its `TypeFilter`. Only permanent types are returned — the legend rule
/// applies solely to permanents — so instant/sorcery words (and anything else)
/// yield `None` and the card stays deferred rather than mis-scoped.
fn legend_rule_permanent_type(word: &str) -> Option<crate::types::ability::TypeFilter> {
    use crate::types::ability::TypeFilter;
    // Composed via nom `alt`/`value` per the combinator mandate; a new permanent
    // type is one extra `value(..., tag(...))` branch. Instant/sorcery words (and
    // anything else) fail every branch, so the card stays deferred.
    let (rest, tf) = alt((
        value(
            TypeFilter::Creature,
            tag::<_, _, OracleError<'_>>("creatures"),
        ),
        value(TypeFilter::Artifact, tag("artifacts")),
        value(TypeFilter::Enchantment, tag("enchantments")),
        value(TypeFilter::Planeswalker, tag("planeswalkers")),
        value(TypeFilter::Land, tag("lands")),
        value(TypeFilter::Battle, tag("battles")),
    ))
    .parse(word)
    .ok()?;
    rest.is_empty().then_some(tf)
}

/// Parse the subject of "X can't be countered" lines.
/// CR 101.2: Returns SelfRef for "~ can't be countered", or a typed filter for
/// "Green spells you control can't be countered", "Creature spells you control can't be countered", etc.
pub(crate) fn parse_cant_be_countered_subject(tp: &TextPair) -> TargetFilter {
    // Find the subject before "can't be countered"
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    if let Some(pos) = tp.lower.find("can't be countered") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        let subject = tp.lower[..pos].trim();
        // Self-referential: "~" or card name (handled by tp.contains matching the card name)
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        if subject.is_empty() || subject == "~" || subject.ends_with(" ~") {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            return TargetFilter::SelfRef;
        }
        let normalized = format!("all {subject}");
        let (filter, rest) = parse_target(&normalized);
        if rest.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
            return filter;
        }
    }
    TargetFilter::SelfRef
}

/// CR 301.5 + CR 303.4 + CR 701.3a: Parse a positive attachment restriction —
/// "~ can be attached only to {filter}" — into a `StaticMode::AttachmentRestriction`
/// carrying the legal-host `TargetFilter`.
///
/// The subject is always the source Aura/Equipment itself: by the time the static
/// parser sees the line, "This Equipment" / the card name has already been
/// normalized to `~` (see `SELF_REF_TYPE_PHRASES` / `normalize_self_refs_for_static`).
/// We therefore require the `~` subject and reject any non-self subject so a
/// hypothetical "other equipment can be attached only to ..." (no such printed
/// card) is deferred rather than mis-scoped.
///
/// Grammar:
///   "~ can be attached only to " <FILTER> "."?
///
/// `<FILTER>` is parsed by the shared `parse_target` building block (the same
/// combinator used for "a creature with power N or greater", "a legendary
/// creature", "an {type}", etc.) — no new filter language is invented. The
/// entire remainder must be consumed; a non-empty tail means the filter phrase
/// was only partially understood, so we bail to avoid a silently-wrong filter.
///
/// Corpus: Strata Scythe ("a creature with power 3 or greater"), Brass Knuckles
/// ("a creature with toughness 4 or greater"), Konda's Banner ("a legendary
/// creature").
pub(crate) fn parse_attach_only_restriction(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    // Require the self-referential subject + verb phrase. `nom_tag_tp` consumes
    // the prefix on the lowercase view while preserving original casing on the
    // returned remainder (needed for `parse_target`'s type-name canonicalization).
    let rest = nom_tag_tp(tp, "~ can be attached only to ")?;

    // Trim the sentence terminator before handing the noun phrase to parse_target.
    // allow-noncombinator: punctuation cleanup on a pre-tokenized chunk, not dispatch.
    let host_phrase = rest.original.trim().trim_end_matches('.').trim();

    let (filter, tail) = parse_target(host_phrase);
    // The whole host phrase must be consumed and must resolve to a real filter —
    // `parse_target` returns `TargetFilter::Any` / `SelfRef` for input it cannot
    // interpret, which would silently whitelist everything/nothing.
    if !tail.trim().is_empty() || matches!(filter, TargetFilter::Any | TargetFilter::SelfRef) {
        return None;
    }

    Some(
        StaticDefinition::new(StaticMode::AttachmentRestriction { filter })
            .affected(TargetFilter::SelfRef)
            .description(text.to_string()),
    )
}

/// CR 605.1a: Parse the optional "unless they're mana abilities" suffix that
/// follows a `CantBeActivated` predicate. Returns `ActivationExemption::None`
/// (and the unconsumed input) when no suffix is present.
///
/// Composed from nom `tag()`/`alt()`/`value()`/`preceded`/`opt` so additional
/// exemption kinds can be added as one combinator branch when a real card needs
/// them — do not add variants speculatively.
pub(crate) fn parse_activation_exemption_suffix(
    input: &str,
) -> OracleResult<'_, ActivationExemption> {
    let mut parser = opt(preceded(
        tag(" unless they're "),
        value(ActivationExemption::ManaAbilities, tag("mana abilities")),
    ));
    let (rest, exemption) = parser.parse(input)?;
    Ok((rest, exemption.unwrap_or_default()))
}

pub(crate) fn parse_cant_be_activated_exemption_in_text(lower: &str) -> ActivationExemption {
    nom_primitives::scan_preceded(lower, |i| {
        preceded(tag("can't be activated"), parse_activation_exemption_suffix).parse(i)
    })
    .and_then(|(_, exemption, tail)| {
        let trimmed_tail = tail.trim_end_matches('.').trim();
        if trimmed_tail.is_empty() {
            Some(exemption)
        } else {
            None
        }
    })
    .unwrap_or_default()
}

/// CR 602.5 + CR 603.2a: Parse global filter-scoped activation prohibitions.
///
/// Shape: `"Activated abilities of <source-filter> can't be activated[ unless they're <kind> abilities]."`
///
/// Source filter dispatch:
/// - `"sources with the chosen name"` → `TargetFilter::HasChosenName` (Pithing Needle,
///   Phyrexian Revoker, Sorcerous Spyglass — the chosen-name name-picker class).
/// - Otherwise delegates to `parse_type_phrase` for type-list + controller-suffix
///   forms (Karn, Clarion Conqueror).
///
/// The scope on the activator axis is always `AllPlayers` — CR 602.5 prohibits the
/// ability itself, not a specific player; opponent-ness rides on the filter's
/// `ControllerRef`.
///
/// CR 605.1a: The optional "unless they're mana abilities" suffix produces
/// `ActivationExemption::ManaAbilities`; runtime enforcement (CR 605.1a definition
/// of mana abilities) lives in `casting.rs::is_blocked_by_cant_be_activated` via
/// `mana_abilities::is_mana_ability` — the single classifier authority.
///
/// Returns `None` for the self-reference case ("its activated abilities can't be activated"
/// / "activated abilities can't be activated" on creature text), which the self-ref
/// branch below handles directly.
pub(crate) fn parse_filter_scoped_cant_be_activated(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    // Require the "activated abilities of " prefix — distinguishes from the self-ref
    // "its activated abilities can't be activated" / bare "activated abilities can't be
    // activated" forms which are handled separately.
    let rest_tp = nom_tag_tp(tp, "activated abilities of ")?;

    // CR 605.1a: Pithing Needle / Phyrexian Revoker / Sorcerous Spyglass class —
    // "sources with the chosen name". Composed nom dispatch: `tag` matches the
    // chosen-name source phrase, then `tag` consumes the predicate, then the
    // exemption combinator handles the optional suffix.
    if let Ok((after_source, source_filter)) = (value(
        TargetFilter::HasChosenName,
        tag::<_, _, OracleError<'_>>("sources with the chosen name"),
    ))
    .parse(rest_tp.lower)
    {
        if let Ok((after_predicate, _)) =
            tag::<_, _, OracleError<'_>>(" can't be activated").parse(after_source)
        {
            // Optional "unless they're..." suffix, then the trailing period (or end-of-input).
            if let Ok((tail, exemption)) = parse_activation_exemption_suffix(after_predicate) {
                let trimmed_tail = tail.trim_end_matches('.').trim();
                if trimmed_tail.is_empty() {
                    return Some(
                        StaticDefinition::new(StaticMode::CantBeActivated {
                            who: ProhibitionScope::AllPlayers,
                            source_filter,
                            exemption,
                        })
                        .description(text.to_string()),
                    );
                }
            }
        }
    }

    // Otherwise fall back to the type-list + controller-suffix form (Karn, Clarion).
    // Require the predicate ending "... can't be activated[.]" at the tail.
    let predicate_tp = rest_tp
        .strip_suffix(" can't be activated.") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        .or_else(|| rest_tp.strip_suffix(" can't be activated"))?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                                                                   // Extract the type-list + optional controller suffix via the shared helper.
                                                                   // `parse_type_phrase` consumes the filter and returns the unconsumed tail —
                                                                   // for this pattern the tail should be empty (the whole predicate IS the filter).
    let (source_filter, tail) = parse_type_phrase(predicate_tp.original);
    if !tail.trim().is_empty() {
        return None;
    }
    // `parse_type_phrase` returns `SelfRef` for unparseable input — treat that as a
    // parse failure and fall through to the self-ref branch in parse_static_line.
    if matches!(source_filter, TargetFilter::SelfRef) {
        return None;
    }
    Some(
        StaticDefinition::new(StaticMode::CantBeActivated {
            who: ProhibitionScope::AllPlayers,
            source_filter,
            // CR 605.1a: Karn/Clarion class — no "unless they're..." suffix.
            exemption: ActivationExemption::None,
        })
        .description(text.to_string()),
    )
}

/// CR 701.23 + CR 609.3: Parse CantSearchLibrary statics.
///
/// Supported Oracle classes:
/// - "Spells and abilities <scope> can't cause their controller to search their
///   library." (Ashiok class)
/// - "Players can't search libraries." / "Each player can't search libraries."
///   (Mindlock Orb class)
pub(crate) fn parse_cant_search_library(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    fn parse_search_negation_prefix(input: &str) -> OracleResult<'_, ()> {
        let (input, _) = alt((
            value((), tag::<_, _, OracleError<'_>>("can't ")),
            value((), tag("cannot ")),
            value((), tag("may not ")),
        ))
        .parse(input)?;
        Ok((input, ()))
    }

    fn parse_cause_controller_search_their_library(input: &str) -> OracleResult<'_, ()> {
        let (input, _) = parse_search_negation_prefix(input)?;
        let (input, _) = tag::<_, _, OracleError<'_>>("cause their controller to ").parse(input)?;
        let (input, _) = tag("search ").parse(input)?;
        let (input, _) = tag("their library").parse(input)?;
        Ok((input, ()))
    }

    fn parse_search_libraries(input: &str) -> OracleResult<'_, ()> {
        let (input, _) = parse_search_negation_prefix(input)?;
        let (input, _) = tag::<_, _, OracleError<'_>>("search ").parse(input)?;
        let (input, _) = tag("libraries").parse(input)?;
        Ok((input, ()))
    }

    // Ashiok class: "Spells and abilities <scope> can't cause their controller to
    // search their library."
    if let Some(rest_tp) = nom_tag_tp(tp, "spells and abilities ") {
        // Strip the controller suffix — scope identifier rides on the possessive phrase.
        let (cause, predicate) = strip_controller_possessive_scope(rest_tp.original)?;
        let predicate_lower = predicate.to_lowercase();
        // Compose as modal + causal clause + search target; avoid verbatim phrase matching.
        nom_on_lower(predicate, &predicate_lower, |i| {
            let (i, _) = parse_cause_controller_search_their_library(i)?;
            let (i, _) = opt(tag(".")).parse(i)?;
            let (i, _) = eof(i)?;
            Ok((i, ()))
        })?;
        return Some(
            StaticDefinition::new(StaticMode::CantSearchLibrary { cause })
                .description(text.to_string()),
        );
    }

    // Mindlock Orb class: "Players can't search libraries." / "Each player can't
    // search libraries." (all players), and opponent-scoped direct search
    // prohibitions ("Your opponents can't search libraries.").
    let (cause, predicate) = strip_casting_prohibition_subject(tp.lower)?;
    if !matches!(
        cause,
        ProhibitionScope::AllPlayers | ProhibitionScope::Opponents
    ) {
        return None;
    }
    let predicate_lower = predicate.to_lowercase();
    // Compose as modal + "search" + object noun, not a single full-string tag.
    nom_on_lower(predicate, &predicate_lower, |i| {
        let (i, _) = parse_search_libraries(i)?;
        let (i, _) = opt(tag(".")).parse(i)?;
        let (i, _) = eof(i)?;
        Ok((i, ()))
    })?;

    Some(
        StaticDefinition::new(StaticMode::CantSearchLibrary { cause })
            .description(text.to_string()),
    )
}

/// CR 603.2 + CR 609.3: Parse "Triggered abilities <scope> can't cause you to
/// sacrifice or exile <affected>." statics (The Master, Multiplied class).
///
/// Supported Oracle class:
/// - "Triggered abilities you control can't cause you to sacrifice or exile
///   creature tokens you control."
pub(crate) fn parse_cant_cause_sacrifice_or_exile(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    fn parse_sacrifice_or_exile_negation(input: &str) -> OracleResult<'_, ()> {
        let (input, _) = alt((
            value((), tag::<_, _, OracleError<'_>>("can't ")),
            value((), tag("cannot ")),
            value((), tag("may not ")),
        ))
        .parse(input)?;
        let (input, _) = tag::<_, _, OracleError<'_>>("cause you to ").parse(input)?;
        let (input, _) =
            alt((tag("sacrifice or exile "), tag("exile or sacrifice "))).parse(input)?;
        Ok((input, ()))
    }

    let rest = nom_tag_tp(tp, "triggered abilities ")?;
    let (cause, predicate) = strip_controller_possessive_scope(rest.original)?;
    let predicate_lower = predicate.to_lowercase();
    nom_on_lower(predicate, &predicate_lower, |i| {
        let (i, _) = parse_sacrifice_or_exile_negation(i)?;
        let (i, _) = tag("creature tokens you control").parse(i)?;
        let (i, _) = opt(tag(".")).parse(i)?;
        let (i, _) = eof(i)?;
        Ok((i, ()))
    })?;
    let affected = TargetFilter::Typed(
        TypedFilter::creature()
            .properties(vec![FilterProp::Token])
            .controller(ControllerRef::You),
    );
    Some(
        StaticDefinition::new(StaticMode::CantCauseSacrificeOrExile { cause })
            .affected(affected)
            .description(text.to_string()),
    )
}

/// CR 603.2g + CR 603.6a + CR 700.4: Parse Torpor Orb / Hushbringer-class
/// "Creatures entering [the battlefield] [and dying] don't cause abilities to trigger."
///
/// The optional `and dying` clause toggles the `Dies` event in the event set.
/// Parser constructs events in canonical order `[EntersBattlefield, Dies]`.
pub(crate) fn parse_suppress_triggers(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    use crate::types::statics::SuppressedTriggerEvent;

    // Consume the type-list + optional controller suffix (e.g., "Creatures your
    // opponents control"). `parse_type_phrase` returns the unconsumed tail.
    let (source_filter, tail) = parse_type_phrase(tp.original);
    // Require a meaningful type constraint — reject the `SelfRef` fallback that
    // `parse_type_phrase` returns when it fails to identify any type.
    if matches!(source_filter, TargetFilter::SelfRef) {
        return None;
    }
    // Match the predicate: "entering [the battlefield] [and dying] don't cause
    // abilities to trigger[.]"
    let tail_trimmed = tail.trim_start();
    let tail_lower = tail_trimmed.to_lowercase();
    // Start with "entering"
    let after_entering = nom_tag_lower(tail_trimmed, &tail_lower, "entering ")?;
    let after_entering_lower = after_entering.to_lowercase();
    // Optional "the battlefield " — accept both with and without (Oracle errata varies).
    let after_tb = nom_tag_lower(after_entering, &after_entering_lower, "the battlefield ")
        .unwrap_or(after_entering);
    let after_tb_lower = after_tb.to_lowercase();
    // Optional "[or|and] dying" clause (Hushbringer — the Oracle uses "or";
    // accept "and" too for defensive parsing of close variants).
    let (events, after_dying) = if let Some(rest) =
        nom_tag_lower(after_tb, &after_tb_lower, "or dying ")
            .or_else(|| nom_tag_lower(after_tb, &after_tb_lower, "and dying "))
    {
        (
            vec![
                SuppressedTriggerEvent::EntersBattlefield,
                SuppressedTriggerEvent::Dies,
            ],
            rest,
        )
    } else {
        (vec![SuppressedTriggerEvent::EntersBattlefield], after_tb)
    };
    let after_dying_lower = after_dying.to_lowercase();
    let after_verb = nom_tag_lower(
        after_dying,
        &after_dying_lower,
        "don't cause abilities to trigger",
    )?;
    // Allow only terminal punctuation (period or empty).
    if !matches!(after_verb.trim(), "" | ".") {
        return None;
    }
    Some(
        StaticDefinition::new(StaticMode::SuppressTriggers {
            source_filter,
            events,
        })
        .description(text.to_string()),
    )
}

/// CR 109.5 + CR 102.1: Strip a "<possessive> control" / "<possessive> controls" suffix
/// from an Oracle noun phrase and return `(ProhibitionScope, remaining_predicate)`.
///
/// Used by Ashiok-class prohibitions where the scope rides on the controller suffix
/// of a preceding noun phrase (e.g., "spells and abilities your opponents control ...").
/// Distinct from `strip_casting_prohibition_subject` which consumes sentence-subject
/// pronoun forms like "you" / "your opponents".
pub(crate) fn strip_controller_possessive_scope(tp: &str) -> Option<(ProhibitionScope, &str)> {
    let lower = tp.to_lowercase();
    // Try "your opponents control " first (plural form — Ashiok).
    if let Some(rest) = nom_tag_lower(tp, &lower, "your opponents control ") {
        return Some((ProhibitionScope::Opponents, rest));
    }
    // "an opponent controls " (singular form).
    if let Some(rest) = nom_tag_lower(tp, &lower, "an opponent controls ") {
        return Some((ProhibitionScope::Opponents, rest));
    }
    // "you control " — Controller scope.
    if let Some(rest) = nom_tag_lower(tp, &lower, "you control ") {
        return Some((ProhibitionScope::Controller, rest));
    }
    None
}

/// Strip a subject prefix that maps to a `ProhibitionScope`.
/// Returns `(scope, remaining_predicate)` or `None` if no known subject prefix matches.
/// Shared by all casting prohibition parsers (CantCastDuring, PerTurnCastLimit, etc.).
pub(crate) fn strip_casting_prohibition_subject(tp: &str) -> Option<(ProhibitionScope, &str)> {
    nom_tag_lower(tp, tp, "each opponent ")
        .or_else(|| nom_tag_lower(tp, tp, "your opponents "))
        .map(|rest| (ProhibitionScope::Opponents, rest))
        .or_else(|| nom_tag_lower(tp, tp, "you ").map(|rest| (ProhibitionScope::Controller, rest)))
        .or_else(|| {
            nom_tag_lower(tp, tp, "each player ")
                .or_else(|| nom_tag_lower(tp, tp, "players "))
                .map(|rest| (ProhibitionScope::AllPlayers, rest))
        })
        .or_else(|| {
            // CR 303.4e: "Enchanted player" — the player enchanted by an aura.
            nom_tag_lower(tp, tp, "enchanted player ")
                .map(|rest| (ProhibitionScope::EnchantedCreatureController, rest))
        })
}

/// CR 601.2 + CR 601.3a + CR 604.1: Parse the "<SUBJECT> who has/have cast a [type] spell
/// this turn can't cast additional [type] spells." phrasing (Ethersworn Canonist) into
/// the equivalent `PerTurnCastLimit { max: 1, spell_filter: <type> }`.
///
/// Casting prohibitions are authorized by CR 601.2 (legality-to-cast check) and CR
/// 601.3a (the "qualities prohibit casting" rule); the per-turn enforcement window
/// is the static itself (CR 604.1).
///
/// The conditional subject ("who has cast a [type] spell this turn") combined with
/// "can't cast additional [type] spells" is logically equivalent to "can't cast more
/// than one [type] spell each turn" — once a player has cast a matching spell, every
/// further matching spell is "additional" and prohibited.
///
/// The subject prefix is parsed via the shared `strip_casting_prohibition_subject`
/// building block so this combinator covers the full subject axis (each player, each
/// opponent, you, your opponents, enchanted player — not just AllPlayers). Both the
/// subject-clause type phrase and the object-clause type phrase must match. If they
/// diverge (a hypothetical future card like "who has cast an artifact spell ... can't
/// cast noncreature spells"), the `max=1` reduction is no longer sound and we return
/// `None` so the line falls through to other parsers (or `Unimplemented`).
pub(crate) fn parse_conditional_subject_per_turn_cast_limit(
    tp: &str,
    text: &str,
) -> Option<StaticDefinition> {
    // 1. Strip subject prefix → scope, via the shared building block. This is the
    //    single authority for subject→`ProhibitionScope` mapping; inlining a
    //    hard-coded "each player" branch here would silently exclude every other
    //    scope (each opponent, you, your opponents, enchanted player).
    let (who, predicate) = strip_casting_prohibition_subject(tp)?;

    // 2. Nom dispatch on the predicate: assemble the conditional-cast grammar as
    //    composed combinators.
    //   ("who has cast " | "who have cast ") ("a " | "an ") <SUBJECT_TYPE>
    //   " spell this turn can't cast additional " <OBJECT_TYPE> (" spell" | " spells") "."?
    //
    // `take_until` is the canonical nom combinator for "everything up to delimiter",
    // the structural counterpart to manually slicing on a found substring.
    let mut parser = (
        alt((
            tag::<_, _, OracleError<'_>>("who has cast "),
            tag("who have cast "),
        )),
        alt((tag("a "), tag("an "))),
        take_until(" spell"),
        tag(" spell"),
        tag(" this turn can't cast additional "),
        take_until(" spell"),
        alt((tag(" spells"), tag(" spell"))),
        opt(tag(".")),
    );
    let (rest, (_, _, subject_type_text, _, _, object_type_text, _, _)) =
        parser.parse(predicate).ok()?;
    // Disallow trailing content — we matched the entire restriction sentence.
    if !rest.trim().is_empty() {
        return None;
    }

    // Both type phrases must canonicalize identically to preserve the `max=1` equivalence.
    let (subject_filter, subject_rest) = parse_type_phrase(subject_type_text.trim());
    let (object_filter, object_rest) = parse_type_phrase(object_type_text.trim());
    if !subject_rest.trim().is_empty() || !object_rest.trim().is_empty() {
        return None;
    }
    if subject_filter != object_filter {
        return None;
    }

    // Verify a real type filter was extracted; mirrors the gate `parse_per_turn_cast_limit`
    // uses on the standard "more than N" phrasing.
    let spell_filter = match &subject_filter {
        TargetFilter::Typed(tf) if !tf.type_filters.is_empty() => Some(subject_filter),
        _ => None,
    };
    // Untyped "<SUBJECT> who has cast a spell" is not a real sentence in printed
    // Magic; require a typed filter to avoid over-matching.
    spell_filter.as_ref()?;

    Some(
        StaticDefinition::new(StaticMode::PerTurnCastLimit {
            who,
            max: 1,
            spell_filter,
        })
        .description(text.to_string()),
    )
}

/// CR 101.2 + CR 604.1: Parse per-turn casting limits from Oracle text.
/// Handles "Each player/opponent can't cast more than N [type] spell(s) each turn"
/// and the alternate phrasing "You can cast no more than N spells each turn."
pub(crate) fn parse_per_turn_cast_limit(tp: &str, text: &str) -> Option<StaticDefinition> {
    // CR 601.2 + CR 601.3a + CR 604.1: Conditional-subject phrasing — "<SUBJECT> who
    // has cast a [type] spell this turn can't cast additional [type] spells."
    // Semantically equivalent to `max=1` per-turn cast limit on the same [type]
    // (Ethersworn Canonist). The two type phrases must match — if they diverge, the
    // equivalence breaks and we bail (defensive: future cards with mismatched types
    // would need a different model).
    if let Some(def) = parse_conditional_subject_per_turn_cast_limit(tp, text) {
        return Some(def);
    }

    // 1. Strip subject → scope, yielding the predicate
    let (who, predicate) = strip_casting_prohibition_subject(tp)?;

    // 2. Strip casting verb → "more than N ..." remainder.
    // If the predicate doesn't start with the limit phrase, check for compound
    // "and" clauses (e.g., "can cast spells only during your turn and you can
    // cast no more than two spells each turn") — re-parse the second clause.
    let after_more_than = nom_tag_lower(predicate, predicate, "can't cast more than ")
        .or_else(|| nom_tag_lower(predicate, predicate, "can cast no more than "))
        .or_else(|| {
            // Compound clause: look for " and " joining two restrictions
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            predicate.split_once(" and ").and_then(|(_, second)| {
                // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                let (_, rest) = strip_casting_prohibition_subject(second)?;
                nom_tag_lower(rest, rest, "can't cast more than ")
                    .or_else(|| nom_tag_lower(rest, rest, "can cast no more than "))
            })
        })?;

    // 3. Extract limit count
    let (max, rest) = parse_number(after_more_than)?;

    // 4. Require "each turn" suffix
    let before_each_turn = rest
        .trim_start()
        .strip_suffix(" each turn.") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        .or_else(|| rest.trim_start().strip_suffix(" each turn"))?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.

    // 5. Extract optional spell type filter between count and "spell(s)"
    let type_text = before_each_turn
        .strip_suffix(" spells") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        .or_else(|| before_each_turn.strip_suffix(" spell")) // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        .unwrap_or("")
        .trim();

    let spell_filter = if type_text.is_empty() {
        None
    } else {
        let (filter, _) = parse_type_phrase(type_text);
        match &filter {
            TargetFilter::Typed(tf) if !tf.type_filters.is_empty() => Some(filter),
            _ => None,
        }
    };

    Some(
        StaticDefinition::new(StaticMode::PerTurnCastLimit {
            who,
            max,
            spell_filter,
        })
        .description(text.to_string()),
    )
}

/// CR 101.2 + CR 109.5 + CR 508.1 + CR 601.3a: "Each [scope] who [did X] this turn
/// can't [Y]" — a static prohibition gated on a PER-AFFECTED-PLAYER turn-activity
/// predicate (Angelic Arbiter).
///
/// The two clauses are:
/// - "Each opponent who attacked with a creature this turn can't cast spells."
///   → `CantBeCast { who: Opponents }` + `per_player_condition: YouAttackedThisTurn`
///   (CR 601.3a cast prohibition).
/// - "Each opponent who cast a spell this turn can't attack with creatures."
///   → `CantAttack` with `affected = opponents' creatures` +
///   `per_player_condition: YouCastSpellThisTurn { filter: None }` (CR 508.1
///   declare-attackers prohibition).
///
/// The turn-activity predicate is stored in `per_player_condition` (CR 109.5:
/// evaluated against the AFFECTED player — the caster, or the attacking creature's
/// controller), NEVER in `condition` (which is the source-relative functioning
/// gate). `condition` stays `None` so the prohibition is not globally gated.
///
/// Composed from the shared `strip_casting_prohibition_subject` building block plus
/// nom `tag`/`alt`/`value` — no string-matching dispatch.
pub(crate) fn parse_per_player_conditional_prohibition(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    // 1. Strip the subject → scope. For "each opponent who ..." this yields
    //    (Opponents, "who ..."). Only opponent-scoped prohibitions are modeled
    //    by this combinator today (the only printed text class).
    let (who, predicate) = strip_casting_prohibition_subject(tp.lower)?;
    if who != ProhibitionScope::Opponents {
        return None;
    }

    // 2. Strip the relative-clause marker and parse the per-player predicate.
    let rest = nom_tag_lower(predicate, predicate, "who ")?;
    let (rest, cond) = alt((
        value(
            ParsedCondition::YouAttackedThisTurn,
            tag::<_, _, OracleError<'_>>("attacked with a creature this turn"),
        ),
        value(
            ParsedCondition::YouCastSpellThisTurn { filter: None },
            tag::<_, _, OracleError<'_>>("cast a spell this turn"),
        ),
    ))
    .parse(rest)
    .ok()?;

    // 3. Strip the prohibition connector " can't " and dispatch on the verb.
    let rest = nom_tag_lower(rest, rest, " can't ")?;

    // CR 601.3a: "... can't cast spells" — cast-side prohibition.
    if let Some(tail) = nom_tag_lower(rest, rest, "cast spells") {
        if tail.trim_end_matches('.').is_empty() {
            return Some(
                StaticDefinition::new(StaticMode::CantBeCast { who })
                    .per_player_condition(cond)
                    .description(text.to_string()),
            );
        }
    }

    // CR 508.1: "... can't attack with creatures" — attack-side prohibition. The
    // `affected` filter is opponents' creatures (CR 109.5: `ControllerRef::Opponent`
    // resolves against the source's controller), so the remote CantAttack scan in
    // combat restricts the Arbiter-controller's opponents' creatures.
    //
    // INVARIANT: `per_player_condition` on a CantAttack/CantAttackOrBlock static is
    // only honored on the remote-scan path (`check_static_ability`). The intrinsic
    // `active_static_definitions` path in combat does NOT apply it, so the `affected`
    // filter here must stay a remote filter (opponents' creatures), never SelfRef —
    // a SelfRef CantAttack would be applied unconditionally, bypassing the gate.
    if let Some(tail) = nom_tag_lower(rest, rest, "attack with creatures") {
        if tail.trim_end_matches('.').is_empty() {
            let affected =
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));
            return Some(
                StaticDefinition::new(StaticMode::CantAttack)
                    .affected(affected)
                    .per_player_condition(cond)
                    .description(text.to_string()),
            );
        }
    }

    None
}

/// CR 101.2: Parse casting prohibition from Oracle text.
/// Handles multiple patterns:
/// - "[Subject] can't cast [type] spells" (Steel Golem, Hymn of the Wilds)
/// - "[Type] spells can't be cast" — passive voice (Aether Storm)
/// - "[Subject] can't cast spells with mana value N or less/greater" (Brisela)
/// - "[Subject] can't cast spells with the chosen name" (Alhammarret)
/// - "[Subject] can't cast spells of the chosen type" (Archon of Valor's Reach)
/// - "Enchanted creature's controller can't cast [type] spells" (Brand of Ill Omen)
pub(crate) fn parse_cant_cast_type_spells(tp: &str, text: &str) -> Option<StaticDefinition> {
    // Exclude patterns handled by other parsers
    if nom_primitives::scan_contains(tp, "can't cast more than")
        || nom_primitives::scan_contains(tp, "can't cast spells during")
        || nom_primitives::scan_contains(tp, "can't cast spells from")
        || nom_primitives::scan_contains(tp, "can cast spells only")
    {
        return None;
    }

    // --- Passive voice: "[Type] spells can't be cast" (Aether Storm) ---
    // CR 101.2: "Creature spells can't be cast" → AllPlayers, Creature filter
    if let Some(def) = parse_passive_cant_be_cast(tp, text) {
        return Some(def);
    }

    // --- "Enchanted creature's controller can't cast [type] spells" ---
    // CR 303.4e: Aura-based restriction on the enchanted creature's controller.
    if let Some(def) = parse_enchanted_controller_cant_cast(tp, text) {
        return Some(def);
    }

    // NOTE: "Each opponent who attacked with a creature this turn can't cast
    // spells" is handled earlier in `parse_static_line_inner` by
    // `parse_per_player_conditional_prohibition`, which preserves the per-affected-
    // player turn-activity predicate (CR 101.2 + CR 601.3a) instead of approximating
    // it as an unconditional opponent cast-lock.

    // 1. Strip subject → scope
    let (who, predicate) = strip_casting_prohibition_subject(tp)?;

    // 2. Match "can't cast "
    let after_cant_cast = nom_tag_lower(predicate, predicate, "can't cast ")?;

    // 3. Strip trailing period and parenthetical conditions
    let trimmed = after_cant_cast.trim_end_matches('.');
    // Strip trailing parenthetical like "(as long as this creature is on the battlefield)"
    let trimmed = if let Some(pos) = trimmed.rfind(" (") {
        trimmed[..pos].trim()
    } else {
        trimmed
    };

    // --- "spells with mana value N or less/greater" ---
    if let Some(rest) = nom_tag_lower(trimmed, trimmed, "spells with mana value ") {
        return parse_cant_cast_mana_value(rest, who, text);
    }

    // --- "spells with the chosen name" ---
    if nom_tag_lower(trimmed, trimmed, "spells with the chosen name").is_some() {
        let def = StaticDefinition::new(StaticMode::CantBeCast { who })
            .affected(TargetFilter::HasChosenName)
            .description(text.to_string());
        return Some(def);
    }

    // --- "spells of the chosen type" ---
    if nom_tag_lower(trimmed, trimmed, "spells of the chosen type").is_some() {
        let filter = TargetFilter::Typed(TypedFilter {
            properties: vec![FilterProp::IsChosenCardType],
            ..TypedFilter::default()
        });
        let def = StaticDefinition::new(StaticMode::CantBeCast { who })
            .affected(filter)
            .description(text.to_string());
        return Some(def);
    }

    // --- "spells of the chosen color" ---
    if nom_tag_lower(trimmed, trimmed, "spells of the chosen color").is_some() {
        let def =
            StaticDefinition::new(StaticMode::CantBeCast { who }).description(text.to_string());
        return Some(def);
    }

    // --- "spells with the same name as ..." ---
    // CR 101.2: "can't cast spells with the same name as [reference]" — approximate as
    // blanket prohibition; the name-matching filter is too dynamic for static representation.
    if nom_tag_lower(trimmed, trimmed, "spells with the same name as ").is_some() {
        let def =
            StaticDefinition::new(StaticMode::CantBeCast { who }).description(text.to_string());
        return Some(def);
    }

    // --- "spells with even mana values" / "spells with odd mana values" ---
    if nom_tag_lower(trimmed, trimmed, "spells with even mana value").is_some()
        || nom_tag_lower(trimmed, trimmed, "spells with odd mana value").is_some()
    {
        let def =
            StaticDefinition::new(StaticMode::CantBeCast { who }).description(text.to_string());
        return Some(def);
    }

    // --- "spells by paying alternative costs" ---
    if nom_tag_lower(trimmed, trimmed, "spells by paying alternative cost").is_some() {
        let def =
            StaticDefinition::new(StaticMode::CantBeCast { who }).description(text.to_string());
        return Some(def);
    }

    // --- "[type] spells" / "[type] spell" — standard type-based prohibition ---
    // 4. Require it ends with "spell" or "spells"
    let before_spells = trimmed
        .strip_suffix(" spells") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        .or_else(|| trimmed.strip_suffix(" spell"))?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.

    // 5. Parse type filter from the remaining text
    let type_text = before_spells.trim();
    let spell_filter = if type_text.is_empty() {
        None
    } else {
        let (filter, _) = parse_type_phrase(type_text);
        match &filter {
            TargetFilter::Typed(tf) if !tf.type_filters.is_empty() => Some(filter),
            _ => None,
        }
    };

    // CR 101.2: Wire the casting prohibition scope from the subject parse.
    let mut def =
        StaticDefinition::new(StaticMode::CantBeCast { who }).description(text.to_string());
    if let Some(filter) = spell_filter {
        def = def.affected(filter);
    }
    Some(def)
}

/// Parse passive voice "[Type] spells can't be cast" pattern.
/// E.g., Aether Storm: "Creature spells can't be cast."
/// Also handles "[Type] spells with mana value N or greater/less can't be cast."
pub(crate) fn parse_passive_cant_be_cast(tp: &str, text: &str) -> Option<StaticDefinition> {
    // Look for "spells can't be cast" suffix
    let trimmed = tp.trim_end_matches('.');
    let before_cant = trimmed.strip_suffix(" can't be cast")?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.

    // Check for "spells with mana value N or less/greater" pattern
    // E.g., "noncreature spells with mana value 4 or greater can't be cast"
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    if let Some(pos) = before_cant.find(" spells with mana value ") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        let type_text = &before_cant[..pos];
        let mv_rest = &before_cant[pos + " spells with mana value ".len()..];
        let (filter, remainder) = parse_type_phrase(type_text);
        if !remainder.trim().is_empty() {
            return None;
        }
        let mut tf = match filter {
            TargetFilter::Typed(tf) if !tf.type_filters.is_empty() => tf,
            _ => return None,
        };
        // Parse mana value condition
        if let Some((n, after_n)) = parse_number(mv_rest) {
            let after_n = after_n.trim_start();
            if nom_tag_lower(after_n, after_n, "or greater").is_some() {
                tf = tf.properties(vec![FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: n as i32 },
                }]);
            } else if nom_tag_lower(after_n, after_n, "or less").is_some() {
                tf = tf.properties(vec![FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: n as i32 },
                }]);
            }
        }
        return Some(
            StaticDefinition::new(StaticMode::CantBeCast {
                who: ProhibitionScope::AllPlayers,
            })
            .affected(TargetFilter::Typed(tf))
            .description(text.to_string()),
        );
    }

    // --- "[Type] spells with {X} in their mana costs can't be cast" (passive voice) ---
    // CR 101.2 + CR 107.3: Gaddock Teeg class — prohibits casting spells whose
    // printed mana cost contains an {X} symbol. Combines an optional type prefix
    // ("noncreature") with the `HasXInManaCost` filter property. Resolved at cast
    // time by `cant_cast_filter_matches` → `SpellCastRecord.has_x_in_cost`.
    fn parse_passive_x_mana_cost_prefix(input: &str) -> OracleResult<'_, &str> {
        let (input, type_text) = terminated(
            take_until(" spells with {x} in "),
            tag(" spells with {x} in "),
        )
        .parse(input)?;
        let (input, _) = alt((tag("their mana costs"), tag("their mana cost"))).parse(input)?;
        let (input, _) = opt(tag(".")).parse(input)?;
        let (input, _) = eof(input)?;
        Ok((input, type_text))
    }
    if let Ok((_, type_text)) = parse_passive_x_mana_cost_prefix(before_cant) {
        let (filter, remainder) = parse_type_phrase(type_text);
        if remainder.trim().is_empty() {
            // Only accept Typed filters with concrete type_filters; reject
            // unsupported shapes (AnyOf, bare Any) to avoid silently broadening
            // the prohibition scope.
            let tf = match filter {
                TargetFilter::Typed(tf) if !tf.type_filters.is_empty() => tf,
                _ => return None,
            };
            let tf = tf.properties(vec![FilterProp::HasXInManaCost]);
            return Some(
                StaticDefinition::new(StaticMode::CantBeCast {
                    who: ProhibitionScope::AllPlayers,
                })
                .affected(TargetFilter::Typed(tf))
                .description(text.to_string()),
            );
        }
    }

    // --- "Spells with the chosen name can't be cast" (passive voice) ---
    // CR 101.2 + CR 201.2: the name-lock hatebears — Meddling Mage, Nevermore,
    // Voidstone Gargoyle. The active-voice equivalent ("[subject] can't cast
    // spells with the chosen name") is handled in `parse_cant_cast_type_spells`;
    // mirror it here for the passive form. `HasChosenName` is resolved at cast
    // time by `cant_cast_filter_matches` against the source's chosen card name.
    if let Some(rest) = nom_tag_lower(before_cant, before_cant, "spells with the chosen name") {
        if rest.trim().is_empty() {
            return Some(
                StaticDefinition::new(StaticMode::CantBeCast {
                    who: ProhibitionScope::AllPlayers,
                })
                .affected(TargetFilter::HasChosenName)
                .description(text.to_string()),
            );
        }
    }

    // Require " spells" at the end of the subject
    let type_text = before_cant.strip_suffix(" spells")?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.

    let (filter, remainder) = parse_type_phrase(type_text);
    if !remainder.trim().is_empty() {
        return None;
    }
    match &filter {
        TargetFilter::Typed(tf) if !tf.type_filters.is_empty() => {}
        _ => return None,
    }

    Some(
        StaticDefinition::new(StaticMode::CantBeCast {
            who: ProhibitionScope::AllPlayers,
        })
        .affected(filter)
        .description(text.to_string()),
    )
}

/// CR 101.2: Parse "During [time], [subject] can't cast [type] spells [or activate abilities]"
/// patterns where the temporal clause appears as a leading prefix.
///
/// Handles:
/// - "During your turn, your opponents can't cast spells or activate abilities..."
/// - "During combat, players can't cast instant spells or activate abilities..."
pub(crate) fn parse_temporal_prefix_cant_cast(tp: &str, text: &str) -> Option<StaticDefinition> {
    // Require "during " prefix
    let after_during = nom_tag_lower(tp, tp, "during ")?;

    // Parse temporal condition
    let (when, after_when) =
        if let Some(rest) = nom_tag_lower(after_during, after_during, "your turn") {
            (CastingProhibitionCondition::DuringYourTurn, rest)
        } else {
            let rest = nom_tag_lower(after_during, after_during, "combat")?;
            (CastingProhibitionCondition::DuringCombat, rest)
        };

    // Require ", " separator after temporal clause
    let after_comma = nom_tag_lower(after_when, after_when, ", ")?;

    // Extract subject scope
    let (who, predicate) = strip_casting_prohibition_subject(after_comma)?;

    // Match "can't cast "
    let after_cant_cast = nom_tag_lower(predicate, predicate, "can't cast ")?;

    // Strip trailing period and "or activate abilities..." suffix
    let trimmed = after_cant_cast.trim_end_matches('.');
    let trimmed = trimmed
        .split(" or activate abilities")
        .next()
        .unwrap_or(trimmed)
        .trim();

    // Extract optional spell type filter: "instant spells", "spells", etc.
    let spell_filter = if let Some(before_spells) = trimmed
        .strip_suffix(" spells") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        .or_else(|| trimmed.strip_suffix(" spell"))
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    {
        let type_text = before_spells.trim();
        if type_text.is_empty() || type_text == "spells" {
            None
        } else {
            let (filter, _) = parse_type_phrase(type_text);
            match &filter {
                TargetFilter::Typed(tf) if !tf.type_filters.is_empty() => Some(filter),
                _ => None,
            }
        }
    } else if trimmed == "spells" || trimmed.is_empty() {
        None
    } else {
        return None;
    };

    let mut def = StaticDefinition::new(StaticMode::CantCastDuring { who, when })
        .description(text.to_string());
    if let Some(filter) = spell_filter {
        def = def.affected(filter);
    }
    Some(def)
}

/// Parse "Enchanted creature's controller can't cast [type] spells" pattern.
/// E.g., Brand of Ill Omen: "Enchanted creature's controller can't cast creature spells."
pub(crate) fn parse_enchanted_controller_cant_cast(
    tp: &str,
    text: &str,
) -> Option<StaticDefinition> {
    let rest = nom_tag_lower(tp, tp, "enchanted creature's controller ")
        .or_else(|| nom_tag_lower(tp, tp, "enchanted creature\u{2019}s controller "))?;
    let after_cant_cast = nom_tag_lower(rest, rest, "can't cast ")
        .or_else(|| nom_tag_lower(rest, rest, "can\u{2019}t cast "))?;

    let trimmed = after_cant_cast.trim_end_matches('.');
    let before_spells = trimmed
        .strip_suffix(" spells") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        .or_else(|| trimmed.strip_suffix(" spell"))?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.

    let type_text = before_spells.trim();
    let spell_filter = if type_text.is_empty() {
        None
    } else {
        let (filter, _) = parse_type_phrase(type_text);
        match &filter {
            TargetFilter::Typed(tf) if !tf.type_filters.is_empty() => Some(filter),
            _ => None,
        }
    };

    let mut def = StaticDefinition::new(StaticMode::CantBeCast {
        who: ProhibitionScope::EnchantedCreatureController,
    })
    .description(text.to_string());
    if let Some(filter) = spell_filter {
        def = def.affected(filter);
    }
    Some(def)
}

/// Parse "mana value N or less" / "mana value N or greater" from the remainder
/// after "spells with mana value ".
pub(crate) fn parse_cant_cast_mana_value(
    rest: &str,
    who: ProhibitionScope,
    text: &str,
) -> Option<StaticDefinition> {
    let (n, after_n) = parse_number(rest)?;
    let after_n = after_n.trim_start();

    let prop = if nom_tag_lower(after_n, after_n, "or less").is_some() {
        FilterProp::Cmc {
            comparator: Comparator::LE,
            value: QuantityExpr::Fixed { value: n as i32 },
        }
    } else if nom_tag_lower(after_n, after_n, "or greater").is_some() {
        FilterProp::Cmc {
            comparator: Comparator::GE,
            value: QuantityExpr::Fixed { value: n as i32 },
        }
    } else {
        return None;
    };

    let filter = TargetFilter::Typed(TypedFilter {
        properties: vec![prop],
        ..TypedFilter::default()
    });
    Some(
        StaticDefinition::new(StaticMode::CantBeCast { who })
            .affected(filter)
            .description(text.to_string()),
    )
}

/// CR 101.2: Parse per-turn draw limit from Oracle text.
/// Handles "[Subject] can't draw more than N card(s) each turn."
/// E.g., Spirit of the Labyrinth: "Each player can't draw more than one card each turn."
/// E.g., Narset, Parter of Veils: "Each opponent can't draw more than one card each turn."
pub(crate) fn parse_per_turn_draw_limit(tp: &str, text: &str) -> Option<StaticDefinition> {
    // 1. Strip subject → scope
    let (who, predicate) = strip_casting_prohibition_subject(tp)?;

    // 2. Match "can't draw more than "
    let after_more_than = nom_tag_lower(predicate, predicate, "can't draw more than ")?;

    // 3. Extract limit count
    let (max, rest) = parse_number(after_more_than)?;

    // 4. Require "card(s) each turn" suffix via nom combinator
    let rest = rest.trim_start();
    let rest_lower = rest.to_lowercase();
    alt((
        value(
            (),
            tag::<&str, &str, (&str, nom::error::ErrorKind)>("card each turn"),
        ),
        value((), tag("cards each turn")),
    ))
    .parse(rest_lower.as_str())
    .ok()?;

    Some(
        StaticDefinition::new(StaticMode::PerTurnDrawLimit { who, max })
            .description(text.to_string()),
    )
}

/// CR 101.2 / CR 121.3: Parse blanket draw prohibition from Oracle text.
/// Handles "[Subject] can't draw cards."
/// E.g., Omen Machine: "Players can't draw cards."
/// E.g., Maralen of the Mornsong: "Players can't draw cards."
pub(crate) fn parse_cant_draw_cards(tp: &str, text: &str) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;

    let (who, predicate) = strip_casting_prohibition_subject(tp)?;
    let rest = nom_tag_lower(predicate, predicate, "can't draw ")
        .or_else(|| nom_tag_lower(predicate, predicate, "can\u{2019}t draw "))?;

    alt((
        value((), tag::<_, _, VE<'_>>("cards")),
        value((), tag::<_, _, VE<'_>>("a card")),
    ))
    .parse(rest.trim_end_matches('.'))
    .ok()?;

    Some(StaticDefinition::new(StaticMode::CantDraw { who }).description(text.to_string()))
}

/// Parse the subject of "[type] cards in [zones] can't enter the battlefield".
/// CR 604.3: Extracts the card type filter and zone restrictions into a TypedFilter.
pub(crate) fn parse_cant_enter_battlefield_subject(tp: &TextPair) -> TargetFilter {
    let mut card_type = None;
    let mut properties = Vec::new();

    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    if let Some(pos) = tp.lower.find("can't enter the battlefield") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        let subject = tp.lower[..pos].trim();
        // "creature cards in graveyards and libraries" → card_type = Creature
        if let Some(type_part) = subject.split(" cards").next() {
            card_type = match type_part.trim() {
                "creature" => Some(TypeFilter::Creature), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                "artifact" => Some(TypeFilter::Artifact), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                "enchantment" => Some(TypeFilter::Enchantment), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                "instant" => Some(TypeFilter::Instant), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                "sorcery" => Some(TypeFilter::Sorcery), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                _ => None,
            };
        }
    }

    let zones = parse_zone_names_from_tp(tp);
    if !zones.is_empty() {
        properties.push(FilterProp::InAnyZone { zones });
    }

    TargetFilter::Typed(TypedFilter {
        type_filters: card_type.into_iter().collect(),
        properties,
        ..TypedFilter::default()
    })
}

/// CR 604.2 + CR 601.2a + CR 305.1: Parse graveyard play/cast permission statics.
/// CR 402.2 + CR 514.1: Parse maximum hand size modification patterns.
///
/// Patterns:
/// - "Your maximum hand size is [N]." → SetTo(N)
/// - "Your maximum hand size is increased by [N]." → AdjustedBy(+N)
/// - "Your maximum hand size is reduced by [N]." → AdjustedBy(-N)
/// - "Each opponent's maximum hand size is reduced by [N]." → AdjustedBy(-N), opponent scope
/// - "The chosen player's maximum hand size is [N]." → SetTo(N), chosen player scope
/// - "Your maximum hand size is equal to [quantity]." → EqualTo(quantity)
pub(crate) fn try_parse_max_hand_size(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    type NomErr<'a> = OracleError<'a>;

    let lower_trimmed = tp.lower.trim_end_matches('.');

    // Dispatch on subject prefix to determine affected filter
    let (affected, rest) = if let Ok((r, _)) =
        tag::<_, _, NomErr>("your maximum hand size is ").parse(lower_trimmed)
    {
        (
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
            r,
        )
    } else if let Ok((r, _)) =
        tag::<_, _, NomErr>("each opponent's maximum hand size is ").parse(lower_trimmed)
    {
        (
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            r,
        )
    } else if let Ok((r, _)) =
        tag::<_, _, NomErr>("the chosen player's maximum hand size is ").parse(lower_trimmed)
    {
        (TargetFilter::Player, r)
    } else {
        return None;
    };

    // Parse the modification kind
    let modification = if let Ok((num_rest, _)) = tag::<_, _, NomErr>("increased by ").parse(rest) {
        let (_, n) = nom_primitives::parse_number(num_rest).ok()?;
        HandSizeModification::AdjustedBy(n as i32)
    } else if let Ok((num_rest, _)) = tag::<_, _, NomErr>("reduced by ").parse(rest) {
        let (_, n) = nom_primitives::parse_number(num_rest).ok()?;
        HandSizeModification::AdjustedBy(-(n as i32))
    } else if let Ok((qty_rest, _)) = tag::<_, _, NomErr>("equal to ").parse(rest) {
        // "equal to the number of hour counters on ~" → dynamic quantity
        let qty_ref = nom_primitives::parse_number(qty_rest)
            .ok()
            .map(|(_, n)| QuantityExpr::Fixed { value: n as i32 })
            .or_else(|| parse_quantity_ref(qty_rest).map(|qr| QuantityExpr::Ref { qty: qr }))?;
        HandSizeModification::EqualTo(qty_ref)
    } else {
        // Plain "is [N]" → SetTo
        let (_, n) = nom_primitives::parse_number(rest).ok()?;
        HandSizeModification::SetTo(n)
    };

    Some(
        StaticDefinition::new(StaticMode::MaximumHandSize { modification })
            .affected(affected)
            .description(text.to_string()),
    )
}

/// Handles three patterns, each with an optional alt-cost rider:
/// 1. "Once during each of your turns, you may cast [filter] from your graveyard[ rider]." (Lurrus, Karador)
/// 2. "You may play [filter] from your graveyard[ rider]." (Crucible of Worlds, Icetill Explorer)
/// 3. "You may cast [filter] from your graveyard[ rider]." (Conduit of Worlds, Ninja Teen)
///
/// Rider grammar (both possessive and number-insensitive):
///   " using " alt("its" | "their") " " <keyword_name> " " alt("ability" | "abilities")
///
/// When present, the rider injects `FilterProp::HasKeywordKind { value: kind }` into the
/// returned `affected: TargetFilter`, so eligibility is gated on that granted keyword.
/// CR 604.2 + CR 118.9: static continuous effect granting permission to cast via an
/// alternative cost associated with the named keyword.
pub(crate) fn try_parse_graveyard_cast_permission(
    text: &str,
    lower: &str,
) -> Option<StaticDefinition> {
    // CR 110.4 + CR 305.1 + CR 601.2a: Muldrotha-class — "During each of your
    // turns, you may play a land or cast a permanent spell of each permanent
    // type from your graveyard." A single permission grants both the land
    // play and permanent-spell cast, with each permanent type acting as an
    // independent per-turn slot. The reminder text "(If a card has multiple
    // permanent types, choose one as you play it.)" is stripped upstream by
    // `strip_reminder_text`, so this matcher only sees the rules-text clause.
    //
    // The combined "play a land or cast a permanent spell" wording is a
    // single-sentence shape — no other shipping card uses it. Match it as a
    // fixed nom prefix and bail out immediately with the typed
    // `OncePerTurnPerPermanentType` frequency + `CardPlayMode::Play` (Play
    // covers both "play a land" and "cast a permanent spell" branches).
    // Accept both the canonical "play a land or cast" Oracle wording and the
    // older "play a land and cast" printing — both are equivalent under CR
    // 110.4 (the per-permanent-type slot is what enforces the cap, not the
    // conjunction). Try each prefix in turn via the file-wide `or_else`
    // chaining idiom (see e.g. the article-stripping `"a "`/`"an "` chain
    // below) — both calls are nom `tag()` matches under the hood.
    let muldrotha_alt = nom_tag_lower(
        lower,
        lower,
        "during each of your turns, you may play a land or cast a permanent spell of each permanent type from your graveyard",
    )
    .or_else(|| {
        nom_tag_lower(
            lower,
            lower,
            "during each of your turns, you may play a land and cast a permanent spell of each permanent type from your graveyard",
        )
    });
    if muldrotha_alt.is_some() {
        // Affected filter: any permanent (CR 110.4 — artifact, battle,
        // creature, enchantment, land, planeswalker). The downstream slot
        // picker enforces the per-permanent-type per-turn limit.
        let affected = TargetFilter::Typed(TypedFilter::new(TypeFilter::Permanent));
        return Some(
            StaticDefinition::new(StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::OncePerTurnPerPermanentType,
                play_mode: CardPlayMode::Play,
                graveyard_destination_replacement: None,
            })
            .affected(affected)
            .description(text.to_string()),
        );
    }

    // CR 305.1 + CR 601.2a + CR 700.6: Disjunctive once-per-turn permission —
    // "Once during each of your turns, you may play a <land-filter> or cast a
    // <spell-filter> from your graveyard." (The Eighth Doctor, Serra Paragon).
    // `Play` mode covers both the land-play branch (CR 305.1) and the
    // spell-cast branch (CR 601.2a); the two branch filters are merged so the
    // permission's `affected` admits either class of card. Parsed before the
    // single-verb dispatch below because the disjunctive lead shares the
    // "once during each of your turns, you may" prefix with the cast-only form.
    //
    // The granted leave-battlefield rider ("If you do, it gains \"…exile…\"")
    // that may follow is a CR 614.1a Moved replacement on the *resolved*
    // permanent (origin Battlefield → Exile), NOT the stack-exit
    // `graveyard_destination_replacement` (which is structurally unreachable for
    // permanent spells). Attaching it requires a resolution-grant carrier on the
    // permission. Until that exists, decline this parser so the unmodeled rider
    // remains an honest coverage gap instead of being silently dropped.
    if let Some(def) = try_parse_disjunctive_graveyard_cast_permission(text, lower) {
        return Some(def);
    }

    // Determine pattern and extract the rest after the prefix
    let (rest, frequency, play_mode) = if let Some(r) = nom_tag_lower(
        lower,
        lower,
        "once during each of your turns, you may cast ",
    ) {
        (r, CastFrequency::OncePerTurn, CardPlayMode::Cast)
    } else if let Some(r) = nom_tag_lower(lower, lower, "you may play ") {
        (r, CastFrequency::Unlimited, CardPlayMode::Play)
    } else {
        let r = nom_tag_lower(lower, lower, "you may cast ")?;
        // Only match if "from your graveyard" follows — avoid catching other "you may cast" statics
        if !nom_primitives::scan_contains(r, "from your graveyard") {
            return None;
        }
        (r, CastFrequency::Unlimited, CardPlayMode::Cast)
    };

    let (filter_text, trailing) = nom_primitives::split_once_on(rest, " from your graveyard")
        .ok()
        .map(|(_, pair)| pair)?;

    // Strip leading article via nom tag ("a ", "an ")
    let filter_text = nom_tag_lower(filter_text, filter_text, "a ")
        .or_else(|| nom_tag_lower(filter_text, filter_text, "an "))
        .unwrap_or(filter_text);

    // Remove " spell"/" spells" — parse_type_phrase expects bare type words.
    // "lands" is already a valid type phrase, so no stripping needed for Play mode.
    let cleaned: Cow<str> = if nom_primitives::scan_contains(filter_text, "spells") {
        Cow::Owned(filter_text.replacen(" spells", "", 1))
    } else if nom_primitives::scan_contains(filter_text, "spell") {
        Cow::Owned(filter_text.replacen(" spell", "", 1))
    } else {
        Cow::Borrowed(filter_text)
    };

    let (filter, self_ref_permission) = parse_graveyard_permission_filter(&cleaned);

    // Parse optional alt-cost rider from the text after "from your graveyard".
    let rider_kind = parse_alt_cost_rider(trailing).ok().map(|(_, k)| k);
    let graveyard_destination_replacement = parse_exile_spell_cast_this_way_rider(trailing)
        .is_ok()
        .then_some(Zone::Exile);
    let condition = parse_graveyard_permission_condition(trailing)
        .ok()
        .and_then(|(rest, condition)| rest.is_empty().then_some(condition));

    let affected = if let Some(kind) = rider_kind {
        inject_keyword_kind_filter_prop(filter, kind)
    } else {
        filter
    };

    let mut def = StaticDefinition::new(StaticMode::GraveyardCastPermission {
        frequency,
        play_mode,
        graveyard_destination_replacement,
    })
    .affected(affected)
    .description(text.to_string());
    if let Some(condition) = condition {
        def = def.condition(condition);
    }
    if self_ref_permission {
        def = def.active_zones(vec![Zone::Graveyard]);
    }
    Some(def)
}

/// CR 305.1 + CR 601.2a + CR 700.6: Parse the disjunctive once-per-turn
/// graveyard play/cast permission — "Once during each of your turns, you may
/// play a <land-filter> or cast a <spell-filter> from your graveyard." — into a
/// single `GraveyardCastPermission { frequency: OncePerTurn, play_mode: Play }`
/// whose `affected` filter is the union of the two branch filters.
///
/// Two zone-placement variants are accepted (both observed in printed cards):
/// - tail-zone: "play a <land> or cast a <spell> from your graveyard"
///   (The Eighth Doctor — "from your graveyard" once, at the end).
/// - per-branch-zone: "play a <land> from your graveyard or cast a <spell> from
///   your graveyard" (Serra Paragon — "from your graveyard" on each branch).
///
/// The parser parses whatever filter each branch carries (it does NOT assume
/// "historic"): Serra Paragon's spell branch carries "mana value 3 or less",
/// proving the filter axis is general. When both branches resolve to the same
/// filter (The Eighth Doctor: both "historic permanent"), the union collapses
/// to that single filter; otherwise it emits `TargetFilter::Or`.
///
/// A trailing rider ("If you do, it gains \"…\"") is intentionally rejected:
/// the granted leave-battlefield exile rider is a CR 614.1a Moved replacement
/// on the resolved permanent. Parsing only the permission would make coverage
/// report support while dropping rules text.
fn try_parse_disjunctive_graveyard_cast_permission(
    text: &str,
    lower: &str,
) -> Option<StaticDefinition> {
    // CR 601.2a: Frequency prefix. Only the once-per-turn lead is a real printed
    // shape for this disjunctive form today; accept both the canonical wording
    // and the shorter "once each turn" synonym via the file-wide `or_else` chain.
    let rest = nom_tag_lower(
        lower,
        lower,
        "once during each of your turns, you may play ",
    )
    .or_else(|| nom_tag_lower(lower, lower, "once each turn, you may play "))?;
    if nom_primitives::scan_contains(rest, "if you do, it gains") {
        return None;
    }

    // CR 305.1 + CR 601.2a: The disjunction connector " or cast " splits the
    // land-play branch from the spell-cast branch. `split_once_on` is the
    // structural "everything up to delimiter" combinator.
    let (land_branch, spell_branch) = nom_primitives::split_once_on(rest, " or cast ")
        .ok()
        .map(|(_, pair)| pair)?;

    // The spell branch must end with the source-zone anchor. Strip a per-branch
    // " from your graveyard" if present (Serra form); otherwise the tail-zone
    // anchor (Eighth form) lives on the spell branch alone.
    let spell_branch = strip_graveyard_zone_anchor(spell_branch)?;

    // The land branch optionally carries its own zone anchor (Serra form); strip
    // it when present so the bare filter phrase reaches the filter parser.
    let land_branch = strip_graveyard_zone_anchor(land_branch).unwrap_or(land_branch);

    let land_filter = parse_graveyard_branch_filter(land_branch)?;
    let spell_filter = parse_graveyard_branch_filter(spell_branch)?;

    // CR 700.6: a land is itself a permanent, so when both branches resolve to
    // the same typed filter (historic land ⊆ historic permanent), collapse the
    // union to that single filter rather than emitting a redundant `Or`.
    let affected = if land_filter == spell_filter {
        land_filter
    } else {
        TargetFilter::Or {
            filters: vec![land_filter, spell_filter],
        }
    };

    Some(
        StaticDefinition::new(StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::OncePerTurn,
            // CR 305.1: `Play` covers both the land-play and spell-cast branches.
            play_mode: CardPlayMode::Play,
            // Stack-exit redirect is wrong for the granted leave-battlefield
            // rider (see doc comment); leave it unset.
            graveyard_destination_replacement: None,
        })
        .affected(affected)
        .description(text.to_string()),
    )
}

/// Strip the trailing " from your graveyard" source-zone anchor (plus any
/// leading whitespace) from a branch phrase, returning the bare filter text.
/// Returns `None` when the anchor is absent.
fn strip_graveyard_zone_anchor(branch: &str) -> Option<&str> {
    nom_primitives::split_once_on(branch, " from your graveyard")
        .ok()
        .map(|(_, (before, _))| before.trim())
}

/// Strip the leading article and trailing " spell"/" spells" from a single
/// disjunctive-permission branch, then resolve it through
/// `parse_graveyard_permission_filter`. Mirrors the cleanup the single-verb
/// graveyard parser performs, so both paths share one filter grammar. Returns
/// `None` if the branch does not resolve to a usable typed filter.
fn parse_graveyard_branch_filter(branch: &str) -> Option<TargetFilter> {
    let branch = branch.trim();
    // Strip the leading article ("a "/"an ").
    let branch = nom_tag_lower(branch, branch, "a ")
        .or_else(|| nom_tag_lower(branch, branch, "an "))
        .unwrap_or(branch);

    // Drop " spell"/" spells" so `parse_type_phrase` sees the bare type word;
    // "land"/"lands" is already a valid type phrase and needs no stripping.
    let cleaned: Cow<str> = if nom_primitives::scan_contains(branch, "spells") {
        Cow::Owned(branch.replacen(" spells", "", 1))
    } else if nom_primitives::scan_contains(branch, "spell") {
        Cow::Owned(branch.replacen(" spell", "", 1))
    } else {
        Cow::Borrowed(branch)
    };

    let (filter, _self_ref) = parse_graveyard_permission_filter(&cleaned);
    // Reject the unparseable fallbacks so a branch we cannot model declines the
    // whole disjunctive parse rather than silently admitting everything.
    match &filter {
        TargetFilter::Typed(tf) if !tf.type_filters.is_empty() => Some(filter),
        _ => None,
    }
}

/// CR 122.2 + CR 113.6b: Parse the counter-persistence static — "Counters
/// remain on ~ as it moves to any zone other than [zone list]." Overrides the
/// default CR 122.2 rule that counters cease to exist on a zone change, except
/// for moves into the excluded destination zones.
///
/// Class members (verbatim): Me, the Immortal and Skullbriar, the Walking
/// Grave, both "... other than a player's hand or library" →
/// `excluded_zones = [Hand, Library]`. The zone-list tail is parsed by a
/// combinator (`alt`) so additional "other than [zones]" exclusion sets slot
/// in without a new variant.
///
/// Anchors on the self-reference glyph `~` (the card name is normalized to `~`
/// upstream), so the parser is name-agnostic and covers the whole class.
pub(crate) fn try_parse_counters_persist_across_zones(
    text: &str,
    lower: &str,
) -> Option<StaticDefinition> {
    // "counters remain on ~ as it moves to any zone other than " <zone-list>
    let rest = nom_tag_lower(
        lower,
        lower,
        "counters remain on ~ as it moves to any zone other than ",
    )?;
    // allow-noncombinator: trailing-period/whitespace cleanup on the tokenized
    // tail before the zone-list combinator runs (matches the file-wide idiom at
    // lines 84/264/304/360); the actual parsing dispatch is the `alt` below.
    let rest = rest.trim_end_matches('.').trim();
    // Zone-list combinator: the only shipping exclusion set is "a player's hand
    // or library". Expressed as an `alt` so future exclusion phrasings are
    // added as sibling arms rather than string-equality checks.
    let excluded_zones = parse_excluded_zone_list(rest)?;
    Some(
        StaticDefinition::new(StaticMode::CountersPersistAcrossZones { excluded_zones })
            // CR 122.2: the persistence applies to this object's own counters.
            .affected(TargetFilter::SelfRef)
            // CR 113.6b: per Me's ruling the ability is read from the zone the
            // object is moving FROM; it must function in every zone the object
            // can carry counters out of, so it is active in all zones.
            .active_zones(vec![
                Zone::Battlefield,
                Zone::Graveyard,
                Zone::Exile,
                Zone::Command,
                Zone::Stack,
            ])
            .description(text.to_string()),
    )
}

/// CR 122.2: Parse the "any zone other than [zones]" exclusion list into the
/// typed destination-zone set whose moves still clear counters. Combinator-only
/// (no `contains` dispatch); add sibling `alt` arms for new exclusion phrasings.
fn parse_excluded_zone_list(rest: &str) -> Option<Vec<Zone>> {
    let res: nom::IResult<&str, Vec<Zone>, OracleError<'_>> = value(
        vec![Zone::Hand, Zone::Library],
        alt((
            tag::<_, _, OracleError<'_>>("a player's hand or library"),
            tag::<_, _, OracleError<'_>>("a player's library or hand"),
        )),
    )
    .parse(rest);
    res.ok()
        .and_then(|(remainder, zones)| remainder.trim().is_empty().then_some(zones))
}

/// CR 601.2a + CR 113.6b + CR 118.9: Parse the Maralen-class exile cast
/// permission line: "Once each turn, you may cast [filter] from among cards
/// exiled with ~ this turn [without paying its mana cost]." Mirrors
/// `try_parse_graveyard_cast_permission` for the exile-pool sibling.
///
/// Accepted shapes:
/// - "once each turn, you may cast a spell with mana value less than or equal
///   to <quantity_ref> from among cards exiled with ~ this turn without paying
///   its mana cost." (Maralen, Fae Ascendant)
/// - The longer "once during each of your turns, you may cast …" synonym.
/// - Unlimited shape ("you may cast …") is left for a future printing — Maralen
///   is the only shipping card today so the `Unlimited` branch is not gated on
///   any anchor; adding it requires an Oracle-confirmed sibling printing first.
///
/// Returns `None` for shapes outside this class (graveyard/top-of-library/hand
/// permissions all anchor on different phrases earlier in the dispatch chain).
pub(crate) fn try_parse_exile_cast_permission(text: &str, lower: &str) -> Option<StaticDefinition> {
    // CR 601.2a: Frequency prefix. Both "once each turn" (Maralen) and the
    // longer "once during each of your turns" synonym map to `OncePerTurn`.
    // Both prefixes are tried via the file-wide `or_else` chain — adding an
    // `Unlimited` ("you may cast …") sibling needs an Oracle-confirmed
    // printing to disambiguate from the existing graveyard / hand handlers.
    let rest = nom_tag_lower(lower, lower, "once each turn, you may cast ").or_else(|| {
        nom_tag_lower(
            lower,
            lower,
            "once during each of your turns, you may cast ",
        )
    })?;
    let frequency = CastFrequency::OncePerTurn;

    // Strip the leading article — `parse_type_phrase` expects the bare noun.
    let rest = nom_tag_lower(rest, rest, "a ")
        .or_else(|| nom_tag_lower(rest, rest, "an "))
        .unwrap_or(rest);

    // CR 113.6b: Anchor on " from among cards exiled with " — the
    // class-defining phrase. Anything before is the affected filter; anything
    // after is the source self-reference plus optional alt-cost / "this turn"
    // markers.
    let (filter_text, trailing) =
        nom_primitives::split_once_on(rest, " from among cards exiled with ")
            .ok()
            .map(|(_, pair)| pair)?;

    // Drop trailing " spell"/" spells" so `parse_type_phrase` sees the bare
    // type. Mirrors the graveyard / top-of-library / hand sibling parsers.
    let cleaned: Cow<str> = if nom_primitives::scan_contains(filter_text, "spells") {
        Cow::Owned(filter_text.replacen(" spells", "", 1))
    } else if nom_primitives::scan_contains(filter_text, "spell") {
        Cow::Owned(filter_text.replacen(" spell", "", 1))
    } else {
        Cow::Borrowed(filter_text)
    };

    // `parse_type_phrase` already composes the dynamic "with mana value …"
    // suffix through `parse_mana_value_suffix`, so Maralen's filter
    // ("spell with mana value less than or equal to the number of Elves and
    // Faeries you control") resolves through one call — no bespoke combinator
    // chain needed here.
    let (filter, remainder) = parse_type_phrase(&cleaned);
    if !remainder.trim().is_empty() {
        // Strict: any unconsumed remainder is a filter shape we don't yet
        // model. Decline so the line either dispatches to the next handler or
        // surfaces as Unimplemented (which the swallow detector picks up as a
        // coverage gap rather than a misparse).
        return None;
    }

    // CR 113.6b + CR 201.5: The source reference is normalized to `~` for
    // `SELF_REF_TYPE_PHRASES` (this creature, this permanent, …) but left
    // verbatim for `SELF_REF_PARSE_ONLY_PHRASES` ("this card"). Accept either
    // form so the static covers future cards that lean on the parse-only set.
    let after_source = strip_self_reference(trailing)?;

    // CR 113.6b: Optional "this turn" suffix selects the per-turn rolling pool
    // (Maralen). Without it the permission reads the persistent `exile_links`
    // pool (Serpent's Soul-Jar).
    let (after_this_turn, pool) =
        if let Some(rest) = nom_tag_lower(after_source, after_source, " this turn") {
            (rest, ExileCardPool::ThisTurn)
        } else {
            let tail = after_source.trim().trim_start_matches('.').trim();
            if !tail.is_empty() {
                return None;
            }
            (after_source, ExileCardPool::Persistent)
        };

    // CR 118.9a: Optional " without paying its mana cost" / "their mana costs"
    // alt-cost rider selects the `WithoutPayingManaCost` shape; absence leaves
    // the static at `PayNormalCost`. The `scan_contains` is the same idiom the
    // sibling graveyard parser uses for its trailing alt-cost detection.
    let cost = if nom_primitives::scan_contains(after_this_turn, "without paying its mana cost")
        || nom_primitives::scan_contains(after_this_turn, "without paying their mana cost")
    {
        ExileCastCost::WithoutPayingManaCost
    } else {
        ExileCastCost::PayNormalCost
    };

    Some(
        StaticDefinition::new(StaticMode::ExileCastPermission {
            frequency,
            play_mode: CardPlayMode::Cast,
            cost,
            // CR 113.6b: The "this turn" suffix scoped the pool to the per-turn
            // rolling list; this Maralen-class permission has no turn-of-use
            // restriction beyond its once-each-turn frequency.
            pool,
            timing: ExileCastTiming::AnyTime,
        })
        .affected(filter)
        .description(text.to_string()),
    )
}

/// CR 113.6b + CR 305.1 + CR 406.6 + CR 117.1c: Parse the persistent,
/// name-anchored exile-play permission — "[During your turn, ][as long as
/// <condition>, ]you may play lands and cast spells from among cards exiled
/// with ~[.]" (The Matrix of Time), the compact "you may play cards exiled
/// with ~" wording (Evendo Brushrazer), and the "you may look at cards exiled
/// with ~, and you may play lands and cast spells from among those cards."
/// variant (the Prosper/Tibalt impulse-commander class).
///
/// Distinguished from `try_parse_exile_cast_permission` (Maralen) by:
/// - **No "this turn" pool bound** → `pool: ExileCardPool::Persistent` reads the
///   lifetime `exile_links` set rather than the per-turn rolling list.
/// - **`Unlimited` frequency** → no once-per-turn cast slot.
/// - **`play_mode: Play`** → CR 305.1: "play lands and cast spells" collapses to
///   `Play`, which covers both lands (played) and non-land cards (cast). The
///   affected filter is `Any`; the persistent pool itself is the scope.
///
/// The optional leading "during your turn, " → `timing: YourTurnOnly`
/// (CR 117.1c). The "you may look at …" preamble is purely informational
/// (CR 601.3f: the controller must be able to look at the cards to cast them;
/// for face-up impulse exile this is always satisfiable) and is consumed
/// without affecting the emitted permission.
pub(crate) fn try_parse_persistent_exile_play_permission(
    text: &str,
    lower: &str,
) -> Option<StaticDefinition> {
    // Optional leading timing qualifier. CR 117.1c: "during your turn, " gates
    // the permission to the source controller's turn.
    let (rest, timing) = match nom_tag_lower(lower, lower, "during your turn, ") {
        Some(r) => (r, ExileCastTiming::YourTurnOnly),
        None => (lower, ExileCastTiming::AnyTime),
    };

    let (rest, condition) = match strip_leading_permission_condition(rest) {
        Some((rest, condition)) => (rest, Some(condition)),
        None => (rest, None),
    };

    // Optional "you may look at cards exiled with <self>, and " preamble
    // (CR 601.3f). When present, the play clause uses the "those cards" anaphor
    // rather than re-naming the source; when absent, the play clause names the
    // source directly via "from among cards exiled with <self>".
    let after_look = strip_look_at_exiled_preamble(rest);
    let uses_anaphor = after_look.is_some();
    let rest = after_look.unwrap_or(rest);

    // Core permission phrase. CR 305.1: both "play lands and cast spells" and
    // "play cards" lower to Play mode.
    let after_clause = if let Some(rest) =
        nom_tag_lower(rest, rest, "you may play lands and cast spells from among ")
    {
        // The play clause either names the source ("cards exiled with <self>") or
        // refers back to the look-at preamble's set ("those cards").
        if uses_anaphor {
            nom_tag_lower(rest, rest, "those cards")?
        } else {
            strip_exile_play_source_reference(rest)?
        }
    } else {
        let rest = nom_tag_lower(rest, rest, "you may play ")?;
        if uses_anaphor {
            nom_tag_lower(rest, rest, "those cards")?
        } else {
            strip_exile_play_source_reference(rest)?
        }
    };

    // CR 113.6b: A trailing period is the only permitted remainder; any other
    // tail is an unmodeled shape — decline so it surfaces as a coverage gap
    // rather than a silent misparse.
    let tail = after_clause.trim_start();
    // allow-noncombinator: punctuation cleanup (drop the sentence terminator) on a pre-tokenized chunk, not parsing dispatch.
    let tail = tail.strip_prefix('.').unwrap_or(tail); // allow-noncombinator: punctuation cleanup on a pre-tokenized chunk, not parsing dispatch.
    if !tail.trim().is_empty() {
        return None;
    }

    let mut definition = StaticDefinition::new(StaticMode::ExileCastPermission {
        // CR 601.2a: No once-per-turn cap on this class.
        frequency: CastFrequency::Unlimited,
        // CR 305.1: Play covers lands (played) and non-land cards (cast).
        play_mode: CardPlayMode::Play,
        // CR 305.1 / CR 601.3: Cards are played/cast at their normal cost.
        cost: ExileCastCost::PayNormalCost,
        // CR 406.6: Lifetime per-source exile-link pool.
        pool: ExileCardPool::Persistent,
        timing,
    })
    // CR 305.1: The permission applies to every card in the source's exile
    // pool; the pool itself is the scope, so no type/MV constraint.
    .affected(TargetFilter::Any)
    .description(text.to_string());
    if let Some(condition) = condition {
        definition = definition.condition(condition);
    }

    Some(definition)
}

fn strip_leading_permission_condition(input: &str) -> Option<(&str, StaticCondition)> {
    let (rest, condition) = nom_condition::parse_condition(input).ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(", ").parse(rest).ok()?;
    Some((rest, condition))
}

fn strip_exile_play_source_reference(rest: &str) -> Option<&str> {
    let after_anchor = nom_tag_lower(rest, rest, "cards exiled with ")
        .or_else(|| nom_tag_lower(rest, rest, "the cards exiled with "))?;
    strip_self_reference(after_anchor)
}

/// CR 601.3f + CR 113.6b: Strip the "you may look at cards exiled with
/// <self>, and " informational preamble. Returns the remainder after the
/// conjunction when present, else `None`.
fn strip_look_at_exiled_preamble(lower: &str) -> Option<&str> {
    let rest = std::iter::once("you may look at cards exiled with ")
        .chain(std::iter::once("you may look at the cards exiled with "))
        .find_map(|prefix| nom_tag_lower(lower, lower, prefix))?;
    let rest = strip_self_reference(rest)?;
    nom_tag_lower(rest, rest, ", and ")
}

/// CR 113.6b + CR 201.5: Strip a self-reference token (`~` normalized name, or
/// any `SELF_REF_PARSE_ONLY_PHRASES` spelling like "this card") from the front.
fn strip_self_reference(lower: &str) -> Option<&str> {
    std::iter::once("~")
        .chain(SELF_REF_PARSE_ONLY_PHRASES.iter().copied())
        .chain([
            "this artifact",
            "this permanent",
            "this creature",
            "this equipment",
            "this land",
            "it",
        ])
        .find_map(|phrase| nom_tag_lower(lower, lower, phrase))
}

/// CR 401.5 + CR 118.9 + CR 601.2a: Parse "you may [play|cast] [filter] from
/// the top of your library [rider]" — top-of-library cast permission class
/// (Realmwalker, Future Sight, Magus of the Future, Bolas's Citadel, Vivien
/// on the Hunt static). Mirror of `try_parse_graveyard_cast_permission` but
/// anchored on " from the top of your library" instead of " from your
/// graveyard". Recognises the compound Bolas form "you may play lands and
/// cast spells from the top of your library" and lowers it to a single
/// `play_mode: Play` static with `affected: TargetFilter::Any` (per CR 305.1,
/// `Play` covers both lands and non-land spells).
///
/// The optional alt-cost rider (Bolas: "If you cast a spell this way, pay
/// life equal to its mana value rather than paying its mana cost.") is
/// recognised via the existing `oracle_effect::try_parse_alt_cost_rider`
/// helper and stamped into `StaticMode::TopOfLibraryCastPermission.alt_cost`.
pub(crate) fn try_parse_top_of_library_cast_permission(
    text: &str,
    lower: &str,
) -> Option<StaticDefinition> {
    // Compound Bolas's Citadel form first — "you may play lands and cast
    // spells from the top of your library". Both halves collapse to a single
    // `Play` permission with `affected: Any`: under CR 305.1, `Play` mode
    // already covers lands (played) and non-land spells (cast).
    if let Some(rest) = nom_tag_lower(
        lower,
        lower,
        "you may play lands and cast spells from the top of your library",
    ) {
        let alt_cost = parse_top_of_library_alt_cost_rider(rest, text);
        let mut def = StaticDefinition::new(StaticMode::TopOfLibraryCastPermission {
            play_mode: CardPlayMode::Play,
            // CR 601.2a: The Bolas's Citadel compound form has no per-turn cap.
            frequency: CastFrequency::Unlimited,
            alt_cost,
        })
        .affected(TargetFilter::Any)
        .description(text.to_string());
        if let Some(condition) = parse_top_of_library_permission_condition(rest) {
            def = def.condition(condition);
        }
        return Some(def);
    }

    // CR 601.2a: Optional once-per-turn frequency prefix. "Once each turn, …"
    // (Assemble the Players) and the longer "Once during each of your turns, …"
    // synonym both lower to OncePerTurn; absence keeps the Unlimited shape
    // (Realmwalker, Future Sight). After stripping the prefix, the standard
    // "you may play/cast" verb-dispatch below is matched.
    let (lower, frequency) = if let Some(r) = nom_tag_lower(lower, lower, "once each turn, ")
        .or_else(|| nom_tag_lower(lower, lower, "once during each of your turns, "))
    {
        (r, CastFrequency::OncePerTurn)
    } else {
        (lower, CastFrequency::Unlimited)
    };

    // Standard form: "you may [play|cast] [filter] from the top of your library".
    let (rest, play_mode) = if let Some(r) = nom_tag_lower(lower, lower, "you may play ") {
        (r, CardPlayMode::Play)
    } else {
        let r = nom_tag_lower(lower, lower, "you may cast ")?;
        (r, CardPlayMode::Cast)
    };

    // Anchor on " from the top of your library". The split helper returns
    // (consumed_so_far, after_split) — we need both halves: the filter text
    // sits before the anchor; the optional alt-cost rider sits after.
    let (filter_text, trailing) =
        nom_primitives::split_once_on(rest, " from the top of your library")
            .ok()
            .map(|(_, pair)| pair)?;

    // Strip leading article — `parse_type_phrase` expects the bare noun.
    let filter_text = nom_tag_lower(filter_text, filter_text, "a ")
        .or_else(|| nom_tag_lower(filter_text, filter_text, "an "))
        .unwrap_or(filter_text);

    // Drop trailing " spell"/" spells" so `parse_type_phrase` sees the bare
    // type/subtype phrase. "lands" is already a valid type phrase.
    let cleaned: Cow<str> = if nom_primitives::scan_contains(filter_text, "spells") {
        Cow::Owned(filter_text.replacen(" spells", "", 1))
    } else if nom_primitives::scan_contains(filter_text, "spell") {
        Cow::Owned(filter_text.replacen(" spell", "", 1))
    } else {
        Cow::Borrowed(filter_text)
    };

    let (filter, _) = parse_type_phrase(&cleaned);

    let alt_cost = parse_top_of_library_alt_cost_rider(trailing, text);

    let mut def = StaticDefinition::new(StaticMode::TopOfLibraryCastPermission {
        play_mode,
        frequency,
        alt_cost,
    })
    .affected(filter)
    .description(text.to_string());
    if let Some(condition) = parse_top_of_library_permission_condition(trailing) {
        def = def.condition(condition);
    }
    Some(def)
}

/// CR 601.2b + CR 118.9a: Parse Omniscience-class restricted free-cast static
/// abilities — "you may cast [filter] [from your hand]? without paying [its|their]
/// mana cost[s]?" — covering Omniscience and the Tamiyo, Field Researcher emblem
/// (no filter, hand qualifier), Zaffai-and-the-Tempests (typed filter, hand
/// qualifier, once-per-turn frequency), and Dracogenesis (subtype filter, no
/// zone qualifier, so it can replace the mana cost from built-in cast zones like
/// hand and command). Continuous static — not a one-shot effect.
pub(crate) fn try_parse_cast_free_permission(text: &str, lower: &str) -> Option<StaticDefinition> {
    // CR 601.2b: Prefix determines frequency. `OncePerTurn` (Zaffai) is the
    // explicit-choice path; `Unlimited` (Omniscience, Dracogenesis) runs silently.
    let (rest, frequency) = if let Some(r) = nom_tag_lower(
        lower,
        lower,
        "once during each of your turns, you may cast ",
    ) {
        (r, CastFrequency::OncePerTurn)
    } else {
        (
            nom_tag_lower(lower, lower, "you may cast ")?,
            CastFrequency::Unlimited,
        )
    };

    // The zone qualifier "from your hand" is optional. When omitted, the static
    // only replaces the mana cost for spells already castable from their current
    // zone; it does not create an independent cast-from-anywhere permission.
    //
    // Both branches must terminate at " without paying" — that token is the
    // single anchor for the static. The qualified branch keeps a permissive
    // type-parse (warns on unconsumed remainder) for established Omniscience /
    // Zaffai / Expertise-cycle shapes; the unqualified branch is strict (rejects
    // unconsumed remainder) so complex filters like Fires of Invention's
    // "spells with mana value less than or equal to the number of lands you
    // control" decline cleanly instead of misparsing as `TargetFilter::Any`.
    let (filter_text, origin) = if let Ok((_, (before, hand_rest))) =
        nom_primitives::split_once_on(rest, " from your hand")
    {
        // "without paying" must follow "from your hand" — reject unusual word orders
        if !nom_primitives::scan_contains(hand_rest, "without paying") {
            return None;
        }
        (before, CastFreeOrigin::Hand)
    } else {
        let (_, (before, _)) = nom_primitives::split_once_on(rest, " without paying").ok()?;
        (before, CastFreeOrigin::DefaultCastPermission)
    };

    // Intentional: "spells" with no qualifier → Any filter (Omniscience) — no warning needed.
    if filter_text == "spells" {
        return Some(
            StaticDefinition::new(StaticMode::CastFromHandFree { frequency, origin })
                .affected(TargetFilter::Any)
                .description(text.to_string()),
        );
    }

    // Strip "a "/"an " article and " spell"/" spells" suffix for type parsing
    let filter_text = nom_tag_lower(filter_text, filter_text, "a ")
        .or_else(|| nom_tag_lower(filter_text, filter_text, "an "))
        .unwrap_or(filter_text);

    let cleaned: Cow<str> = if nom_primitives::scan_contains(filter_text, "spells") {
        Cow::Owned(filter_text.replacen(" spells", "", 1))
    } else if nom_primitives::scan_contains(filter_text, "spell") {
        Cow::Owned(filter_text.replacen(" spell", "", 1))
    } else {
        Cow::Borrowed(filter_text)
    };

    let (filter, remainder) = parse_type_phrase(&cleaned);
    if !remainder.trim().is_empty() && matches!(origin, CastFreeOrigin::DefaultCastPermission) {
        // Unqualified branch is strict: an unconsumed remainder signals a
        // complex filter we don't yet model (e.g. Fires of Invention's
        // dynamic mana-value bound). Decline rather than emit a partial
        // `Any` filter that would be wrong in a different way.
        return None;
    }

    Some(
        StaticDefinition::new(StaticMode::CastFromHandFree { frequency, origin })
            .affected(filter)
            .description(text.to_string()),
    )
}
