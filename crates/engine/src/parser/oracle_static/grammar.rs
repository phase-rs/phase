// CR 604 / CR 613 - shared static parser grammar utilities.

use super::oracle_trigger::NthEventTimingKind;
#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;
use crate::types::ability::PlayerFilter;
use nom::character::complete::{alphanumeric1, char, digit1, one_of};
use nom::combinator::{all_consuming, map_res, not, opt, peek, recognize};
use nom::sequence::{delimited, pair, terminated};

/// Lower a parsed rule-static predicate into the runtime static mode.
pub(crate) fn lower_rule_static(
    predicate: RuleStaticPredicate,
    affected: TargetFilter,
    description: &str,
) -> StaticDefinition {
    match predicate {
        RuleStaticPredicate::CantUntap => StaticDefinition::new(StaticMode::CantUntap)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::CantAttack => StaticDefinition::new(StaticMode::CantAttack)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::CantBlock => StaticDefinition::new(StaticMode::CantBlock)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::CantAttackOrBlock => {
            StaticDefinition::new(StaticMode::CantAttackOrBlock)
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::CantCrew => StaticDefinition::new(StaticMode::CantCrew)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::CantBeActivated => {
            StaticDefinition::new(StaticMode::CantBeActivated {
                who: ProhibitionScope::AllPlayers,
                source_filter: TargetFilter::SelfRef,
                exemption: ActivationExemption::None,
                // CR 606.2: not kind-narrowed — blocks any activated ability.
                kind: None,
            })
            .affected(affected)
            .description(description.to_string())
        }
        RuleStaticPredicate::CantBeSacrificed => {
            StaticDefinition::new(StaticMode::Other("CantBeSacrificed".to_string()))
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::MustAttack => StaticDefinition::new(StaticMode::MustAttack)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::MustBlock => StaticDefinition::new(StaticMode::MustBlock)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::MustBeBlocked => {
            StaticDefinition::new(StaticMode::MustBeBlocked { by: None })
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::Goaded => StaticDefinition::new(StaticMode::Goaded)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::BlockOnlyCreaturesWithFlying => {
            StaticDefinition::new(StaticMode::BlockRestriction {
                filter: crate::types::statics::block_only_creatures_with_flying_filter(),
            })
            .affected(affected)
            .description(description.to_string())
        }
        RuleStaticPredicate::Shroud if rule_static_affected_is_player_scope(&affected) => {
            StaticDefinition::new(StaticMode::Shroud)
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::Shroud => StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Shroud,
            }])
            .description(description.to_string()),
        RuleStaticPredicate::Hexproof if rule_static_affected_is_player_scope(&affected) => {
            StaticDefinition::new(StaticMode::Hexproof)
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::Hexproof => StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }])
            .description(description.to_string()),
        RuleStaticPredicate::MayLookAtTopOfLibrary => {
            StaticDefinition::new(StaticMode::MayLookAtTopOfLibrary)
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::LoseAllAbilities => StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::RemoveAllAbilities])
            .description(description.to_string()),
        RuleStaticPredicate::NoMaximumHandSize => {
            StaticDefinition::new(StaticMode::NoMaximumHandSize)
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::MayPlayAdditionalLand => {
            StaticDefinition::new(StaticMode::MayPlayAdditionalLand)
                .affected(affected)
                .description(description.to_string())
        }
    }
}

pub(crate) fn rule_static_affected_is_player_scope(affected: &TargetFilter) -> bool {
    matches!(
        affected,
        TargetFilter::Player
            | TargetFilter::AllPlayers
            | TargetFilter::Controller
            | TargetFilter::OriginalController
            | TargetFilter::ScopedPlayer
            | TargetFilter::SpecificPlayer { .. }
            // CR 607.2d / CR 607.2m (by analogy): "players who last chose <anchor>"
            // is a player-scope subject for rule statics (Two Streams Facility's
            // land-drop grant).
            | TargetFilter::PlayerWhoChoseLabel { .. }
            | TargetFilter::SourceChosenPlayer
            | TargetFilter::ParentTargetController
            | TargetFilter::ParentTargetOwner
            | TargetFilter::TriggeringSpellController
            | TargetFilter::TriggeringSpellOwner
            | TargetFilter::TriggeringPlayer
            | TargetFilter::DefendingPlayer
    ) || matches!(
        affected,
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: Some(_),
            properties,
        }) if type_filters.is_empty() && properties.is_empty()
    )
}

/// Determine player scope for "can't [verb]" patterns based on subject phrasing.
/// Handles "your opponents can't ...", "you can't ...", and "players can't ..." subjects.
pub(crate) fn parse_player_scope_filter(tp: &TextPair<'_>) -> TargetFilter {
    if nom_primitives::scan_contains(tp.lower, "your opponents")
        || nom_tag_tp(tp, "opponents").is_some()
    {
        TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
    } else if nom_tag_tp(tp, "enchanted player").is_some()
        || nom_primitives::scan_contains(tp.lower, "enchanted player")
    {
        // CR 303.4e + CR 702.5d: Player Auras (Curse cycle) scope restrictions
        // to the player this Aura enchants.
        TargetFilter::AttachedTo
    } else if nom_tag_tp(tp, "you ").is_some()
        || nom_primitives::scan_contains(tp.lower, "you can't")
    {
        TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You))
    } else {
        TargetFilter::Typed(TypedFilter::default())
    }
}

/// CR 119.7 + CR 119.8: Determine player scope for "[possessor] life total[s]
/// can't change" patterns. The possessor is a possessive noun phrase ("your",
/// "your opponents'", "each opponent's", "players'") rather than the bare
/// subject form handled by `parse_player_scope_filter`.
pub(crate) fn parse_life_total_scope_filter(lower: &str) -> TargetFilter {
    // Opponent possessives — scoped to opponents only.
    if nom_primitives::scan_contains(lower, "your opponents'")
        || nom_primitives::scan_contains(lower, "each opponent's")
        || nom_primitives::scan_contains(lower, "an opponent's")
    {
        return TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
    }
    // Self possessive — "your life total" / "your life totals" — scoped to controller.
    if nom_primitives::scan_contains(lower, "your life total") {
        return TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You));
    }
    // All-players plural — "players' life totals" / "each player's life total".
    if nom_primitives::scan_contains(lower, "players'")
        || nom_primitives::scan_contains(lower, "each player's")
    {
        return TargetFilter::Typed(TypedFilter::default());
    }
    // Default: all players (matches "Players' life totals can't change" etc.).
    TargetFilter::Typed(TypedFilter::default())
}

/// Extract zone names referenced in Oracle text.
/// Handles "graveyards", "libraries", "exile" and their singular/plural forms.
pub(crate) fn parse_zone_names_from_tp(tp: &TextPair) -> Vec<Zone> {
    let mut zones = Vec::new();
    if nom_primitives::scan_contains(tp.lower, "graveyard") {
        zones.push(Zone::Graveyard);
    }
    if nom_primitives::scan_contains(tp.lower, "librar") {
        zones.push(Zone::Library);
    }
    if nom_primitives::scan_contains(tp.lower, "exile") {
        zones.push(Zone::Exile);
    }
    zones
}

/// CR 601.3 + CR 601.2a: Parse a "from anywhere other than <zone>" casting
/// restriction into the explicit list of *prohibited* zones.
///
/// "Anywhere other than [hand]" is the inverse of `parse_zone_names_from_tp`'s
/// "from [zone-list]" form: it names the single zone a spell *may* be cast from,
/// so the prohibited set is "every cast-capable zone except that one". The only
/// zones a spell can be cast from are Hand, Graveyard, Library, Exile, and
/// Command (Stack/Battlefield are never cast-from zones — CR 601.2a moves a card
/// "from where it is to the stack"), so the complement of `allowed` over that set
/// is the prohibited list. Drannith Magistrate ("anywhere other than their
/// hands") and the static half of the Avatar's Wrath class collapse here.
///
/// Returns `None` when the restriction is not of the "anywhere other than" form,
/// so the caller falls back to the explicit zone-list parser.
pub(crate) fn parse_cast_from_anywhere_other_than_tp(tp: &TextPair) -> Option<Vec<Zone>> {
    if !nom_primitives::scan_contains(tp.lower, "from anywhere other than") {
        return None;
    }
    // Currently only the "their hand(s)" allowed-zone phrasing is printed. The
    // allowed zone is the hand; the prohibited set is its complement.
    let allowed = if nom_primitives::scan_contains(tp.lower, "their hand")
        || nom_primitives::scan_contains(tp.lower, "your hand")
    {
        Zone::Hand
    } else {
        return None;
    };
    Some(crate::parser::oracle_target::cast_capable_zones_except(
        allowed,
    ))
}

/// Parse a color name from Oracle text, delegating to the shared nom color combinator.
///
/// Accepts leading/trailing whitespace and requires complete consumption (no trailing text
/// beyond whitespace). This preserves the original behavior of the match-based implementation.
pub(crate) fn parse_named_color(text: &str) -> Option<ManaColor> {
    let lower = text.trim().to_ascii_lowercase();
    let (rest, color) = nom_primitives::parse_color.parse(&lower).ok()?;
    if rest.is_empty() {
        Some(color)
    } else {
        None
    }
}

/// CR 614.1b: Parse a step name from Oracle text using nom combinators.
pub(crate) fn parse_step_name_nom(input: &str) -> OracleResult<'_, Phase> {
    alt((
        value(Phase::Draw, tag("draw step")),
        value(Phase::Untap, tag("untap step")),
        value(Phase::Upkeep, tag("upkeep step")),
    ))
    .parse(input)
}

/// CR 205.2a: Check if a lowercase descriptor names a core card type that can modify
/// "creatures" (e.g., "artifact" in "artifact creatures"). Returns the TypeFilter if so.
/// Delegates to the existing nom type-word combinator for authoritative type recognition.
pub(crate) fn try_parse_core_type_descriptor(descriptor_lower: &str) -> Option<TypeFilter> {
    match nom_target::parse_type_filter_word(descriptor_lower) {
        Ok(("", tf)) => match tf {
            TypeFilter::Artifact
            | TypeFilter::Enchantment
            | TypeFilter::Land
            | TypeFilter::Planeswalker => Some(tf),
            _ => None, // "creature", "instant", "sorcery" are not creature modifiers
        },
        _ => None,
    }
}

/// Check that a string is one or more capitalized words.
/// Build a TypedFilter for a subtype, using the correct core type.
/// Uses `infer_core_type_for_subtype` to map artifact/land/enchantment subtypes
/// to their parent type instead of defaulting everything to Creature.
pub(crate) fn typed_filter_for_subtype(subtype: &str) -> TypedFilter {
    use crate::types::ability::TypeFilter;
    // CR 205.4a + CR 205.3m: a compound "<supertype> <subtype>" descriptor
    // ("Legendary Human", "Snow Elf") peels its leading supertype word into a
    // `HasSupertype` property so the remainder resolves to the REAL subtype —
    // rather than fabricating a zero-match `Subtype("Legendary Human")` (General's
    // Enforcer "Legendary Humans you control", Kashi-Tribe Elite "Legendary
    // Snakes you control").
    if let Some((supertype, rest)) = split_leading_supertype(subtype) {
        let mut filter = typed_filter_for_subtype(rest);
        filter
            .properties
            .push(FilterProp::HasSupertype { value: supertype });
        return filter;
    }
    // CR 110.5a + CR 506.3: a bare battlefield descriptor ("Untapped", "Tapped",
    // "Attacking", …) used as a whole creature descriptor names a status the
    // creature HAS (CR 110.5a: "status is not a characteristic") or a combat role
    // it's in (CR 506.3), not a creature subtype — resolve it to a typed FilterProp
    // instead of fabricating a zero-match `Subtype("Untapped")` (Builder's
    // Blessing / Castle "Untapped creatures you control get +0/+2").
    if let Some(filter) = bare_status_creature_filter(subtype) {
        return filter;
    }
    if let Some(core_type) = infer_core_type_for_subtype(subtype) {
        let type_filter = match core_type {
            crate::types::card_type::CoreType::Artifact => TypeFilter::Artifact,
            crate::types::card_type::CoreType::Land => TypeFilter::Land,
            crate::types::card_type::CoreType::Enchantment => TypeFilter::Enchantment,
            _ => TypeFilter::Creature,
        };
        TypedFilter::new(type_filter).subtype(subtype.to_string())
    } else {
        TypedFilter::creature().subtype(subtype.to_string())
    }
}

/// CR 110.5a + CR 506.3: Recognize a bare battlefield descriptor ("untapped",
/// "tapped", "attacking", "blocking", "transformed", "suspected") used as a whole
/// creature descriptor and resolve it to a creature filter carrying the matching
/// `FilterProp`. These name a permanent's status (CR 110.5, "not a characteristic"
/// per CR 110.5a) or its combat role (CR 506.3), never a creature subtype.
/// Reuses the `parse_combat_status_prefix`
/// allowlist (appending a space to satisfy its prefix-boundary rule, then
/// requiring the whole word be consumed) so "Untapped creatures you control"
/// filters on `FilterProp::Untapped` rather than a zero-match `Subtype("Untapped")`.
fn bare_status_creature_filter(descriptor: &str) -> Option<TypedFilter> {
    let with_space = format!("{} ", descriptor.to_lowercase());
    let (prop, consumed) = crate::parser::oracle_target::parse_combat_status_prefix(&with_space)?;
    (consumed == with_space.len()).then(|| TypedFilter::creature().properties(vec![prop]))
}

/// CR 205.4a: Peel a leading supertype word off a compound "<supertype>
/// <subtype>" subject descriptor ("Legendary Human", "Snow Elf"), returning the
/// supertype and the original-case remainder. Returns `None` for a bare
/// supertype (no following subtype) or a descriptor with no leading supertype,
/// so a plain subtype falls through to the subtype path unchanged.
fn split_leading_supertype(descriptor: &str) -> Option<(Supertype, &str)> {
    let lower = descriptor.to_lowercase();
    // Consume the supertype word AND its separating space atomically: a bare
    // supertype (no following subtype) fails the trailing ` ` and declines here,
    // so a plain subtype falls through to the subtype path unchanged.
    let (rest_lower, supertype) = terminated(
        nom_target::parse_supertype_word,
        tag::<_, _, OracleError<'_>>(" "),
    )
    .parse(&lower)
    .ok()?;
    let rest = descriptor[descriptor.len() - rest_lower.len()..].trim();
    (!rest.is_empty()).then_some((supertype, rest))
}

pub(crate) fn is_capitalized_words(s: &str) -> bool {
    let trimmed = s.trim();
    !trimmed.is_empty()
        && trimmed
            .split_whitespace()
            .all(|w| w.chars().next().is_some_and(|c| c.is_uppercase()))
}

/// CR 205.3m: Parse a capitalized-subtype list of the form
/// `<Subtype>[ (or|and)[ a] <Subtype>]*` followed by space-delimited predicate text.
/// Returns (filter, remainder_starting_at_predicate). Invoked AFTER the caller has
/// already consumed a leading `"<subject> that's a "` prefix.
///
/// For a single subtype → `TargetFilter::Typed(typed_filter_for_subtype(X).controller(You))`.
/// For multiple → `TargetFilter::Or` of per-subtype typed filters (all controller=You).
/// Plural subtypes are normalized via `parse_subtype`.
pub(crate) fn try_parse_thats_a_subtype_list(input: &str) -> Option<(TargetFilter, &str)> {
    use nom::multi::separated_list1;

    fn parse_subtype_word(input: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
        use nom::bytes::complete::take_while1;
        let (rest, word) = take_while1(|c: char| c.is_alphabetic() || c == '-').parse(input)?;
        if !word.chars().next().is_some_and(|c| c.is_uppercase()) {
            return Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Fail,
            )));
        }
        Ok((rest, word))
    }

    fn parse_conjunction(input: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
        alt((tag(" or a "), tag(" and a "), tag(" or "), tag(" and "))).parse(input)
    }

    let (rest, words): (&str, Vec<&str>) = separated_list1(parse_conjunction, parse_subtype_word)
        .parse(input)
        .ok()?;
    // Predicate must follow a space
    let predicate = rest.strip_prefix(' ')?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    if predicate.is_empty() {
        return None;
    }
    let filters: Vec<TargetFilter> = words
        .iter()
        .map(|w| {
            let canonical = parse_subtype(w)
                .map(|(c, _)| c)
                .unwrap_or_else(|| w.to_string());
            TargetFilter::Typed(typed_filter_for_subtype(&canonical).controller(ControllerRef::You))
        })
        .collect();
    let filter = if filters.len() == 1 {
        filters.into_iter().next()?
    } else {
        TargetFilter::Or { filters }
    };
    Some((filter, predicate))
}

/// CR 604.1 + CR 611.3a + CR 613.1f: a non-Continuous restriction primary
/// (e.g. `CantBeBlocked`) may be conjoined with a trailing keyword grant
/// ("can't be blocked and has shroud."). A single `StaticDefinition` can carry
/// only one `StaticMode`, so when the primary is NON-Continuous and a trailing
/// "and has <kw-list>" clause is present, emit a companion `Continuous` def for
/// the recovered keyword(s), inheriting the primary's suffix condition.
///
/// GAP-1 guard: only appends a companion when the primary is non-Continuous —
/// benign Continuous lines ("gets +1/+1 and has trample and lifelink") are
/// already merged into one def by `parse_continuous_modifications` and must NOT
/// be split.
pub(crate) fn with_keyword_companion(
    primary: StaticDefinition,
    predicate: &str,
    affected: &TargetFilter,
    description: &str,
    suffix_cond: Option<&StaticCondition>,
) -> Vec<StaticDefinition> {
    if matches!(primary.mode, StaticMode::Continuous) {
        return vec![primary];
    }
    let mut companion_mods = Vec::new();
    if let Some(keyword_text) = extract_keyword_clause(predicate) {
        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
            push_grant_clause_modifications(&mut companion_mods, part.as_ref(), None);
        }
    }
    if companion_mods.is_empty() {
        return vec![primary];
    }
    let mut companion = StaticDefinition::continuous()
        .affected(affected.clone())
        .modifications(companion_mods)
        .description(description.to_string());
    if let Some(cond) = suffix_cond {
        companion.condition = Some(cond.clone());
    }
    vec![primary, companion]
}

/// CR 613.1f + CR 611.3a: Parse a comma-and list of "<keyword> if <condition>"
/// clauses (Multiclass Baldric: "lifelink if you control a Cleric, deathtouch
/// if you control a Rogue, ..."). The successful parse IS the detector — no
/// `contains`. The leading "has " prefix is stripped by the caller.
pub(crate) fn parse_conditional_keyword_list(
    input: &str,
) -> OracleResult<'_, Vec<(Keyword, StaticCondition)>> {
    separated_list1(
        // Oxford-comma tolerant: longest separator first.
        alt((tag(", and "), tag(" and "), tag(", "))),
        nom::sequence::pair(
            map_keyword_run,
            preceded(tag(" if "), parse_attached_condition_run),
        ),
    )
    .parse(input)
}

/// Parse a single keyword spelled as a run of alphabetic words, returning the
/// mapped `Keyword`. Consumes greedily up to (but not including) " if ".
pub(crate) fn map_keyword_run(input: &str) -> OracleResult<'_, Keyword> {
    let (rest, word) = take_until::<_, _, OracleError<'_>>(" if ").parse(input)?;
    match map_keyword(word.trim()) {
        Some(kw) => Ok((rest, kw)),
        None => Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::MapRes,
        ))),
    }
}

/// Parse a condition run up to the next list separator (", " / " and ") or the
/// end of input, delegating the recovered text to `parse_attached_static_condition`.
///
/// `take_until(", ")` is tried before `take_until(" and ")` and both before the
/// `rest` fallback: a `, ` separator (also the prefix of `, and `) terminates the
/// clause first; the bare ` and ` form is the joiner of the final two members;
/// `rest` captures the last member, which has no trailing separator. The
/// recovered span is a single subtype-presence condition with no embedded
/// separators, so the shortest non-empty match is always the correct boundary.
pub(crate) fn parse_attached_condition_run(input: &str) -> OracleResult<'_, StaticCondition> {
    let (remaining, cond_span) = alt((
        take_until::<_, _, OracleError<'_>>(", "),
        take_until(" and "),
        rest,
    ))
    .parse(input)?;
    let cond_text = cond_span.trim().trim_end_matches('.');
    match parse_attached_static_condition(cond_text) {
        Some(cond) => Ok((remaining, cond)),
        None => Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::MapRes,
        ))),
    }
}

/// Parse the predicate of an enchanted/equipped grant, handling:
/// - Non-standard keyword phrasings: "can attack as though it had haste", "can't be blocked"
/// - Conditional grants: "gets +1/+1 as long as you control a Wizard"
/// - Compound restriction + keyword grants: "can't be blocked and has shroud"
/// - Per-subtype conditional keyword lists: "lifelink if you control a Cleric, ..."
/// - Turn-gated alternatives: "has deathtouch during your turn. Otherwise, it has reach."
/// - Standard continuous grants: "gets +N/+M", "has keyword", "for each", "where X is"
///
/// Returns a `Vec` because compound forms produce more than one
/// `StaticDefinition` (a single `StaticDefinition` carries only one
/// `StaticMode`). Simple lines return a length-1 vec; unparsed lines an empty
/// vec.
///
/// CR 509.1c: Capture the inner "<quality>" of a filtered "must be blocked by
/// <quality> if able" lure conjunct.
///
/// The BARE form ("must be blocked if able" → `StaticMode::MustBeBlocked { by:
/// None }`) is modeled by `try_split_and_must_attack_block` /
/// `RuleStaticPredicate::MustBeBlocked`. The FILTERED form ("must be blocked by
/// a Dalek if able", "must be blocked by an Eldrazi if able") lowers to the
/// parameterized `StaticMode::MustBeBlocked { by: Some(filter) }`. This
/// combinator captures ONLY the filtered form — the `tag("must be blocked by ")`
/// requires the "by " that the bare form lacks — and returns the inner quality
/// span; the caller parses it into a `TargetFilter`. The successful combinator
/// parse IS the detector (no `contains`/`find` dispatch).
pub(crate) fn parse_must_be_blocked_by_quality(input: &str) -> OracleResult<'_, &str> {
    let (rest, _) = tag("must be blocked by ").parse(input)?;
    let (after, inner) = take_until(" if able").parse(rest)?;
    let (after, _) = tag(" if able").parse(after)?;
    Ok((after, inner))
}

/// CR 509.1c + CR 105.4: Lower a captured "<quality>" span (e.g.
/// "a Dalek", "an Eldrazi", "a creature of the chosen color") to the blocker
/// `TargetFilter`. Composes the SAME quality combinators `CantBeBlockedBy` uses
/// (`parse_chosen_qualifier_subject`, then `parse_type_phrase`). Returns `None`
/// when the quality fails to constrain the blocker at all — either
/// `TargetFilter::Any` or the empty `Typed` filter `parse_type_phrase` yields for
/// an UNRECOGNIZED noun — so an unparseable requirement is never silently
/// weakened to "any blocker satisfies".
fn must_be_blocked_quality_to_filter(quality: &str) -> Option<TargetFilter> {
    // Operate on the lowercase quality (mirrors the `CantBeBlockedBy` path, whose
    // `filter_text` is a slice of the already-lowercased predicate). `TextPair`
    // requires `lower` to be the lowercase of `original`, so pair them honestly
    // even when the caller passes mixed-case input (e.g. a direct unit test).
    let quality_lower = quality.to_lowercase();
    let quality_tp = TextPair::new(quality, &quality_lower);
    let filter = parse_chosen_qualifier_subject(&quality_tp).unwrap_or_else(|| {
        let (f, _) = parse_type_phrase(&quality_lower);
        f
    });
    filter_constrains_blocker(&filter).then_some(filter)
}

/// CR 509.1c: Does `filter` actually narrow the set of legal blockers? An
/// unconstrained filter — `TargetFilter::Any`, or an empty `Typed` carrying no
/// type, property, or controller constraint (what `parse_type_phrase` returns for
/// an unrecognized noun like "a splorf") — matches every blocker and therefore
/// expresses no quality requirement. Lowering such a filter into a
/// `MustBeBlocked { by }` would silently degrade "must be blocked by <X>" to
/// "must be blocked by anything"; rejecting it lets callers surface the gap.
fn filter_constrains_blocker(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Any => false,
        TargetFilter::Typed(typed) => {
            !typed.type_filters.is_empty()
                || !typed.properties.is_empty()
                || typed.controller.is_some()
        }
        _ => true,
    }
}

/// CR 509.1c: Parse a filtered "must be blocked by <quality> if able" span
/// anchored at the start of `input` → the blocker `TargetFilter`. Used by the
/// conditional attached-grant path (Ace's Baseball Bat) where the residual
/// conjunct is already isolated. Returns `None` for the bare form or an
/// unrecognized quality.
pub(crate) fn parse_must_be_blocked_by_filter(input: &str) -> Option<TargetFilter> {
    let (_, quality) = parse_must_be_blocked_by_quality(input).ok()?;
    must_be_blocked_quality_to_filter(quality)
}

/// CR 509.1c: Classification of a scanned "must be blocked by <quality> if able"
/// conjunct. Distinguishes an ABSENT conjunct (no `Some`) from a PRESENT one,
/// and within present, whether the quality is recognized vs. unrecognized — so
/// the un-gated attached-grant path can surface an `Unimplemented` residual for
/// an unrecognized quality instead of silently dropping the block requirement.
pub(crate) enum MustBeBlockedByConjunct {
    /// The quality lowered to a recognized blocker `TargetFilter`.
    Recognized(TargetFilter),
    /// The conjunct is present but its quality is unrecognized (would weaken to
    /// `TargetFilter::Any`). Carries the reconstructed conjunct text so the
    /// requirement can be surfaced as an `Unimplemented` residual diagnostic —
    /// never silently dropped.
    Unrecognized(String),
}

/// Scan the predicate for a filtered "must be blocked by <quality> if able"
/// conjunct at any word boundary and classify it. The successful combinator
/// parse IS the detector — `scan_at_word_boundaries` tries
/// `parse_must_be_blocked_by_quality` at each word start, so there is no
/// `contains`/`find` dispatch. Returns `None` when only the bare form or no lure
/// is present.
pub(crate) fn extract_must_be_blocked_by_conjunct(
    predicate: &str,
) -> Option<MustBeBlockedByConjunct> {
    let lower = predicate.to_lowercase();
    let quality =
        nom_primitives::scan_at_word_boundaries(&lower, parse_must_be_blocked_by_quality)?;
    Some(match must_be_blocked_quality_to_filter(quality) {
        Some(filter) => MustBeBlockedByConjunct::Recognized(filter),
        None => {
            MustBeBlockedByConjunct::Unrecognized(format!("must be blocked by {quality} if able"))
        }
    })
}

/// CR 509.1c: Build the sibling `Effect::Unimplemented` residual for an attached
/// grant conjunct the typed static modes can't model. Carried inside a
/// `GrantAbility` continuous modification so coverage flags the gap (stable
/// category key `"attached_grant_unmodeled_conjunct"`) and the swallow check
/// defers, rather than silently dropping the requirement. Shared by the gated
/// (`try_parse_inverted_attached_combat_grant`) and un-gated attached-grant
/// paths so both surface unrecognized conjuncts identically.
pub(crate) fn attached_grant_unmodeled_conjunct_residual(
    affected: TargetFilter,
    residual_text: &str,
) -> StaticDefinition {
    StaticDefinition::continuous()
        .affected(affected)
        .modifications(vec![ContinuousModification::GrantAbility {
            definition: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                crate::types::ability::Effect::unimplemented(
                    "attached_grant_unmodeled_conjunct",
                    residual_text.to_string(),
                ),
            )),
        }])
        .description(residual_text.to_string())
}

/// CR 509.1b + CR 604.1 + CR 611.3a + CR 613.1f: Enchanted/equipped predicate dispatch.
pub(crate) fn parse_enchanted_equipped_predicate(
    predicate: &str,
    affected: TargetFilter,
    description: &str,
) -> Vec<StaticDefinition> {
    let pred_lower = predicate.to_lowercase();
    let pred_tp = TextPair::new(predicate, &pred_lower);

    // --- PATTERN 3b: ". Otherwise, [it] has <kw>" turn-gated alternative ---
    // CR 604.1 + CR 611.3a + CR 613.1f: head clause gated DuringYourTurn (via the
    // standard predicate path's `strip_suffix_turn_condition`), companion gated
    // Not(DuringYourTurn). Hunter's Blowgun: "Equipped creature has deathtouch
    // during your turn. Otherwise, it has reach."
    type VE<'a> = OracleError<'a>;
    if let Some((head_tp, tail_tp)) = pred_tp
        .split_around(". otherwise, ")
        .or_else(|| pred_tp.split_around(". otherwise "))
    {
        let head = head_tp.original.trim();
        // CR 604.1: the head carries the gating turn condition
        // ("has deathtouch during your turn"). Strip it to DuringYourTurn, then
        // parse the bare keyword grant.
        let (head_predicate, turn_condition) = strip_suffix_turn_condition(head);
        if let Some(mut primary) =
            parse_continuous_gets_has(&head_predicate, affected.clone(), description)
        {
            // Recover the head's EFFECTIVE gating condition (CR 611.3a — the
            // companion must be the strict complement of whatever gates the head):
            //   (a) a trailing turn condition ("during your turn") stripped above
            //       → DuringYourTurn (Hunter's Blowgun); or
            //   (b) an "as long as <cond>" condition carried on the parsed head def
            //       (e.g. Clutch of Undeath "gets +3/+3 as long as it's a Zombie");
            //       `parse_continuous_gets_has` populates `primary.condition` from
            //       its own " as long as " split.
            // If neither is present there is no recoverable head condition: do NOT
            // emit an unconditional companion (that would apply both clauses at
            // once). Bail out of PATTERN 3b so the line falls through to the
            // single-def path, preventing any regression on unanticipated
            // "otherwise" phrasings.
            let head_condition = turn_condition.clone().or_else(|| primary.condition.clone());
            if let Some(head_condition) = head_condition {
                // The head def retains its own gating condition: for the turn case
                // re-assert it; for the as-long-as case it is already preserved.
                primary.condition = Some(head_condition.clone());
                // The tail may start with "it " / "it has " — strip both to reach the
                // bare continuous predicate, then re-add "has " so
                // `parse_continuous_gets_has` sees a verb.
                let tail_lower = tail_tp.lower;
                let tail_orig = tail_tp.original;
                let tail_predicate =
                    if let Some(rest) = nom_tag_lower(tail_orig, tail_lower, "it has ") {
                        format!("has {rest}")
                    } else if let Some(rest) = nom_tag_lower(tail_orig, tail_lower, "it ") {
                        rest.to_string()
                    } else {
                        tail_orig.trim().to_string()
                    };
                if let Some(mut companion) =
                    parse_continuous_gets_has(&tail_predicate, affected.clone(), description)
                {
                    // CR 611.3a + CR 613.1f: companion is the strict complement gate
                    // of the head's effective condition. Mutually exclusive so the
                    // two clauses never apply simultaneously.
                    companion.condition = Some(StaticCondition::Not {
                        condition: Box::new(head_condition),
                    });
                    return vec![primary, companion];
                }
            }
        }
    }

    // --- PATTERN 3a: "[has ]<kw> if <cond>, <kw> if <cond>, ..." list ---
    // CR 613.1f + CR 611.3a: per-subtype conditional keyword grants. Each clause
    // becomes a Continuous{AddKeyword} gated on its own condition. The combinator
    // parse IS the detector (no contains). Multiclass Baldric.
    {
        let list_input = nom_tag_lower(&pred_lower, &pred_lower, "has ").unwrap_or(&pred_lower);
        if let Ok((rest, pairs)) = parse_conditional_keyword_list(list_input) {
            if rest.trim().trim_end_matches('.').is_empty() && pairs.len() > 1 {
                return pairs
                    .into_iter()
                    .map(|(kw, cond)| {
                        StaticDefinition::continuous()
                            .affected(affected.clone())
                            .modifications(vec![ContinuousModification::AddKeyword { keyword: kw }])
                            .condition(cond)
                            .description(description.to_string())
                    })
                    .collect();
            }
        }
    }

    // CR 502.3: enchanted/equipped host untap restriction with optional trailing
    // "if …" / "as long as …" (Venarian Gold, Winter's Rest canonical rewrite).
    if nom_primitives::scan_contains(&pred_lower, "doesn't untap during")
        || nom_primitives::scan_contains(&pred_lower, "don\u{2019}t untap during")
    {
        if let Some(predicate) = parse_rule_static_predicate(predicate) {
            let mut def = lower_rule_static(predicate, affected.clone(), description);
            if matches!(predicate, RuleStaticPredicate::CantUntap) {
                if let Some((_, after_cond)) = pred_tp.split_around(" as long as ") {
                    let condition_text = after_cond.original.trim().trim_end_matches('.');
                    def.condition = Some(
                        parse_static_condition(condition_text)
                            .or_else(|| parse_attached_static_condition(condition_text))
                            .unwrap_or(StaticCondition::Unrecognized {
                                text: condition_text.to_string(),
                            }),
                    );
                } else if let Some(condition) = extract_cant_untap_condition(&pred_lower) {
                    def.condition = Some(condition);
                }
            }
            return vec![def];
        }
    }

    // CR 611.2 + CR 701.27: restriction-only enchanted/equipped predicates
    // ("can't attack, block, or transform" — Bound by Moonsilver class). Must
    // precede continuous-grant parsing, which would otherwise return an empty vec
    // and let the line fall through to a SelfRef combat lock on the Aura source.
    if let Some(modes) = parse_restriction_modes(pred_lower.trim().trim_end_matches('.')) {
        return modes
            .into_iter()
            .map(|mode| {
                StaticDefinition::new(mode)
                    .affected(affected.clone())
                    .description(description.to_string())
            })
            .collect();
    }

    // --- Non-standard keyword phrasings (check before continuous grants) ---

    // CR 702.10: "can attack as though it had haste" → AddKeyword(Haste)
    if nom_primitives::scan_contains(&pred_lower, "can attack as though it had haste") {
        return vec![StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }])
            .description(description.to_string())];
    }

    // CR 702.3b: "can attack as though <pronoun> didn't have defender" →
    // CanAttackWithDefender. Accepts both pronoun forms so plural subjects
    // ("Creatures you control …they didn't…") routed through the
    // creatures-you-control prefix handler (line ~620) land here.
    if alt((
        tag::<_, _, VE>("can attack as though it didn't have defender"),
        tag::<_, _, VE>("can attack as though they didn't have defender"),
    ))
    .parse(pred_lower.as_str())
    .is_ok()
    {
        return vec![StaticDefinition::new(StaticMode::CanAttackWithDefender)
            .affected(affected)
            .description(description.to_string())];
    }

    // CR 509.1b: "can't be blocked" on enchanted/equipped creature
    //
    // Only peel a trailing static-grant " unless " rider (Heroic Defiance:
    // "gets +3/+3 unless it shares a color…") when the split point sits OUTSIDE a
    // quoted/granted ability. A granted ability's own inner "unless" (e.g. Sunken
    // Field's "Counter target spell unless its controller pays {1}") must stay
    // with the quoted text — the body has balanced double quotes iff the split is
    // outside any "...".
    let unless_split = pred_tp
        .split_around(" unless ")
        .filter(|(body, _)| body.original.chars().filter(|&c| c == '"').count() % 2 == 0);
    let (body_tp, suffix_condition) = if let Some((body_tp, _)) = unless_split {
        (
            body_tp,
            super::shared::parse_unless_static_condition(&pred_tp),
        )
    } else if let Some((body_tp, condition_tp)) = pred_tp.split_around(" as long as ") {
        let condition_text = condition_tp.original.trim().trim_end_matches('.');
        (
            body_tp,
            Some(parse_attached_static_condition(condition_text).unwrap_or(
                StaticCondition::Unrecognized {
                    text: condition_text.to_string(),
                },
            )),
        )
    } else {
        (pred_tp, None)
    };
    let body_lower = body_tp.lower;

    if nom_tag_lower(body_lower, body_lower, "can't be blocked").is_some() {
        // "can't be blocked except by" → CantBeBlockedExceptBy
        if let Some(rest) = nom_tag_lower(body_lower, body_lower, "can't be blocked except by ") {
            let mut def = StaticDefinition::new(StaticMode::CantBeBlockedExceptBy {
                kind: classify_block_exception(rest),
            })
            .affected(affected.clone())
            .description(description.to_string());
            if let Some(condition) = &suffix_condition {
                def.condition = Some(condition.clone());
            }
            return with_keyword_companion(
                def,
                body_tp.original,
                &affected,
                description,
                suffix_condition.as_ref(),
            );
        }
        // CR 509.1b: "can't be blocked by <filter>" → CantBeBlockedBy
        if let Some(rest) = nom_tag_lower(body_lower, body_lower, "can't be blocked by ") {
            let filter_text = rest.trim_end_matches('.');
            // CR 105.4 + CR 608.2c (issue #327): see parallel comment in
            // `parse_static_line_inner`'s CantBeBlockedBy branch.
            let filter_text_tp = TextPair::new(filter_text, filter_text);
            let filter = parse_chosen_qualifier_subject(&filter_text_tp).unwrap_or_else(|| {
                let (f, _) = parse_type_phrase(filter_text);
                f
            });
            if !matches!(filter, TargetFilter::Any) {
                let mut def = StaticDefinition::new(StaticMode::CantBeBlockedBy { filter })
                    .affected(affected.clone())
                    .description(description.to_string());
                if let Some(condition) = &suffix_condition {
                    def.condition = Some(condition.clone());
                }
                return with_keyword_companion(
                    def,
                    body_tp.original,
                    &affected,
                    description,
                    suffix_condition.as_ref(),
                );
            }
        }
        let mut def = StaticDefinition::new(StaticMode::CantBeBlocked)
            .affected(affected.clone())
            .description(description.to_string());
        if let Some(condition) = &suffix_condition {
            def.condition = Some(condition.clone());
        }
        return with_keyword_companion(
            def,
            body_tp.original,
            &affected,
            description,
            suffix_condition.as_ref(),
        );
    }

    // --- Conditional grants: split "as long as" before passing to continuous parser ---
    // Handles both "gets +1/+1 as long as ..." and "has flying as long as ..."
    if let Some((before_cond, after_cond)) = pred_tp.split_around(" as long as ") {
        let continuous_text = before_cond.original;
        let condition_text = after_cond.original.trim().trim_end_matches('.');
        if let Some(mut def) =
            parse_continuous_gets_has(continuous_text, affected.clone(), description)
        {
            let condition = parse_attached_static_condition(condition_text).unwrap_or(
                StaticCondition::Unrecognized {
                    text: condition_text.to_string(),
                },
            );
            def.condition = Some(condition);
            return vec![def];
        }
    }

    // --- STANDARD DEFAULT (GAP-1 regression guard): whole-predicate continuous
    // parse. "gets +N/+M and has trample and lifelink" is merged into ONE
    // Continuous def by `parse_continuous_modifications`, so it returns here and
    // is NEVER split. ---
    {
        let mut defs = Vec::new();
        // CR 611.3a: parse the grant from the unless/as-long-as-stripped body and
        // attach any trailing `suffix_condition` (Heroic Defiance: "gets +3/+3
        // unless it shares a color with the most common color among all
        // permanents"), rather than parsing the whole predicate and dropping it.
        if let Some(mut def) =
            parse_continuous_gets_has(body_tp.original, affected.clone(), description)
        {
            if let Some(condition) = &suffix_condition {
                def.condition = Some(condition.clone());
            }
            defs.push(def);
        }
        // CR 509.1c: "<grant> and must be blocked by <filter> if able"
        // (Slayer's Cleaver: "Equipped creature gets +3/+1 and must be blocked
        // by an Eldrazi if able."). `parse_continuous_modifications` models the
        // P/T/keyword grant; this branch models the filtered blocking
        // requirement as the typed `MustBeBlocked { by: Some(filter) }` static
        // (unconditional — this non-conditional path has no "as long as" gate).
        match extract_must_be_blocked_by_conjunct(predicate) {
            Some(MustBeBlockedByConjunct::Recognized(filter)) => {
                defs.push(
                    StaticDefinition::new(StaticMode::MustBeBlocked { by: Some(filter) })
                        .affected(affected.clone())
                        .description(description.to_string()),
                );
            }
            // CR 509.1c: the lure conjunct is present but its quality is
            // unrecognized (would weaken to `TargetFilter::Any`). Surface an
            // `Unimplemented` residual so coverage flags the gap — mirroring the
            // gated path (`try_parse_inverted_attached_combat_grant`) — instead
            // of silently dropping the blocking requirement.
            Some(MustBeBlockedByConjunct::Unrecognized(residual)) => {
                defs.push(attached_grant_unmodeled_conjunct_residual(
                    affected.clone(),
                    &residual,
                ));
            }
            None => {}
        }
        defs
    }
}

pub(crate) fn push_dynamic_pt_modifications(
    modifications: &mut Vec<ContinuousModification>,
    power: i32,
    toughness: i32,
    quantity: QuantityExpr,
) {
    if power != 0 {
        modifications.push(ContinuousModification::AddDynamicPower {
            value: scale_pt_quantity(power, &quantity),
        });
    }
    if toughness != 0 {
        modifications.push(ContinuousModification::AddDynamicToughness {
            value: scale_pt_quantity(toughness, &quantity),
        });
    }
}

pub(crate) fn scale_pt_quantity(amount: i32, quantity: &QuantityExpr) -> QuantityExpr {
    match amount {
        1 => quantity.clone(),
        -1 => QuantityExpr::Multiply {
            factor: -1,
            inner: Box::new(quantity.clone()),
        },
        n => QuantityExpr::Multiply {
            factor: n,
            inner: Box::new(quantity.clone()),
        },
    }
}

/// A member of a "loses all [other] abilities, card types, and creature types"
/// enumeration. Parser-local — maps to one `ContinuousModification` each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LossMember {
    Abilities,
    CardTypes,
    CreatureTypes,
}

/// CR 205.1a + CR 613.1d/f: Parse a "loses all [other] <list>" enumeration at
/// the start of `input` (lowercase). The list is a comma-and enumeration of
/// `abilities` / `card types` / `creature types` in any subset and order, so
/// `separated_list1` over a three-way `alt` covers every combination — the
/// literal substrings "loses all other card types" never appear contiguously
/// in the Oxford-comma form, so whole-phrase `tag()` arms would be dead code.
pub(crate) fn parse_loss_enumeration(input: &str) -> OracleResult<'_, Vec<LossMember>> {
    preceded(
        alt((
            tag("loses all other "),
            tag("lose all other "),
            tag("loses all "),
            tag("lose all "),
        )),
        separated_list1(
            // Oxford-comma tolerant: longest separator first so ", and "
            // is not pre-consumed by ", ".
            alt((tag(", and "), tag(" and "), tag(", "))),
            alt((
                value(LossMember::Abilities, tag("abilities")),
                value(LossMember::CardTypes, tag("card types")),
                value(LossMember::CreatureTypes, tag("creature types")),
            )),
        ),
    )
    .parse(input)
}

/// Scan `lower` for a "loses all [other] ..." enumeration at any word boundary
/// (the clause appears mid-string in "is a [type] ... and it loses all ...")
/// and return the parsed loss members. The successful parse is the detector —
/// no `contains()`.
pub(crate) fn scan_loss_enumeration(lower: &str) -> Vec<LossMember> {
    let mut remaining = lower;
    loop {
        if let Ok((_, members)) = parse_loss_enumeration(remaining) {
            return members;
        }
        match remaining.find(' ') {
            Some(i) => remaining = remaining[i + 1..].trim_start(),
            None => return Vec::new(),
        }
    }
}

/// Opening anchors for a nested single-quoted granted ability inside a
/// double-quoted grant body. Grant verbs are mid-sentence and lowercase in
/// Oracle text; ` and '` covers chained grants (Old-Growth Troll: `'{T}: …' and
/// '{1}, {T}, …'`).
const NESTED_ABILITY_QUOTE_OPENERS: [&str; 5] =
    [" have '", " has '", " gain '", " gains '", " and '"];

/// Find the next nested single-quoted ability span as `(open_quote, close_quote)`
/// indices (both point at the `'` delimiter). Each opener is tried from
/// `search_from`; the earliest match wins. The closing quote is the next `'`
/// after the opener — never `rfind`, so multiple chained grants promote
/// independently.
fn next_nested_ability_quote_span(body: &str, search_from: usize) -> Option<(usize, usize)> {
    let mut next_open: Option<usize> = None;
    for anchor in NESTED_ABILITY_QUOTE_OPENERS {
        if let Some(rel) = body[search_from..].find(anchor) {
            let abs_open = search_from + rel + anchor.len() - 1;
            next_open = Some(next_open.map_or(abs_open, |cur| cur.min(abs_open)));
        }
    }
    let open_quote = next_open?;
    let content_start = open_quote + 1;
    let rel_close = body[content_start..].find('\'')?;
    let close_quote = content_start + rel_close;
    Some((open_quote, close_quote))
}

/// A granted ability may nest one quote level deep inside a double-quoted grant
/// body (Koth emblem: `Mountains you control have '{T}: …'`; Roar saga chapter
/// II: `Creatures you control have '{T}: Add {R}, {G}, or {W}.'`; Old-Growth
/// Troll / Harold and Bob: `Enchanted Forest has '{T}: …' and '{1}, {T}, …'`).
/// Downstream grant parsers recognise only double-quoted ability bodies — single
/// quotes are ambiguous with apostrophes — so promote each nested pair to double
/// quotes independently. Openers are anchored on grant verbs (`have '` / `has '`
/// / `gain '` / `gains '`) or chained ` and '` so a possessive apostrophe in the
/// subject phrase is never mistaken for the delimiter.
pub(crate) fn promote_nested_ability_quotes(body: &str) -> String {
    let mut pairs = Vec::new();
    let mut search_from = 0;
    while let Some((open_quote, close_quote)) = next_nested_ability_quote_span(body, search_from) {
        pairs.push((open_quote, close_quote));
        search_from = close_quote + 1;
    }
    if pairs.is_empty() {
        return body.to_string();
    }
    let mut promoted = String::with_capacity(body.len());
    let mut last = 0;
    for (open_quote, close_quote) in pairs {
        promoted.push_str(&body[last..open_quote]);
        promoted.push('"');
        promoted.push_str(&body[open_quote + 1..close_quote]);
        promoted.push('"');
        last = close_quote + 1;
    }
    promoted.push_str(&body[last..]);
    promoted
}

pub(crate) fn strip_quoted_segments(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut in_quote = false;
    for ch in text.chars() {
        if ch == '"' {
            if !in_quote {
                remove_trailing_quote_connector(&mut output);
            }
            in_quote = !in_quote;
            output.push(' ');
        } else if in_quote {
            output.push(' ');
        } else {
            output.push(ch);
        }
    }
    output
}

pub(crate) fn remove_trailing_quote_connector(text: &mut String) {
    let trimmed_len = text.trim_end().len();
    text.truncate(trimmed_len);
    for connector in [" and", " or"] {
        if text.ends_with(connector) {
            let new_len = text.len() - connector.len();
            text.truncate(new_len);
            break;
        }
    }
    text.push(' ');
}

/// CR 613.4c: Scan text for "get(s) +X/+X" and resolve X via where_x_expression.
/// Returns AddDynamicPower + AddDynamicToughness modifications if found.
/// CR 613.4c: Parse a variable P/T modifier pattern like "+x/+x", "-x/-0", "+0/-x".
/// Returns (power_sign, power_is_x, toughness_sign, toughness_is_x) and remaining text.
/// CR 613.4c: parse a variable P/T grant body "±P/±T" where each axis is either
/// the variable X (dynamic — returned as `None`) or a fixed integer magnitude
/// (returned as `Some(n)`, `n >= 0`). Accepting a fixed magnitude alongside X is
/// what lets a MIXED grant like Cranial Ram "+X/+1" parse: previously each axis
/// was restricted to `x`/`0`, so the fixed `+1` failed `digit`-matching and the
/// whole pattern was rejected, dropping the equip static. The sign is returned
/// separately per axis so the caller applies it uniformly to the dynamic
/// One axis of a variable P/T grant: a fixed integer magnitude, the primary
/// variable `x`, or the secondary variable `y`. `y` is meaningful only on a
/// "+X/+Y" pump whose two axes bind to different quantities (Aspect of Wolf);
/// the caller must accept a distinct `y` axis only when a paired
/// "where X is <A>, and Y is <B>" binding was structurally parsed — otherwise
/// the pattern is left unsupported (Snowblind's `-X/-Y`, whose X/Y are defined
/// by later conditional sentences, must NOT synthesize a cost-X static).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PtAxisMag {
    Fixed(i32),
    VarX,
    VarY,
}

/// Parsed axes of a variable P/T grant: `(p_sign, p_mag, t_sign, t_mag)`.
type VariablePtAxes = (i32, PtAxisMag, i32, PtAxisMag);

pub(crate) fn parse_variable_pt_pattern(
    input: &str,
) -> nom::IResult<&str, VariablePtAxes, OracleError<'_>> {
    fn axis(input: &str) -> nom::IResult<&str, (i32, PtAxisMag), OracleError<'_>> {
        let (rest, sign) = alt((value(-1i32, tag("-")), value(1i32, tag("+")))).parse(input)?;
        let (rest, mag) = alt((
            value(PtAxisMag::VarX, tag("x")),
            value(PtAxisMag::VarY, tag("y")),
            map_res(digit1, |d: &str| d.parse::<i32>().map(PtAxisMag::Fixed)),
        ))
        .parse(rest)?;
        Ok((rest, (sign, mag)))
    }
    let (rest, (p_sign, p_mag)) = axis(input)?;
    let (rest, _) = tag("/").parse(rest)?;
    let (rest, (t_sign, t_mag)) = axis(rest)?;
    Ok((rest, (p_sign, p_mag, t_sign, t_mag)))
}

pub(crate) fn parse_fixed_pt_in_text(lower: &str) -> Option<(i32, i32)> {
    // CR 613.4c: Layer 7c additive P/T grant — "gets/has +N/+M". The copula
    // ("has"/"have") is accepted alongside "gets"/"get" so equip/anthem lines
    // that phrase the grant as "Equipped creature has +2/+2 and has …"
    // (Tinfoil Helm) resolve to the same additive modification as "gets +2/+2".
    nom_primitives::scan_at_word_boundaries(lower, |input| {
        let (rest, _) = alt((
            tag::<_, _, OracleError<'_>>("gets "),
            tag::<_, _, OracleError<'_>>("get "),
            tag::<_, _, OracleError<'_>>("has "),
            tag::<_, _, OracleError<'_>>("have "),
        ))
        .parse(input)?;
        // sign-required: "protection"/"flying"/etc. after "has " fail here.
        let (rest, pt) = nom_primitives::parse_pt_modifier.parse(rest)?;
        // CR 122.1a + CR 613.4c: a "+N/+M counter" is a counter placement, NOT a
        // static P/T grant — exclude it so counter-placement lines (e.g. Melira,
        // Sylvok Outcast "can't have -1/-1 counters put on them") do not misfire
        // into an anthem. This counter-suffix guard is the load-bearing exclusion:
        // `scan_at_word_boundaries` retries at every word, so a front "can't have"
        // lookahead would be positionally ineffective; the suffix guard here is
        // what actually rejects the counter-placement class.
        peek(not(preceded(
            space0,
            alt((tag("counters"), tag("counter"))),
        )))
        .parse(rest)?;
        Ok((rest, pt))
    })
}

/// CR 205.4a + CR 205.4b: recognize a "... is <supertype>" grant riding on an
/// attached-subject predicate body and return the granted supertype. Supertypes
/// are additive (CR 205.4b) and are never card types. Generalizes the former
/// legendary-only recognizer to every CR 205.4a supertype via
/// [`nom_target::parse_supertype_word`] (Legendary/Basic/Snow), so Glittering
/// Frost ("Enchanted land is snow.") and In Bolas's Clutches ("Enchanted
/// permanent is legendary.") both flow through this ONE seam:
/// `parse_continuous_modifications` pushes `AddSupertype { supertype }` for the
/// returned supertype.
///
/// Scans at word boundaries so the grant is still found when it is one conjunct
/// of a compound aura predicate ("... is legendary, gets +1/+1, and has
/// flying"). `parse_supertype_word` consumes no trailing boundary by contract,
/// so the `peek(not(alphanumeric1))` guard rejects a longer word that merely
/// starts with a supertype (e.g. "snow" in "snowman").
pub(crate) fn parse_supertype_grant(lower: &str) -> Option<Supertype> {
    nom_primitives::scan_at_word_boundaries(lower, |input| {
        let (rest, _) = tag::<_, _, OracleError<'_>>("is ").parse(input)?;
        let (rest, supertype) = nom_target::parse_supertype_word(rest)?;
        peek(not(alphanumeric1::<_, OracleError<'_>>)).parse(rest)?;
        Ok((rest, supertype))
    })
}

pub(crate) fn parse_clause_before_optional_period(input: &str) -> OracleResult<'_, &str> {
    terminated(alt((take_until("."), rest)), opt(tag("."))).parse(input)
}

pub(crate) fn split_type_retention_clause(input: &str) -> Option<(&str, CoreType)> {
    let (descriptor, retention_clause) =
        nom_primitives::scan_split_at_phrase(input, |i| parse_type_retention_clause(i))?;
    let (_, retained_core_type) = parse_type_retention_clause(retention_clause).ok()?;
    Some((descriptor, retained_core_type))
}

pub(crate) fn parse_type_retention_clause(input: &str) -> OracleResult<'_, CoreType> {
    let (input, is_plural) = alt((
        value(false, alt((tag("it's still "), tag("that's still ")))),
        value(true, tag("they're still ")),
        // CR 205.1b: relative-clause retention attached to a plural/singular
        // subject — "[plural] that are still lands", "[singular] that is still
        // a land". Distinct from the standalone-sentence forms above so the
        // animation building block can keep land/artifact types when the
        // retention rides on the same clause rather than a new sentence.
        value(true, tag("that are still ")),
        value(false, tag("that is still ")),
    ))
    .parse(input)?;

    let (input, _) = if is_plural {
        (input, None)
    } else {
        let (input, article) = opt(nom_primitives::parse_article).parse(input)?;
        (input, article)
    };

    alt((
        value(CoreType::Artifact, alt((tag("artifact"), tag("artifacts")))),
        value(CoreType::Battle, alt((tag("battle"), tag("battles")))),
        value(CoreType::Creature, alt((tag("creature"), tag("creatures")))),
        value(
            CoreType::Enchantment,
            alt((tag("enchantment"), tag("enchantments"))),
        ),
        value(CoreType::Instant, alt((tag("instant"), tag("instants")))),
        value(CoreType::Kindred, alt((tag("kindred"), tag("kindreds")))),
        value(CoreType::Land, alt((tag("land"), tag("lands")))),
        value(
            CoreType::Planeswalker,
            alt((tag("planeswalker"), tag("planeswalkers"))),
        ),
        value(CoreType::Sorcery, alt((tag("sorcery"), tag("sorceries")))),
    ))
    .parse(input)
}

pub(crate) fn push_base_pt_mana_value_dynamic_modifications(
    modifications: &mut Vec<ContinuousModification>,
    lower: &str,
) -> bool {
    let Some(value) = parse_base_pt_mana_value_dynamic(lower) else {
        return false;
    };
    modifications.push(ContinuousModification::SetPowerDynamic {
        value: value.clone(),
    });
    modifications.push(ContinuousModification::SetToughnessDynamic { value });
    true
}

/// One side of a dynamic base-P/T value token like `X/X` or `-X/2`.
/// Dynamic sides carry the sign (`+X` vs `-X`); fixed sides carry the literal.
#[derive(Clone, Copy)]
pub(crate) enum BasePtSide {
    Dynamic { sign: i32 },
    Fixed { value: i32 },
}

/// Build a `QuantityExpr` for one side of a dynamic base-P/T pattern.
pub(crate) fn base_pt_side_to_expr(side: BasePtSide, x_ref: &QuantityRef) -> QuantityExpr {
    match side {
        BasePtSide::Fixed { value } => QuantityExpr::Fixed { value },
        BasePtSide::Dynamic { sign } => {
            let inner = QuantityExpr::Ref { qty: x_ref.clone() };
            if sign < 0 {
                QuantityExpr::Multiply {
                    factor: -1,
                    inner: Box::new(inner),
                }
            } else {
                inner
            }
        }
    }
}

/// Resolve the `QuantityRef` that X binds to for a dynamic base-P/T effect.
/// Spell-cast contexts (Biomass Mutation) have no explicit "where X is" clause:
/// X is the cost X paid when the spell was cast, so fall back to `CostXPaid`.
/// When a "where X is …" expression is present, parse it via the nom quantity grammar.
pub(crate) fn resolve_base_pt_x_ref(where_x_expression: Option<&str>) -> Option<QuantityRef> {
    if let Some(expr) = where_x_expression {
        return super::oracle_nom::quantity::parse_quantity_ref_complete(expr)
            .ok()
            .map(|(_, qty)| qty);
    }
    // CR 107.3m: In a spell-cast context, X refers to the value paid for {X}.
    Some(QuantityRef::CostXPaid)
}

pub(crate) fn parse_base_power_mod(text: &str) -> Option<i32> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    if nom_primitives::scan_contains(tp.lower, "base power and toughness") {
        return None;
    }
    let power_text = tp.strip_after("base power ")?.original.trim();
    parse_single_pt_value(power_text)
}

pub(crate) fn parse_base_toughness_mod(text: &str) -> Option<i32> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    if nom_primitives::scan_contains(tp.lower, "base power and toughness") {
        return None;
    }
    let toughness_text = tp.strip_after("base toughness ")?.original.trim();
    parse_single_pt_value(toughness_text)
}

pub(crate) fn parse_single_pt_value(text: &str) -> Option<i32> {
    let value = text
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | ','))
        .next()?;
    value.replace('+', "").parse::<i32>().ok()
}

pub(crate) fn parse_quoted_rule_static_modifications(
    text: &str,
) -> Option<Vec<ContinuousModification>> {
    // CR 602.1: A cost separator inside a double-quoted granted ability
    // (`Creatures you control have "{T}: Add {R}."`) is part of the inner
    // activated ability, not the outer static line — mask quoted spans before
    // testing so the static grant path is not skipped (Roar of the Fifth People
    // chapter II, #5978).
    if find_cost_separator(&strip_quoted_segments(text)).is_some() {
        return None;
    }

    // CR 113.3d + CR 604.1: A quoted static ability is granted to the recipient
    // verbatim. If `parse_static_line_multi` produces nothing, the inner text
    // isn't a recognized static — fall through to the spell-like `GrantAbility`
    // path. Otherwise, emit one `ContinuousModification` per inner static:
    //   - `affected == Some(SelfRef)` with no condition / no layered modifications
    //     stays on the existing `AddStaticMode` path (the trivial recipient-anchored
    //     case — e.g. "can't be blocked", "must attack each combat").
    //   - Everything else (non-SelfRef scope, conditional, or carrying layered
    //     P/T / keyword modifications — e.g. Dancer's Chakrams' inner clause
    //     "Other commanders you control get +2/+2 and have lifelink") emits
    //     `GrantStaticAbility` so the inner static's scope, condition, and
    //     modifications are preserved verbatim on the recipient (CR 611.2c +
    //     CR 613.1f).
    //
    // Trailing punctuation: the host clause leaves the inner text bookended
    // by a list comma or period (e.g. `..., "Other commanders you control get
    // +2/+2 and have lifelink," and is a Performer ...`). Strip it before
    // delegating so the inner keyword-list parser doesn't choke on the comma.
    let trimmed = text.trim().trim_end_matches([',', '.', ';']).trim();
    let defs = parse_static_line_multi(trimmed);
    if defs.is_empty() {
        return None;
    }
    let modifications: Vec<_> = defs
        .into_iter()
        .map(|definition| {
            if definition.affected == Some(TargetFilter::SelfRef)
                && definition.condition.is_none()
                && definition.modifications.is_empty()
            {
                ContinuousModification::AddStaticMode {
                    mode: definition.mode,
                }
            } else {
                ContinuousModification::GrantStaticAbility {
                    definition: Box::new(definition),
                }
            }
        })
        .collect();
    Some(modifications)
}

/// Parse a single quoted ability string into a typed AbilityDefinition.
///
/// If the text contains a cost separator (e.g., `{T}: ...`), it's treated as an
/// activated ability with the cost parsed separately. Otherwise it's treated as
/// a spell-like effect.
pub(crate) fn parse_quoted_ability(text: &str) -> AbilityDefinition {
    let lower = text.to_lowercase();

    // CR 702.142a: Detect "Boast — " prefix and strip it, adding the implicit
    // Boast activation restrictions + tag. This handles cards that grant Boast
    // abilities via quoted text (e.g., Besieged Viking Village).
    if let Some(((), rest_original)) = nom_on_lower(text, &lower, |i| {
        value(
            (),
            alt((
                tag("boast \u{2014} "),
                tag("boast -- "),
                tag("boast—"),
                tag("boast-"),
            )),
        )
        .parse(i)
    }) {
        let mut def = parse_quoted_ability(rest_original);
        // CR 702.142a: "Activate only if this creature attacked this turn
        // and only once each turn."
        def.activation_restrictions
            .push(ActivationRestriction::OnlyOnceEachTurn);
        def.activation_restrictions
            .push(ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::SourceAttackedThisTurn),
            });
        // CR 702.142b: Tag as Boast for meta-reference effects.
        def.ability_tag = Some(AbilityTag::Boast);
        def.description = Some(format!(
            "Boast \u{2014} {}",
            sanitize_granting_placeholder(rest_original)
        ));
        return def;
    }

    // CR 603.1: Detect trigger prefixes and route to trigger parser.
    // Quoted ability text starting with "When"/"Whenever"/"At the beginning of" is a
    // triggered ability, not a spell-like effect chain. Extract the trigger's execute
    // chain as the granted AbilityDefinition (trigger metadata like mode/condition is
    // handled by the GrantTrigger path if available, but the effect chain is always useful).
    if nom_tag_lower(&lower, &lower, "when ").is_some()
        || nom_tag_lower(&lower, &lower, "whenever ").is_some()
        || nom_tag_lower(&lower, &lower, "at the beginning of ").is_some()
        || nom_tag_lower(&lower, &lower, "at the end of ").is_some()
    {
        let trigger = super::oracle_trigger::parse_trigger_line(text, "~");
        if let Some(execute) = trigger.execute {
            return *execute;
        }
        // Fallback: parse as effect chain if trigger parsing produced no execute
    }

    // Find the cost/effect separator — look for ": " after a cost-like prefix
    // (mana symbols, {T}, loyalty, etc.)
    if let Some(colon_pos) = find_cost_separator(text) {
        let cost_text = text[..colon_pos].trim();
        let effect_text = text[colon_pos + 1..].trim();
        let cost = parse_oracle_cost(cost_text);
        // CR 602.5c: When an object acquires an activated ability with a
        // use restriction (e.g. "Activate only as a sorcery", "Activate only
        // once each turn") from another object, that restriction applies to
        // the acquired ability. The restriction lives inside the granted
        // quoted text, so strip it with the single authority used by
        // standalone activated abilities (CR 602.5d/602.5e timing rules)
        // instead of leaving it as an unparsed trailing sentence.
        let (effect_text, constraints) =
            crate::parser::oracle::strip_activated_constraints(effect_text);
        // CR 116.2b + CR 708.7: flag the granted activated-ability body so a head
        // clause of "turn this/~ creature face up" lowers to the printed
        // `Effect::TurnFaceUp { SelfRef }` resolving effect (Etrata, Deadly
        // Fugitive's "{2}{U}{B}: Turn this creature face up. ..."), rather than
        // being rejected as the rule-based morph/disguise special action.
        let mut ctx = ParseContext {
            in_granted_activated_ability: true,
            ..ParseContext::default()
        };
        let mut def =
            parse_effect_chain_with_context(&effect_text, AbilityKind::Activated, &mut ctx);
        def.cost = Some(cost);
        def.activation_restrictions.extend(constraints.restrictions);
        // CR 601.2f: Fold a trailing self-referential "This ability costs {X}
        // less to activate, where X is ~'s power" node into `cost_reduction`
        // (the same AST-level extractor standalone activated abilities use). The
        // reduction's `Power{Source}` is host-referential (the equipped
        // creature), an untouched third channel — no interaction with the
        // GrantingObject cost/effect rewrite. Enables The Dominion Bracelet.
        crate::parser::oracle::extract_cost_reduction_from_chain(&mut def);
        def.description = Some(sanitize_granting_placeholder(text));
        def
    } else {
        // No cost separator — treat as spell-like ability text
        let mut def = parse_effect_chain(text, AbilityKind::Spell);
        def.description = Some(sanitize_granting_placeholder(text));
        def
    }
}

/// CR 201.5a: Descriptions render the granter self-reference as `~` (matching
/// pre-fix display); the `GRANTING_SELF_PLACEHOLDER` marker is a parse-time
/// signal only and must never leak the raw private-use char into stored text.
fn sanitize_granting_placeholder(text: &str) -> String {
    text.replace(crate::parser::oracle_util::GRANTING_SELF_PLACEHOLDER, "~")
}

/// True when `trimmed_prefix` is a bracketed planeswalker loyalty cost (`[+N]`,
/// `[−N]`, `[0]`, `[-N]`) as printed in granted-ability text (Ichormoon Gauntlet).
fn is_bracket_loyalty_cost_prefix(trimmed_prefix: &str) -> bool {
    parse_bracket_loyalty_cost_prefix(trimmed_prefix).is_ok()
}

fn parse_bracket_loyalty_cost_prefix(input: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
    let loyalty_number = recognize(pair(opt(one_of("+−–-")), digit1));
    all_consuming(delimited(tag("["), loyalty_number, tag("]"))).parse(input)
}

/// Find the position of the cost/effect separator colon in ability text.
///
/// Looks for `: ` or `:\n` that appears after cost-like content (mana symbols,
/// {T}, numeric loyalty, or text-based costs like "Sacrifice this token").
/// Returns the byte offset of the colon, or None.
pub(crate) fn find_cost_separator(text: &str) -> Option<usize> {
    // Walk through looking for ':' that follows a closing brace or known cost prefix
    for (idx, ch) in text.char_indices() {
        if ch == ':' && idx > 0 {
            let prefix = &text[..idx];
            // Must have cost-like content before the colon
            let trimmed_prefix = prefix.trim();
            let lower_prefix = trimmed_prefix.to_lowercase();
            let has_cost = prefix.contains('{')
                || trimmed_prefix.parse::<i32>().is_ok()
                || is_bracket_loyalty_cost_prefix(trimmed_prefix)
                || trimmed_prefix.strip_prefix('+').is_some() // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                || trimmed_prefix.strip_prefix('\u{2212}').is_some() // minus sign for loyalty // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                // CR 118.12: Text-based costs — sacrifice, discard, pay life, tap/untap, exile, remove
                || is_text_based_cost_prefix(&lower_prefix);
            if has_cost {
                return Some(idx);
            }
        }
    }
    None
}

/// Check if a prefix string looks like a text-based activated ability cost.
/// Handles common Oracle text cost patterns that don't use mana symbols:
/// "Sacrifice this token", "Discard a card", "Pay 2 life", "Tap an untapped creature",
/// "Exile ~ from your graveyard", "Remove a counter from ~", etc.
pub(crate) fn is_text_based_cost_prefix(lower_prefix: &str) -> bool {
    type E<'a> = OracleError<'a>;

    alt((
        value((), tag::<_, _, E>("sacrifice ")),
        value((), tag("discard ")),
        value((), tag("pay ")),
        value((), tag("tap ")),
        value((), tag("untap ")),
        value((), tag("exile ")),
        value((), tag("remove ")),
        value((), tag("reveal ")),
        value((), tag("return ")),
    ))
    .parse(lower_prefix)
    .is_ok()
}

/// CR 613.4c: For "+N/+M for each X and has [keyword]" patterns, the for-each
/// filter clause ends at " and has " / " and gains " / " and have ". Returns
/// the input slice truncated at the first matching boundary, or unchanged if
/// no boundary is present. Mirrors the keyword recognition in
/// `extract_keyword_clause` but in the inverse direction (returns the
/// pre-boundary span instead of the post-boundary one).
/// Peel a trailing grant conjunct off a dynamic "for each <count>" clause so the
/// count itself parses cleanly. Strips a trailing keyword grant (" and has
/// flying") and — CR 205.1b — a trailing type-addition (" and is an Avatar in
/// addition to its other types"). The peeled clause is recovered separately by
/// the caller (`extract_keyword_clause` / `parse_additive_type_clause_modifications`
/// over the full description); without this the count parse fails on the tail and
/// the whole dynamic pump collapses to a fixed +N/+M (Avatar Destiny, Machinist's
/// Arsenal). The type-addition arm is guarded on the "in addition to" marker so a
/// genuine "<count> and is <...>" count phrase is never mis-truncated.
pub(crate) fn strip_trailing_keyword_clause(clause: &str) -> &str {
    for needle in [" and gains ", " and gain ", " and has ", " and have "] {
        if let Some(pos) = clause.find(needle) {
            return &clause[..pos];
        }
    }
    // CR 205.1b: peel a trailing type-addition (" and is an Avatar in addition
    // to its other types"), guarded on the "in addition to " tail so a genuine
    // "<count> and is <...>" count phrase is never mis-truncated. Mirrors the
    // type-addition grammar in `type_change.rs`: scan word boundaries for the
    // "and is " verb boundary whose remainder reaches " in addition to ", and
    // return the span preceding it. `clause` is already lowercase (caller passes
    // `after_for_each.lower`), so tags match directly.
    if let Some((before, _)) = nom_primitives::scan_split_at_phrase(clause, |i| {
        (
            tag::<_, _, OracleError<'_>>("and is "),
            take_until::<_, _, OracleError<'_>>(" in addition to "),
            tag::<_, _, OracleError<'_>>(" in addition to "),
        )
            .parse(i)
    }) {
        return before.trim_end();
    }
    clause
}

pub(crate) fn extract_keyword_clause(text: &str) -> Option<&str> {
    let lower = text.to_lowercase();

    for needle in [
        " and gains ",
        " and gain ",
        " and has ",
        " and have ",
        " gains ",
        " gain ",
        " has ",
        " have ",
    ] {
        if let Some(pos) = lower.find(needle) {
            return Some(&text[pos + needle.len()..]);
        }
    }

    for prefix in ["gains ", "gain ", "has ", "have "] {
        if nom_tag_lower(&lower, &lower, prefix).is_some() {
            return Some(&text[prefix.len()..]);
        }
    }

    None
}

/// Extract the keyword text from "lose [keyword]" / "loses [keyword]" clauses.
/// Mirrors `extract_keyword_clause` but for keyword removal.
pub(crate) fn extract_lose_keyword_clause(text: &str) -> Option<&str> {
    let lower = text.to_lowercase();

    for needle in [" and loses ", " and lose "] {
        if let Some(pos) = lower.find(needle) {
            let after = &text[pos + needle.len()..];
            // Stop before "and gains" to avoid consuming the gain clause
            let end = lower[pos + needle.len()..]
                .find(" and gain") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                .unwrap_or(after.len());
            return Some(&after[..end]);
        }
    }

    for prefix in ["loses ", "lose "] {
        if let Some(rest) = nom_tag_lower(&lower, &lower, prefix) {
            let after = &text[prefix.len()..];
            // Stop before "and gains"/"and gain" to avoid consuming the gain clause
            let end = rest.find(" and gain").unwrap_or(after.len()); // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            return Some(&after[..end]);
        }
    }

    None
}

/// Parse a leading P/T pair from Oracle text, returning values and remainder.
///
/// CR 613.4b: Layer 7b base power/toughness literals after "with base power
/// and toughness". Composes the signed [`nom_primitives::parse_pt_modifier`]
/// path and an unsigned `N/N` path so trailing clause text (e.g. "and loses
/// all abilities") is left in the nom remainder for downstream parsers.
pub(crate) fn parse_pt_mod_with_remainder(input: &str) -> OracleResult<'_, (i32, i32)> {
    let input = input.trim();
    alt((
        nom_primitives::parse_pt_modifier,
        (
            nom_primitives::parse_number,
            char('/'),
            nom_primitives::parse_number,
        )
            .map(|(power, _, toughness)| (power as i32, toughness as i32)),
    ))
    .parse(input)
}

/// Parse a P/T modifier like "+2/+3", "-1/-1", "+3/-2" from Oracle text.
///
/// Delegates to the shared nom P/T combinator for signed P/T values.
/// Falls back to manual parsing for unsigned values (e.g. "0/0") which the
/// nom combinator doesn't handle (it requires explicit +/- signs).
pub(crate) fn parse_pt_mod(text: &str) -> Option<(i32, i32)> {
    let text = text.trim();
    // CR 613.4c: consume an optional "an additional " qualifier ("gets an
    // additional +N/+M" — Taste for Mayhem, Divine Sacrament, Patriarch's Desire,
    // Strange Augmentation). It marks a second Layer 7c grant stacked on the base
    // modification but carries no P/T semantics of its own, so the underlying
    // +N/+M is parsed identically (the enclosing gate is attached separately). The
    // cheap "an" guard keeps the common no-qualifier call off the lowercase
    // allocation path.
    let text = match text.get(..2) {
        Some(head) if head.eq_ignore_ascii_case("an") => {
            let lower = text.to_lowercase();
            nom_tag_lower(text, &lower, "an additional ").unwrap_or(text)
        }
        _ => text,
    };
    // Try the nom combinator first — handles +N/+M, -N/-M, +N/-M patterns.
    if let Ok((_, (p, t))) = nom_primitives::parse_pt_modifier.parse(text) {
        return Some((p, t));
    }
    // Fallback for unsigned values: "0/0", "1/1", etc. (used in base P/T contexts).
    let slash = text.find('/')?;
    let p_str = &text[..slash];
    let rest = &text[slash + 1..];
    let t_end = rest
        .find(|c: char| c.is_whitespace() || c == '.' || c == ',')
        .unwrap_or(rest.len());
    let t_str = &rest[..t_end];
    let p = p_str.replace('+', "").parse::<i32>().ok()?;
    let t = t_str.replace('+', "").parse::<i32>().ok()?;
    Some((p, t))
}

/// CR 702.34a / CR 702.128a / CR 702.180a: Map a bare graveyard alt-cost keyword
/// token (one whose cost, when granted with no explicit value, is the recipient
/// card's own mana cost) to the `Keyword` carrying `ManaCost::SelfManaCost`.
/// Parameterized over the keyword by a single `alt()` of token tags — adding a
/// future self-cost keyword is one more `value(..)` arm, not a new sibling
/// branch in `map_keyword`. Returns `None` for any other text so `map_keyword`
/// continues its normal dispatch.
fn map_self_cost_graveyard_keyword(word: &str) -> Option<Keyword> {
    let lower = word.to_ascii_lowercase();
    let (_, keyword) = all_consuming(alt((
        value(
            Keyword::Flashback(crate::types::keywords::FlashbackCost::Mana(
                ManaCost::SelfManaCost,
            )),
            tag::<_, _, OracleError<'_>>("flashback"),
        ),
        value(
            Keyword::Embalm(crate::types::keywords::EmbalmCost::Mana(
                ManaCost::SelfManaCost,
            )),
            tag("embalm"),
        ),
        value(Keyword::Harmonize(ManaCost::SelfManaCost), tag("harmonize")),
    )))
    .parse(lower.as_str())
    .ok()?;
    Some(keyword)
}

/// Map a keyword text to a Keyword enum variant using the FromStr impl.
/// Returns None only for `Keyword::Unknown`.
pub(crate) fn map_keyword(text: &str) -> Option<Keyword> {
    let word = text.trim().trim_end_matches('.').trim();
    if word.is_empty() {
        return None;
    }
    // CR 702.34a (Flashback) / CR 702.128a (Embalm) / CR 702.180a (Harmonize):
    // a bare graveyard alt-cost keyword granted by an effect ("target ... gains
    // flashback/embalm/harmonize until end of turn. The [keyword] cost is equal
    // to its mana cost") carries no printed cost — its cost is the granted card's
    // own mana cost. `ManaCost::SelfManaCost` is the single building block that
    // resolves to the recipient's mana cost at cast time (see
    // `game::keywords::resolve_keyword_mana_cost`), so the grant is parameterized
    // by keyword over one self-cost representation rather than baking a concrete
    // cost. The trailing "the [keyword] cost is equal to its mana cost" sentence
    // is therefore redundant reminder text (dropped by the effect-chain parser).
    if let Some(keyword) = map_self_cost_graveyard_keyword(word) {
        return Some(keyword);
    }
    // CR 702.73a: "all creature types" is the Changeling CDA effect.
    // Granting Changeling keyword triggers layer system post-fixup to add all types.
    if word.eq_ignore_ascii_case("all creature types") {
        return Some(Keyword::Changeling);
    }
    if let Some(keyword) = parse_landwalk_keyword(word) {
        return Some(keyword);
    }
    match Keyword::from_str(word) {
        Ok(Keyword::Unknown(_)) => {
            // Fall through to Oracle-format parser for parameterized keywords
            // like "protection from red" that use spaces instead of colons.
            super::oracle_keyword::parse_granted_keyword_fragment(word)
        }
        Ok(kw) => Some(kw),
        Err(_) => None, // Infallible, but satisfy the compiler
    }
}

pub(crate) fn parse_landwalk_keyword(text: &str) -> Option<Keyword> {
    match text.trim().to_ascii_lowercase().as_str() {
        "plainswalk" => Some(Keyword::Landwalk("Plains".to_string())), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "islandwalk" => Some(Keyword::Landwalk("Island".to_string())), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "swampwalk" => Some(Keyword::Landwalk("Swamp".to_string())), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "mountainwalk" => Some(Keyword::Landwalk("Mountain".to_string())), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "forestwalk" => Some(Keyword::Landwalk("Forest".to_string())), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        _ => None,
    }
}

/// CR 702.14a: Parse one of the five basic-land landwalk keyword tokens
/// (`plainswalk`, `islandwalk`, `swampwalk`, `mountainwalk`, `forestwalk`)
/// and return the canonical capitalized basic subtype string that
/// `Keyword::Landwalk(String)` carries (e.g. `swampwalk` → `"Swamp"`).
///
/// This is a *qualifier extractor* used by static-line parsers that need
/// to reference the land subtype directly. It does NOT replace
/// `parse_landwalk_keyword` (which produces a `Keyword`), and the existing
/// allow-list at `oracle_target.rs` for landwalk tokens is unaffected.
pub(crate) fn parse_basic_landwalk_qualifier(input: &str) -> OracleResult<'_, &'static str> {
    alt((
        value("Plains", tag("plainswalk")),
        value("Island", tag("islandwalk")),
        value("Swamp", tag("swampwalk")),
        value("Mountain", tag("mountainwalk")),
        value("Forest", tag("forestwalk")),
    ))
    .parse(input)
}

/// CR 601.3 + CR 113.6b: Parse the affected-card filter of a graveyard
/// cast-permission ability. When the filter text is a self-reference phrase
/// ("this card", "this creature", "this permanent", ...), the permission
/// applies only to the source card itself, so it lowers to
/// `TargetFilter::SelfRef`. The returned `bool` is the `self_ref_permission`
/// flag: when `true`, the caller restricts the static to
/// `active_zones: [Graveyard]` (CR 113.6b — a zone-restricted ability functions
/// only from the zones it names). A non-self-reference filter (e.g. a creature
/// type) falls through to `parse_type_phrase` and is not zone-restricted here.
pub(crate) fn parse_graveyard_permission_filter(input: &str) -> (TargetFilter, bool) {
    // The self-reference token `~` is substituted for type phrases ("this
    // creature", "this permanent", ...) by `normalize_self_references` before
    // this parser runs; `SELF_REF_PARSE_ONLY_PHRASES` (e.g. "this card") are
    // *excluded* from that normalization and reach this function verbatim. Both
    // forms denote the permission's own source card.
    for phrase in std::iter::once("~").chain(SELF_REF_PARSE_ONLY_PHRASES.iter().copied()) {
        if all_consuming(tag::<_, _, OracleError<'_>>(phrase))
            .parse(input)
            .is_ok()
        {
            return (TargetFilter::SelfRef, true);
        }
    }
    let (filter, _) = parse_type_phrase(input);
    (filter, false)
}

/// CR 601.3 + CR 113.6b: Parse the trailing condition gate on a graveyard
/// cast-permission ability ("You may cast this card from your graveyard
/// [as long as|if] [condition]"). The permission is a zone-restricted ability
/// (CR 113.6b) that allows a cast under CR 601.3; the condition restricts when
/// the permission applies. Both the durative "as long as" form and the
/// turn-history "if" form (Oathsworn Vampire — "if you gained life this turn")
/// are evaluated when the permission is queried, so they share the same
/// `StaticCondition` carrier. The condition body is delegated to
/// `parse_inner_condition` — the single authority for game-state conditions.
pub(crate) fn parse_graveyard_permission_condition(
    input: &str,
) -> OracleResult<'_, StaticCondition> {
    let (rest, condition) = preceded(
        alt((tag(" as long as "), tag(" if "))),
        nom_condition::parse_inner_condition,
    )
    .parse(input)?;
    let (rest, _) = opt(tag(".")).parse(rest)?;
    Ok((rest, condition))
}

pub(crate) fn parse_exile_spell_cast_this_way_rider(input: &str) -> OracleResult<'_, ()> {
    all_consuming(preceded(
        terminated(opt(tag(".")), space0),
        value(
            (),
            terminated(
                tag("if a spell cast this way would be put into your graveyard, exile it instead"),
                opt(tag(".")),
            ),
        ),
    ))
    .parse(input)
}

pub(crate) fn parse_top_of_library_permission_condition(trailing: &str) -> Option<StaticCondition> {
    let (rest, condition) = preceded(
        tag::<_, _, OracleError<'_>>(" as long as "),
        nom_condition::parse_inner_condition,
    )
    .parse(trailing)
    .ok()?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(rest).ok()?;
    if !rest.is_empty() {
        return None;
    }
    Some(condition)
}

/// CR 118.9 + CR 119.4: Helper to parse the optional alt-cost rider that may
/// follow a top-of-library cast permission. Bolas's Citadel form: "If you
/// cast a spell this way, pay life equal to its mana value rather than pay
/// its mana cost." Scans for the rider's opening "if you cast" inside the
/// trailing text and the full line, slicing from that index forward so the
/// existing `try_parse_alt_cost_rider` (which expects the input to start at
/// the rider) sees a clean prefix.
pub(crate) fn parse_top_of_library_alt_cost_rider(
    trailing: &str,
    text: &str,
) -> Option<crate::types::ability::AbilityCost> {
    fn try_from(input: &str) -> Option<crate::types::ability::AbilityCost> {
        // Scan past any leading text (the "you may play ... library."
        // sentence) until the rider's opening anchor; pure-nom
        // `take_until + alt` keeps this on the combinator path. Both
        // anchors map to the same underlying rider parser.
        let lower = input.to_lowercase();
        type E<'a> = OracleError<'a>;
        let mut anchor = nom::branch::alt((
            nom::bytes::complete::take_until::<_, _, E>("if you cast a spell this way"),
            nom::bytes::complete::take_until::<_, _, E>("if you cast it this way"),
        ));
        let (after_skip, _) = anchor.parse(lower.as_str()).ok()?;
        // Slice the original (preserves casing) at the same offset; nom's
        // `take_until` returned the consumed prefix, so the rider starts at
        // `input.len() - after_skip.len()`.
        let idx = input.len() - after_skip.len();
        super::oracle_effect::try_parse_alt_cost_rider(&input[idx..])
    }
    try_from(trailing).or_else(|| try_from(text))
}

/// Parse the optional " using (its|their) <keyword> (ability|abilities)" rider on
/// graveyard-cast-permission statics. Returns the named alt-cost keyword's kind.
/// CR 118.9: the rider restricts the permission to casting via the named alt cost.
pub(crate) fn parse_alt_cost_rider(input: &str) -> OracleResult<'_, KeywordKind> {
    preceded(
        tag(" using "),
        preceded(
            terminated(alt((tag("its"), tag("their"))), tag(" ")),
            terminated(
                nom_primitives::parse_alt_cost_keyword_name_to_kind,
                preceded(tag(" "), alt((tag("abilities"), tag("ability")))),
            ),
        ),
    )
    .parse(input)
}

/// Inject a `HasKeywordKind` property into a `TargetFilter`. If the filter is already
/// `Typed`, push into its `properties`. Otherwise wrap with `And` over a new typed
/// filter carrying only the keyword constraint.
pub(crate) fn inject_keyword_kind_filter_prop(
    filter: TargetFilter,
    kind: KeywordKind,
) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut tf) => {
            tf.properties
                .push(FilterProp::HasKeywordKind { value: kind });
            TargetFilter::Typed(tf)
        }
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![],
                    controller: None,
                    properties: vec![FilterProp::HasKeywordKind { value: kind }],
                }),
            ],
        },
    }
}

/// CR 601.2f: Classification of a cost-modifier subject against the
/// "the <ordinal> <qualifier> spell <timing> costs …" template.
///
/// Three outcomes, kept as a typed enum rather than an `Option` so the caller
/// can tell "not an Nth-spell line" apart from "an Nth-spell line whose
/// qualifier we can't yet represent." The latter MUST decline the whole cost
/// static — emitting a filterless, gateless reducer would silently drop both
/// the printed "<ordinal> … each turn" once-per-turn restriction and the
/// qualifier (e.g. "kicked"), reducing every spell the controller casts.
pub(crate) enum NthQualifiedSpell {
    /// The subject is not a "the <ordinal> … spell <timing> costs …" line; the
    /// caller proceeds with its ordinary cost-modifier parsing.
    NotApplicable,
    /// A representable Nth-spell subject: the qualifying spell filter, the timing
    /// window over which the ordinal is measured, and the 1-based ordinal `N`
    /// ("first" → 1, "second" → 2, …). The gate is
    /// `SpellsCastThisTurn(filter) == N - 1` (see [`nth_qualified_spell_condition`]).
    Supported {
        filter: TargetFilter,
        timing: NthEventTimingKind,
        ordinal: u32,
    },
    /// The "the <ordinal> … spell <timing>" shape is present, but the qualifier or
    /// timing window can't be lowered to a spell filter + once-per-turn gate
    /// (e.g. "the first kicked spell you cast each turn" — kicker-paid state is
    /// not a representable spell-cost filter, or an opponent-/their-turn window
    /// with no static condition). The caller must decline the cost static.
    UnsupportedQualifier,
}

/// CR 601.2f: Parse the leading "the <ordinal> " of an
/// "the <ordinal> <qualifier> spell <timing> costs/has …" subject, returning the
/// 1-based ordinal (`"first"` → 1, `"second"` → 2, …) and the remaining subject.
///
/// Parameterizes what was a hardcoded `"the first "` prefix so every printed
/// ordinal — not just the first — is covered (Highspire Bell-Ringer, Uthros
/// Psionicist, Monk Class, Raging Battle Mouse, and Alisaie Leveilleur all print
/// "the second spell you cast each turn costs …"). The ordinal threads into
/// [`nth_qualified_spell_condition`] as the `SpellsCastThisTurn == N - 1` gate.
fn parse_spell_ordinal_prefix(subject: &str) -> Option<(u32, &str)> {
    preceded(
        tag::<_, _, OracleError<'_>>("the "),
        terminated(parse_ordinal_word, tag(" ")),
    )
    .parse(subject)
    .ok()
    .map(|(rest, ordinal)| (ordinal, rest))
}

/// Map an English ordinal word to its 1-based value. Only "first"/"second" occur
/// on printed once-per-turn spell-cost modifiers today; the higher ordinals are
/// included so the whole ordinal class stays covered without a follow-up edit.
/// The trailing `" "` guard in [`parse_spell_ordinal_prefix`] enforces a word
/// boundary, so "firstborn" / "seconds" never partial-match.
fn parse_ordinal_word(i: &str) -> OracleResult<'_, u32> {
    alt((
        value(1u32, tag("first")),
        value(2, tag("second")),
        value(3, tag("third")),
        value(4, tag("fourth")),
        value(5, tag("fifth")),
        value(6, tag("sixth")),
        value(7, tag("seventh")),
        value(8, tag("eighth")),
        value(9, tag("ninth")),
        value(10, tag("tenth")),
    ))
    .parse(i)
}

/// CR 601.2f + CR 107.3: Parse a "first qualified spell <timing> costs less"
/// cost-reduction subject into the spell filter that qualifies the spell and the
/// timing window over which "first" is measured.
///
/// Builds for the class, not the card. The subject decomposes into three
/// independent axes, each parsed by a shared building block:
///   1. Pre-spell type qualifier ("non-Lemur creature spell with flying") — via
///      `parse_type_phrase`, which preserves keyword qualifiers.
///   2. Post-spell modifier ("with {X} in its mana cost", CR 107.3 + CR 202.1) —
///      via `oracle_trigger::parse_post_spell_modifier`, the same combinator the
///      paired "whenever you cast your first spell with {X}…" trigger uses.
///   3. Timing phrase ("each turn" vs "during each of your turns") — via
///      `split_first_spell_timing`, which distinguishes the unrestricted
///      ("each turn", any player's turn) from the controller-turn restricted
///      form (reusing `NthEventTimingKind` from the paired trigger).
///
/// Examples this covers (previously only the "during each of your turns" form
/// parsed; the "each turn" + "with {X}" form silently dropped its filter and
/// once-per-turn gate):
///   - Zimone, Infinite Analyst: "The first spell you cast with {X} in its mana
///     cost each turn costs {1} less to cast for each +1/+1 counter on ~."
///   - "The first non-Lemur creature spell with flying you cast during each of
///     your turns costs {1} less to cast."
///
/// Returns [`NthQualifiedSpell::UnsupportedQualifier`] when the
/// "the <ordinal> … spell <timing>" shape is present but the qualifier/timing
/// can't be represented (e.g. "the first kicked spell you cast each turn"), so
/// the caller declines the static instead of emitting a broad reducer.
pub(crate) fn parse_nth_qualified_spell_filter(lower: &str) -> NthQualifiedSpell {
    let Some((ordinal, after_prefix)) = parse_spell_ordinal_prefix(lower) else {
        return NthQualifiedSpell::NotApplicable;
    };

    // Split the subject at the cast infix that separates the pre-spell
    // qualifier ("<type> spell") from the post-spell modifier + timing region
    // ("with {X} in its mana cost each turn cost[s] ..."). CR templating always
    // places the caster phrase between the spell noun and any post-spell modifier.
    let Some((pre, post)) = split_first_spell_cast_region(after_prefix) else {
        return NthQualifiedSpell::NotApplicable;
    };

    // Scan the post-caster region for the timing phrase. Everything before
    // it is a leading post-spell modifier ("with {X} in its mana cost"); the
    // cost-modification verb ("costs {1} less …") follows the timing phrase and
    // is discarded. The timing phrase — not a " cost" literal — is the anchor,
    // because " cost" also occurs inside "in its mana cost". A missing timing
    // phrase means this is some other "the first … you cast" construction, not
    // the per-turn first-spell cost template.
    let Some((timing, post_modifier_text)) = split_first_spell_timing(post.trim()) else {
        return NthQualifiedSpell::NotApplicable;
    };

    // From here the "the first … spell <timing> costs …" shape is confirmed, so
    // any failure to represent the qualifier/timing is an UnsupportedQualifier
    // (decline the whole static), never a silent fall-through to a broad reducer.

    // Only the unrestricted ("each turn") and controller-turn ("during each of
    // your turns") windows have a representable `StaticCondition` for a cost
    // static. Opponent-/their-turn windows are not printed on cost reducers and
    // have no static condition — decline rather than emit a reduction that drops
    // the printed timing restriction.
    if !matches!(
        timing,
        NthEventTimingKind::Unrestricted | NthEventTimingKind::Restricted(PlayerFilter::Controller)
    ) {
        return NthQualifiedSpell::UnsupportedQualifier;
    }

    // Pre-spell type/keyword qualifier (strip a bare trailing "spell" noun so a
    // post-modifier-only subject collapses to no type filter).
    let pre_trimmed = pre.trim();
    let pre_type = if all_consuming(tag::<_, _, OracleError<'_>>("spell"))
        .parse(pre_trimmed)
        .is_ok()
    {
        ""
    } else if let Ok((_, stripped)) = all_consuming(terminated(
        take_until::<_, _, OracleError<'_>>(" spell"),
        tag(" spell"),
    ))
    .parse(pre_trimmed)
    {
        stripped
    } else {
        pre_trimmed
    }
    .trim();
    let type_filter = if pre_type.is_empty() {
        None
    } else if all_consuming(tag::<_, _, OracleError<'_>>("historic"))
        .parse(pre_type)
        .is_ok()
    {
        // CR 700.6: bare "historic" is a card-property adjective, not a type word.
        // `parse_type_phrase` only emits `FilterProp::Historic` when a type word
        // follows (oracle_target.rs), so the bare "historic spell" subject must
        // lower the property here. Covers every "first historic spell you cast …"
        // grantor (Peri Brown class) without weakening the `parse_type_phrase`
        // guard, and benefits both the keyword-grant and cost-modifier callers.
        Some(TargetFilter::Typed(
            TypedFilter::card().properties(vec![FilterProp::Historic]),
        ))
    } else if all_consuming(tag::<_, _, OracleError<'_>>("kicked"))
        .parse(pre_type)
        .is_ok()
    {
        // CR 702.33d: bare "kicked spell" — kicker-paid qualifier for Vine Gecko
        // class cost reducers.
        Some(TargetFilter::Typed(
            TypedFilter::card().properties(vec![FilterProp::WasKicked]),
        ))
    } else {
        let (filter, remainder) = parse_type_phrase(pre_type);
        if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
            Some(filter)
        } else {
            // Unrecognized pre-spell qualifier — decline rather than emit a cost
            // reduction that ignores the printed restriction.
            return NthQualifiedSpell::UnsupportedQualifier;
        }
    };

    // Post-spell modifier ("with {X} in its mana cost", etc.). Empty is allowed
    // (bare "spell"), but non-empty text that the modifier combinator rejects
    // means an unsupported qualifier — decline.
    let post_filter = if post_modifier_text.is_empty() {
        None
    } else {
        match super::oracle_trigger::parse_post_spell_modifier(post_modifier_text) {
            Some(filter) => Some(filter),
            None => return NthQualifiedSpell::UnsupportedQualifier,
        }
    };

    let filter = match (type_filter, post_filter) {
        (None, None) => TargetFilter::Typed(TypedFilter::card()),
        (Some(f), None) | (None, Some(f)) => f,
        (Some(a), Some(b)) => TargetFilter::And {
            filters: vec![a, b],
        },
    };
    NthQualifiedSpell::Supported {
        filter,
        timing,
        ordinal,
    }
}

/// CR 601.2f: Audit that a "the <ordinal> … spell <timing> has [keyword]" subject
/// is FULLY represented by `parse_nth_qualified_spell_filter` — i.e. the text
/// AFTER the timing phrase is empty.
///
/// `parse_nth_qualified_spell_filter` discards everything after the timing
/// phrase (the cost-modification verb on the cost-reducer path, "costs {1} less
/// …"). On the keyword-grant path the grant verb was already split off by the
/// caller, so a clean subject must terminate at the timing phrase. Any trailing
/// residue means an unrepresentable qualifier sits in the discarded region
/// (Rain of Riches' "that mana from a Treasure was spent to cast"; TARDIS Bay's
/// post-timing "with mana value 2 or greater"), so the keyword-grant caller must
/// decline rather than emit a residue-blind gate.
///
/// Lives next to the parser whose discard it audits. Called ONLY from the
/// keyword-grant arm — the cost-modifier consumer legitimately expects trailing
/// cost-verb text, so this guard must NOT move into the shared
/// `parse_nth_qualified_spell_filter`.
pub(crate) fn nth_qualified_spell_subject_fully_consumed(subject: &str) -> bool {
    let Some((_ordinal, after_prefix)) = parse_spell_ordinal_prefix(subject) else {
        return false;
    };
    let Some((_, post)) = split_first_spell_cast_region(after_prefix) else {
        return false;
    };
    let Some((_, _, after_timing)) =
        nom_primitives::scan_preceded(post.trim(), parse_first_spell_timing_phrase)
    else {
        return false;
    };
    after_timing.trim().is_empty()
}

fn split_first_spell_cast_region(subject: &str) -> Option<(&str, &str)> {
    [
        " you cast ",
        " your opponents cast ",
        " opponents cast ",
        " each opponent casts ",
    ]
    .into_iter()
    .find_map(|phrase| {
        nom_primitives::split_once_on(subject, phrase)
            .ok()
            .map(|(_, split)| split)
    })
}

/// CR 601.2 + CR 102.1: Match the timing phrase of a first-qualified-spell
/// subject at the current position, returning the timing kind. Unlike
/// `oracle_trigger::parse_timing_tail`, this is NOT `all_consuming` — the
/// cost-modification verb ("costs {1} less …") follows the timing phrase mid-
/// string. Longest alternatives are ordered first so "during each of your turns"
/// wins over "during your turn", and the controller forms over "each turn".
fn parse_first_spell_timing_phrase(i: &str) -> OracleResult<'_, NthEventTimingKind> {
    alt((
        value(
            NthEventTimingKind::Restricted(PlayerFilter::Controller),
            alt((tag("during each of your turns"), tag("during your turn"))),
        ),
        value(
            NthEventTimingKind::Restricted(PlayerFilter::Opponent),
            alt((
                tag("during each opponent's turn"),
                tag("during each opponent\u{2019}s turn"),
            )),
        ),
        value(
            NthEventTimingKind::Restricted(PlayerFilter::TriggeringPlayer),
            alt((tag("during their turn"), tag("during each of their turns"))),
        ),
        value(NthEventTimingKind::Unrestricted, tag("each turn")),
        value(NthEventTimingKind::Unrestricted, tag("in a turn")),
    ))
    .parse(i)
}

/// Isolate the timing phrase of a first-qualified-spell subject from any leading
/// post-spell modifier. Word-boundary scans for the position where
/// `parse_first_spell_timing_phrase` matches; text before that is the post-spell
/// modifier. Returns `None` when no recognized timing phrase is present.
fn split_first_spell_timing(text: &str) -> Option<(NthEventTimingKind, &str)> {
    let (before, timing, _) = nom_primitives::scan_preceded(text, parse_first_spell_timing_phrase)?;
    Some((timing, before.trim_end()))
}

/// CR 601.2f + CR 107.3: Build the "the <ordinal> qualified spell <timing>" gate.
/// The 1-based `ordinal` (`"first"` → 1, `"second"` → 2, …) lowers to
/// `SpellsCastThisTurn(filter) == ordinal - 1`: the reduction applies precisely
/// while exactly `ordinal - 1` matching spells have already been cast this turn,
/// so the spell now being cast is the Nth (the currently-casting spell is not yet
/// counted, matching the merged `first == 0` behavior). The timing axis adds a
/// turn-owner restriction only for the "during each of your turns" form; "each
/// turn" allows the Nth qualifying spell on any player's turn.
pub(crate) fn nth_qualified_spell_condition(
    filter: &TargetFilter,
    timing: &NthEventTimingKind,
    ordinal: u32,
) -> StaticCondition {
    let nth_this_turn = StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter: Some(filter.clone()),
            },
        },
        comparator: Comparator::EQ,
        rhs: QuantityExpr::Fixed {
            value: ordinal as i32 - 1,
        },
    };

    match timing {
        // "each turn" — no turn-ownership restriction (CR 601.2: the Nth
        // qualifying spell of the turn regardless of whose turn it is).
        NthEventTimingKind::Unrestricted => nth_this_turn,
        // "during each of your turns" — additionally gate on the controller's
        // turn (CR 102.1 active-player reading for a cost static).
        NthEventTimingKind::Restricted(PlayerFilter::Controller) => StaticCondition::And {
            conditions: vec![StaticCondition::DuringYourTurn, nth_this_turn],
        },
        // Other player-scoped turn windows have no representable `StaticCondition`
        // for a cost static; the caller declines these via the filter parser.
        NthEventTimingKind::Restricted(_) => {
            unreachable!("unsupported player-scoped turn window for cost static")
        }
    }
}

/// CR 117.7 + CR 601.2f: Detect a self-spell cost-modification subject.
/// Matches the leading "this spell ", "this card ", or "~ " prefix used when
/// a spell reduces/raises its own cast cost (e.g., Tolarian Terror:
/// "This spell costs {1} less to cast for each instant and sorcery card in
/// your graveyard."). Callers use this to flag self-reference so the static
/// is emitted with `affected = SelfRef` and `active_zones = self_spell_cost_mod_active_zones()`
/// instead of the default battlefield scope.
pub(crate) fn parse_self_spell_cost_subject(lower: &str) -> Option<()> {
    nom_on_lower(lower, lower, |i| {
        value((), alt((tag("this spell "), tag("this card "), tag("~ ")))).parse(i)
    })
    .map(|_| ())
}

pub(crate) fn parse_self_spell_target_cost_filter(lower: &str) -> Option<TargetFilter> {
    let (_, target_text) = preceded(
        take_until::<_, _, OracleError<'_>>(" if "),
        preceded(
            alt((tag(" if it targets "), tag(" if this spell targets "))),
            preceded(opt(alt((tag("a "), tag("an "), tag("one or more ")))), rest),
        ),
    )
    .parse(lower)
    .ok()?;

    let target_text = target_text.trim().trim_end_matches('.');
    let (target_filter, remainder) = parse_type_phrase(target_text);
    if !remainder.trim().is_empty() || matches!(target_filter, TargetFilter::Any) {
        return None;
    }

    Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
        FilterProp::Targets {
            filter: Box::new(target_filter),
        },
    ])))
}

pub(crate) fn parse_cost_modifier_target_filter(lower: &str) -> Option<TargetFilter> {
    type VE<'a> = OracleError<'a>;

    let (input, _) = take_until::<_, _, VE>(" that target").parse(lower).ok()?;
    let (input, _) = tag::<_, _, VE>(" that target").parse(input).ok()?;
    let (input, _) = opt(tag::<_, _, VE>("s")).parse(input).ok()?;
    let (input, _) = tag::<_, _, VE>(" ").parse(input).ok()?;
    let (input, _) = opt(alt((
        tag::<_, _, VE>("one or more "),
        tag("a "),
        tag("an "),
    )))
    .parse(input)
    .ok()?;
    let (_, target_text) = take_until::<_, _, VE>(" cost").parse(input).ok()?;

    let target_text = target_text.trim();
    let target_filter = if matches!(
        target_text,
        "this creature" | "this permanent" | "this card" | "~" | "itself"
    ) {
        Some(TargetFilter::SelfRef)
    } else {
        parse_commander_subject_filter(target_text).or_else(|| {
            let (filter, remainder) = parse_type_phrase(target_text);
            if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
                Some(filter)
            } else {
                None
            }
        })
    }?;

    Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
        FilterProp::Targets {
            filter: Box::new(target_filter),
        },
    ])))
}

pub(crate) fn strip_cost_modifier_target_clause(prefix: &str) -> &str {
    take_until::<_, _, OracleError<'_>>(" that target")
        .parse(prefix)
        .map_or(prefix, |(_, before)| before)
}

pub(crate) fn merge_cost_modifier_target_filter(
    spell_filter: Option<TargetFilter>,
    target_filter: Option<TargetFilter>,
) -> Option<TargetFilter> {
    let Some(target_filter) = target_filter else {
        return spell_filter;
    };

    let TargetFilter::Typed(target_typed) = target_filter else {
        return match spell_filter {
            Some(spell_filter) => Some(TargetFilter::And {
                filters: vec![spell_filter, target_filter],
            }),
            None => Some(target_filter),
        };
    };

    let target_props = target_typed.properties;
    match spell_filter {
        Some(TargetFilter::Typed(mut tf)) => {
            tf.properties.extend(target_props);
            Some(TargetFilter::Typed(tf))
        }
        Some(spell_filter) => Some(TargetFilter::And {
            filters: vec![
                spell_filter,
                TargetFilter::Typed(TypedFilter::card().properties(target_props)),
            ],
        }),
        None => Some(TargetFilter::Typed(
            TypedFilter::card().properties(target_props),
        )),
    }
}

/// CR 601.2f: Parse the Trinisphere-class cost-floor static.
///
/// Pattern (canonical form, with optional trailing "as long as <condition>"):
///   "each spell that would cost less than <N> mana to cast costs <N> mana to cast"
///
/// Both numbers must be the same — that's the floor. Per the Trinisphere
/// ruling, this is a "directly affect the total cost" effect applied after
/// every additive/subtractive modifier, just before the cost is "locked in".
///
/// Returns a `StaticMode::ModifyCost` (Minimum) with `spell_filter = None` (the printed
/// pattern affects all spells; future filtered variants would attach a filter
/// here) and any trailing "as long as" / "if" condition lifted into the
/// `StaticDefinition.condition` field (handles Trinisphere's "as long as this
/// artifact is untapped" gate).
pub(crate) fn try_parse_cost_floor(text: &str, lower: &str) -> Option<StaticDefinition> {
    use nom::sequence::preceded;

    // Strip optional trailing condition before matching the body — keeps the
    // body combinator focused on the cost-floor shape only.
    let (body_lower, condition_text) = if let Some((cond_pos, marker)) = [" as long as ", " if "]
        .into_iter()
        .filter_map(|marker| lower.rfind(marker).map(|pos| (pos, marker)))
        .max_by_key(|(pos, _)| *pos)
    {
        let cond = lower[cond_pos + marker.len()..]
            .trim()
            .trim_end_matches('.')
            .to_string();
        (lower[..cond_pos].trim_end_matches('.'), Some(cond))
    } else {
        (lower.trim_end_matches('.'), None)
    };

    // Body combinator: "each spell that would cost less than <N> mana to cast costs <N> mana to cast"
    let parse_body = (
        tag::<_, _, OracleError<'_>>("each spell that would cost less than "),
        nom_primitives::parse_number_or_x,
        tag(" mana to cast costs "),
        nom_primitives::parse_number_or_x,
        tag(" mana to cast"),
    );
    let (rest, (_, n1, _, n2, _)) = preceded(
        // Tolerate leading whitespace from the canonical-rewrite path.
        nom::character::complete::space0,
        parse_body,
    )
    .parse(body_lower)
    .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    if n1 != n2 {
        return None;
    }
    let amount = ManaCost::generic(n1);

    let mut definition = StaticDefinition::new(StaticMode::ModifyCost {
        mode: CostModifyMode::Minimum,
        amount,
        spell_filter: None,
        dynamic_count: None,
    })
    .description(text.to_string());

    if let Some(cond_text) = condition_text {
        if let Some(sc) = parse_cost_modifier_condition(&cond_text) {
            definition.condition = Some(sc);
        } else if let Ok((rest_cond, sc)) = nom_condition::parse_inner_condition(&cond_text) {
            if rest_cond.trim().is_empty() || rest_cond.trim() == "." {
                definition.condition = Some(sc);
            }
        }
    }

    Some(definition)
}

#[cfg(test)]
mod tests {
    use super::promote_nested_ability_quotes;

    #[test]
    fn promote_nested_ability_quotes_old_growth_troll_chained_grants() {
        let body = "Enchanted Forest has '{T}: Add {G}{G}' and '{1}, {T}, Sacrifice this land: Create a tapped 4/4 green Troll Warrior creature token with trample.'";
        assert_eq!(
            promote_nested_ability_quotes(body),
            "Enchanted Forest has \"{T}: Add {G}{G}\" and \"{1}, {T}, Sacrifice this land: Create a tapped 4/4 green Troll Warrior creature token with trample.\""
        );
    }

    #[test]
    fn promote_nested_ability_quotes_harold_and_bob_single_grant() {
        let body = "Enchanted Forest has '{T}: Add three mana of any one color. You get two rad counters.'";
        assert_eq!(
            promote_nested_ability_quotes(body),
            "Enchanted Forest has \"{T}: Add three mana of any one color. You get two rad counters.\""
        );
    }

    #[test]
    fn promote_nested_ability_quotes_roar_creature_tap_mana_grant() {
        let body = "Creatures you control have '{T}: Add {R}, {G}, or {W}.'";
        assert_eq!(
            promote_nested_ability_quotes(body),
            "Creatures you control have \"{T}: Add {R}, {G}, or {W}.\""
        );
    }

    #[test]
    fn promote_nested_ability_quotes_leaves_unquoted_bodies_unchanged() {
        let body = "Creatures you control have flying";
        assert_eq!(promote_nested_ability_quotes(body), body);
    }
}
