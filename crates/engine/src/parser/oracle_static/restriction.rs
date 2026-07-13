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
    // CR 605.1a: the " unless they're mana abilities" carve-out, tolerant of both
    // the ASCII and U+2019 apostrophe via the shared suffix combinator.
    let mut parser = opt(value(
        ActivationExemption::ManaAbilities,
        super::shared::parse_mana_ability_exemption_suffix,
    ));
    let (rest, exemption) = parser.parse(input)?;
    Ok((rest, exemption.unwrap_or_default()))
}

pub(crate) fn parse_cant_be_activated_exemption_in_text(lower: &str) -> ActivationExemption {
    nom_primitives::scan_preceded(lower, |i| {
        // CR 605.1a: dual-apostrophe predicate + dual-apostrophe exemption suffix,
        // so a U+2019 self-reference / attached line still records the carve-out.
        preceded(
            super::shared::parse_cant_be_activated_predicate,
            parse_activation_exemption_suffix,
        )
        .parse(i)
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
        // CR 605.1a: the predicate carries either apostrophe glyph — mirror the
        // dual-branch used everywhere else (no global apostrophe normalization).
        if let Ok((after_predicate, _)) = alt((
            tag::<_, _, OracleError<'_>>(" can't be activated"),
            tag::<_, _, OracleError<'_>>(" can\u{2019}t be activated"),
        ))
        .parse(after_source)
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

    // Otherwise fall back to the type-list + controller-suffix form (Karn,
    // Clarion, Damping Matrix, Sharkey). Split ON the "... can't be activated"
    // predicate — rather than requiring the line to END there — so an optional
    // " unless they're mana abilities" carve-out (Damping Matrix: "Activated
    // abilities of artifacts and creatures can't be activated unless they're mana
    // abilities") is parsed the same way as the chosen-name branch above.
    // CR 605.1a: accept both apostrophe glyphs on the type-list predicate too.
    // `parse_type_phrase` consumes the filter and leaves the predicate. Re-wrap
    // that tail as a TextPair so the predicate stays in the nom parser family.
    let (source_filter, filter_tail) = parse_type_phrase(rest_tp.original);
    let filter_end = rest_tp.original.len().checked_sub(filter_tail.len())?;
    let filter_tail = TextPair::new(
        &rest_tp.original[filter_end..],
        &rest_tp.lower[filter_end..],
    );
    // `parse_type_phrase` returns `SelfRef` for unparseable input — treat that as a
    // parse failure and fall through to the self-ref branch in parse_static_line.
    if matches!(source_filter, TargetFilter::SelfRef) {
        return None;
    }
    let after_predicate = nom_tag_tp(&filter_tail, " can't be activated")
        .or_else(|| nom_tag_tp(&filter_tail, " can\u{2019}t be activated"))?;
    // CR 605.1a: optional " unless they're mana abilities" carve-out (Damping
    // Matrix); the suffix combinator yields `ActivationExemption::None` when it is
    // absent (Karn, Clarion). Nothing but a trailing period may follow it.
    let (exempt_tail, exemption) = parse_activation_exemption_suffix(after_predicate.lower).ok()?;
    if !exempt_tail.trim_end_matches('.').trim().is_empty() {
        return None;
    }
    Some(
        StaticDefinition::new(StaticMode::CantBeActivated {
            who: ProhibitionScope::AllPlayers,
            source_filter,
            exemption,
        })
        .description(text.to_string()),
    )
}

/// CR 701.23 + CR 101.2: Parse CantSearchLibrary statics — a "can't search"
/// continuous effect takes precedence over any effect directing a search.
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

/// CR 701.23f + CR 614.1a: Parse the top-N library-search restriction class —
/// "If an opponent would search a library, that player searches the top N cards
/// of that library instead." (Aven Mindcensor). Parameterized by SEARCHER scope
/// (`who`) and the visible-portion `count`. Lowers to
/// `StaticMode::RestrictLibrarySearchToTop`; runtime enforcement lives in
/// game/effects/search_library.rs.
///
/// Supported Oracle classes:
/// - "If an opponent would search a library, that player searches the top N
///   cards of that library instead." (`who = Opponents`)
/// - "If a player would search a library, that player searches the top N cards
///   of that library instead." (`who = AllPlayers`)
pub(crate) fn parse_restrict_search_to_top(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    fn parse_restrict_clause(input: &str) -> OracleResult<'_, (ProhibitionScope, u32)> {
        let (input, _) = tag::<_, _, OracleError<'_>>("if ").parse(input)?;
        // Searcher scope: "an opponent" → Opponents, "a player" → AllPlayers.
        let (input, who) = alt((
            value(ProhibitionScope::Opponents, tag("an opponent")),
            value(ProhibitionScope::AllPlayers, tag("a player")),
        ))
        .parse(input)?;
        let (input, _) =
            tag(" would search a library, that player searches the top ").parse(input)?;
        let (input, count) = nom_primitives::parse_number(input)?;
        let (input, _) = tag(" cards of that library instead").parse(input)?;
        let (input, _) = opt(tag(".")).parse(input)?;
        let (input, _) = eof(input)?;
        Ok((input, (who, count)))
    }

    let (who, count) =
        nom_on_lower(tp.original, tp.lower, parse_restrict_clause).map(|(parsed, _rest)| parsed)?;
    Some(
        StaticDefinition::new(StaticMode::RestrictLibrarySearchToTop { who, count })
            .description(text.to_string()),
    )
}

/// CR 603.2 + CR 101.2: Parse "Triggered abilities <scope> can't cause you to
/// sacrifice or exile <affected>." statics (The Master, Multiplied class). The
/// "can't" effect takes precedence over the triggered ability directing the action.
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
        // CR 508.6: "attacked you or a planeswalker you control this turn" — the
        // attacked-defender is the source's controller (CR 109.5 "you"), distinct
        // from `YouAttackedThisTurn`'s attacked-anyone. Longer disjunctive form
        // first; the bare "attacked you this turn" is the same predicate (the
        // planeswalker disjunct collapses to the controller via CR 508.5).
        // Sandswirl Wanderglyph.
        value(
            ParsedCondition::YouAttackedSourceControllerThisTurn,
            alt((
                tag::<_, _, OracleError<'_>>(
                    "attacked you or a planeswalker you control this turn",
                ),
                tag("attacked you this turn"),
            )),
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
///
/// `raw_lower` is the pre-`strip_reminder_text` lowercase line so trailing
/// `(as long as/if <condition>)` gates are not lost to reminder stripping.
pub(crate) fn parse_cant_cast_type_spells(
    tp: &str,
    text: &str,
    raw_lower: &str,
) -> Option<StaticDefinition> {
    // CR 611.3a: gate parentheticals must be read from the raw line — `strip_reminder_text`
    // removes every parenthetical span before this parser runs.
    let gate_condition_text = match extract_trailing_parenthetical_gate_condition(raw_lower) {
        ParentheticalGateExtract::Unrecognized => return None,
        ParentheticalGateExtract::Recognized(cond) => Some(cond),
        ParentheticalGateExtract::Absent | ParentheticalGateExtract::Benign => None,
    };

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

    let trimmed = after_cant_cast.trim_end_matches('.');

    // --- "spells with mana value N or less/greater" ---
    if let Some(rest) = nom_tag_lower(trimmed, trimmed, "spells with mana value ") {
        return attach_parsed_static_gate(
            parse_cant_cast_mana_value(rest, who, text)?,
            gate_condition_text,
        );
    }

    // --- "spells with the chosen name" ---
    if nom_tag_lower(trimmed, trimmed, "spells with the chosen name").is_some() {
        let def = StaticDefinition::new(StaticMode::CantBeCast { who })
            .affected(TargetFilter::HasChosenName)
            .description(text.to_string());
        return attach_parsed_static_gate(def, gate_condition_text);
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
        return attach_parsed_static_gate(def, gate_condition_text);
    }

    // --- "spells of the chosen color" ---
    // CR 101.2 + CR 105.4: scope the prohibition to spells whose colors (CR 105.2)
    // include the source's chosen color — not every spell. Mirrors the "chosen
    // type" branch above; runtime resolution lives in `cant_cast_filter_matches`.
    if nom_tag_lower(trimmed, trimmed, "spells of the chosen color").is_some() {
        let filter = TargetFilter::Typed(TypedFilter {
            properties: vec![FilterProp::IsChosenColor],
            ..TypedFilter::default()
        });
        let def = StaticDefinition::new(StaticMode::CantBeCast { who })
            .affected(filter)
            .description(text.to_string());
        return attach_parsed_static_gate(def, gate_condition_text);
    }

    // --- "spells with the same name as ..." ---
    // CR 101.2: "can't cast spells with the same name as [reference]" — approximate as
    // blanket prohibition; the name-matching filter is too dynamic for static representation.
    if nom_tag_lower(trimmed, trimmed, "spells with the same name as ").is_some() {
        let def =
            StaticDefinition::new(StaticMode::CantBeCast { who }).description(text.to_string());
        return attach_parsed_static_gate(def, gate_condition_text);
    }

    // --- "spells with even mana values" / "spells with odd mana values" ---
    if nom_tag_lower(trimmed, trimmed, "spells with even mana value").is_some()
        || nom_tag_lower(trimmed, trimmed, "spells with odd mana value").is_some()
    {
        let def =
            StaticDefinition::new(StaticMode::CantBeCast { who }).description(text.to_string());
        return attach_parsed_static_gate(def, gate_condition_text);
    }

    // --- "spells by paying alternative costs" ---
    if nom_tag_lower(trimmed, trimmed, "spells by paying alternative cost").is_some() {
        let def =
            StaticDefinition::new(StaticMode::CantBeCast { who }).description(text.to_string());
        return attach_parsed_static_gate(def, gate_condition_text);
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
    attach_parsed_static_gate(def, gate_condition_text)
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

/// CR 121.1 + CR 613.11: Parse a draw-source redirection static from Oracle
/// text: "[Subject] draw(s) cards from the bottom of [your|their] library
/// rather than/instead of the top." → `DrawFromBottom { who }`.
/// E.g., River Song ("Meet in Reverse"): "You draw cards from the bottom of
/// your library rather than the top."
pub(crate) fn parse_draw_from_bottom(tp: &str, text: &str) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;

    // Subject axis → `ProhibitionScope` via the shared building block.
    let (who, predicate) = strip_casting_prohibition_subject(tp)?;
    // Verb axis ("draw"/"draws") × quantity axis ("cards"/"a card") — composed, not permuted.
    // Covers "draw a card", "draw cards", "draws a card", "draws cards".
    let (rest, _) = alt((tag::<_, _, VE<'_>>("draw "), tag("draws ")))
        .parse(predicate)
        .ok()?;
    let (rest, _) = alt((tag::<_, _, VE<'_>>("cards"), tag("a card")))
        .parse(rest)
        .ok()?;
    let (rest, _) = tag::<_, _, VE<'_>>(" from the bottom of ")
        .parse(rest)
        .ok()?;
    // Possessive axis composed (not permuted).
    let (rest, _) = alt((tag::<_, _, VE<'_>>("your "), tag("their ")))
        .parse(rest)
        .ok()?;
    let (rest, _) = tag::<_, _, VE<'_>>("library ").parse(rest).ok()?;
    // Connector axis composed (not permuted).
    let (rest, _) = alt((tag::<_, _, VE<'_>>("rather than"), tag("instead of")))
        .parse(rest)
        .ok()?;
    let (rest, _) = tag::<_, _, VE<'_>>(" the top").parse(rest).ok()?;
    // STRICT: reject a silently-dropped trailing clause — the redirect must be
    // the whole sentence, not a fragment with an unparsed remainder.
    let tail = rest.trim();
    if !matches!(tail, "" | ".") {
        return None;
    }
    Some(StaticDefinition::new(StaticMode::DrawFromBottom { who }).description(text.to_string()))
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
                extra_cost: None,
                enters_with_counter: None,
            })
            .affected(affected)
            .description(text.to_string()),
        );
    }

    // CR 305.1 + CR 601.2a + CR 114.4: Unlimited combined permission —
    // "You may play lands and cast permanent spells from your graveyard."
    // (Wrenn and Realmbreaker emblem). Composed through the shared branch
    // filter parser so trailing rules text is not silently dropped.
    if let Some(def) = try_parse_unlimited_combined_graveyard_permission(text, lower) {
        return Some(def);
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

    // CR 117.1c: Optional "during your turn, " timing qualifier (Festival of
    // Embers). When present, the permission is gated to the source controller's
    // turn via a `ParsedCondition::IsYourTurn` static condition
    // (`evaluate_condition` → `state.active_player == controller`), the
    // rules-correct enforcement at CR 102.1 — not silently dropped.
    let (lower, your_turn_only) = match nom_tag_lower(lower, lower, "during your turn, ") {
        Some(r) => (r, true),
        None => (lower, false),
    };

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

    // CR 607.1 + CR 122.1 + CR 614.1c: peel the linked "if you cast a spell this
    // way, that <permanent> enters with a [counter] counter on it" rider off the
    // trailing text so the enters-with counter rides the permission and the
    // remaining riders (extra_cost, condition) parse against the counter-free
    // tail (Noctis, Prince of Lucis; Leonardo, Sewer Samurai — both finality).
    let (trailing, enters_with_counter) = split_cast_this_way_enters_rider(trailing);

    // Parse optional alt-cost rider from the text after "from your graveyard".
    let rider_kind = parse_alt_cost_rider(trailing).ok().map(|(_, k)| k);
    let graveyard_destination_replacement = parse_exile_spell_cast_this_way_rider(trailing)
        .is_ok()
        .then_some(Zone::Exile);
    // CR 601.2f: Optional "by paying ... in addition to their other costs"
    // ADDITIONAL non-mana cost rider (Festival of Embers). Recognized before the
    // permission-condition fallback so it isn't misread as a condition tail.
    let extra_cost =
        parse_cast_permission_additional_cost_rider(trailing).map(|cost| CastExtraCost {
            cost,
            mode: CastCostMode::Additional,
        });
    // `.trim()` (not `.is_empty()`): after the enters-with rider is split off, a
    // two-sentence "if X. If you do, Y." permission leaves a whitespace-only
    // residual (Undead Sprinter) that must still be treated as fully consumed so
    // the gate condition is not re-dropped. Matches the other trim checks here.
    let condition = parse_graveyard_permission_condition(trailing)
        .ok()
        .and_then(|(rest, condition)| rest.trim().is_empty().then_some(condition));

    let affected = if let Some(kind) = rider_kind {
        inject_keyword_kind_filter_prop(filter, kind)
    } else {
        filter
    };

    let mut def = StaticDefinition::new(StaticMode::GraveyardCastPermission {
        frequency,
        play_mode,
        graveyard_destination_replacement,
        extra_cost,
        enters_with_counter,
    })
    .affected(affected)
    .description(text.to_string());
    if let Some(condition) = condition {
        def = def.condition(condition);
    } else if your_turn_only {
        // CR 102.1 + CR 117.1c: gate the permission to the source controller's
        // turn (Festival of Embers' "During your turn, ...").
        def = def.condition(StaticCondition::DuringYourTurn);
    }
    if self_ref_permission {
        def = def.active_zones(vec![Zone::Graveyard]);
    }
    Some(def)
}

/// CR 601.2f: Parse a trailing ADDITIONAL-cost rider on a cast-from-zone
/// permission — "by paying <N> life in addition to their other costs" (Festival
/// of Embers). The cost is paid on TOP of the spell's normal mana cost (CR
/// 601.2f), distinct from the CR 118.9 alternative rider parsed by
/// `oracle_effect::try_parse_alt_cost_rider`. Composed from nom combinators so
/// the prefix × quantity × suffix axes stay independent and future shapes
/// (other costs, "its"/"their" pronoun) extend without permutation blowup.
/// Returns `None` when the rider shape is absent.
fn parse_cast_permission_additional_cost_rider(
    trailing: &str,
) -> Option<crate::types::ability::AbilityCost> {
    let lower = trailing.trim_start();
    // CR 601.2f: "by paying " opens the rider; "in addition to" distinguishes
    // the additional shape from a CR 118.9 "rather than" alternative.
    if !nom_primitives::scan_contains(lower, "in addition to") {
        return None;
    }
    let rest = nom_tag_lower(lower, lower, "by paying ")?;
    // CR 119.4: "<N> life" — the only additional-cost shape used by the current
    // class that this permission carries (Festival of Embers).
    let (after_num, n) = nom_primitives::parse_number(rest).ok()?;
    let after_life = nom_tag_lower(after_num, after_num, " life")?;
    // CR 601.2f: the tail must be the "in addition to (their|its) other costs"
    // closer — anything else is an unmodeled shape.
    let after_life = after_life.trim_start();
    let after_in_addition = nom_tag_lower(after_life, after_life, "in addition to ")?;
    // CR 601.2f: tolerate the optional "paying" gerund — "in addition to PAYING
    // their other costs" (Noctis, Prince of Lucis) alongside the bare "in
    // addition to their other costs" (Festival of Embers). Additive `opt` strip:
    // Festival (no gerund) keeps the original slice unchanged.
    let after_in_addition =
        nom_tag_lower(after_in_addition, after_in_addition, "paying ").unwrap_or(after_in_addition);
    let after_pronoun = nom_tag_lower(after_in_addition, after_in_addition, "their other costs")
        .or_else(|| nom_tag_lower(after_in_addition, after_in_addition, "its other costs"))?;
    // allow-noncombinator: punctuation cleanup (drop the sentence terminator) on a pre-tokenized chunk, not parsing dispatch.
    let trimmed_pronoun = after_pronoun.trim_start();
    let after_pronoun = trimmed_pronoun.strip_prefix('.').unwrap_or(trimmed_pronoun); // allow-noncombinator: punctuation cleanup on a pre-tokenized chunk, not parsing dispatch.
    if !after_pronoun.trim().is_empty() {
        return None;
    }
    Some(crate::types::ability::AbilityCost::PayLife {
        amount: QuantityExpr::Fixed { value: n as i32 },
    })
}

/// CR 607.1 + CR 122.1 + CR 614.1c: Peel the linked "if you cast a spell this
/// way, that <permanent> enters with a [counter] counter on it" rider off a
/// trailing text run, returning `(text-before-rider, Some(counter))`. The rider
/// is a CR 607.1 linked-permission back-reference — the enters-with counter
/// rides the cast permission (carried on the static's `enters_with_counter`
/// field), so it must be split off before the extra_cost / condition / pool
/// parsers consume the trailing text. Delegates the counter-subject grammar to
/// the shared `oracle_effect::parse_cast_this_way_enters_with_counter` authority
/// so the effect path (Osteomancer/Tomb) and the static path recognize the same
/// shapes. Returns `(trailing, None)` unchanged when no such rider is present.
fn split_cast_this_way_enters_rider(
    trailing: &str,
) -> (&str, Option<crate::types::counter::CounterType>) {
    // "if you do" covers the self-granting shape (Undead Sprinter's "If you do,
    // this creature enters with a +1/+1 counter on it"). The slice is
    // recognizer-guarded below (only commits when the shared enters-with-counter
    // recognizer succeeds), so "if you do" cannot over-split an unrelated rider —
    // "if you don't …" fails the recognizer and returns the trailing unchanged.
    for marker in [
        "if you cast a spell this way",
        "if you cast it this way",
        "if you do",
    ] {
        if let Ok((_, (before, _after))) = nom_primitives::split_once_on(trailing, marker) {
            // allow-noncombinator: structural offset back to the rider start (the
            // `text.len() - rest.len()` idiom) so the shared recognizer sees the
            // full "if you cast … this way, …" clause including its marker.
            let rider = &trailing[before.len()..];
            if let Some((counter_type, _rest)) =
                super::oracle_effect::parse_cast_this_way_enters_with_counter(rider)
            {
                return (before, Some(counter_type));
            }
        }
    }
    (trailing, None)
}

/// CR 108.3 + CR 109.5: Attach a "cards you own" ownership constraint to a typed
/// affected filter — the card's owner must be the permission's controller
/// (`you`). Returns `None` when the filter is not a `Typed` shape that can carry
/// the `FilterProp::Owned` constraint, so the caller declines rather than
/// silently dropping the ownership gate (Intrepid Paleontologist can exile an
/// opponent's graveyard card; only owned cards may be cast via the permission).
fn inject_owner_you_filter_prop(filter: TargetFilter) -> Option<TargetFilter> {
    match filter {
        TargetFilter::Typed(mut tf) => {
            tf.properties.push(FilterProp::Owned {
                controller: ControllerRef::You,
            });
            Some(TargetFilter::Typed(tf))
        }
        _ => None,
    }
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
            extra_cost: None,
            enters_with_counter: None,
        })
        .affected(affected)
        .description(text.to_string()),
    )
}

/// CR 305.1 + CR 601.2a + CR 114.4: Parse unlimited combined graveyard
/// permission — "You may play <land-filter> and cast <spell-filter> from your
/// graveyard." — using the shared branch-filter grammar and rejecting any
/// trailing rules-bearing suffix.
fn try_parse_unlimited_combined_graveyard_permission(
    text: &str,
    lower: &str,
) -> Option<StaticDefinition> {
    let rest = nom_tag_lower(lower, lower, "you may play ")?;
    let (land_branch, spell_branch) = nom_primitives::split_once_on(rest, " and cast ")
        .ok()
        .map(|(_, pair)| pair)?;
    let (spell_branch, after_graveyard) =
        nom_primitives::split_once_on(spell_branch, " from your graveyard")
            .ok()
            .map(|(_, pair)| pair)?;
    if !after_graveyard.trim().trim_start_matches('.').is_empty() {
        return None;
    }
    let land_filter = parse_graveyard_branch_filter(land_branch.trim())?;
    let spell_filter = parse_graveyard_branch_filter(spell_branch.trim())?;
    let affected = if land_filter == spell_filter {
        land_filter
    } else {
        TargetFilter::Or {
            filters: vec![land_filter, spell_filter],
        }
    };
    Some(
        StaticDefinition::new(StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::Unlimited,
            play_mode: CardPlayMode::Play,
            graveyard_destination_replacement: None,
            extra_cost: None,
            enters_with_counter: None,
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

/// Returns true when a disjunctive play/cast branch resolved to a concrete
/// typed object filter rather than a broad parser fallback or contextual
/// reference.
fn usable_disjunctive_permission_filter(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => !tf.type_filters.is_empty() || !tf.properties.is_empty(),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            !filters.is_empty() && filters.iter().all(usable_disjunctive_permission_filter)
        }
        TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SelfRef
        | TargetFilter::GrantingObject
        | TargetFilter::SourceOrPaired
        | TargetFilter::Not { .. }
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::PlayerWhoChoseLabel { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::CostPaidObject
        | TargetFilter::ChosenCard
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::EventTarget
        | TargetFilter::TriggeringSourceController
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::OriginalSource
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::ChosenDamageSource { .. }
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => false,
    }
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

    // CR 700.6: "historic spells" lowers to the Historic property on spell
    // cards; suffix stripping leaves the bare adjective, so expand to the card
    // phrase the type parser expects.
    let cleaned: Cow<str> = if cleaned == "historic" {
        Cow::Borrowed("historic card")
    } else {
        cleaned
    };

    let (filter, _self_ref) = parse_graveyard_permission_filter(&cleaned);
    // Reject the unparseable fallbacks so a branch we cannot model declines the
    // whole disjunctive parse rather than silently admitting everything.
    if usable_disjunctive_permission_filter(&filter) {
        Some(filter)
    } else {
        None
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
///   its mana cost." (Maralen, Fae Ascendant → `OncePerTurn`, `ThisTurn`)
/// - The longer "once during each of your turns, you may cast …" synonym.
/// - Unlimited shape "you may cast <filter> from among cards you own exiled with
///   ~[.] [If you cast a spell this way, that <permanent> enters with a
///   [counter] counter on it.]" (Intrepid Paleontologist → `Unlimited`,
///   `Persistent`, owner=You constraint, finality `enters_with_counter`).
///
/// Returns `None` for shapes outside this class (graveyard/top-of-library/hand
/// permissions all anchor on different phrases earlier in the dispatch chain).
/// A non-finality trailing extra-cost (Dawnhand Dissident's "by removing three
/// counters …") is not a finality rider, so it survives the peel, trips the
/// strict Persistent empty-tail check, and declines cleanly (no misparse).
pub(crate) fn try_parse_exile_cast_permission(text: &str, lower: &str) -> Option<StaticDefinition> {
    // CR 601.2a: Frequency prefix. Both "once each turn" (Maralen) and the
    // longer "once during each of your turns" synonym map to `OncePerTurn`; the
    // bare "you may cast" lead (Intrepid Paleontologist) has no per-turn cap →
    // `Unlimited`. The bare branch is safe: the mandatory " from among cards
    // [you own ]exiled with " anchor below gates it to this exile-pool class, so
    // a "you may cast … from your graveyard/hand" line still declines here.
    let (rest, frequency) = if let Some(rest) =
        nom_tag_lower(lower, lower, "once each turn, you may cast ").or_else(|| {
            nom_tag_lower(
                lower,
                lower,
                "once during each of your turns, you may cast ",
            )
        }) {
        (rest, CastFrequency::OncePerTurn)
    } else {
        // Bare "you may cast" lead (Intrepid Paleontologist) → Unlimited.
        let rest = nom_tag_lower(lower, lower, "you may cast ")?;
        (rest, CastFrequency::Unlimited)
    };

    // Strip the leading article — `parse_type_phrase` expects the bare noun.
    let rest = nom_tag_lower(rest, rest, "a ")
        .or_else(|| nom_tag_lower(rest, rest, "an "))
        .unwrap_or(rest);

    // CR 113.6b: Anchor on " from among cards [you own ]exiled with " — the
    // class-defining phrase. Anything before is the affected filter; anything
    // after is the source self-reference plus optional finality / alt-cost /
    // "this turn" markers. CR 108.3: the "you own" ownership infix (Intrepid
    // Paleontologist) restricts eligibility to owned cards — recorded in
    // `owned_only` and applied as a `FilterProp::Owned` constraint below.
    let (filter_text, trailing, owned_only) = if let Ok((_, (f, t))) =
        nom_primitives::split_once_on(rest, " from among cards you own exiled with ")
    {
        (f, t, true)
    } else if let Ok((_, (f, t))) =
        nom_primitives::split_once_on(rest, " from among cards exiled with ")
    {
        (f, t, false)
    } else {
        return None;
    };

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

    // CR 607.1 + CR 122.1 + CR 614.1c: peel the linked "if you cast a spell this
    // way, that <permanent> enters with a [counter] counter on it" rider off the
    // trailing text (Intrepid Paleontologist — finality) so the enters-with
    // counter rides the permission and the Persistent strict-remainder check
    // below sees the counter-free tail. Also gates Dawnhand Dissident cleanly:
    // its "by removing three counters … in addition to paying their other costs"
    // tail is NOT a finality rider, stays in the remainder, and trips the
    // strict Persistent empty-tail check → declines (clean gap, not a misparse).
    let (after_source, enters_with_counter) = split_cast_this_way_enters_rider(after_source);

    // CR 108.3 + CR 109.5: apply the "cards you own" ownership constraint to the
    // affected filter — the card's owner must be the permission's controller.
    let filter = if owned_only {
        inject_owner_you_filter_prop(filter)?
    } else {
        filter
    };

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
            // CR 609.4b / CR 702.8a: Maralen grants no mana-spend or flash
            // concession.
            mana_spend_permission: None,
            grants_flash: false,
            // CR 118.9 / CR 601.2f: Maralen casts at its alt-cost shape via
            // `cost`, not an extra non-mana rider.
            extra_cost: None,
            // CR 122.1 + CR 614.1c: linked enters-with counter rider peeled off
            // the trailing text above (Intrepid Paleontologist — finality).
            enters_with_counter,
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

    // Core permission phrase. CR 305.1: "play lands and cast spells" / "play
    // cards" lower to Play mode (lands are played, non-land cards are cast).
    // CR 601.2a: the bare "cast cards exiled with ~" wording (Azula, Cunning
    // Usurper) is spell-cast only and lowers to `Cast` — lands cannot be
    // "cast", so the Cast branch never admits exiled lands.
    let (after_clause, play_mode) = if let Some(rest) =
        nom_tag_lower(rest, rest, "you may play lands and cast spells from among ")
    {
        // The play clause either names the source ("cards exiled with <self>") or
        // refers back to the look-at preamble's set ("those cards").
        let rest = if uses_anaphor {
            nom_tag_lower(rest, rest, "those cards")?
        } else {
            strip_exile_play_source_reference(rest)?
        };
        (rest, CardPlayMode::Play)
    } else if let Some(rest) = nom_tag_lower(rest, rest, "you may cast ") {
        // CR 601.2a: "you may cast cards exiled with ~" — spell-cast only.
        let rest = if uses_anaphor {
            nom_tag_lower(rest, rest, "those cards")?
        } else {
            strip_exile_play_source_reference(rest)?
        };
        (rest, CardPlayMode::Cast)
    } else {
        let rest = nom_tag_lower(rest, rest, "you may play ")?;
        let rest = if uses_anaphor {
            nom_tag_lower(rest, rest, "those cards")?
        } else {
            strip_exile_play_source_reference(rest)?
        };
        (rest, CardPlayMode::Play)
    };

    // CR 601.3b + CR 609.4b: Optional payment/timing-concession riders that ride
    // alongside the cast permission (Azula, Cunning Usurper). Parse them in
    // order off the tail so any leftover proves an unmodeled shape.
    let (after_riders, grants_flash, mana_spend_permission) =
        strip_exile_cast_concession_riders(after_clause);

    // CR 118.9: Optional trailing ALTERNATIVE-cost rider sentence — "If you cast
    // a spell this way, pay life equal to its mana value rather than pay its mana
    // cost." (Valgavoth, Terror Eater). Reuses the shared
    // `try_parse_alt_cost_rider` authority (the same helper that stamps
    // `TopOfLibraryCastPermission.alt_cost`), so the recognized cost shapes stay
    // in lockstep with the top-of-library Bolas's Citadel class. When present the
    // rider IS the only permitted remainder.
    let tail = after_riders.trim_start();
    // allow-noncombinator: punctuation cleanup (drop the sentence terminator) on a pre-tokenized chunk, not parsing dispatch.
    let tail = tail.strip_prefix('.').unwrap_or(tail).trim_start(); // allow-noncombinator: punctuation cleanup on a pre-tokenized chunk, not parsing dispatch.

    let extra_cost = if tail.is_empty() {
        None
    } else {
        // CR 113.6b: A non-empty tail must be the recognized alt-cost rider; an
        // unmodeled remainder declines so it surfaces as a coverage gap rather
        // than a silent misparse.
        let cost = super::oracle_effect::try_parse_alt_cost_rider(tail)?;
        Some(CastExtraCost {
            cost,
            mode: CastCostMode::Alternative,
        })
    };

    let mut definition = StaticDefinition::new(StaticMode::ExileCastPermission {
        // CR 601.2a: No once-per-turn cap on this class.
        frequency: CastFrequency::Unlimited,
        // CR 305.1 / CR 601.2a: `Play` covers lands + non-land cards; `Cast`
        // covers spells only, set by the "you may cast" wording.
        play_mode,
        // CR 305.1 / CR 601.3: Cards are played/cast at their normal cost.
        cost: ExileCastCost::PayNormalCost,
        // CR 406.6: Lifetime per-source exile-link pool.
        pool: ExileCardPool::Persistent,
        timing,
        mana_spend_permission,
        grants_flash,
        // CR 118.9: Valgavoth's alternative pay-life cost (or None).
        extra_cost,
        // CR 122.1: no printed persistent exile-PLAY permission (The Matrix of
        // Time, Prosper/Tibalt, Azula, Valgavoth) carries an enters-with counter
        // rider — those riders appear only on the Cast-mode Maralen/Intrepid
        // class. Left `None`; the shared recognizer would slot here if such a
        // card ships.
        enters_with_counter: None,
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

/// CR 601.3b + CR 609.4b: Strip the optional flash-grant and any-type-mana
/// concession riders that follow the core exile-cast clause (Azula, Cunning
/// Usurper: "… and you may cast them as though they had flash. Mana of any type
/// can be spent to cast those spells."). Returns the remainder plus the parsed
/// `(grants_flash, mana_spend_permission)` pair. Each rider is optional and
/// recognized independently so future cards mixing only one of the two still
/// parse. Riders not present leave the defaults `(false, None)`.
fn strip_exile_cast_concession_riders(
    input: &str,
) -> (
    &str,
    bool,
    Option<crate::types::ability::ManaSpendPermission>,
) {
    let mut rest = input.trim_start();
    let mut grants_flash = false;
    let mut mana_spend_permission = None;

    // CR 601.3b: "[and] you may cast them as though they had flash[.]"
    // Optional leading "and "/". " connective consumed via the file-wide
    // nom-tag idiom so the rider may follow either the period or the conjunction.
    if let Some(after) = nom_tag_lower(rest, rest, "and you may cast them as though they had flash")
        .or_else(|| nom_tag_lower(rest, rest, "you may cast them as though they had flash"))
    {
        grants_flash = true;
        let trimmed = after.trim_start();
        // allow-noncombinator: punctuation cleanup (drop the sentence terminator) between riders on a pre-tokenized chunk, not parsing dispatch.
        rest = trimmed.strip_prefix('.').unwrap_or(trimmed).trim_start(); // allow-noncombinator: punctuation cleanup between riders, not parsing dispatch.
    }

    // CR 609.4b: "Mana of any type can be spent to cast those spells[.]"
    if let Some(after) = nom_tag_lower(
        rest,
        rest,
        "mana of any type can be spent to cast those spells",
    ) {
        mana_spend_permission = Some(crate::types::ability::ManaSpendPermission::AnyTypeOrColor);
        rest = after;
    }

    (rest, grants_flash, mana_spend_permission)
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

/// CR 609.4b: Parse the activation-source-filtered any-color-mana spend static —
/// "You may spend mana as though it were mana of any color to activate abilities
/// of <subject>." (Agatha's Soul Cauldron / Joiner Adept). Lowers to
/// `StaticMode::SpendManaAsAnyColor { spell_filter: None,
/// activation_source_filter: Some(filter) }`, scoping the any-color concession to
/// activated abilities whose source permanent matches the subject filter (CR
/// 609.4b: the concession changes only how a cost is paid, never the cost).
///
/// The unfiltered board-wide form ("you may spend mana as though it were mana of
/// any color", Chromatic Orrery) and the spell-class-filtered form ("to cast
/// creature spells", Vizier) are handled separately and must not be swallowed
/// here — this handler requires the explicit "to activate abilities of <subject>"
/// scope.
pub(crate) fn try_parse_spend_any_color_to_activate_abilities(
    text: &str,
    tp: &TextPair<'_>,
) -> Option<StaticDefinition> {
    let rest = nom_tag_tp(
        tp,
        "you may spend mana as though it were mana of any color to activate abilities of ",
    )
    .or_else(|| {
        nom_tag_tp(
            tp,
            "spend mana as though it were mana of any color to activate abilities of ",
        )
    })?;

    let subject = rest.trim_end().trim_end_matches('.');
    let activation_source_filter = parse_continuous_subject_filter(subject.original)?;

    Some(
        StaticDefinition::new(StaticMode::SpendManaAsAnyColor {
            spell_filter: None,
            activation_source_filter: Some(activation_source_filter),
        })
        .affected(TargetFilter::Player)
        .description(text.to_string()),
    )
}

/// CR 609.4b: Parse the spell-class-filtered any-type-mana spend static —
/// "You (may|can) spend mana of any type to cast <spell-filter> spells."
/// (Vizier of the Menagerie: "creature spells"). Lowers to
/// `StaticMode::SpendManaAsAnyColor { spell_filter: Some(filter) }`, scoping the
/// any-type-mana concession to spells the controller casts that match the filter
/// (CR 609.4b: the concession changes only how a cost is paid, never the cost).
///
/// The unfiltered board-wide form ("you may spend mana as though it were mana of
/// any color", Chromatic Orrery) is handled separately in `dispatch.rs` and
/// lowers to `spell_filter: None`. This handler requires the explicit "to cast
/// <X> spells" scope so it never swallows the board-wide phrasing.
///
/// The spell-filter is parsed with the same idiom as
/// [`try_parse_top_of_library_cast_permission`]: strip the leading article and
/// the trailing " spell"/" spells", then delegate to `parse_type_phrase` so one
/// branch covers every spell class (creature, artifact, …), not just creatures.
pub(crate) fn try_parse_filtered_spend_any_type_to_cast(
    text: &str,
    lower: &str,
) -> Option<StaticDefinition> {
    // CR 609.4b: "you may"/"you can" surface, then "spend mana of any type to
    // cast ". The "mana of any type" wording (vs "any color") is the spell-cast
    // any-type concession; the runtime treats both as `any_color` in
    // mana_payment.rs (any mana satisfies a colored requirement).
    let rest = nom_tag_lower(text, lower, "you may spend mana of any type to cast ")
        .or_else(|| nom_tag_lower(text, lower, "you can spend mana of any type to cast "))?;

    // Trailing period is optional; strip it so the type phrase is clean.
    let rest = rest.trim_end().trim_end_matches('.').trim_end();

    // Strip a leading article — `parse_type_phrase` expects the bare noun.
    let rest_lower = rest.to_ascii_lowercase();
    let filter_text = nom_tag_lower(rest, &rest_lower, "a ")
        .or_else(|| nom_tag_lower(rest, &rest_lower, "an "))
        .unwrap_or(rest);

    // Drop the trailing " spells"/" spell" token so `parse_type_phrase` sees the
    // bare type/subtype phrase. `strip_suffix` (not `replacen`) anchors to the
    // end, so an interior "spell" (e.g. a hypothetical "spellshaper spells") is
    // never clipped. Without the "spell(s)" anchor this is not the targeted
    // class — bail so the static stays an honest defer rather than over-matching.
    let cleaned = filter_text
        .strip_suffix(" spells") // allow-noncombinator: suffix cleanup on the pre-tokenized filter chunk, not parse dispatch
        .or_else(|| filter_text.strip_suffix(" spell"))?; // allow-noncombinator: suffix cleanup on the pre-tokenized filter chunk, not parse dispatch

    let (filter, tail) = parse_type_phrase(cleaned);
    // A non-empty unconsumed tail means an unrecognised spell class — defer.
    if !tail.trim().is_empty() {
        return None;
    }
    // `parse_type_phrase` never yields `SelfRef`; for input it cannot classify
    // (e.g. an empty `cleaned` from "to cast  spells") it returns a degenerate
    // `Typed` carrying no type constraints and no properties, which would match
    // EVERY spell — exactly the board-wide concession this filtered handler must
    // not emit (CR 609.4b: the scope is the named spell class only). Bail on that
    // empty-filter shape so the line stays an honest defer; any non-degenerate
    // filter (`Or`/`And`, or a `Typed` with constraints) is kept.
    if matches!(
        &filter,
        TargetFilter::Typed(typed) if typed.type_filters.is_empty() && typed.properties.is_empty()
    ) {
        return None;
    }

    Some(
        StaticDefinition::new(StaticMode::SpendManaAsAnyColor {
            spell_filter: Some(filter),
            activation_source_filter: None,
        })
        // For the filtered (`Some`) path `affected` is documentation-only:
        // controller-scoping is enforced at runtime by the explicit
        // `obj.controller != player_id` gate in
        // `player_can_spend_as_any_color_for_spell_object`, which never reads
        // `def.affected`. Kept for intent + structural parity with the
        // board-wide (`None`) form, which DOES consult `affected`.
        .affected(TargetFilter::Controller)
        .description(text.to_string()),
    )
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
    // CR 601.2a: Strip an optional once-per-turn frequency prefix FIRST, before
    // the Bolas-compound and disjunctive branches, so each branch sees the bare
    // "you may play/cast …" lead and inherits the correct frequency. "Once each
    // turn, …" (Assemble the Players, The Fourth Doctor) and the longer "Once
    // during each of your turns, …" synonym both lower to OncePerTurn; absence
    // keeps the Unlimited shape (Realmwalker, Future Sight, Crystal Skull).
    // Strip both `text` and `lower` in lockstep so downstream helpers
    // (alt-cost rider, condition parser, description) see aligned slices from
    // the same character offset.
    let (text, lower, frequency) = if let Some(r) = nom_tag_lower(lower, lower, "once each turn, ")
        .or_else(|| nom_tag_lower(lower, lower, "once during each of your turns, "))
    {
        let stripped_len = lower.len() - r.len();
        (&text[stripped_len..], r, CastFrequency::OncePerTurn)
    } else {
        (text, lower, CastFrequency::Unlimited)
    };

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
            // CR 601.2a: inherit the once-per-turn cap stripped above (the
            // shipping Bolas's Citadel form has no prefix → Unlimited).
            frequency,
            alt_cost,
        })
        .affected(TargetFilter::Any)
        .description(text.to_string());
        if let Some(condition) = parse_top_of_library_permission_condition(rest) {
            def = def.condition(condition);
        }
        return Some(def);
    }

    // CR 305.1 + CR 601.2a + CR 700.6: Disjunctive filtered permission —
    // "You may play <land-filter> and/or cast <spell-filter> from the top of
    // your library." (Crystal Skull, Isu Spyglass, The Fourth Doctor). `Play`
    // mode covers both branches; distinct branch filters merge to
    // `TargetFilter::Or`. Inherits the hoisted frequency so once-per-turn
    // disjunctive permissions keep their per-turn cap. Parsed after the
    // unfiltered Bolas compound so "play lands and cast spells" stays `Any`.
    if let Some(def) = try_parse_disjunctive_top_of_library_cast_permission(text, lower, frequency)
    {
        return Some(def);
    }

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

/// CR 702.170f: Parse "You may plot [filter] cards from the top of your library"
/// — the plot-from-library PERMISSION line (Fblthp, Lost on the Range L4). This
/// is the CR 702.170f effect that allows the plot ability to function in a zone
/// other than hand and authorizes taking the special action there; it emits
/// [`StaticMode::TopOfLibraryPlotPermission`], the permission role — DISTINCT
/// from [`StaticMode::TopOfLibraryHasPlot`] (the L3 grant that the top card
/// *has* plot). The runtime requires both. (Both are also categorically
/// distinct from the cast-permission family: plot is a CR 702.170 special action
/// — Library → Exile, then a later Exile → Stack free cast — not a CR 601.2a
/// Library → Stack cast.)
///
/// Structurally a clone of [`try_parse_top_of_library_cast_permission`], anchored
/// on the plot verb ("you may plot "). The eligibility filter ("nonland") rides
/// `StaticDefinition.affected`, exactly as the cast-permission sibling carries
/// its eligibility. Built for the class: any type/subtype phrase the cast arm
/// accepts is accepted here, so future "you may plot <type> cards from the top
/// of your library" printings slot in without parser changes.
pub(crate) fn try_parse_top_of_library_plot_permission(
    text: &str,
    lower: &str,
) -> Option<StaticDefinition> {
    // "you may plot <filter> from the top of your library"
    let rest = nom_tag_lower(lower, lower, "you may plot ")?;

    // Anchor on " from the top of your library"; the filter text precedes it.
    let (filter_text, _trailing) =
        nom_primitives::split_once_on(rest, " from the top of your library")
            .ok()
            .map(|(_, pair)| pair)?;

    // Strip a leading article so `parse_type_phrase` sees the bare noun.
    let filter_text = nom_tag_lower(filter_text, filter_text, "a ")
        .or_else(|| nom_tag_lower(filter_text, filter_text, "an "))
        .unwrap_or(filter_text);

    // Drop trailing " cards"/" card" so `parse_type_phrase` sees the bare
    // type/subtype phrase ("nonland cards" → "nonland"). Mirrors the
    // " spells"/" spell" replacen idiom of the cast-permission arm; plot
    // operates on cards (it exiles a card), so the noun is "card(s)".
    let cleaned: Cow<str> = if nom_primitives::scan_contains(filter_text, "cards") {
        Cow::Owned(filter_text.replacen(" cards", "", 1))
    } else if nom_primitives::scan_contains(filter_text, "card") {
        Cow::Owned(filter_text.replacen(" card", "", 1))
    } else {
        Cow::Borrowed(filter_text)
    };

    let (filter, _) = parse_type_phrase(&cleaned);

    Some(
        StaticDefinition::new(StaticMode::TopOfLibraryPlotPermission)
            .affected(filter)
            .description(text.to_string()),
    )
}

/// CR 702.170a + CR 702.170f: Parse "The top card of your library has plot[. The
/// plot cost is equal to its mana cost]" — the GRANT line that gives the top
/// library card the plot ability (Fblthp, Lost on the Range L3). Emits
/// [`StaticMode::TopOfLibraryHasPlot`] (the grant role) with `affected =
/// TargetFilter::Any`; the *permission* to actually plot from the library (and
/// its nonland scope) is the companion L4 `TopOfLibraryPlotPermission` — the
/// runtime requires both.
///
/// The optional second sentence is consumed (no capture) so the full line
/// classifies: the plot cost is the card's own mana cost (CR 702.170a),
/// computed at activation synthesis from the live top card, not data carried on
/// the static. The remainder must be empty after consuming the known sentences
/// so an unexpected longer line is not silently swallowed.
pub(crate) fn try_parse_top_of_library_has_plot(
    text: &str,
    lower: &str,
) -> Option<StaticDefinition> {
    let rest = nom_tag_lower(lower, lower, "the top card of your library has plot")?;

    // CR 702.170a: optional intrinsic cost sentence — consumed, never captured.
    let rest =
        nom_tag_lower(rest, rest, ". the plot cost is equal to its mana cost").unwrap_or(rest);

    // Allow a trailing sentence period after either sentence.
    let rest = nom_tag_lower(rest, rest, ".").unwrap_or(rest);

    if !rest.trim().is_empty() {
        return None;
    }

    Some(
        StaticDefinition::new(StaticMode::TopOfLibraryHasPlot)
            .affected(TargetFilter::Any)
            .description(text.to_string()),
    )
}

/// CR 305.1 + CR 601.2a + CR 700.6: Parse the disjunctive filtered top-of-
/// library play/cast permission — "You may play <land-filter> and cast
/// <spell-filter> from the top of your library." — into a single
/// `TopOfLibraryCastPermission { play_mode: Play }` whose `affected` filter is
/// the union of the two branch filters. The `frequency` is supplied by the
/// caller (hoisted once-per-turn prefix), so the disjunctive form covers both
/// the unlimited (Crystal Skull) and once-per-turn (The Fourth Doctor) classes.
///
/// Accepts both "and cast" (Crystal Skull) and "or cast" (The Fourth Doctor,
/// mirroring the graveyard disjunctive connector) before the shared library-top
/// anchor.
fn try_parse_disjunctive_top_of_library_cast_permission(
    text: &str,
    lower: &str,
    frequency: CastFrequency,
) -> Option<StaticDefinition> {
    let rest = nom_tag_lower(lower, lower, "you may play ")?;

    // CR 305.1 + CR 601.2a: Split the land-play branch from the spell-cast
    // branch. Prefer " and cast " (Crystal Skull) but accept " or cast "
    // for the same structural class.
    let (land_branch, spell_branch) = nom_primitives::split_once_on(rest, " and cast ")
        .ok()
        .map(|(_, pair)| pair)
        .or_else(|| {
            nom_primitives::split_once_on(rest, " or cast ")
                .ok()
                .map(|(_, pair)| pair)
        })?;

    let (spell_filter_text, trailing) =
        nom_primitives::split_once_on(spell_branch, " from the top of your library")
            .ok()
            .map(|(_, pair)| pair)?;

    let land_filter = parse_graveyard_branch_filter(land_branch.trim())?;
    let spell_filter = parse_graveyard_branch_filter(spell_filter_text.trim())?;

    // CR 700.6: when both branches resolve to the same filter, collapse the
    // union rather than emitting a redundant `Or`.
    let affected = if land_filter == spell_filter {
        land_filter
    } else {
        TargetFilter::Or {
            filters: vec![land_filter, spell_filter],
        }
    };

    let alt_cost = parse_top_of_library_alt_cost_rider(trailing, text);

    let mut def = StaticDefinition::new(StaticMode::TopOfLibraryCastPermission {
        play_mode: CardPlayMode::Play,
        frequency,
        alt_cost,
    })
    .affected(affected)
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
    // only replaces the mana cost for spells in the controller's built-in cast
    // zones (runtime scope: hand plus command-zone commanders/signature spells,
    // per `cast_free_origin_admits_object`); it does not create an independent
    // cast-from-anywhere permission.
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

#[cfg(test)]
mod spend_any_color_to_activate_abilities_tests {
    use super::*;
    use crate::types::ability::{ControllerRef, TargetFilter, TypeFilter};

    /// CR 609.4b: Agatha's Soul Cauldron / Joiner Adept — activation-source-scoped
    /// any-color spend must lower to `activation_source_filter: Some(creatures you
    /// control)`, not the board-wide form.
    #[test]
    fn parses_creatures_you_control_activation_scope() {
        let text = "You may spend mana as though it were mana of any color to activate abilities of creatures you control.";
        let lower = text.to_ascii_lowercase();
        let tp = TextPair::new(text, &lower);
        let def = try_parse_spend_any_color_to_activate_abilities(text, &tp)
            .expect("activation-source-scoped spend line must parse");

        assert_eq!(def.affected, Some(TargetFilter::Player));

        match def.mode {
            StaticMode::SpendManaAsAnyColor {
                spell_filter: None,
                activation_source_filter: Some(TargetFilter::Typed(typed)),
            } => {
                assert!(typed.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(typed.controller, Some(ControllerRef::You));
            }
            other => panic!(
                "expected SpendManaAsAnyColor {{ activation_source_filter: Some(creatures you control) }}, got {other:?}"
            ),
        }
    }
}

#[cfg(test)]
mod filtered_spend_any_type_tests {
    use super::*;

    /// CR 609.4b: the building block must lower a recognised spell class to a
    /// spell-class-FILTERED `SpendManaAsAnyColor { spell_filter: Some(Typed) }`,
    /// scoped to the source's controller. Drives the helper directly (the
    /// building block, not a single card) so the class — not just Vizier — is
    /// covered.
    #[test]
    fn parses_creature_spell_class_to_filtered_static() {
        let text = "You can spend mana of any type to cast creature spells.";
        let lower = text.to_ascii_lowercase();
        let def = try_parse_filtered_spend_any_type_to_cast(text, &lower)
            .expect("recognised spell class must lower to a filtered static");

        // The concession is "you may" — scoped to the source's controller.
        assert_eq!(def.affected, Some(TargetFilter::Controller));

        match def.mode {
            StaticMode::SpendManaAsAnyColor {
                spell_filter: Some(TargetFilter::Typed(typed)),
                activation_source_filter: None,
            } => assert!(
                typed.type_filters.contains(&TypeFilter::Creature),
                "spell filter must scope to creature spells; got {typed:?}"
            ),
            other => {
                panic!("expected SpendManaAsAnyColor {{ Some(Typed(creature)) }}, got {other:?}")
            }
        }
    }

    /// The "you may" surface must also parse (building block covers both modal
    /// surfaces, not just Vizier's "you can").
    #[test]
    fn parses_you_may_surface() {
        let text = "You may spend mana of any type to cast artifact spells.";
        let lower = text.to_ascii_lowercase();
        assert!(
            try_parse_filtered_spend_any_type_to_cast(text, &lower).is_some(),
            "the \"you may\" surface must parse the same as \"you can\""
        );
    }

    /// CR 609.4b: a degenerate input whose spell class is empty (here a double
    /// space before "spells", so `cleaned` is "") reaches the empty-`Typed`
    /// path in `parse_type_phrase` — a filter that would match EVERY spell, i.e.
    /// the board-wide concession. The empty-filter guard must decline it.
    ///
    /// Non-vacuity: this is the exact input the guard exists for. Remove the
    /// empty-filter guard and this assertion flips (the helper returns
    /// `Some(SpendManaAsAnyColor { Some(Typed{}) })`, an over-match), proving the
    /// guard is load-bearing. The dead `SelfRef` guard the WIP shipped never
    /// fired on this input because `parse_type_phrase` does not yield `SelfRef`.
    #[test]
    fn declines_degenerate_empty_spell_class() {
        let text = "You can spend mana of any type to cast  spells.";
        let lower = text.to_ascii_lowercase();
        assert!(
            try_parse_filtered_spend_any_type_to_cast(text, &lower).is_none(),
            "an empty spell class (matches every spell) must NOT lower to a filtered static"
        );
    }

    /// An unrecognised spell class leaves a non-empty unconsumed tail — the
    /// strict tail guard declines so the line stays an honest defer rather than
    /// over-matching on a partial parse.
    #[test]
    fn declines_unrecognised_spell_class() {
        let text = "You can spend mana of any type to cast wibble spells.";
        let lower = text.to_ascii_lowercase();
        assert!(
            try_parse_filtered_spend_any_type_to_cast(text, &lower).is_none(),
            "an unrecognised spell class must stay an honest defer"
        );
    }
}

#[cfg(test)]
mod draw_from_bottom_tests {
    use super::*;

    /// CR 121.1 + CR 613.11: River Song's exact "Meet in Reverse" line (after
    /// ability-word stripping) lowers to `DrawFromBottom { Controller }`.
    #[test]
    fn parses_controller_draw_from_bottom() {
        let text = "You draw cards from the bottom of your library rather than the top.";
        let lower = text.to_ascii_lowercase();
        let def = parse_draw_from_bottom(&lower, text)
            .expect("River Song's draw-redirect line must lower to a static");
        assert_eq!(
            def.mode,
            StaticMode::DrawFromBottom {
                who: ProhibitionScope::Controller
            }
        );
    }

    /// The subject axis composes with the shared `strip_casting_prohibition_subject`
    /// building block — "each opponent" scopes to `Opponents`, and "instead of"
    /// is an accepted connector variant. Covers the class, not just River Song.
    #[test]
    fn parses_opponents_scope_and_instead_of_connector() {
        let text = "Each opponent draws cards from the bottom of their library instead of the top.";
        let lower = text.to_ascii_lowercase();
        let def = parse_draw_from_bottom(&lower, text)
            .expect("opponent-scoped draw-redirect must lower to a static");
        assert_eq!(
            def.mode,
            StaticMode::DrawFromBottom {
                who: ProhibitionScope::Opponents
            }
        );
    }

    /// The static dispatch chain (`parse_static_line_multi`) must route the
    /// stripped body to `parse_draw_from_bottom` — guards against a future
    /// dispatch-registration regression.
    #[test]
    fn static_line_multi_routes_to_draw_from_bottom() {
        let defs = crate::parser::oracle_static::parse_static_line_multi(
            "You draw cards from the bottom of your library rather than the top.",
        );
        assert_eq!(
            defs.iter().map(|d| &d.mode).collect::<Vec<_>>(),
            vec![&StaticMode::DrawFromBottom {
                who: ProhibitionScope::Controller
            }],
            "static dispatch must lower to a single DrawFromBottom(controller)"
        );
    }

    /// STRICT: a trailing clause must NOT be silently dropped — the redirect has
    /// to be the whole sentence, else the line stays an honest defer.
    #[test]
    fn declines_trailing_clause() {
        let text =
            "You draw cards from the bottom of your library rather than the top and you lose 1 life.";
        let lower = text.to_ascii_lowercase();
        assert!(
            parse_draw_from_bottom(&lower, text).is_none(),
            "a silently-dropped trailing clause must NOT parse"
        );
    }
}
