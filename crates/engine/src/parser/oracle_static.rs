use std::borrow::Cow;
use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::{tag, tag_no_case, take_until};
use nom::character::complete::{alpha1, space1};
use nom::combinator::{opt, rest, value};
use nom::sequence::{preceded, terminated};
use nom::Parser;
use nom_language::error::VerboseError;

use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::parse_effect_chain;
use super::oracle_nom::bridge::nom_on_lower;
use super::oracle_nom::condition as nom_condition;
use super::oracle_nom::error::OracleResult;
use super::oracle_nom::filter as nom_filter;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::target as nom_target;
use super::oracle_quantity::{parse_cda_quantity, parse_quantity_ref};
use super::oracle_target::{
    parse_combat_status_prefix, parse_counter_suffix, parse_mana_value_suffix, parse_type_phrase,
};
use super::oracle_util::{
    has_unconsumed_conditional, infer_core_type_for_subtype, parse_comparator_prefix,
    parse_mana_symbols, parse_number, parse_subtype, strip_after, strip_reminder_text, TextPair,
    SELF_REF_PARSE_ONLY_PHRASES, SELF_REF_TYPE_PHRASES,
};
use crate::parser::oracle_warnings::push_warning;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, BasicLandType, CardPlayMode, ChosenSubtypeKind, Comparator,
    ContinuousModification, ControllerRef, FilterProp, ObjectScope, QuantityExpr, QuantityRef,
    StaticCondition, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::counter::{parse_counter_type, CounterMatch};
use crate::types::keywords::{Keyword, KeywordKind};
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::phase::Phase;
use crate::types::statics::{
    ActivationExemption, CastFrequency, CastingProhibitionCondition, HandSizeModification,
    ProhibitionScope, StaticMode, TriggerCause,
};
use crate::types::zones::Zone;

/// Try matching a nom `tag()` against the lowercase text, returning the remaining original-case
/// text on success. This bridges nom's exact-match combinators with the TextPair dual-string
/// pattern used throughout the parser.
fn nom_tag_lower<'a>(text: &'a str, lower: &str, prefix: &str) -> Option<&'a str> {
    tag::<_, _, nom_language::error::VerboseError<&str>>(prefix)
        .parse(lower)
        .ok()
        .map(|(_, matched)| &text[matched.len()..])
}

/// Like `nom_tag_lower`, but operates on a `TextPair` and returns a new `TextPair`
/// with both original and lowercase remainders advanced past the matched prefix.
fn nom_tag_tp<'a>(tp: &TextPair<'a>, prefix: &str) -> Option<TextPair<'a>> {
    tag::<_, _, nom_language::error::VerboseError<&str>>(prefix)
        .parse(tp.lower)
        .ok()
        .map(|(rest_lower, matched)| {
            let rest_original = &tp.original[matched.len()..];
            TextPair::new(rest_original, rest_lower)
        })
}

/// Recognizes the first token/phrase of an effect clause that follows the
/// condition-vs-effect comma in an inverted `"As long as <cond>, <effect>"` line.
///
/// Every alternative ends on a word boundary (trailing space or apostrophe) so
/// `tag("it ")` does not accept `"its "`. The set is derived from the 134-row
/// corpus of currently-affected cards in `client/public/card-data.json` and is
/// intentionally conservative: bare nouns/verbs that commonly appear inside
/// condition clauses (e.g. `"creatures "`, `"lands "`, `"a "`) are omitted.
fn parse_effect_subject_prefix(input: &str) -> OracleResult<'_, ()> {
    alt((
        // Self-reference pronouns ("it …", "it's …").
        value(
            (),
            alt((
                tag("it "),
                tag("it's "),
                tag("it has "),
                tag("it gets "),
                tag("it can "),
                tag("it assigns "),
                tag("it deals "),
                tag("it doesn't "),
            )),
        ),
        // Self-reference tilde token.
        value(
            (),
            alt((
                tag("~ "),
                tag("~'s "),
                tag("~ is "),
                tag("~ has "),
                tag("~ gets "),
                tag("~ can "),
                tag("~ and "),
            )),
        ),
        // Anaphoric subjects for paired/attached/enchanted interactions.
        value(
            (),
            alt((
                tag("that creature "),
                tag("those creatures "),
                tag("both creatures "),
                tag("each of those "),
                tag("that permanent "),
                tag("that card "),
            )),
        ),
        // Typed bulk subjects.
        value(
            (),
            alt((
                tag("each "),
                tag("all "),
                tag("other "),
                tag("enchanted "),
                tag("equipped "),
                tag("creatures you control "),
                tag("lands you control "),
                tag("permanents you control "),
                tag("cards in your hand "),
                tag("cards in your graveyard "),
                tag("the top card "),
                tag("the turn order "),
                tag("the first time "),
            )),
        ),
        // Player-directed and global subjects.
        value(
            (),
            alt((
                tag("you may "),
                tag("you can't "),
                tag("you control "),
                tag("you "),
                tag("players "),
                tag("no more than "),
                tag("defending player "),
                tag("each opponent "),
                tag("each player "),
            )),
        ),
        // Effect-starter verbs/nouns (when no explicit subject).
        value(
            (),
            alt((
                tag("if "),
                tag("prevent "),
                tag("damage "),
                tag("untap all "),
                tag("they "),
            )),
        ),
    ))
    .parse(input)
    .map(|(rest, _)| (rest, ()))
}

/// Scan `tp.lower` for the first `", "` whose tail begins with a recognized
/// effect-subject prefix (see `parse_effect_subject_prefix`). Returns the
/// `(condition, effect)` halves, each as a `TextPair` aligned with the source.
///
/// Uses `match_indices(", ")` for structural iteration over candidate split
/// points (not for parsing dispatch); the dispatch itself is a nom combinator.
/// This mirrors the word-boundary-scan pattern used by `scan_timing_restrictions`
/// in `oracle_casting.rs`.
fn split_on_effect_subject_comma<'a>(tp: &TextPair<'a>) -> Option<(TextPair<'a>, TextPair<'a>)> {
    for (pos, sep) in tp.lower.match_indices(", ") {
        let after = pos + sep.len();
        let tail_lower = &tp.lower[after..];
        if parse_effect_subject_prefix(tail_lower).is_ok() {
            let (condition, _) = tp.split_at(pos);
            let (_, effect) = tp.split_at(after);
            return Some((condition, effect));
        }
    }
    None
}

/// Result of splitting an inverted `"As long as <cond>, <effect>"` line.
struct InvertedSplit {
    /// Canonical-form rewrite `"<effect> as long as <condition>"` ready for
    /// re-dispatch through `parse_static_line_inner`.
    canonical: String,
    /// The effect clause in original case.
    effect_text: String,
    /// The condition clause in original case, suitable for
    /// `StaticCondition::Unrecognized { text }` when the recursed parse fails.
    condition_text: String,
}

/// Detect inverted static form `"As long as <condition>, <effect>"` and split
/// it into a canonical rewrite plus the isolated condition text. Returns
/// `None` when the line does not start with `"as long as "` or when no comma
/// boundary has a recognized effect-subject tail (in which case the caller
/// falls through to the existing generic fallback, preserving today's
/// behavior).
///
/// CR 611.3a: Continuous effects from static abilities apply when their stated
/// condition is true; orientation of the condition clause in the printed text
/// is irrelevant to rules semantics.
fn try_split_inverted_as_long_as(tp: &TextPair<'_>) -> Option<InvertedSplit> {
    let rest = nom_tag_tp(tp, "as long as ")?;
    // Trim a trailing period from both sides before splitting so the canonical
    // form does not carry a stray `.` at the condition boundary.
    let trimmed_original = rest.original.trim_end_matches('.');
    let trimmed_lower = rest.lower.trim_end_matches('.');
    let body = TextPair::new(trimmed_original, trimmed_lower);
    let (condition, effect) = split_on_effect_subject_comma(&body)?;
    let condition_text = condition.original.trim().to_string();
    let effect_text = effect.original.trim();
    let canonical = format!("{effect_text} as long as {condition_text}");
    Some(InvertedSplit {
        canonical,
        effect_text: effect_text.to_string(),
        condition_text,
    })
}

fn try_parse_inverted_attached_subject_grant(
    split: &InvertedSplit,
    description: &str,
) -> Option<StaticDefinition> {
    let condition_lower = split.condition_text.to_lowercase();
    let condition_tp = TextPair::new(&split.condition_text, &condition_lower);
    let affected = parse_attached_subject_is_legendary(&condition_tp)?;

    let effect_lower = split.effect_text.to_lowercase();
    let effect_tp = TextPair::new(&split.effect_text, &effect_lower);
    let predicate = nom_tag_tp(&effect_tp, "it ").or_else(|| nom_tag_tp(&effect_tp, "they "))?;

    parse_continuous_gets_has(predicate.original, affected, description)
}

fn parse_attached_subject_is_legendary(condition: &TextPair<'_>) -> Option<TargetFilter> {
    let rest = nom_tag_tp(condition, "equipped ")?;
    let rest = nom_tag_tp(&rest, "creature is legendary")?;
    if !rest.original.trim().is_empty() {
        return None;
    }

    Some(TargetFilter::Typed(TypedFilter::creature().properties(
        vec![
            FilterProp::EquippedBy,
            FilterProp::HasSupertype {
                value: Supertype::Legendary,
            },
        ],
    )))
}

fn target_filter_is_your_graveyard(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => {
            tf.controller == Some(ControllerRef::You)
                && tf.properties.iter().any(|prop| {
                    matches!(
                        prop,
                        FilterProp::InZone {
                            zone: Zone::Graveyard
                        }
                    )
                })
        }
        TargetFilter::Or { filters } => filters.iter().all(target_filter_is_your_graveyard),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleStaticPredicate {
    CantUntap,
    CantAttack,
    CantBlock,
    CantAttackOrBlock,
    MustAttack,
    MustBlock,
    BlockOnlyCreaturesWithFlying,
    Shroud,
    Hexproof,
    MayLookAtTopOfLibrary,
    LoseAllAbilities,
    NoMaximumHandSize,
    MayPlayAdditionalLand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraveyardGrantedKeywordKind {
    Flashback,
    Escape,
}

impl GraveyardGrantedKeywordKind {
    pub(crate) fn matches_keyword(self, keyword: &Keyword) -> bool {
        match self {
            GraveyardGrantedKeywordKind::Flashback => {
                keyword.kind() == crate::types::keywords::KeywordKind::Flashback
            }
            GraveyardGrantedKeywordKind::Escape => {
                keyword.kind() == crate::types::keywords::KeywordKind::Escape
            }
        }
    }
}

pub(crate) fn try_parse_graveyard_keyword_grant_clause(
    text: &str,
) -> Option<(TargetFilter, GraveyardGrantedKeywordKind)> {
    let stripped = strip_reminder_text(text);
    let lower = stripped.to_lowercase();
    let rest = nom_tag_lower(&stripped, &lower, "each ")?;
    let rest_lower = rest.to_lowercase();
    let (subject, keyword_text) =
        super::oracle_nom::bridge::split_once_on_lower(rest, &rest_lower, " has ").or_else(
            || super::oracle_nom::bridge::split_once_on_lower(rest, &rest_lower, " have "),
        )?;
    let subject = subject.trim();
    let keyword_text = keyword_text.trim().trim_end_matches('.');

    let kind = nom_on_lower(keyword_text, &keyword_text.to_lowercase(), |i| {
        alt((
            value(GraveyardGrantedKeywordKind::Flashback, tag("flashback")),
            value(GraveyardGrantedKeywordKind::Escape, tag("escape")),
        ))
        .parse(i)
    })?
    .0;

    let (filter, remainder) = parse_type_phrase(subject);
    if !remainder.trim().is_empty() || !target_filter_is_your_graveyard(&filter) {
        return None;
    }

    Some((filter, kind))
}

/// Whether the inverted `"As long as <cond>, <effect>"` detector may fire.
///
/// Used as a one-way recursion gate: the outer call runs with `Allow`; when the
/// detector rewrites the line into canonical form `"<effect> as long as <cond>"`
/// and re-invokes `parse_static_line_inner`, it passes `Skip` so the detector
/// cannot re-enter. Any call path that does not originate from the inverted-form
/// rewrite uses `Allow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvertedAsLongAs {
    Allow,
    Skip,
}

/// Parse a static/continuous ability line into a StaticDefinition.
/// Handles: "Enchanted creature gets +N/+M", "has {keyword}",
/// "Creatures you control get +N/+M", etc.
#[tracing::instrument(level = "debug")]
pub fn parse_static_line(text: &str) -> Option<StaticDefinition> {
    let mut def = parse_static_line_inner(text, InvertedAsLongAs::Allow)?;
    populate_active_zones_from_condition(&mut def);
    Some(def)
}

/// CR 113.6 + CR 113.6b: When a static ability's condition asserts the source
/// is in a non-battlefield zone (e.g., "as long as this card is in your
/// graveyard"), that zone is an opt-in functional zone for the static. This
/// mirrors `self_recursion_trigger_zone` for `TriggerDefinition.trigger_zones`.
///
/// Walks the `StaticCondition` tree and collects every `SourceInZone { zone }`
/// where `zone != Zone::Battlefield`, populating `StaticDefinition.active_zones`.
/// The battlefield is NOT included (battlefield is the CR 113.6 default and
/// does not need opt-in). If the condition does not reference a
/// non-battlefield zone, `active_zones` is left empty (battlefield-only).
fn populate_active_zones_from_condition(def: &mut StaticDefinition) {
    let mut zones: Vec<crate::types::zones::Zone> = Vec::new();
    if let Some(cond) = def.condition.as_ref() {
        collect_source_in_zones(cond, &mut zones);
    }
    // Deduplicate while preserving order.
    zones.dedup();
    // Don't clobber an explicitly-set active_zones: upstream callers may pin
    // non-battlefield zones directly on the StaticDefinition (e.g. hand-zone
    // statics) and the condition-derived inference should only fill in zones
    // when nothing has been specified.
    if !zones.is_empty() && def.active_zones.is_empty() {
        def.active_zones = zones;
    }
}

fn collect_source_in_zones(cond: &StaticCondition, out: &mut Vec<crate::types::zones::Zone>) {
    use crate::types::zones::Zone;
    match cond {
        StaticCondition::SourceInZone { zone }
            if *zone != Zone::Battlefield && !out.contains(zone) =>
        {
            out.push(*zone);
        }
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            for c in conditions {
                collect_source_in_zones(c, out);
            }
        }
        StaticCondition::Not { condition } => collect_source_in_zones(condition, out),
        _ => {}
    }
}

fn parse_static_line_inner(text: &str, inverted: InvertedAsLongAs) -> Option<StaticDefinition> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();
    let tp = TextPair::new(&text, &lower);

    // CR 611.3a: An inverted static of the form "As long as <condition>, <effect>"
    // is semantically equivalent to the canonical "<effect> as long as <condition>".
    // Rewrite to canonical form and re-dispatch so the existing conditional-continuous
    // pipeline (parse_enchanted_equipped_predicate → parse_continuous_gets_has at the
    // " as long as " splitter, plus parse_static_condition) handles both orientations
    // uniformly. The `Allow`/`Skip` gate makes recursion re-entry architecturally
    // impossible: the rewrite target cannot begin with "as long as ".
    if matches!(inverted, InvertedAsLongAs::Allow) {
        if let Some(split) = try_split_inverted_as_long_as(&tp) {
            if let Some(def) = try_parse_inverted_attached_subject_grant(&split, &text) {
                return Some(def);
            }
            if let Some(def) = parse_static_line_inner(&split.canonical, InvertedAsLongAs::Skip) {
                return Some(def.description(text.to_string()));
            }
            // Rewrite succeeded (we cleanly separated condition from effect), but the
            // recursed parser could not model the effect clause. Produce a generic
            // Continuous static whose condition is typed via `parse_static_condition`
            // (the same helper `parse_continuous_gets_has` uses at the " as long as "
            // splitter). Fall back to `Unrecognized` only when that helper cannot type
            // the text. Recursion safety: `parse_static_condition` delegates to
            // `nom_condition::parse_inner_condition` which never re-enters this parser.
            let condition = parse_static_condition(&split.condition_text).unwrap_or(
                StaticCondition::Unrecognized {
                    text: split.condition_text,
                },
            );
            return Some(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .condition(condition)
                    .description(text.to_string()),
            );
        }
    }

    if tp.lower == "your speed can increase beyond 4."
        || tp.lower == "your speed can increase beyond 4"
    {
        return Some(
            StaticDefinition::new(StaticMode::SpeedCanIncreaseBeyondFour)
                .affected(TargetFilter::Player)
                .description(text.to_string()),
        );
    }

    // CR 701.38d: "While voting, you may vote an additional time." (Tivit,
    // Seller of Secrets and the Council's-dilemma extra-vote family.) Built
    // for the class — covers any phrasing where the controller gets one
    // additional vote per session. Dispatched via nom so future variants
    // ("two additional times", "while voting on a Council's dilemma you cast")
    // can be added as new combinator arms rather than as additional
    // string-equality checks.
    {
        let lower_trim = tp.lower.trim_end_matches('.').trim();
        let res: nom::IResult<&str, (), nom_language::error::VerboseError<&str>> =
            nom::combinator::value(
                (),
                nom::branch::alt((
                    nom::bytes::complete::tag("while voting, you may vote an additional time"),
                    nom::bytes::complete::tag("while voting you may vote an additional time"),
                )),
            )
            .parse(lower_trim);
        if res.is_ok() {
            return Some(
                StaticDefinition::new(StaticMode::GrantsExtraVote)
                    .affected(TargetFilter::Player)
                    .description(text.to_string()),
            );
        }
    }

    // CR 401.5 + CR 118.9 + CR 601.2a: "You may [play|cast] [filter] from the
    // top of your library [rider]." Top-of-library cast permission class
    // (Realmwalker, Future Sight, Bolas's Citadel, Magus of the Future, Vivien
    // on the Hunt static). Dispatched ahead of the graveyard helper because
    // both anchor on "you may [play|cast]"; the library helper's anchor
    // (" from the top of your library") is unique so there is no overlap, but
    // ordering keeps the flow readable.
    if let Some(result) = try_parse_top_of_library_cast_permission(&text, &lower) {
        return Some(result);
    }

    // CR 604.3 + CR 601.2a: "Once during each of your turns, you may cast [filter] from your graveyard."
    if let Some(result) = try_parse_graveyard_cast_permission(&text, &lower) {
        return Some(result);
    }

    // CR 601.2b + CR 118.9a + CR 601.2: Omniscience-class restricted free-cast
    // static. Optional " from your hand" zone qualifier — Dracogenesis's
    // "you may cast Dragon spells without paying their mana costs" relies on
    // CR 601.2's implicit hand zone.
    if let Some(result) = try_parse_cast_free_permission(&text, &lower) {
        return Some(result);
    }

    // CR 609.4b: "You may spend mana as though it were mana of any color."
    if tp.lower.trim_end_matches('.') == "you may spend mana as though it were mana of any color" {
        return Some(
            StaticDefinition::new(StaticMode::SpendManaAsAnyColor)
                .affected(TargetFilter::Player)
                .description(text.to_string()),
        );
    }

    if nom_tag_tp(&tp, "you may choose not to untap ").is_some()
        && nom_primitives::scan_contains(tp.lower, "during your untap step")
    {
        return Some(
            StaticDefinition::new(StaticMode::MayChooseNotToUntap)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "Untap all <type> you control during each other player's untap step." ---
    // CR 502.3 + CR 113.6: Seedborn Muse class — continuous static granting a
    // second untap pass during each OTHER player's untap step. The parser lowers
    // this to `StaticMode::UntapsDuringEachOtherPlayersUntapStep` with the
    // `affected` filter carrying the permanent class to untap (typically
    // "permanents you control"). Runtime integration lives in
    // `turns::execute_untap`, which scans the battlefield for this variant
    // after the active player's normal untap step.
    if let Some(rest) = nom_tag_tp(&tp, "untap all ") {
        // The subject is the thing being untapped (e.g. "permanents you
        // control", "creatures you control"). Delegate to `parse_type_phrase`
        // which handles the full range of type + controller phrases.
        let (filter, remainder) = parse_type_phrase(rest.original);
        let remainder_lower = remainder.to_lowercase();
        // Accept "during each other player's untap step" with straight and curly apostrophes.
        let tail = remainder_lower.trim().trim_end_matches('.');
        let during_ok = nom_on_lower(tail, tail, |i| {
            value(
                (),
                alt((
                    tag("during each other player's untap step"),
                    tag("during each other player\u{2019}s untap step"),
                )),
            )
            .parse(i)
        })
        .is_some();
        // Require the subject filter to be controlled by "you" — rules text
        // variations outside this ("each player's permanents") would not be
        // Seedborn semantics and fall through.
        let controller_is_you = matches!(
            &filter,
            TargetFilter::Typed(tf) if tf.controller == Some(ControllerRef::You)
        );
        if during_ok && controller_is_you {
            return Some(
                StaticDefinition::new(StaticMode::UntapsDuringEachOtherPlayersUntapStep)
                    .affected(filter)
                    .description(text.to_string()),
            );
        }
    }

    // --- "Play with the top card of your library revealed" ---
    // CR 400.2: Continuous effect making top card public information.
    if nom_primitives::scan_contains(tp.lower, "play with the top card") {
        if has_unconsumed_conditional(tp.lower) {
            tracing::warn!(
                text = text,
                "Unconsumed conditional in 'play with the top card' catch-all — parser may need extension"
            );
        } else {
            let all_players = nom_primitives::scan_contains(tp.lower, "their libraries")
                || nom_primitives::scan_contains(tp.lower, "each player");
            return Some(
                StaticDefinition::new(StaticMode::RevealTopOfLibrary { all_players })
                    .affected(TargetFilter::SelfRef)
                    .description(text.to_string()),
            );
        }
    }

    // --- "Skip your [step] step" ---
    // CR 614.1b + CR 614.10: Replacement effect that replaces the named step with nothing.
    if let Some(rest_tp) = nom_tag_tp(&tp, "skip your ") {
        if let Some(step) = parse_step_name(rest_tp.lower.trim_end_matches('.')) {
            return Some(
                StaticDefinition::new(StaticMode::SkipStep { step })
                    .affected(TargetFilter::SelfRef)
                    .description(text.to_string()),
            );
        }
    }

    // CR 402.2 + CR 514.1: Maximum hand size modification.
    if let Some(result) = try_parse_max_hand_size(&tp, &text) {
        return Some(result);
    }

    // --- "You control enchanted creature/permanent/land/artifact" (Control Magic pattern) ---
    // CR 303.4e + CR 613.2: Aura-based continuous control-changing effects.
    if let Some(type_word) = nom_tag_lower(
        tp.lower.trim_end_matches('.'),
        tp.lower.trim_end_matches('.'),
        "you control enchanted ",
    ) {
        let (type_filter, remainder) = parse_type_phrase(type_word);
        if remainder.is_empty() {
            if let TargetFilter::Typed(mut tf) = type_filter {
                tf.properties.push(FilterProp::EnchantedBy);
                return Some(
                    StaticDefinition::continuous()
                        .affected(TargetFilter::Typed(tf))
                        .modifications(vec![ContinuousModification::ChangeController])
                        .description(text.to_string()),
                );
            }
        }
    }

    // --- "Enchanted creature gets +N/+M" or "has {keyword}" ---
    if let Some(rest) = nom_tag_tp(&tp, "enchanted creature ") {
        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]));
        if let Some(def) = parse_enchanted_equipped_predicate(rest.original, filter, &text) {
            return Some(def);
        }
    }

    // --- "Enchanted permanent gets/has ..." ---
    if let Some(rest) = nom_tag_tp(&tp, "enchanted permanent ") {
        let filter =
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy]));
        if let Some(def) = parse_enchanted_equipped_predicate(rest.original, filter, &text) {
            return Some(def);
        }
    }

    // CR 613.1d + CR 205.1a: "Enchanted [permanent-type] is a [type] [with base P/T N/N]
    // [in addition to its other types]" — type-changing aura effects.
    // Must come before the basic-land-type handler which is a subset of this pattern.
    if let Some(def) = parse_enchanted_is_type(&tp, &text) {
        return Some(def);
    }

    // CR 305.7: "Enchanted land is a [type]" — must be before general "enchanted land" handler.
    if let Some(rest) = nom_tag_tp(&tp, "enchanted land is a ") {
        let rest = rest.trim_end_matches('.');
        // "in addition to its other types" → AddSubtype (not replacement)
        if let Some(land_name) = rest.strip_suffix(" in addition to its other types") {
            if let Some(basic_type) = parse_basic_land_type(land_name.lower) {
                return Some(
                    StaticDefinition::continuous()
                        .affected(TargetFilter::Typed(
                            TypedFilter::land().properties(vec![FilterProp::EnchantedBy]),
                        ))
                        .modifications(vec![ContinuousModification::AddSubtype {
                            subtype: basic_type.as_subtype_str().to_string(),
                        }])
                        .description(text.to_string()),
                );
            }
        }
        // Default: replacement semantics per CR 305.7
        if let Some(basic_type) = parse_basic_land_type(rest.lower.trim()) {
            return Some(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::land().properties(vec![FilterProp::EnchantedBy]),
                    ))
                    .modifications(vec![ContinuousModification::SetBasicLandType {
                        land_type: basic_type,
                    }])
                    .description(text.to_string()),
            );
        }
    }

    if let Some(rest) = nom_tag_tp(&tp, "enchanted land ") {
        let filter =
            TargetFilter::Typed(TypedFilter::land().properties(vec![FilterProp::EnchantedBy]));
        if let Some(def) = parse_enchanted_equipped_predicate(rest.original, filter, &text) {
            return Some(def);
        }
    }

    // --- "Equipped creature gets +N/+M" ---
    if let Some(rest) = nom_tag_tp(&tp, "equipped creature ") {
        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy]));
        if let Some(def) = parse_enchanted_equipped_predicate(rest.original, filter, &text) {
            return Some(def);
        }
    }

    // CR 508.1b: "All creatures attacking you <predicate>" — filter scoped to attackers
    // whose defending player is the source's controller. Must precede the generic
    // "all creatures " branch below since that would otherwise consume the prefix
    // and leave "attacking you <predicate>" as input to `parse_continuous_gets_has`,
    // which expects a verb ("gets"/"has"/"is"), not a subject continuation.
    if let Some(rest) = nom_tag_tp(&tp, "all creatures attacking you ") {
        let filter = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::AttackingController]),
        );
        if let Some(def) = parse_continuous_gets_has(rest.original, filter, &text) {
            return Some(def);
        }
    }

    // CR 205.3m + CR 613.1: "Each creature you control that's a <Subtype>[ or a <Subtype>] <predicate>"
    // Example (Auriok Steelshaper): "each creature you control that's a Soldier or a Knight gets +1/+1"
    // Consumes a capitalized-subtype list joined by " or a " / " and a " / " or " / " and ",
    // stopping at the first non-capitalized word (start of the predicate). Reuses
    // `typed_filter_for_subtype` + `parse_subtype` (plural normalization) for the filter
    // construction and `TargetFilter::Or` for the union case.
    if let Some(rest) = nom_tag_tp(&tp, "each creature you control that's a ") {
        if let Some((filter, predicate)) = try_parse_thats_a_subtype_list(rest.original) {
            if let Some(def) = parse_continuous_gets_has(predicate, filter, &text) {
                return Some(def);
            }
        }
    }

    // --- "All creatures get/have ..." ---
    if let Some(rest) = nom_tag_tp(&tp, "all creatures ") {
        if let Some(def) = parse_continuous_gets_has(
            rest.original,
            TargetFilter::Typed(TypedFilter::creature()),
            &text,
        ) {
            return Some(def);
        }
    }

    // CR 205.1a: "All permanents are [type] in addition to their other types."
    // Global type-addition effect (e.g., Mycosynth Lattice, Enchanted Evening).
    if let Some(def) = parse_all_permanents_are_type(&tp, &text) {
        return Some(def);
    }

    // CR 613.1e + CR 105.1 / CR 105.2c / CR 105.3: "All [subject] are [color(s)]."
    // — a global color-defining static (Layer 5) that sets every matching object
    // to a new color or to colorless. Covers Darkest Hour, Thran Lens, Ghostflame
    // Sliver, and the wider class of "All X are Y" color-setting cards. Must
    // dispatch AFTER the "are [type] in addition..." branch (that is a
    // type-addition, not a color set) and AFTER `parse_continuous_gets_has`-driven
    // branches (those require a verb like "gets"/"has", so they cleanly return
    // None for "are black" predicates). Must dispatch BEFORE
    // `parse_land_type_change` — color-rejected "All lands are Plains."-shaped
    // lines fall through to that branch correctly.
    if let Some(def) = parse_all_subject_are_color(&tp, &text) {
        return Some(def);
    }

    // CR 508.1d / CR 509.1c: Subject-scoped "attack/block each combat if able" patterns.
    // These apply MustAttack/MustBlock to a class of creatures (not just self).
    // Compound forms ("attacks or blocks") produce multiple statics; return the first here.
    // Use `parse_static_line_multi()` for callers that need all results.
    if let Some(defs) = try_parse_scoped_must_attack_block(&lower, &text) {
        return defs.into_iter().next();
    }

    // CR 702.3b + CR 611.3a: "<subject> can attack as though <pronoun>
    // didn't have defender [as long as <condition>]" — conditional or
    // unconditional grant of CanAttackWithDefender to a subject class.
    // Handles ~, "this creature", core-type filter subjects ("Creatures
    // you control", "Modified creatures you control"), and the
    // "each creature you control with defender" pattern. Enchanted/Equipped
    // subjects are handled by parse_enchanted_equipped_predicate; this
    // branch covers non-attached-subject forms.
    //
    // The helper returns None when the phrase is absent or when the subject
    // cannot be resolved to a known filter — both cases fall through to
    // subsequent dispatch branches.
    if let Some(def) = parse_can_attack_despite_defender(&tp, &text) {
        return Some(def);
    }

    // --- "Each creature you control [with condition] assigns combat damage equal to its toughness" ---
    // CR 510.1c: Doran-class effects that cause creatures to use toughness for combat damage.
    if let Some(def) = parse_assigns_damage_from_toughness(&lower, &text) {
        return Some(def);
    }

    // --- "You may have this creature assign its combat damage as though it weren't blocked." ---
    // CR 510.1c: Thorn Elemental-class self static.
    if let Some(def) = parse_assign_damage_as_though_unblocked(&lower, &text) {
        return Some(def);
    }

    // --- "Enchanted/Equipped creature's controller may have it assign..." ---
    if let Some(def) = parse_attached_creature_assign_damage_as_though_unblocked(&tp, &text) {
        return Some(def);
    }

    // --- "Creatures you control [with counter condition] get/have ..." ---
    // Must come BEFORE parse_typed_you_control to prevent core type words like
    // "Creatures" from falling through to the subtype path (A1 fix: 162+ cards).
    if let Some(rest_tp) = nom_tag_tp(&tp, "creatures you control ") {
        let after_prefix = rest_tp.original;
        let (filter, predicate_text) =
            if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                (
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![prop]),
                    ),
                    rest,
                )
            // CR 613.1: "Creatures you control that are [property] get/have ..."
            } else if let Some(that_rest_tp) = nom_tag_tp(&rest_tp, "that are ") {
                if let Some((prop, prop_rest_original)) = nom_on_lower(
                    that_rest_tp.original,
                    that_rest_tp.lower,
                    nom_filter::parse_property_filter,
                ) {
                    (
                        TargetFilter::Typed(
                            TypedFilter::creature()
                                .controller(ControllerRef::You)
                                .properties(vec![prop]),
                        ),
                        prop_rest_original.trim_start(),
                    )
                } else if let Some((color, color_rest_original)) = nom_on_lower(
                    that_rest_tp.original,
                    that_rest_tp.lower,
                    nom_primitives::parse_color,
                ) {
                    (
                        TargetFilter::Typed(
                            TypedFilter::creature()
                                .controller(ControllerRef::You)
                                .properties(vec![FilterProp::HasColor { color }]),
                        ),
                        color_rest_original.trim_start(),
                    )
                } else {
                    (
                        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                        after_prefix,
                    )
                }
            } else {
                (
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                    after_prefix,
                )
            };
        if let Some(def) = parse_continuous_gets_has(predicate_text, filter, &text) {
            return Some(def);
        }
    }

    // --- "Other creatures you control [with counter condition] get/have ..." ---
    // CR 613.7: "Other" excludes the source permanent itself via FilterProp::Another.
    if let Some(rest_tp) = nom_tag_tp(&tp, "other creatures you control ") {
        let after_prefix = rest_tp.original;
        let (filter, predicate_text) = if let Some((prop, rest)) =
            strip_counter_condition_prefix(after_prefix)
        {
            (
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![prop, FilterProp::Another]),
                ),
                rest,
            )
        // CR 613.1: "Other creatures you control that are [property] get/have ..."
        } else if let Some(that_rest_tp) = nom_tag_tp(&rest_tp, "that are ") {
            if let Some((prop, prop_rest_original)) = nom_on_lower(
                that_rest_tp.original,
                that_rest_tp.lower,
                nom_filter::parse_property_filter,
            ) {
                (
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![prop, FilterProp::Another]),
                    ),
                    prop_rest_original.trim_start(),
                )
            } else if let Some((color, color_rest_original)) = nom_on_lower(
                that_rest_tp.original,
                that_rest_tp.lower,
                nom_primitives::parse_color,
            ) {
                (
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::HasColor { color }, FilterProp::Another]),
                    ),
                    color_rest_original.trim_start(),
                )
            } else {
                (
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::Another]),
                    ),
                    after_prefix,
                )
            }
        } else {
            (
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::Another]),
                ),
                after_prefix,
            )
        };
        if let Some(def) = parse_continuous_gets_has(predicate_text, filter, &text) {
            return Some(def);
        }
    }

    // --- "Other [Subtype] creatures you control get/have..." ---
    // e.g. "Other Zombies you control get +1/+1"
    if let Some(rest_tp) = nom_tag_tp(&tp, "other ") {
        if let Some(result) = parse_typed_you_control(rest_tp.original, rest_tp.lower, true) {
            return Some(result);
        }
    }

    // --- "[Subtype] creatures you control get/have..." ---
    // e.g. "Elf creatures you control get +1/+1"
    // Skip for "other" prefix — already handled above with is_other=true.
    if nom_tag_tp(&tp, "other ").is_none() {
        if let Some(result) = parse_typed_you_control(tp.original, tp.lower, false) {
            return Some(result);
        }
    }

    // CR 305.7: "[Subject] lands are [type]" — land type-changing statics.
    // Must come before parse_subject_continuous_static (which splits on "gets/has/gains"
    // verbs and would not match "are" predicates).
    if let Some(def) = parse_land_type_change(&tp, &text) {
        return Some(def);
    }

    if let Some(def) = parse_subject_continuous_static(&text) {
        return Some(def);
    }

    // --- "Lands you control have '[type]'" ---
    if let Some(rest_tp) = nom_tag_tp(&tp, "lands you control have ") {
        let rest_cleaned = rest_tp
            .original
            .trim()
            .trim_end_matches('.')
            .trim_matches(|c: char| c == '\'' || c == '"');
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    TypedFilter::land().controller(ControllerRef::You),
                ))
                .modifications(vec![ContinuousModification::AddSubtype {
                    subtype: rest_cleaned.to_string(),
                }])
                .description(text.to_string()),
        );
    }

    // --- "During your turn, as long as ~ has [counters], [pronoun]'s a [P/T] [types] and has [keyword]" ---
    // Compound condition: DuringYourTurn + HasCounters → animation pattern (Kaito, Gideon, etc.)
    if let Some(def) = parse_compound_turn_counter_animation(tp.lower, tp.original) {
        return Some(def);
    }

    // --- "During your turn, [subject] has/gets ..." ---
    // --- "During turns other than yours, [subject] has/gets ..." ---
    let (turn_rest_tp, turn_condition) =
        if let Some(rest_tp) = nom_tag_tp(&tp, "during your turn, ") {
            (Some(rest_tp), Some(StaticCondition::DuringYourTurn))
        } else if let Some(rest_tp) = nom_tag_tp(&tp, "during turns other than yours, ") {
            (
                Some(rest_tp),
                Some(StaticCondition::Not {
                    condition: Box::new(StaticCondition::DuringYourTurn),
                }),
            )
        } else {
            (None, None)
        };
    if let (Some(rest_tp), Some(condition)) = (turn_rest_tp, turn_condition) {
        if let Some(subject_end) = find_continuous_predicate_start(rest_tp.lower) {
            let subject = rest_tp.original[..subject_end].trim();
            let predicate = rest_tp.original[subject_end + 1..].trim();
            if let Some(affected) = parse_continuous_subject_filter(subject) {
                let modifications = parse_continuous_modifications(predicate);
                if !modifications.is_empty() {
                    return Some(
                        StaticDefinition::continuous()
                            .affected(affected)
                            .modifications(modifications)
                            .condition(condition)
                            .description(text.to_string()),
                    );
                }
            }
        }
    }

    if let Some(def) = parse_subject_rule_static(&text) {
        return Some(def);
    }

    // --- "~ is the chosen type in addition to its other types" ---
    // Distinguish creature type (Metallic Mimic) vs basic land type (Multiversal Passage)
    if nom_primitives::scan_contains(tp.lower, "is the chosen type") {
        let kind = if nom_tag_tp(&tp, "this creature").is_some()
            || nom_primitives::scan_contains(tp.lower, "creature is the chosen")
        {
            ChosenSubtypeKind::CreatureType
        } else {
            ChosenSubtypeKind::BasicLandType
        };
        let modification = ContinuousModification::AddChosenSubtype { kind };
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![modification])
                .description(text.to_string()),
        );
    }

    // --- CDA: "~'s power is equal to the number of card types among cards in all graveyards
    //     and its toughness is equal to that number plus 1" (Tarmogoyf) ---
    if let Some(def) = parse_cda_pt_equality(tp.lower, tp.original) {
        return Some(def);
    }

    if let Some(def) = parse_conditional_static(&text) {
        return Some(def);
    }

    // --- "~ has [keyword] as long as ..." (must be before generic self-ref "has") ---
    if let Some(has_pos) = tp.find(" has ") {
        if let Some(cond_pos) = tp.find(" as long as ") {
            if has_pos < cond_pos {
                let keyword_text = tp.lower[has_pos + 5..cond_pos].trim();
                let condition_text = text[cond_pos + 12..].trim().trim_end_matches('.');
                let mut modifications = Vec::new();
                if let Some(kw) = map_keyword(keyword_text) {
                    modifications.push(ContinuousModification::AddKeyword { keyword: kw });
                }
                let condition = parse_static_condition(condition_text).unwrap_or(
                    StaticCondition::Unrecognized {
                        text: condition_text.to_string(),
                    },
                );
                return Some(
                    StaticDefinition::continuous()
                        .affected(TargetFilter::SelfRef)
                        .modifications(modifications)
                        .condition(condition)
                        .description(text.to_string()),
                );
            }
        }
    }

    // --- "~ has/gets ..." (self-referential) ---
    // Match lines like "CARDNAME has deathtouch" or "CARDNAME gets +1/+1"
    if let Some(pos) = tp
        .find(" has ")
        .or_else(|| tp.find(" gets "))
        .or_else(|| tp.find(" get "))
    {
        let verb_slice = &tp.lower[pos..];
        let (verb_len, verb_prefix) = if nom_tag_lower(verb_slice, verb_slice, " has ").is_some() {
            (5, "has ")
        } else if nom_tag_lower(verb_slice, verb_slice, " gets ").is_some() {
            (6, "gets ")
        } else {
            (5, "gets ") // " get " maps to "gets " for continuous parsing
        };
        let subject = &tp.lower[..pos];
        // Only match if the subject doesn't look like a known prefix we handle elsewhere
        if !nom_primitives::scan_contains(subject, "creature")
            && !nom_primitives::scan_contains(subject, "permanent")
            && !nom_primitives::scan_contains(subject, "land")
            && nom_tag_lower(subject, subject, "all ").is_none()
            && nom_tag_lower(subject, subject, "other ").is_none()
        {
            let after = &tp.original[pos + verb_len..];
            let predicate = format!("{}{}", verb_prefix, after);
            let predicate_lower = predicate.to_lowercase();

            // CR 604.1: Strip suffix turn conditions —
            // "has first strike during your turn" → condition + "has first strike"
            let (effective_predicate, suffix_condition) =
                strip_suffix_turn_condition(&predicate_lower);

            if let Some(mut def) =
                parse_continuous_gets_has(&effective_predicate, TargetFilter::SelfRef, tp.original)
            {
                if let Some(cond) = suffix_condition {
                    def.condition = Some(cond);
                }
                return Some(def);
            }
        }
    }

    // --- "~ isn't a [type] [as long as <cond>]" (layer-4 type removal) ---
    // CR 613.1d: Layer 4 type-changing effects. The clause splitter upstream
    // (`try_split_inverted_as_long_as`) rewrites "As long as <cond>, ~ isn't
    // a <type>." into canonical "~ isn't a <type> as long as <cond>"; we
    // mirror the " as long as " split used by `parse_continuous_gets_has`
    // (CR 611.3a) so both orientations produce non-empty modifications plus
    // an attached condition.
    if let Ok((_, (_, type_rest))) = nom_primitives::split_once_on(tp.lower, "isn't a ") {
        // type_rest is a suffix of tp.lower; original/lower have equal byte
        // lengths, so we can recover the original-case slice by offsetting
        // from tp.original by the same length.
        let type_rest_original = &tp.original[tp.original.len() - type_rest.len()..];
        let type_rest_tp = TextPair::new(type_rest_original, type_rest);
        let (type_text_tp, condition_tp) = match type_rest_tp.split_around(" as long as ") {
            Some((before, after)) => (before, Some(after)),
            None => (type_rest_tp, None),
        };
        let type_name = type_text_tp.lower.trim().trim_end_matches('.');
        // Pre-anchored slice — `split_once_on("isn't a ")` consumed everything
        // up to and including "isn't a ", and we then stripped a trailing
        // " as long as …" clause. What remains is the type word plus an
        // optional trailing period, so a literal `match` on the five core
        // types is idiomatic enum-conversion (not parsing dispatch).
        let core_type = match type_name {
            "creature" => Some(CoreType::Creature),
            "artifact" => Some(CoreType::Artifact),
            "enchantment" => Some(CoreType::Enchantment),
            "land" => Some(CoreType::Land),
            "planeswalker" => Some(CoreType::Planeswalker),
            _ => None,
        };
        if let Some(ct) = core_type {
            let mut def = StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::RemoveType { core_type: ct }])
                .description(text.to_string());
            if let Some(cond_tp) = condition_tp {
                let cond_text = cond_tp.original.trim().trim_end_matches('.');
                let condition =
                    parse_static_condition(cond_text).unwrap_or(StaticCondition::Unrecognized {
                        text: cond_text.to_string(),
                    });
                def = def.condition(condition);
            }
            return Some(def);
        }
    }

    // --- "~ can't be blocked [by filter] [as long as condition]" ---
    // CR 509.1b: Handles unconditional, conditional, and filter-based "can't be blocked".
    // "except by" patterns are handled separately by CantBeBlockedExceptBy.
    if nom_primitives::scan_contains(tp.lower, "can't be blocked")
        && !nom_primitives::scan_contains(tp.lower, "except by")
    {
        // Find text after "can't be blocked" and try to parse a condition or filter
        if let Some((_, blocked_rest)) =
            nom_primitives::scan_split_at_phrase(tp.lower, |i| tag("can't be blocked").parse(i))
        {
            let after_blocked = blocked_rest["can't be blocked".len()..]
                .trim()
                .trim_end_matches('.');

            // CR 509.1b: "can't be blocked by <filter>" — extract blocker restriction filter.
            if let Ok((by_rest, _)) =
                tag::<_, _, nom_language::error::VerboseError<&str>>("by ").parse(after_blocked)
            {
                let (filter, remainder) = parse_type_phrase(by_rest);
                if !matches!(filter, TargetFilter::Any) {
                    let mut def = StaticDefinition::new(StaticMode::CantBeBlockedBy { filter })
                        .affected(TargetFilter::SelfRef)
                        .description(text.to_string());
                    // Check for trailing condition after the filter (e.g., "as long as...")
                    let trailing = remainder.trim().trim_end_matches('.');
                    if !trailing.is_empty() {
                        if let Some(condition) = nom_condition::parse_condition(trailing)
                            .ok()
                            .and_then(|(r, c)| r.trim().is_empty().then_some(c))
                        {
                            def.condition = Some(condition);
                        }
                    }
                    return Some(def);
                }
            }

            let condition = if after_blocked.is_empty() {
                None
            } else {
                // CR 509.1h: parse_condition handles "as long as " prefix via nom combinator
                nom_condition::parse_condition(after_blocked)
                    .ok()
                    .and_then(|(r, c)| r.trim().is_empty().then_some(c))
                    .or_else(|| {
                        Some(StaticCondition::Unrecognized {
                            text: after_blocked.to_string(),
                        })
                    })
            };
            let mut def = StaticDefinition::new(StaticMode::CantBeBlocked)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string());
            if let Some(c) = condition {
                def.condition = Some(c);
            }
            return Some(def);
        }
    }

    // --- "Creatures can't attack [you | you or planeswalkers you control] unless
    //     their controller pays {N} [for each of those creatures]" ---
    // CR 508.1d + CR 508.1h + CR 118.12a: Attack-tax static family
    // (Ghostly Prison, Propaganda, Sphere of Safety, Windborn Muse, Archangel of
    // Tithes, Baird, etc.). Produces a typed UnlessPay condition with
    // per-affected-creature scaling, so the runtime can aggregate across every
    // declared attacker covered by the filter.
    //
    // Also covers the block side ("Creatures can't block unless...") via a
    // shared combinator, and the "Enchanted creature can't attack unless its
    // controller pays {N}" aura variant (Brainwash) via `~ can't attack`
    // below — the aura variant already yields `TargetFilter::SelfRef` and
    // `StaticMode::CantAttack`, so only the unless-scaling needs to flow
    // through.
    if let Some(def) = parse_combat_tax_static(&tp, &text) {
        return Some(def);
    }

    if let Some(def) = parse_subject_combat_rule_static(&text) {
        return Some(def);
    }

    // --- "~ can't block" ---
    if nom_primitives::scan_contains(tp.lower, "can't block")
        && !nom_primitives::scan_contains(tp.lower, "can't be blocked")
    {
        let mut def = StaticDefinition::new(StaticMode::CantBlock)
            .affected(TargetFilter::SelfRef)
            .description(text.to_string());
        if let Some(condition) = parse_unless_static_condition(&tp) {
            def.condition = Some(condition);
        }
        return Some(def);
    }

    // --- "~ can't attack" ---
    if nom_primitives::scan_contains(tp.lower, "can't attack") {
        let mode = if nom_primitives::scan_contains(tp.lower, "can't attack or block") {
            StaticMode::CantAttackOrBlock
        } else {
            StaticMode::CantAttack
        };
        let mut def = StaticDefinition::new(mode)
            .affected(TargetFilter::SelfRef)
            .description(text.to_string());
        if let Some(condition) = parse_unless_static_condition(&tp) {
            def.condition = Some(condition);
        }
        return Some(def);
    }

    // --- "Activated abilities of <type-list> [your opponents control|you control] can't be activated" ---
    // CR 602.5 + CR 603.2a: Global filter-scoped activation prohibition — Clarion Conqueror,
    // Karn the Great Creator. Opponent-ness rides on the TargetFilter's `ControllerRef`,
    // NOT on the activator scope (`who = AllPlayers`) — per CR 602.5, the prohibition is
    // on the ability itself, not a specific activator.
    if let Some(def) = parse_filter_scoped_cant_be_activated(&tp, &text) {
        return Some(def);
    }

    // --- "Spells and abilities <scope> can't cause their controller to search their library" ---
    // CR 701.23 + CR 609.3: Ashiok, Dream Render's first static. Subject-scoped
    // prohibition where `cause` identifies whose spells/abilities are muzzled.
    if let Some(def) = parse_cant_search_library(&tp, &text) {
        return Some(def);
    }

    // --- "Creatures entering [the battlefield] [and dying] don't cause abilities to trigger" ---
    // CR 603.2g + CR 603.6a + CR 700.4: Torpor Orb (ETB only), Hushbringer (ETB + Dies).
    if let Some(def) = parse_suppress_triggers(&tp, &text) {
        return Some(def);
    }

    // --- "its activated abilities can't be activated" / "activated abilities can't be activated" ---
    // CR 602.5 + CR 603.2a: Prevents activated abilities of the affected permanent from
    // being activated. The self-reference case: `who = AllPlayers, source_filter = SelfRef`.
    // Global filter-scoped variants (Clarion/Karn) are handled by parse_filter_scoped_cant_be_activated
    // which runs earlier via the "activated abilities of " prefix dispatch.
    if nom_primitives::scan_contains(tp.lower, "activated abilities can't be activated") {
        let mut def = StaticDefinition::new(StaticMode::CantBeActivated {
            who: ProhibitionScope::AllPlayers,
            source_filter: TargetFilter::SelfRef,
            // CR 605.1a: Self-ref form has no "unless they're..." suffix.
            exemption: ActivationExemption::None,
        })
        .affected(TargetFilter::SelfRef)
        .description(text.to_string());
        if let Some(condition) = parse_unless_static_condition(&tp) {
            def.condition = Some(condition);
        }
        return Some(def);
    }

    // --- "this spell can't be copied" ---
    // CR 707.10: Self-referential uncopyability, attached to the spell's
    // GameObject at cast time via the static pipeline. Runtime enforcement
    // lives in effects/copy_spell.rs. "this spell" is in SELF_REF_PARSE_ONLY_PHRASES
    // (not normalized to `~`), so match it literally.
    if nom_primitives::scan_contains(tp.lower, "can't be copied") {
        return Some(
            StaticDefinition::new(StaticMode::CantBeCopied)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "can't be countered" ---
    // CR 101.2: "Can't" effects override "can" effects.
    if nom_primitives::scan_contains(tp.lower, "can't be countered") {
        if has_unconsumed_conditional(tp.lower) {
            tracing::warn!(
                text = text,
                "Unconsumed conditional in 'can't be countered' catch-all — parser may need extension"
            );
        } else {
            let affected = parse_cant_be_countered_subject(&tp);
            return Some(
                StaticDefinition::new(StaticMode::CantBeCountered)
                    .affected(affected)
                    .description(text.to_string()),
            );
        }
    }

    // --- "~ can't be the target" or "~ can't be targeted" ---
    if nom_primitives::scan_contains(tp.lower, "can't be the target")
        || nom_primitives::scan_contains(tp.lower, "can't be targeted")
    {
        return Some(
            StaticDefinition::new(StaticMode::CantBeTargeted)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't be sacrificed" (CR 701.21) ---
    // Self-referential prohibition on sacrifice. Runtime enforcement lives in
    // `game::sacrifice` via `object_has_static_other(state, id, "CantBeSacrificed")`.
    if nom_primitives::scan_contains(tp.lower, "can't be sacrificed") {
        return Some(
            StaticDefinition::new(StaticMode::Other("CantBeSacrificed".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't be equipped or enchanted" (CR 701.3 + CR 702.5 + CR 702.6) ---
    // Compound attach prohibition. MUST be scanned BEFORE the solo "can't be enchanted"
    // and "can't be equipped" blocks below, otherwise the compound phrase falls through
    // and only a single definition is emitted here (losing one half of the prohibition).
    // The full two-definition form is produced by `parse_static_line_multi` so callers
    // that iterate all statics on a line get both. Here we return the first mode so
    // `parse_static_line` has a non-None answer for the self-ref case.
    if nom_primitives::scan_contains(tp.lower, "can't be equipped or enchanted") {
        return Some(
            StaticDefinition::new(StaticMode::Other("CantBeEquipped".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't be enchanted [by other auras]" (CR 702.5) ---
    if nom_primitives::scan_contains(tp.lower, "can't be enchanted") {
        return Some(
            StaticDefinition::new(StaticMode::Other("CantBeEnchanted".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't be equipped" (CR 702.6) ---
    if nom_primitives::scan_contains(tp.lower, "can't be equipped") {
        return Some(
            StaticDefinition::new(StaticMode::Other("CantBeEquipped".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't transform" (CR 701.27) ---
    // Self-referential transform prohibition (e.g., Immerwolf for non-Human Werewolves).
    // Runtime enforcement lives in `game::transform` via
    // `object_has_static_other(state, id, "CantTransform")`.
    if nom_primitives::scan_contains(tp.lower, "can't transform") {
        return Some(
            StaticDefinition::new(StaticMode::Other("CantTransform".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- CR 604.3: "[type] cards in [zones] can't enter the battlefield" ---
    // e.g., Grafdigger's Cage: "Creature cards in graveyards and libraries can't enter the battlefield."
    if nom_primitives::scan_contains(tp.lower, "can't enter the battlefield") {
        let affected = parse_cant_enter_battlefield_subject(&tp);
        return Some(
            StaticDefinition::new(StaticMode::CantEnterBattlefieldFrom)
                .affected(affected)
                .description(text.to_string()),
        );
    }

    // --- CR 101.2 + CR 604.1: Per-turn casting limits ---
    // e.g., Rule of Law: "Each player can't cast more than one spell each turn."
    // e.g., Deafening Silence: "Each player can't cast more than one noncreature spell each turn."
    // e.g., Fires of Invention: "You can cast no more than two spells each turn."
    // Must be checked before CantCastDuring/CantCastFrom to avoid false matches.
    if let Some(def) = parse_per_turn_cast_limit(tp.lower, &text) {
        return Some(def);
    }

    // --- CR 117.1a + CR 604.1: "can cast spells only during your turn" ---
    // E.g., Fires of Invention: "You can cast spells only during your turn."
    // Must be checked AFTER PerTurnCastLimit (which handles "no more than N" in compound clauses)
    // and BEFORE the generic CantCastDuring block (which matches "can't cast spells during").
    // Guard: exclude compound lines containing "each turn" — those are split at the oracle.rs level
    // so both CantCastDuring and PerTurnCastLimit are emitted independently.
    if nom_primitives::scan_contains(tp.lower, "can cast spells only during your turn")
        && !nom_primitives::scan_contains(tp.lower, "each turn")
    {
        let who = strip_casting_prohibition_subject(tp.lower)
            .map(|(scope, _)| scope)
            .unwrap_or(ProhibitionScope::Controller);
        return Some(
            StaticDefinition::new(StaticMode::CantCastDuring {
                who,
                when: CastingProhibitionCondition::NotDuringYourTurn,
            })
            .description(text.to_string()),
        );
    }

    // CR 117.1: "can cast spells only any time they could cast a sorcery"
    // E.g., Teferi, Time Raveler; Teferi, Mage of Zhalfir.
    if nom_primitives::scan_contains(
        tp.lower,
        "can cast spells only any time they could cast a sorcery",
    ) {
        let who = strip_casting_prohibition_subject(tp.lower)
            .map(|(scope, _)| scope)
            .unwrap_or(ProhibitionScope::Opponents);
        return Some(
            StaticDefinition::new(StaticMode::CantCastDuring {
                who,
                when: CastingProhibitionCondition::NotSorcerySpeed,
            })
            .description(text.to_string()),
        );
    }

    // --- CR 101.2: Temporal-prefix casting prohibitions ---
    // e.g., "During your turn, your opponents can't cast spells or activate abilities..."
    // e.g., "During combat, players can't cast instant spells or activate abilities..."
    // Handles "During [time], [subject] can't cast [type] spells" with leading temporal clause.
    if let Some(def) = parse_temporal_prefix_cant_cast(tp.lower, &text) {
        return Some(def);
    }

    // --- CR 101.2: Turn/phase-scoped casting prohibitions ---
    // e.g., Teferi, Time Raveler: "Your opponents can't cast spells during your turn."
    // e.g., "Players can't cast spells during combat."
    // Must be checked before CantCastFrom to avoid false matches on "can't cast spells".
    if nom_primitives::scan_contains(tp.lower, "can't cast spells during") {
        let who = strip_casting_prohibition_subject(tp.lower)
            .map(|(scope, _)| scope)
            .unwrap_or(ProhibitionScope::AllPlayers);
        let when = if nom_primitives::scan_contains(tp.lower, "during your turn") {
            CastingProhibitionCondition::DuringYourTurn
        } else if nom_primitives::scan_contains(tp.lower, "during combat") {
            CastingProhibitionCondition::DuringCombat
        } else {
            // Fallback: treat unknown conditions as combat-scoped
            CastingProhibitionCondition::DuringCombat
        };
        return Some(
            StaticDefinition::new(StaticMode::CantCastDuring { who, when })
                .description(text.to_string()),
        );
    }

    // --- CR 604.3: "Players can't cast spells from [zones]" ---
    // e.g., Grafdigger's Cage: "Players can't cast spells from graveyards or libraries."
    if nom_primitives::scan_contains(tp.lower, "can't cast spells from") {
        let zones = parse_zone_names_from_tp(&tp);
        let affected = if zones.is_empty() {
            push_warning(
                "target-fallback: no zones parsed for casting prohibition, defaulting to Any"
                    .to_string(),
            );
            TargetFilter::Any
        } else {
            TargetFilter::Typed(TypedFilter {
                properties: vec![FilterProp::InAnyZone { zones }],
                ..TypedFilter::default()
            })
        };
        return Some(
            StaticDefinition::new(StaticMode::CantCastFrom)
                .affected(affected)
                .description(text.to_string()),
        );
    }

    // --- CR 101.2: Blanket casting prohibition ("can't cast [type] spells") ---
    // e.g., Steel Golem: "You can't cast creature spells."
    // e.g., Hymn of the Wilds: "You can't cast instant or sorcery spells."
    // Excludes lines handled by PerTurnCastLimit ("can't cast more than"),
    // CantCastDuring ("can't cast spells during"), and CantCastFrom ("can't cast spells from").
    if let Some(def) = parse_cant_cast_type_spells(tp.lower, &text) {
        return Some(def);
    }

    // --- CR 101.2: Per-turn draw limit ("can't draw more than N card(s) each turn") ---
    // e.g., Spirit of the Labyrinth: "Each player can't draw more than one card each turn."
    // e.g., Narset, Parter of Veils: "Each opponent can't draw more than one card each turn."
    if let Some(def) = parse_per_turn_draw_limit(tp.lower, &text) {
        return Some(def);
    }

    // --- CR 101.2 / CR 121.3: Blanket draw prohibition ("can't draw cards") ---
    // e.g., Omen Machine: "Players can't draw cards."
    // e.g., Maralen of the Mornsong: "Players can't draw cards."
    if let Some(def) = parse_cant_draw_cards(tp.lower, &text) {
        return Some(def);
    }

    // --- "~ doesn't untap during your untap step [as long as / if condition]" ---
    // CR 502.3: Effects can keep permanents from untapping during the untap step.
    if nom_primitives::scan_contains(tp.lower, "doesn't untap during")
        || nom_primitives::scan_contains(tp.lower, "doesn\u{2019}t untap during")
    {
        // Check for trailing condition after the untap-step phrase
        let condition = extract_cant_untap_condition(tp.lower);
        let mut def = StaticDefinition::new(StaticMode::CantUntap)
            .affected(TargetFilter::SelfRef)
            .description(text.to_string());
        if let Some(cond) = condition {
            def.condition = Some(cond);
        }
        return Some(def);
    }

    // --- "You may look at the top card of your library any time." ---
    if nom_tag_tp(&tp, "you may look at the top card of your library").is_some() {
        return Some(
            StaticDefinition::new(StaticMode::MayLookAtTopOfLibrary)
                .affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                ))
                .description(text.to_string()),
        );
    }

    // NOTE: "enters with N counters" patterns are now handled by oracle_replacement.rs
    // as proper Moved replacement effects (paralleling the "enters tapped" pattern).

    // --- "{Ability} abilities you activate cost {N} less to activate" ---
    // CR 601.2f: Ability-type-specific cost reduction (e.g., Silver-Fur Master, Fluctuator).
    if nom_primitives::scan_contains(tp.lower, "abilities you activate cost")
        && nom_primitives::scan_contains(tp.lower, "less to activate")
    {
        // Extract keyword name and amount via nom combinators
        if let Some(((keyword, amount), _)) = nom_on_lower(tp.original, tp.lower, |i| {
            let (i, kw) = terminated(
                nom::bytes::complete::take_until(" abilities you activate cost "),
                tag(" abilities you activate cost "),
            )
            .parse(i)?;
            let (i, amt) =
                nom::sequence::delimited(tag("{"), nom_primitives::parse_number, tag("}"))
                    .parse(i)?;
            let (i, _) = tag(" less to activate").parse(i)?;
            Ok((i, (kw.to_string(), amt)))
        }) {
            return Some(
                StaticDefinition::new(StaticMode::ReduceAbilityCost { keyword, amount })
                    .affected(TargetFilter::Typed(
                        TypedFilter::card().controller(ControllerRef::You),
                    ))
                    .description(text.to_string()),
            );
        }
    }

    // --- "Activated abilities of [filter] cost {N} less to activate" ---
    // CR 602.1 + CR 601.2f: Generic activated ability cost reduction (e.g., Training Grounds).
    if let Some(rest) = nom_tag_lower(tp.lower, tp.lower, "activated abilities of ") {
        if let Ok((_, (filter_part, after_cost))) = nom_primitives::split_once_on(rest, " cost ") {
            if nom_primitives::scan_contains(after_cost, "less to activate") {
                let amount = nom_primitives::split_once_on(after_cost, " less")
                    .ok()
                    .and_then(|(_, (mana_str, _))| {
                        let stripped = mana_str.trim().trim_matches('{').trim_matches('}');
                        stripped.parse::<u32>().ok()
                    })
                    .unwrap_or(1);
                // Parse the filter between "of" and "cost" using parse_type_phrase
                let filter_text =
                    &tp.original["activated abilities of ".len()..][..filter_part.len()];
                let (affected, _rest) = parse_type_phrase(filter_text);
                return Some(
                    StaticDefinition::new(StaticMode::ReduceAbilityCost {
                        keyword: "activated".to_string(),
                        amount,
                    })
                    .affected(affected)
                    .description(text.to_string()),
                );
            }
        }
    }

    // --- CR 601.2f: Cost modification statics ---
    // Patterns: "[Type] spells [you/your opponents] cast cost {N} less/more to cast"
    // Also: "Noncreature spells cost {1} more to cast" (Thalia, no "you cast")
    if nom_primitives::scan_contains(tp.lower, "cost")
        && nom_primitives::scan_contains(tp.lower, "spell")
        && (nom_primitives::scan_contains(tp.lower, "less")
            || nom_primitives::scan_contains(tp.lower, "more"))
    {
        if let Some(def) = try_parse_cost_modification(&text, &lower) {
            return Some(def);
        }
    }

    // --- "must be blocked if able" (CR 509.1b) ---
    if nom_primitives::scan_contains(tp.lower, "must be blocked") {
        return Some(
            StaticDefinition::new(StaticMode::MustBeBlocked)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "can't gain life" (CR 119.7) ---
    if nom_primitives::scan_contains(tp.lower, "can't gain life") {
        let affected = parse_player_scope_filter(&tp);
        return Some(
            StaticDefinition::new(StaticMode::CantGainLife)
                .affected(affected)
                .description(text.to_string()),
        );
    }

    // --- "can't play lands" (CR 305.1) ---
    // CR 305.1: A player may play a land card from their hand during a main phase
    // of their turn when the stack is empty. Static effects can prohibit this.
    // Runtime enforcement lives via `player_has_static_other(state, pid, "CantPlayLand")`.
    if nom_primitives::scan_contains(tp.lower, "can't play lands")
        || nom_primitives::scan_contains(tp.lower, "cannot play lands")
    {
        let affected = parse_player_scope_filter(&tp);
        return Some(
            StaticDefinition::new(StaticMode::Other("CantPlayLand".to_string()))
                .affected(affected)
                .description(text.to_string()),
        );
    }

    // --- "can't win the game" / "can't lose the game" (CR 104.3a/b) ---
    if nom_primitives::scan_contains(tp.lower, "can't win the game") {
        let affected = parse_player_scope_filter(&tp);
        return Some(
            StaticDefinition::new(StaticMode::CantWinTheGame)
                .affected(affected)
                .description(text.to_string()),
        );
    }
    if nom_primitives::scan_contains(tp.lower, "can't lose the game")
        || nom_primitives::scan_contains(tp.lower, "don't lose the game")
    {
        let affected = parse_player_scope_filter(&tp);
        return Some(
            StaticDefinition::new(StaticMode::CantLoseTheGame)
                .affected(affected)
                .description(text.to_string()),
        );
    }

    // --- "as though it/they had flash" (CR 702.8a) ---
    if nom_primitives::scan_contains(tp.lower, "as though it had flash")
        || nom_primitives::scan_contains(tp.lower, "as though they had flash")
    {
        return Some(
            StaticDefinition::new(StaticMode::CastWithFlash).description(text.to_string()),
        );
    }

    // --- "[Type] spells you cast [from zone] have [keyword]" (CR 702.51a) ---
    // E.g., "Creature spells you cast have convoke."
    // Also: "Creature cards you own that aren't on the battlefield have flash."
    if let Some(def) = parse_spells_have_keyword(&tp, &text) {
        return Some(def);
    }

    // --- "can block an additional creature" / "can block any number" (CR 509.1b) ---
    if nom_primitives::scan_contains(tp.lower, "can block any number") {
        return Some(
            StaticDefinition::new(StaticMode::ExtraBlockers { count: None })
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }
    if nom_primitives::scan_contains(tp.lower, "can block an additional") {
        return Some(
            StaticDefinition::new(StaticMode::ExtraBlockers { count: Some(1) })
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "play an additional land" / "play two additional lands" ---
    // CR 305.2: Determine the count at parse time and carry it as typed data.
    if nom_primitives::scan_contains(tp.lower, "play two additional lands") {
        return Some(
            StaticDefinition::new(StaticMode::AdditionalLandDrop { count: 2 })
                .description(text.to_string()),
        );
    }
    if nom_primitives::scan_contains(tp.lower, "play an additional land") {
        return Some(
            StaticDefinition::new(StaticMode::AdditionalLandDrop { count: 1 })
                .description(text.to_string()),
        );
    }

    // --- "As long as ..." (generic conditional static, no comma separator) ---
    if let Some(rest_tp) = nom_tag_tp(&tp, "as long as ") {
        let condition_text = rest_tp.original.trim_end_matches('.');
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .condition(StaticCondition::Unrecognized {
                    text: condition_text.to_string(),
                })
                .description(text.to_string()),
        );
    }

    // CR 603.2d: Trigger doubling — "triggers an additional time".
    //
    // Cause classification by phrasing:
    // - "attacking causes" — Isshin, Two Heavens as One (CreatureAttacking).
    // - "entering" / "enters the battlefield" / "enters" — Panharmonicon-class
    //   (EntersBattlefield). Panharmonicon itself names "artifact or creature
    //   entering", so both CoreTypes qualify; narrower wordings ("creature
    //   entering") collapse to [Creature] only.
    // - Otherwise (e.g. "If a triggered ability ... triggers, it triggers an
    //   additional time" — Roaming Throne, Strionic Resonator copies) use the
    //   unrestricted `Any` cause; the doubler's `affected` filter narrows
    //   which source's triggers qualify.
    if nom_primitives::scan_contains(tp.lower, "triggers an additional time") {
        let cause = if nom_primitives::scan_contains(tp.lower, "attacking causes") {
            TriggerCause::CreatureAttacking
        } else if nom_primitives::scan_contains(tp.lower, "dying causes") {
            TriggerCause::CreatureDying
        } else if nom_primitives::scan_contains(tp.lower, "entering")
            || nom_primitives::scan_contains(tp.lower, "enters the battlefield")
        {
            // CR 603.6a: The entering-permanent's type is named in the
            // qualifier. "artifact or creature entering" = both; a bare
            // "creature entering" or "permanent entering" narrows
            // accordingly.
            let mut core_types: Vec<CoreType> = Vec::new();
            if nom_primitives::scan_contains(tp.lower, "artifact") {
                core_types.push(CoreType::Artifact);
            }
            if nom_primitives::scan_contains(tp.lower, "creature") {
                core_types.push(CoreType::Creature);
            }
            if nom_primitives::scan_contains(tp.lower, "enchantment") {
                core_types.push(CoreType::Enchantment);
            }
            if nom_primitives::scan_contains(tp.lower, "land") {
                core_types.push(CoreType::Land);
            }
            if nom_primitives::scan_contains(tp.lower, "planeswalker") {
                core_types.push(CoreType::Planeswalker);
            }
            // Empty core_types (e.g. "a permanent entering") means any type.
            TriggerCause::EntersBattlefield { core_types }
        } else {
            TriggerCause::Any
        };
        return Some(
            StaticDefinition::new(StaticMode::DoubleTriggers { cause })
                .description(text.to_string()),
        );
    }

    None
}

/// Like `parse_static_line`, but returns all `StaticDefinition`s produced by a line.
///
/// Most lines produce zero or one static. Compound forms like
/// "All creatures attack or block each combat if able" produce two
/// (one `MustAttack`, one `MustBlock`). Callers that push into a `Vec`
/// should prefer this over `parse_static_line` to avoid silently dropping modes.
pub fn parse_static_line_multi(text: &str) -> Vec<StaticDefinition> {
    let mut defs = parse_static_line_multi_inner(text);
    for def in defs.iter_mut() {
        populate_active_zones_from_condition(def);
    }
    defs
}

fn parse_static_line_multi_inner(text: &str) -> Vec<StaticDefinition> {
    let stripped = strip_reminder_text(text);
    let lower = stripped.to_lowercase();

    if let Some(defs) = parse_compound_subject_rule_static(&stripped, &lower) {
        return defs;
    }

    // Check compound must-attack/block first — may return multiple.
    if let Some(defs) = try_parse_scoped_must_attack_block(&lower, &stripped) {
        return defs;
    }

    // CR 701.3 + CR 702.5 + CR 702.6: Compound "can't be equipped or enchanted"
    // produces two static definitions (CantBeEquipped + CantBeEnchanted). Fortifications
    // are intentionally excluded by the Oracle wording, so CantBeAttached is NOT emitted.
    if nom_primitives::scan_contains(&lower, "can't be equipped or enchanted") {
        return vec![
            StaticDefinition::new(StaticMode::Other("CantBeEquipped".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(stripped.to_string()),
            StaticDefinition::new(StaticMode::Other("CantBeEnchanted".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(stripped.to_string()),
        ];
    }

    // CR 119.7 + CR 119.8: "[scope] life total can't change" — bidirectional
    // life-lock. Emits both CantGainLife and CantLoseLife with the same
    // player-scope filter (Platinum Emperion: "Your life total can't change.";
    // also covers "Players' life totals can't change", "Your opponents' life
    // totals can't change", etc.).
    if nom_primitives::scan_contains(&lower, "life total can't change")
        || nom_primitives::scan_contains(&lower, "life totals can't change")
        || nom_primitives::scan_contains(&lower, "life total cannot change")
        || nom_primitives::scan_contains(&lower, "life totals cannot change")
    {
        let affected = parse_life_total_scope_filter(&lower);
        return vec![
            StaticDefinition::new(StaticMode::CantGainLife)
                .affected(affected.clone())
                .description(stripped.to_string()),
            StaticDefinition::new(StaticMode::CantLoseLife)
                .affected(affected)
                .description(stripped.to_string()),
        ];
    }

    // CR 602.5: Compound "can't attack/block" + "activated abilities can't be activated"
    // produces two static definitions (e.g., CantAttackOrBlock + CantBeActivated).
    if nom_primitives::scan_contains(&lower, "activated abilities can't be activated")
        && (nom_primitives::scan_contains(&lower, "can't attack")
            || nom_primitives::scan_contains(&lower, "can't block"))
    {
        let mut defs = Vec::new();
        let combat_mode = if nom_primitives::scan_contains(&lower, "can't attack or block") {
            StaticMode::CantAttackOrBlock
        } else if nom_primitives::scan_contains(&lower, "can't attack") {
            StaticMode::CantAttack
        } else {
            StaticMode::CantBlock
        };
        defs.push(
            StaticDefinition::new(combat_mode)
                .affected(TargetFilter::SelfRef)
                .description(stripped.to_string()),
        );
        defs.push(
            // CR 602.5 + CR 603.2a: Self-reference case — the affected permanent's
            // own activated abilities can't be activated by anyone.
            StaticDefinition::new(StaticMode::CantBeActivated {
                who: ProhibitionScope::AllPlayers,
                source_filter: TargetFilter::SelfRef,
                // CR 605.1a: Self-ref form has no "unless they're..." suffix.
                exemption: ActivationExemption::None,
            })
            .affected(TargetFilter::SelfRef)
            .description(stripped.to_string()),
        );
        return defs;
    }

    // CR 702.3b + CR 611.3a + CR 613: Cross-mode conjunctions of the form
    // "<predicate_1> and can attack as though <pronoun> didn't have defender
    // [as long as <cond>]" combine a Continuous modification (keyword grant,
    // +N/+M, assigns-damage-from-toughness) with a `CanAttackWithDefender`
    // permission. A single `StaticDefinition` cannot carry both static modes,
    // so decompose: strip the conjunction phrase, re-parse the remainder, then
    // emit a companion `CanAttackWithDefender` inheriting `affected` + `condition`.
    // Corpus: Arcades, the Strategist; Colossus of Akros; Spire Serpent.
    if let Some(defs) = try_split_and_can_attack_despite_defender(&stripped) {
        return defs;
    }

    // Fall back to the single-return parser.
    parse_static_line(text).into_iter().collect()
}

fn parse_compound_subject_rule_static(text: &str, lower: &str) -> Option<Vec<StaticDefinition>> {
    let (subject_lower, left, after_left) =
        nom_primitives::scan_preceded(lower, parse_rule_static_predicate_nom)?;
    let (rest, right) = preceded(
        tag::<_, _, VerboseError<&str>>(" and "),
        parse_rule_static_predicate_nom,
    )
    .parse(after_left)
    .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    let subject = text[..subject_lower.len()].trim();
    let affected = parse_rule_static_subject_filter(subject)?;
    Some(vec![
        lower_rule_static(left, affected.clone(), text),
        lower_rule_static(right, affected, text),
    ])
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
fn try_split_and_can_attack_despite_defender(text: &str) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = nom_language::error::VerboseError<&'a str>;
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

/// CR 105.2c / CR 205.4a: Parse property-based creature descriptors that are not subtypes.
/// Handles "colorless", "multicolored", "snow", and "snow and [Subtype]" patterns.
/// Returns a fully constructed `TargetFilter` with the appropriate properties.
fn parse_property_descriptor(
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
        props.push(FilterProp::Colorless);
        return Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(props),
        ));
    }

    // CR 105.2: "multicolored creatures" — two or more colors
    if desc_lower == "multicolored" {
        props.push(FilterProp::Multicolored);
        return Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(props),
        ));
    }

    // CR 205.4a: "snow and [Subtype]" — supertype + subtype compound
    if let Some(rest) = desc_lower.strip_prefix("snow and ") {
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
fn try_parse_compound_subtypes(
    descriptor: &str,
    extra_props: &[FilterProp],
    is_other: bool,
) -> Option<TargetFilter> {
    let (left, right) = descriptor
        .split_once(" and ")
        .or_else(|| descriptor.split_once(" or "))?;
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

/// Try to parse "[Subtype] creatures you control get/have ..." patterns.
/// `text` is the original-case text starting at the subtype word.
/// `lower` is the lowercased version of `text`.
/// `is_other` indicates whether this was preceded by "Other ".
fn parse_typed_you_control(text: &str, lower: &str, is_other: bool) -> Option<StaticDefinition> {
    let tp = TextPair::new(text, lower);
    // Try "X creatures you control get/have" first
    if let Some(creatures_pos) = tp.find(" creatures you control ") {
        let (before, after) = tp.split_at(creatures_pos);
        let descriptor = before.original.trim();
        if !descriptor.is_empty() {
            let after_prefix = &after.original[" creatures you control ".len()..];
            let full_subject = tp.original[..creatures_pos + " creatures you control".len()].trim();
            // CR 509.1h: Strip combat-status prefixes ("Attacking Ninja" → props=[Attacking], subtype="Ninja")
            let mut extra_props = Vec::new();
            let mut desc_remaining = descriptor;
            let mut desc_lower = descriptor.to_lowercase();
            while let Some((prop, consumed)) = parse_combat_status_prefix(&desc_lower) {
                extra_props.push(prop);
                desc_remaining = desc_remaining[consumed..].trim_start();
                desc_lower = desc_remaining.to_lowercase();
            }
            // CR 105.2c / CR 205.4a: Property-descriptor recognition for colorless,
            // multicolored, and snow creatures before subtype parsing.
            if let Some(prop_filter) =
                parse_property_descriptor(&desc_lower, desc_remaining, &extra_props, is_other)
            {
                let (prop_filter, after_prefix) =
                    if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                        (add_property(prop_filter, prop), rest)
                    } else {
                        (prop_filter, after_prefix)
                    };
                return parse_continuous_gets_has(after_prefix, prop_filter, text);
            }
            // CR 205.3m: Try compound subtypes first ("Ninja and Rogue", "Elf or Warrior")
            // The helper bakes in extra_props and is_other, so skip add_another_filter below.
            if let Some(compound_filter) =
                try_parse_compound_subtypes(desc_remaining, &extra_props, is_other)
            {
                // CR 613.7: Check for counter condition before returning
                let (compound_filter, after_prefix) =
                    if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                        (add_property(compound_filter, prop), rest)
                    } else {
                        (compound_filter, after_prefix)
                    };
                return parse_continuous_gets_has(after_prefix, compound_filter, text);
            }
            let typed_filter = if extra_props.is_empty() {
                // No combat-status prefix — use original dispatch path
                if let Some(filter) = parse_modified_creature_subject_filter(full_subject) {
                    filter
                } else if let Some(color) = parse_named_color(descriptor) {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::HasColor { color }]),
                    )
                // CR 205.2a: "artifact creatures" = Creature + Artifact conjunctive type filter
                } else if let Some(core_tf) =
                    try_parse_core_type_descriptor(&descriptor.to_lowercase())
                {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .with_type(core_tf)
                            .controller(ControllerRef::You),
                    )
                // CR 903.3d: "Commander creatures you control" — bare "Commander"
                // descriptor on a creature subject is the commander designation,
                // not an MTG subtype. Constrain to creatures + IsCommander.
                } else if descriptor.eq_ignore_ascii_case("commander") {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::IsCommander]),
                    )
                } else if is_capitalized_words(descriptor) {
                    TargetFilter::Typed(
                        typed_filter_for_subtype(descriptor).controller(ControllerRef::You),
                    )
                } else {
                    return None;
                }
            } else if desc_remaining.eq_ignore_ascii_case("commander") {
                // CR 903.3d: Combat-status prefix + "Commander creature" — same
                // designation guard as the no-prefix branch above.
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties({
                            let mut p = extra_props.clone();
                            p.push(FilterProp::IsCommander);
                            p
                        }),
                )
            } else if is_capitalized_words(desc_remaining) {
                // Combat-status prefix found + remaining is a subtype
                TargetFilter::Typed(
                    typed_filter_for_subtype(desc_remaining)
                        .controller(ControllerRef::You)
                        .properties(extra_props),
                )
            } else {
                return None;
            };
            // CR 613.7: Check for "with [counter] on it/them" condition between
            // "you control" and the predicate (e.g., "Elf creatures you control
            // with a +1/+1 counter on it has trample").
            let (typed_filter, after_prefix) =
                if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                    (add_property(typed_filter, prop), rest)
                } else {
                    (typed_filter, after_prefix)
                };
            let typed_filter = if is_other {
                add_another_filter(typed_filter)
            } else {
                typed_filter
            };
            return parse_continuous_gets_has(after_prefix, typed_filter, text);
        }
    }

    // Try "Xs you control get/have" (e.g. "Zombies you control get +1/+1")
    if let Some(yc_pos) = tp.find(" you control ") {
        let (before, after) = tp.split_at(yc_pos);
        let descriptor = before.original.trim();
        if !descriptor.is_empty() {
            let after_prefix = &after.original[" you control ".len()..];
            let full_subject = tp.original[..yc_pos + " you control".len()].trim();
            // CR 509.1h: Strip combat-status prefixes
            let mut extra_props = Vec::new();
            let mut desc_remaining = descriptor;
            let mut desc_lower = descriptor.to_lowercase();
            while let Some((prop, consumed)) = parse_combat_status_prefix(&desc_lower) {
                extra_props.push(prop);
                desc_remaining = desc_remaining[consumed..].trim_start();
                desc_lower = desc_remaining.to_lowercase();
            }
            // CR 205.3m: Try compound subtypes first ("Ninja and Rogue", "Elf or Warrior")
            if let Some(compound_filter) =
                try_parse_compound_subtypes(desc_remaining, &extra_props, is_other)
            {
                // CR 613.7: Check for counter condition before returning
                let (compound_filter, after_prefix) =
                    if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                        (add_property(compound_filter, prop), rest)
                    } else {
                        (compound_filter, after_prefix)
                    };
                return parse_continuous_gets_has(after_prefix, compound_filter, text);
            }
            let typed_filter = if extra_props.is_empty() {
                if let Some(filter) = parse_modified_creature_subject_filter(full_subject) {
                    filter
                } else if let Some(color) = parse_named_color(descriptor) {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::HasColor { color }]),
                    )
                // CR 205.2a: "Artifacts you control" — standalone core type as permanent filter
                } else if let Some(core_tf) =
                    try_parse_core_type_descriptor(&descriptor.to_lowercase())
                {
                    TargetFilter::Typed(TypedFilter::new(core_tf).controller(ControllerRef::You))
                // CR 903.3d: "Commander(s) you control" — commander designation is
                // NOT an MTG subtype (CR 903.3); route to FilterProp::IsCommander
                // before the capitalized-subtype fallback would synthesize a
                // bogus `Subtype("Commander")`.
                } else if matches!(
                    descriptor.to_lowercase().as_str(),
                    "commander" | "commanders"
                ) {
                    TargetFilter::Typed(
                        TypedFilter::permanent()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::IsCommander]),
                    )
                } else if is_capitalized_words(descriptor) {
                    // CR 205.3m: Normalize plural subtypes to canonical singular form
                    let subtype_name = parse_subtype(descriptor)
                        .map(|(canonical, _)| canonical)
                        .unwrap_or_else(|| descriptor.trim_end_matches('s').to_string());
                    TargetFilter::Typed(
                        typed_filter_for_subtype(&subtype_name).controller(ControllerRef::You),
                    )
                } else {
                    return None;
                }
            } else if is_capitalized_words(desc_remaining) {
                // CR 205.3m: Normalize plural subtypes to canonical singular form
                let subtype_name = parse_subtype(desc_remaining)
                    .map(|(canonical, _)| canonical)
                    .unwrap_or_else(|| desc_remaining.trim_end_matches('s').to_string());
                TargetFilter::Typed(
                    typed_filter_for_subtype(&subtype_name)
                        .controller(ControllerRef::You)
                        .properties(extra_props),
                )
            } else {
                return None;
            };
            // CR 613.7: Check for "with [counter] on it/them" condition
            let (typed_filter, after_prefix) =
                if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                    (add_property(typed_filter, prop), rest)
                } else {
                    (typed_filter, after_prefix)
                };
            let typed_filter = if is_other {
                add_another_filter(typed_filter)
            } else {
                typed_filter
            };
            return parse_continuous_gets_has(after_prefix, typed_filter, text);
        }
    }

    None
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
fn parse_assigns_damage_from_toughness(lower: &str, text: &str) -> Option<StaticDefinition> {
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

/// CR 510.1c: Parse "you may have this creature assign its combat damage as though it
/// weren't blocked" self-referential static.
fn parse_assign_damage_as_though_unblocked(lower: &str, text: &str) -> Option<StaticDefinition> {
    type VE<'a> = nom_language::error::VerboseError<&'a str>;

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
fn parse_attached_creature_assign_damage_as_though_unblocked(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    type VE<'a> = nom_language::error::VerboseError<&'a str>;

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

fn parse_subject_rule_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let (affected, predicate_text) = strip_rule_static_subject(tp.original, tp.lower)?;
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

fn parse_subject_continuous_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Additive-type clauses do not use any of the get/has/have/lose verbs that
    // `find_continuous_predicate_start` scans for. They split on "are"/"is"
    // instead and may embed a " have " inside a granted-ability quote that
    // would otherwise confuse the verb scanner. Route them to their own
    // extractor before falling through to the general predicate parser.
    if let Some(def) = parse_subject_additive_type_static(text) {
        return Some(def);
    }

    let subject_end = find_continuous_predicate_start(tp.lower)?;
    let subject = tp.original[..subject_end].trim();
    let predicate = tp.original[subject_end + 1..].trim();
    if parse_rule_static_predicate(predicate).is_some() {
        return None;
    }
    let affected = parse_continuous_subject_filter(subject)?;

    // CR 613.4c / CR 611.3a: Route "for each" and "as long as" predicates through
    // parse_continuous_gets_has which handles dynamic P/T and condition splitting.
    let pred_lower = predicate.to_lowercase();
    if nom_primitives::scan_contains(&pred_lower, "for each")
        || nom_primitives::scan_contains(&pred_lower, "as long as")
    {
        return parse_continuous_gets_has(predicate, affected, text);
    }

    // CR 604.1: Strip suffix turn conditions from predicate —
    // "has first strike during your turn" → "has first strike" + DuringYourTurn
    let (effective_predicate, suffix_condition) = strip_suffix_turn_condition(&pred_lower);

    let modifications = parse_continuous_modifications(&effective_predicate);
    if !modifications.is_empty() {
        let mut def = StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(text.to_string());
        if let Some(cond) = suffix_condition {
            def.condition = Some(cond);
        }
        return Some(def);
    }

    None
}

/// CR 205.1 / CR 205.3a: Top-level dispatcher for additive-type-only statics
/// whose predicate begins with `"are"` / `"is"` — e.g.
/// `"Other creatures are Food artifacts in addition to their other types and
/// have \"…\""`. These do not contain a get/has/have/lose verb at the
/// grammatical top level, so `parse_subject_continuous_static`'s main path
/// would mis-split on a " have " buried inside the granted-ability quote.
///
/// Compound predicates (P/T + additive type, e.g. Kudo:
/// `"have base power and toughness 2/2 and are Bears in addition to their
/// other types"`) go through the main path instead and reach the same
/// extractor via `parse_continuous_modifications`.
fn parse_subject_additive_type_static(text: &str) -> Option<StaticDefinition> {
    type VE<'a> = nom_language::error::VerboseError<&'a str>;
    let lower = text.to_lowercase();
    let (subject_lower, predicate_lower) = nom_primitives::scan_split_at_phrase(&lower, |i| {
        alt((tag::<_, _, VE>("are "), tag::<_, _, VE>("is "))).parse(i)
    })?;
    let subject = text[..subject_lower.len()].trim();
    let predicate = &text[text.len() - predicate_lower.len()..];
    let affected = parse_continuous_subject_filter(subject)?;
    let modifications = parse_additive_type_clause_modifications(predicate)?;
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(text.to_string()),
    )
}

/// CR 205.1 / CR 205.3a: Extract additive-type modifications from a predicate
/// like `"are Food artifacts in addition to their other types"` or its
/// compound/granted-ability variants. Used both as the body of
/// `parse_subject_additive_type_static` (pure additive predicates) and as a
/// fallback inside `parse_continuous_modifications` (compound predicates
/// whose leading `have …` clause is already consumed upstream).
///
/// Returns `None` when:
/// * the clause does not contain an additive-type phrase,
/// * the type-word region is a placeholder handled by another specialized
///   extractor (`every basic land type`, `the chosen type`), or
/// * no valid type or subtype was recognized (unknown words are dropped —
///   the curated `SUBTYPES` list is authoritative).
fn parse_additive_type_clause_modifications(text: &str) -> Option<Vec<ContinuousModification>> {
    type VE<'a> = nom_language::error::VerboseError<&'a str>;

    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower)
        .trim_start()
        .trim_end()
        .trim_end_matches('.');
    let (_, clause_lower) = nom_primitives::scan_split_at_phrase(tp.lower, |i| {
        alt((
            tag::<_, _, VE>("are "),
            tag::<_, _, VE>("is "),
            tag::<_, _, VE>("and are "),
            tag::<_, _, VE>("and is "),
        ))
        .parse(i)
    })?;
    let clause_original = &tp.original[tp.original.len() - clause_lower.len()..];
    let (after_verb_lower, _) = alt((
        tag::<_, _, VE>("are "),
        tag::<_, _, VE>("is "),
        tag::<_, _, VE>("and are "),
        tag::<_, _, VE>("and is "),
    ))
    .parse(clause_lower)
    .ok()?;
    let after_verb_original = &clause_original[clause_original.len() - after_verb_lower.len()..];
    let (after_suffix_lower, type_words_lower) = terminated(
        take_until::<_, _, VE>(" in addition to "),
        alt((
            tag::<_, _, VE>(" in addition to its other types"),
            tag::<_, _, VE>(" in addition to its other land types"),
            tag::<_, _, VE>(" in addition to their other types"),
            tag::<_, _, VE>(" in addition to their other land types"),
        )),
    )
    .parse(after_verb_lower)
    .ok()?;
    let type_words = &after_verb_original[..type_words_lower.len()];
    let normalized_type_words = type_words_lower.trim();
    // Placeholders owned by other specialized extractors (basic-land-type copies,
    // chosen-type statics). Let those branches produce the correct modification.
    if matches!(
        normalized_type_words,
        "every basic land type" | "the chosen type"
    ) {
        return None;
    }
    let granted_lower = opt(preceded(
        alt((tag::<_, _, VE>(" and have "), tag::<_, _, VE>(" and has "))),
        rest::<_, VE>,
    ))
    .parse(after_suffix_lower)
    .ok()?
    .1;
    let granted_modifications = granted_lower
        .map(|granted| &clause_original[clause_original.len() - granted.len()..])
        .map(parse_quoted_ability_modifications)
        .unwrap_or_default();

    let mut modifications = Vec::new();
    for raw_word in type_words.split_whitespace() {
        let word = raw_word.trim_matches(|c: char| c == ',' || c == '.');
        if word.is_empty() {
            continue;
        }
        let lower_word = word.to_lowercase();
        if let Some(core_type) = core_type_from_additive_word(lower_word.as_str()) {
            modifications.push(ContinuousModification::AddType { core_type });
            continue;
        }
        // CR 205.3a: Only canonical subtypes from the curated list may be
        // added. Unrecognized words are silently dropped rather than
        // fabricated — a heuristic capitalize-and-strip-s would synthesize
        // non-MTG subtypes from noise tokens.
        if let Some((canonical, _)) = parse_subtype(lower_word.as_str()) {
            modifications.push(ContinuousModification::AddSubtype { subtype: canonical });
        }
    }

    modifications.extend(granted_modifications);
    (!modifications.is_empty()).then_some(modifications)
}

/// CR 205.1: Map a bare type word (singular or plural) to its `CoreType`.
fn core_type_from_additive_word(word: &str) -> Option<CoreType> {
    match word {
        "artifact" | "artifacts" => Some(CoreType::Artifact),
        "creature" | "creatures" => Some(CoreType::Creature),
        "enchantment" | "enchantments" => Some(CoreType::Enchantment),
        "land" | "lands" => Some(CoreType::Land),
        "planeswalker" | "planeswalkers" => Some(CoreType::Planeswalker),
        "battle" | "battles" => Some(CoreType::Battle),
        _ => None,
    }
}

/// Parse compound condition + animation pattern:
/// "During your turn, as long as ~ has one or more [counter] counters on [pronoun],
///  [pronoun]'s a [P/T] [types] and has [keyword]"
///
/// Produces `StaticCondition::And { DuringYourTurn, HasCounters { .. } }` with
/// `ContinuousModification` list for type/subtype/P-T/keyword changes.
fn parse_compound_turn_counter_animation(lower: &str, text: &str) -> Option<StaticDefinition> {
    // Strip "during your turn, " prefix via nom tag
    let (rest, _) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("during your turn, ")(lower).ok()?;

    // Strip "as long as " prefix from the remainder
    let (rest, _) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("as long as ")(rest).ok()?;

    // Parse "~ has one or more [type] counters on [pronoun], "
    let (rest, _) = tag::<_, _, nom_language::error::VerboseError<&str>>("~ has ")(rest).ok()?;

    // Parse the counter count requirement: "one or more" / "N or more" / "a"
    let (minimum, rest) = parse_counter_minimum(rest)?;

    // Parse "[type] counters on [pronoun], "
    let rest = rest.trim_start();
    let counters_pos = rest.find(" counter")?;
    let counter_type_text = rest[..counters_pos].trim();
    // CR 122.1: bare "a counter on it" with no type word → Any; typed "a [type]
    // counter on it" → OfType(ct). Routes through the shared mapping in
    // `types::counter::parse_counter_type` to keep the canonical set in one place.
    let counters = if counter_type_text.is_empty() {
        CounterMatch::Any
    } else {
        CounterMatch::OfType(parse_counter_type(counter_type_text))
    };

    // Skip past "counters on [pronoun], " to get the modification text
    let rest = &rest[counters_pos..];
    let modification_text = strip_after(rest, ", ")?.trim();

    let modifications = parse_animation_modifications(modification_text.trim_end_matches('.'));
    if modifications.is_empty() {
        return None;
    }

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .condition(StaticCondition::And {
                conditions: vec![
                    StaticCondition::DuringYourTurn,
                    StaticCondition::HasCounters {
                        counters,
                        minimum,
                        maximum: None,
                    },
                ],
            })
            .modifications(modifications)
            .description(text.to_string()),
    )
}

/// Parse "one or more" / "N or more" / "a" into a counter minimum count.
/// Returns (minimum, remaining text).
fn parse_counter_minimum(text: &str) -> Option<(u32, &str)> {
    if let Some(rest) = nom_tag_lower(text, text, "one or more ") {
        return Some((1, rest));
    }
    if let Some(rest) = nom_tag_lower(text, text, "a ") {
        return Some((1, rest));
    }
    // "N or more" pattern
    if let Some((n, rest)) = parse_number(text) {
        let rest = rest.trim_start();
        if let Some(rest) = nom_tag_lower(rest, rest, "or more ") {
            return Some((n, rest));
        }
    }
    None
}

/// Parse "[pronoun]'s a [P/T] [types] and has [keyword]" into modifications.
///
/// Handles patterns like:
/// - "he's a 3/4 ninja creature and has hexproof"
/// - "it's a 3/4 ninja creature with hexproof"
fn parse_animation_modifications(text: &str) -> Vec<ContinuousModification> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let mut modifications = Vec::new();

    // Strip pronoun prefix via nom tag: "he's a", "she's a", "it's a", "~'s a"
    let body = nom_tag_lower(tp.original, tp.lower, "he's a ")
        .or_else(|| nom_tag_lower(tp.original, tp.lower, "she's a "))
        .or_else(|| nom_tag_lower(tp.original, tp.lower, "it's a "))
        .or_else(|| nom_tag_lower(tp.original, tp.lower, "~'s a "));

    let body = match body {
        Some(b) => b.trim_start(),
        None => return modifications,
    };

    // Split on " and has " or " with " to separate type/PT from keywords
    let body_lower = body.to_lowercase();
    let (type_pt_part, keyword_part) = if let Some(pos) = body_lower.find(" and has ") {
        (&body[..pos], Some(&body[pos + 9..]))
    } else if let Some(pos) = body_lower.find(" with ") {
        (&body[..pos], Some(&body[pos + 6..]))
    } else {
        (body, None)
    };

    // Parse P/T from the beginning: "3/4 ninja creature"
    let remaining = if let Some((p, t)) = parse_pt_mod(type_pt_part) {
        modifications.push(ContinuousModification::SetPower { value: p });
        modifications.push(ContinuousModification::SetToughness { value: t });
        // Skip past the P/T value
        let slash = type_pt_part.find('/').unwrap();
        let rest = &type_pt_part[slash + 1..];
        let pt_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        rest[pt_end..].trim()
    } else {
        type_pt_part
    };

    // Parse types and subtypes from remaining: "ninja creature", "human ninja creature"
    for word in remaining.split_whitespace() {
        let word = word.trim_end_matches('.').trim_end_matches(',');
        if word.is_empty() {
            continue;
        }
        use std::str::FromStr;
        let capitalized = format!("{}{}", word[..1].to_uppercase(), &word[1..]);
        if let Ok(core_type) = crate::types::card_type::CoreType::from_str(&capitalized) {
            modifications.push(ContinuousModification::AddType { core_type });
        } else {
            modifications.push(ContinuousModification::AddSubtype {
                subtype: capitalized,
            });
        }
    }

    // Parse keywords from keyword part
    if let Some(kw_text) = keyword_part {
        for part in split_keyword_list(kw_text.trim().trim_end_matches('.')) {
            if let Some(kw) = map_keyword(part.trim().trim_end_matches('.')) {
                modifications.push(ContinuousModification::AddKeyword { keyword: kw });
            }
        }
    }

    modifications
}

fn parse_conditional_static(text: &str) -> Option<StaticDefinition> {
    let conditional = text.strip_prefix("As long as ")?;
    let (condition_text, remainder) = conditional.split_once(", ")?;

    let condition =
        parse_static_condition(condition_text).unwrap_or(StaticCondition::Unrecognized {
            text: condition_text.to_string(),
        });

    let mut def = parse_static_line(remainder.trim())?;
    if def.condition.is_some() {
        return None;
    }
    def.condition = Some(condition);
    def.description = Some(text.to_string());
    Some(def)
}

/// Parse a condition clause (the text between "As long as" and the comma).
///
/// Returns a typed `StaticCondition` for known patterns, or `None` if the
/// condition text is not recognized. Callers may fall back to `Unrecognized`.
///
/// Try splitting a condition on " and " into compound `StaticCondition::And`.
/// Only succeeds when BOTH halves parse as valid conditions — prevents false splits
/// on noun phrases like "artifacts and creatures".
fn try_split_compound_and(text: &str) -> Option<StaticCondition> {
    let lower = text.to_lowercase();
    // Find " and " boundaries — try each occurrence in case the first is a noun conjunction.
    let mut search_from = 0;
    while let Some(pos) = lower[search_from..].find(" and ") {
        let abs_pos = search_from + pos;
        let left = &text[..abs_pos];
        let right = &text[abs_pos + 5..]; // " and " is 5 bytes
        if let (Some(lhs), Some(rhs)) =
            (parse_static_condition(left), parse_static_condition(right))
        {
            return Some(StaticCondition::And {
                conditions: vec![lhs, rhs],
            });
        }
        search_from = abs_pos + 5;
    }
    None
}

/// Supported patterns:
/// - "you have at least N life more than your starting life total" → LifeMoreThanStartingBy
/// - "your devotion to [colors] is less than N" → DevotionGE (with inverted threshold)
/// - "it's your turn" → DuringYourTurn
/// - "you control a/an [type]" → IsPresent with filter
fn parse_static_condition(text: &str) -> Option<StaticCondition> {
    let text = text.trim().trim_end_matches('.');
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Delegate to shared nom condition combinator (prefix already stripped by callers).
    // Callers like parse_conditional_static strip "As long as " before calling us,
    // so we use parse_inner_condition (no prefix required), not parse_condition.
    if let Ok((rest, condition)) = nom_condition::parse_inner_condition(&lower) {
        if rest.trim().is_empty() {
            return Some(condition);
        }
    }

    // Compound " and " splitting: try splitting on " and ", parse both halves recursively.
    // Only succeeds if BOTH halves parse independently — avoids false splits on
    // noun phrases like "artifacts and creatures".
    if let Some(condition) = try_split_compound_and(text) {
        return Some(condition);
    }

    // "you have at least N life more than your starting life total"
    if let Some(amount_text) = nom_tag_lower(tp.lower, tp.lower, "you have at least ")
        .and_then(|s| s.strip_suffix(" life more than your starting life total"))
    {
        let (amount, rest) = parse_number(amount_text)?;
        if rest.trim().is_empty() {
            return Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeAboveStarting,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed {
                    value: amount as i32,
                },
            });
        }
    }

    if tp.lower == "you have max speed" || tp.lower == "have max speed" {
        return Some(StaticCondition::HasMaxSpeed);
    }
    if tp.lower == "you don't have max speed" || tp.lower == "don't have max speed" {
        return Some(StaticCondition::Not {
            condition: Box::new(StaticCondition::HasMaxSpeed),
        });
    }
    if let Some(speed_text) = nom_tag_lower(tp.lower, tp.lower, "your speed is ") {
        if let Some(number_text) = speed_text.strip_suffix(" or higher") {
            if let Some((threshold, remainder)) = parse_number(number_text) {
                if remainder.trim().is_empty() {
                    return Some(StaticCondition::SpeedGE {
                        threshold: u8::try_from(threshold).ok()?,
                    });
                }
            }
        }
    }

    // "your devotion to [color(s)] is less than N" (Theros gods)
    if let Some(condition) = parse_devotion_condition(tp.lower) {
        return Some(condition);
    }

    // "the number of [quantity] is [comparator] [quantity]"
    if let Some(condition) = parse_quantity_comparison(tp.lower) {
        return Some(condition);
    }

    // "the chosen color is [color]"
    if let Some(color_name) = nom_tag_lower(tp.lower, tp.lower, "the chosen color is ") {
        let trimmed = color_name.trim().trim_end_matches('.');
        if let Ok((rest, color)) = nom_primitives::parse_color.parse(trimmed) {
            if rest.is_empty() {
                return Some(StaticCondition::ChosenColorIs { color });
            }
        }
    }

    None
}

fn parse_unless_static_condition(tp: &TextPair<'_>) -> Option<StaticCondition> {
    let (_, unless_text) = tp.split_around(" unless ")?;
    parse_static_condition(unless_text.original)
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
fn parse_combat_tax_static(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
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
    } = outcome;
    let mut def = StaticDefinition::new(mode)
        .affected(affected)
        .description(text.to_string());
    def.condition = Some(StaticCondition::UnlessPay {
        cost: base_cost,
        scaling,
    });
    Some(def)
}

fn parse_subject_combat_rule_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let (subject_lower, predicate, rest) =
        nom_primitives::scan_preceded(&lower, parse_combat_rule_static_predicate_nom)?;
    let (rest, _) = opt(tag::<_, _, VerboseError<&str>>(".")).parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    let subject = text[..subject_lower.len()].trim();
    let affected = parse_rule_static_subject_filter(subject)?;
    Some(lower_rule_static(predicate, affected, text))
}

/// Result of the combat-tax nom parse.
struct CombatTaxParse {
    mode: StaticMode,
    affected: TargetFilter,
    base_cost: ManaCost,
    scaling: crate::types::ability::UnlessPayScaling,
}

/// Subject axis of the combat-tax grammar.
#[derive(Debug, Clone, Copy)]
enum CombatTaxSubject {
    /// "Creatures [can't attack you]" — applies to opponents' creatures.
    Creatures,
    /// "Enchanted creature [can't attack]" — aura attached-to creature form (Brainwash).
    EnchantedCreature,
    /// CR 122.1: "Each creature with one or more counters on it [can't attack you]"
    /// — counter-gated subject form (Nils, Discipline Enforcer). Applies to every
    /// creature on the battlefield carrying at least one counter; pairs naturally
    /// with per-affected cost scaling driven by the attacker's counter count.
    EachCreatureWithCounters,
}

/// Nom 8.0 parser for the combat-tax body.
///
/// Grammar (case-insensitive, leading "creatures " already consumed by nom):
///   body      := subject restriction scope? " unless " payer mana_cost suffix?
///   subject   := "creatures "
///   restriction := "can't attack" | "can't block"
///   scope     := " you" | " you or planeswalkers you control"
///   payer     := "their controller pays " | "its controller pays "
///   suffix    := " for each of those creatures" dynamic_x?
///   dynamic_x := ", where x is the number of " <filter-phrase>
fn parse_combat_tax_body(input: &str) -> OracleResult<'_, CombatTaxParse> {
    use crate::types::ability::UnlessPayScaling;
    use nom_language::error::VerboseError;

    // Subject: either "Creatures " (opponents' creatures — the prison family),
    // "Enchanted creature " (aura form — Brainwash), or "Each creature with one
    // or more counters on it " (counter-gated form — Nils, Discipline Enforcer).
    // Each subject type drives the affected-filter shape independently.
    //
    // Order matters: the counter-gated form must be tried before the bare
    // "creatures " tag because the counter phrasing starts with "each" rather
    // than "creatures" and so does not conflict with the primary alt branch;
    // it is listed first for clarity of grammar.
    let (input, subject) = alt((
        value(
            CombatTaxSubject::EachCreatureWithCounters,
            tag_no_case::<_, _, VerboseError<&str>>(
                "each creature with one or more counters on it ",
            ),
        ),
        value(
            CombatTaxSubject::Creatures,
            tag_no_case::<_, _, VerboseError<&str>>("creatures "),
        ),
        value(
            CombatTaxSubject::EnchantedCreature,
            tag_no_case::<_, _, VerboseError<&str>>("enchanted creature "),
        ),
    ))
    .parse(input)?;

    let (input, is_attack) = alt((
        value(
            true,
            tag_no_case::<_, _, VerboseError<&str>>("can't attack"),
        ),
        value(
            false,
            tag_no_case::<_, _, VerboseError<&str>>("can't block"),
        ),
    ))
    .parse(input)?;

    // Optional attack scope: " you" or " you or planeswalkers you control".
    // For the block side the scope is implicit ("can't block" has no "you").
    let (input, _scope) = opt(alt((
        tag_no_case::<_, _, VerboseError<&str>>(" you or planeswalkers you control"),
        tag_no_case::<_, _, VerboseError<&str>>(" you"),
    )))
    .parse(input)?;

    let (input, _) = tag_no_case::<_, _, VerboseError<&str>>(" unless ").parse(input)?;
    let (input, _) = alt((
        tag_no_case::<_, _, VerboseError<&str>>("their controller pays "),
        tag_no_case::<_, _, VerboseError<&str>>("its controller pays "),
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
        tag_no_case::<_, _, VerboseError<&str>>(" for each of those creatures"),
        tag_no_case::<_, _, VerboseError<&str>>(
            " for each creature they control that's attacking you or a planeswalker you control",
        ),
        tag_no_case::<_, _, VerboseError<&str>>(
            " for each creature they control that's attacking you",
        ),
        tag_no_case::<_, _, VerboseError<&str>>(" for each attacking creature they control"),
    )))
    .parse(input)?;

    // Optional ", where X is the number of <filter>" — only valid when the base
    // cost carried an {X} shard. Used by Sphere of Safety.
    let (input, dynamic_qty) = opt(parse_dynamic_x_clause).parse(input)?;

    let mode = if is_attack {
        StaticMode::CantAttack
    } else {
        StaticMode::CantBlock
    };

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
        CombatTaxSubject::Creatures => TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: Some(ControllerRef::Opponent),
            properties: vec![],
        }),
        CombatTaxSubject::EnchantedCreature => TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![FilterProp::EnchantedBy],
        }),
        CombatTaxSubject::EachCreatureWithCounters => TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![FilterProp::HasAnyCounter],
        }),
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

    Ok((
        input,
        CombatTaxParse {
            mode,
            affected,
            base_cost,
            scaling,
        },
    ))
}

/// Parse ", where X is the number of <filter>" → `QuantityRef::ObjectCount {...}`.
/// Used by Sphere of Safety. Delegates to the shared `parse_quantity_ref`
/// which handles "the number of <filter>" as a single alternative.
///
/// CR 122.1: Also recognizes the untyped-counter anaphoric phrasing ", where X
/// is the number of counters on that creature" → `QuantityRef::AnyCountersOnTarget`.
/// The shared `parse_quantity_ref` rejects this because it requires a non-empty
/// counter-type prefix; Nils, Discipline Enforcer's text omits the counter type,
/// so the dedicated branch is tried first.
fn parse_dynamic_x_clause(input: &str) -> OracleResult<'_, QuantityRef> {
    use nom_language::error::VerboseError;

    let (input, _) = tag_no_case::<_, _, VerboseError<&str>>(", where x is ").parse(input)?;

    // CR 122.1: Untyped counter anaphor — consume the rest of the clause and
    // emit `AnyCountersOnTarget`. Accepted variants mirror the counter-on-target
    // anaphor family (no type prefix).
    if let Ok((_, _)) = alt((
        tag_no_case::<_, _, VerboseError<&str>>("the number of counters on that creature"),
        tag_no_case::<_, _, VerboseError<&str>>("the number of counters on that permanent"),
    ))
    .parse(input)
    {
        return Ok((
            "",
            QuantityRef::CountersOn {
                scope: ObjectScope::Target,
                counter_type: None,
            },
        ));
    }

    // Delegate to the shared quantity-ref combinator which is case-sensitive on
    // lowercase patterns ("the number of"). Normalize to lowercase for the
    // remaining phrase so the upstream combinators match.
    let lowered = input.to_lowercase();
    let (_, quantity) =
        super::oracle_nom::quantity::parse_quantity_ref(&lowered).map_err(|e| match e {
            nom::Err::Error(_) | nom::Err::Failure(_) => {
                nom::Err::Error(nom_language::error::VerboseError {
                    errors: vec![(
                        input,
                        nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
                    )],
                })
            }
            nom::Err::Incomplete(n) => nom::Err::Incomplete(n),
        })?;
    // Don't try to keep a &str reference into the lowered string — accept that the
    // dynamic-X clause consumes the rest of the phrase and return empty remainder.
    Ok(("", quantity))
}

/// Parse "your devotion to [color(s)] is less than N" or "is N or greater".
fn parse_devotion_condition(lower: &str) -> Option<StaticCondition> {
    let rest = nom_tag_lower(lower, lower, "your devotion to ")?;

    // Split at " is " to get colors and comparison
    let (color_text, comparison) = rest.split_once(" is ")?;

    // Parse colors: "white", "blue and red", "white and black"
    let colors = parse_color_list(color_text)?;

    // Parse comparison: "less than N" or "N or greater"
    // CR 110.4b: "less than N" means NOT (devotion >= N), "N or greater" means devotion >= N.
    if let Some(n_text) = nom_tag_lower(comparison, comparison, "less than ") {
        let threshold = parse_number(n_text.trim())?.0;
        return Some(StaticCondition::Not {
            condition: Box::new(StaticCondition::DevotionGE { colors, threshold }),
        });
    }

    if let Some(n_rest) = comparison.strip_suffix(" or greater") {
        let threshold = parse_number(n_rest.trim())?.0;
        return Some(StaticCondition::DevotionGE { colors, threshold });
    }

    None
}

/// Parse a color list like "white", "blue and red", "white, blue, and black".
/// Parse a list of color names: "red", "white and blue", "red, white, and blue".
///
/// Delegates individual color word recognition to the shared nom color combinator.
fn parse_color_list(text: &str) -> Option<Vec<crate::types::mana::ManaColor>> {
    /// Parse a single color name using the nom combinator with case normalization.
    fn color_from_name(s: &str) -> Option<crate::types::mana::ManaColor> {
        let lower = s.trim().to_ascii_lowercase();
        let (rest, color) = nom_primitives::parse_color.parse(&lower).ok()?;
        if rest.is_empty() {
            Some(color)
        } else {
            None
        }
    }

    // Try single color first
    if let Some(c) = color_from_name(text) {
        return Some(vec![c]);
    }

    // "X and Y"
    if let Some((a, b)) = text.split_once(" and ") {
        let mut colors = Vec::new();
        // Handle "X, Y, and Z" — a would be "X, Y" and b would be "Z"
        for part in a.split(", ") {
            colors.push(color_from_name(part)?);
        }
        colors.push(color_from_name(b)?);
        return Some(colors);
    }

    None
}

/// Parse "the number of [quantity] is [comparator] [quantity]" into a QuantityComparison.
fn parse_quantity_comparison(lower: &str) -> Option<StaticCondition> {
    let rest = nom_tag_lower(lower, lower, "the number of ")?;
    let (lhs_text, comparison) = rest.split_once(" is ")?;
    let lhs = parse_quantity_ref(lhs_text)?;
    let (comparator, rhs_text) = parse_comparator_prefix(comparison)?;
    let rhs = parse_quantity_ref(rhs_text.trim())?;
    Some(StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref { qty: lhs },
        comparator,
        rhs: QuantityExpr::Ref { qty: rhs },
    })
}

fn find_continuous_predicate_start(lower: &str) -> Option<usize> {
    [
        " gets ", " get ", " gains ", " gain ", " has ", " have ", " loses ", " lose ",
    ]
    .into_iter()
    .filter_map(|marker| lower.find(marker))
    .min()
}

/// Parse "[Type] spells you cast [from zone] have [keyword]" patterns.
/// CR 702.51a: Grants a keyword (typically convoke) to spells matching a filter during casting.
/// Also handles "Creature cards you own that aren't on the battlefield have flash."
fn parse_spells_have_keyword(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    type VE<'a> = nom_language::error::VerboseError<&'a str>;

    // Pattern 1: "[type] spells you cast [from zone] have [keyword]."
    // Find " have " to split subject from keyword
    let have_pos = tp.lower.rfind(" have ")?;
    let subject = &tp.lower[..have_pos];
    let keyword_str = tp.lower[have_pos + " have ".len()..]
        .trim()
        .trim_end_matches('.');

    // Parse the keyword — must be a valid keyword
    let keyword = Keyword::from_str(keyword_str).ok()?;

    // Find "spells you cast" in the subject — may be preceded by a type descriptor
    let spells_marker = "spells you cast";
    if let Some(marker_pos) = subject.find(spells_marker) {
        let type_part = subject[..marker_pos].trim();
        let after_spells = subject[marker_pos + spells_marker.len()..].trim();

        // Walk a cursor through optional qualifiers — zone first, then MV —
        // so combinations like "from exile with mana value 4 or greater" parse
        // correctly. Each qualifier consumes its own bytes.
        let mut cursor = after_spells;

        // Parse optional zone qualifier: "from exile", "from your graveyard"
        let zone_filter = if let Ok((rest, _)) = tag::<_, _, VE<'_>>("from exile").parse(cursor) {
            cursor = rest.trim_start();
            Some(FilterProp::InZone { zone: Zone::Exile })
        } else {
            None
        };

        // CR 202.3: Optional "with mana value N or greater/less" qualifier
        // (Imoti, Celebrant of Bounty: "Spells you cast with mana value 6 or
        // greater have cascade."). Restricted to fixed-value variants — the
        // dynamic Ref/Offset variants (e.g. "less than that creature's mana
        // value") presume a triggering source object that doesn't exist in a
        // CastWithKeyword grant evaluation.
        let mv_filter = parse_mana_value_suffix(cursor).and_then(|(prop, consumed)| {
            let is_fixed = matches!(
                &prop,
                FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { .. }
                } | FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { .. }
                } | FilterProp::Cmc {
                    comparator: Comparator::EQ,
                    value: QuantityExpr::Fixed { .. }
                },
            );
            if is_fixed {
                cursor = cursor[consumed..].trim_start();
                Some(prop)
            } else {
                None
            }
        });
        let _ = cursor; // qualifiers are optional; remaining slice is unused

        // Build the affected filter
        let mut typed = if type_part.is_empty() {
            // "Spells you cast" (no type prefix) — applies to all spells
            TypedFilter::card()
        } else {
            // Parse the spell type filter from the prefix
            let type_prefix_original = &tp.original[..marker_pos].trim();
            let (base_filter, _) = parse_type_phrase(type_prefix_original);
            match base_filter {
                TargetFilter::Typed(tf) => tf,
                _ => TypedFilter::card(),
            }
        };
        typed = typed.controller(ControllerRef::You);
        if let Some(zone_prop) = zone_filter {
            typed.properties.push(zone_prop);
        }
        if let Some(mv_prop) = mv_filter {
            typed.properties.push(mv_prop);
        }

        return Some(
            StaticDefinition::new(StaticMode::CastWithKeyword { keyword })
                .affected(TargetFilter::Typed(typed))
                .description(text.to_string()),
        );
    }

    // Pattern 2: "Creature cards you own that aren't on the battlefield have flash"
    // This grants flash to cards in non-battlefield zones.
    if nom_primitives::scan_contains(subject, "cards you own that aren't on the battlefield") {
        let (prefix, _) = nom_primitives::scan_split_at_phrase(subject, |i| tag("cards").parse(i))?;
        let type_end = prefix.len();
        let type_part = &tp.original[..type_end];
        let (base_filter, _) = parse_type_phrase(type_part);
        let affected = match base_filter {
            TargetFilter::Typed(mut typed) => {
                typed = typed.controller(ControllerRef::You);
                // "aren't on the battlefield" means any zone except battlefield
                typed.properties.push(FilterProp::InAnyZone {
                    zones: vec![Zone::Hand, Zone::Graveyard, Zone::Exile, Zone::Command],
                });
                TargetFilter::Typed(typed)
            }
            _ => base_filter,
        };
        return Some(
            StaticDefinition::new(StaticMode::CastWithKeyword { keyword })
                .affected(affected)
                .description(text.to_string()),
        );
    }

    None
}

/// Parse creature subject phrases containing "of the chosen color/type" qualifiers.
/// Handles patterns like:
/// - "Creatures you control of the chosen color"
/// - "Creatures of the chosen color"
/// - "Creatures of the chosen type your opponents control"
/// - "creature you control of the chosen type other than this Vehicle"
///
/// CR 105.4: "of the chosen color" → `FilterProp::IsChosenColor`
/// CR 205.3m: "of the chosen type" → `FilterProp::IsChosenCreatureType`
fn parse_chosen_qualifier_subject(tp: &TextPair<'_>) -> Option<TargetFilter> {
    type VE<'a> = nom_language::error::VerboseError<&'a str>;

    // Must start with "creature" or "creatures"
    let rest = if let Ok((r, _)) = tag::<_, _, VE<'_>>("creatures ")(tp.lower) {
        r
    } else if let Ok((r, _)) = tag::<_, _, VE<'_>>("creature ")(tp.lower) {
        r
    } else {
        return None;
    };

    // Try to find "of the chosen color" or "of the chosen type" somewhere in the rest
    let chosen_prop: FilterProp;
    let before_chosen: &str;
    let after_chosen: &str;

    if let Ok((_, (before, after))) = nom_primitives::split_once_on(rest, "of the chosen color") {
        chosen_prop = FilterProp::IsChosenColor;
        before_chosen = before.trim();
        after_chosen = after.trim();
    } else if let Ok((_, (before, after))) =
        nom_primitives::split_once_on(rest, "of the chosen type")
    {
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

fn parse_continuous_subject_filter(subject: &str) -> Option<TargetFilter> {
    let trimmed = subject.trim();
    let lower = trimmed.to_lowercase();
    let tp = TextPair::new(trimmed, &lower);

    // Strip "Each " / "All " quantifier prefixes — "Each creature you control" and
    // "All Sliver creatures" are semantically identical to the bare type phrase for
    // filter purposes (CR 205.3 / CR 700.1). Without this, "All Sliver creatures"
    // flows into parse_type_phrase which treats "All Sliver" as a verbatim subtype
    // string and matches zero real creatures.
    if let Some(rest_tp) = nom_tag_tp(&tp, "each ").or_else(|| nom_tag_tp(&tp, "all ")) {
        return parse_continuous_subject_filter(rest_tp.original.trim());
    }

    if let Some(rest_tp) = nom_tag_tp(&tp, "other ") {
        return parse_continuous_subject_filter(rest_tp.original.trim()).map(add_another_filter);
    }

    // CR 105.4 / CR 205.3m: "Creatures [you control] of the chosen color/type [opponent control]"
    // Handle "of the chosen color/type" qualifiers that appear in creature subject phrases.
    if let Some(filter) = parse_chosen_qualifier_subject(&tp) {
        return Some(filter);
    }

    // CR 201.3 / CR 113.6: "<type-phrase> with the chosen name" — the chosen-name
    // name-picker class (Petrified Hamlet, Cheering Fanatic, Disruptor Flute, ...).
    // The type prefix selects the object class; `HasChosenName` restricts it to
    // objects whose name matches the source's `ChosenAttribute::CardName` (bound
    // by a preceding `Effect::Choose { CardName, persist: true }`).
    if let Ok((_, (type_part, _))) =
        nom_primitives::split_once_on(tp.lower, " with the chosen name")
    {
        let type_part_original = tp.original[..type_part.len()].trim();
        let (type_filter, type_rest) = parse_type_phrase(type_part_original);
        if type_rest.trim().is_empty() && !matches!(type_filter, TargetFilter::Any) {
            return Some(TargetFilter::And {
                filters: vec![type_filter, TargetFilter::HasChosenName],
            });
        }
    }

    if let Some(filter) = parse_modified_creature_subject_filter(trimmed) {
        return Some(filter);
    }

    // CR 903.3d: "commander(s) you control" / "commander(s)" subject phrase.
    // Must run before parse_creature_subject_filter because the bare token
    // "Commanders" otherwise falls into the capitalized-subtype fallback and
    // emits a bogus `Subtype: "Commander"` (Commander is not an MTG subtype).
    if let Some(filter) = parse_commander_subject_filter(trimmed) {
        return Some(filter);
    }

    if let Some(filter) = parse_creature_subject_filter(trimmed) {
        return Some(filter);
    }

    let (filter, rest) = parse_type_phrase(trimmed);
    if rest.trim().is_empty() {
        return Some(filter);
    }

    parse_rule_static_subject_filter(trimmed)
}

/// Try to strip a leading "with [counter] counter(s) on it/them" clause from `text`,
/// returning the `FilterProp` and the remaining text after the clause.
/// CR 613.1 + CR 613.7: Used to parse conditional static keyword grants in layer 6.
fn strip_counter_condition_prefix(text: &str) -> Option<(FilterProp, &str)> {
    let lower = text.to_lowercase();
    nom_tag_lower(&lower, &lower, "with ")?;
    // parse_counter_suffix expects optional leading whitespace before "with"
    let (prop, consumed) = parse_counter_suffix(&lower)?;
    Some((prop, text[consumed..].trim_start()))
}

fn parse_modified_creature_subject_filter(subject: &str) -> Option<TargetFilter> {
    let lower = subject.to_lowercase();
    let tp = TextPair::new(subject, &lower);
    if tp.lower == "equipped creature" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
        ));
    }

    let controlled_patterns = [
        ("tapped creatures you control", FilterProp::Tapped),
        ("attacking creatures you control", FilterProp::Attacking),
        ("equipped creatures you control", FilterProp::EquippedBy),
        // CR 700.9: "modified creatures you control" — permanents with
        // counters, equipped, or enchanted by own-controlled Aura.
        ("modified creatures you control", FilterProp::Modified),
        ("modified creature you control", FilterProp::Modified),
    ];

    for (pattern, property) in controlled_patterns {
        if tp.lower == pattern {
            return Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![property]),
            ));
        }
    }

    if tp.lower == "attacking creatures" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::Attacking]),
        ));
    }

    // CR 700.9 + CR 700.4: "modified creature(s)" and "other modified
    // creature(s) [you control]" — includes "Another" variant for triggers
    // that exclude the source (Ondu Knotmaster, Golden-Tail Trainer).
    let controller_suffix_patterns: [(&str, Option<ControllerRef>); 3] = [
        (" you control", Some(ControllerRef::You)),
        (" your opponents control", Some(ControllerRef::Opponent)),
        ("", None),
    ];
    for (suffix, controller) in controller_suffix_patterns {
        let Some(core) = tp.lower.strip_suffix(suffix) else {
            continue;
        };
        for (phrase, has_other) in [
            ("other modified creatures", true),
            ("other modified creature", true),
            ("modified creatures", false),
            ("modified creature", false),
        ] {
            if core == phrase {
                let mut props = vec![FilterProp::Modified];
                if has_other {
                    props.push(FilterProp::Another);
                }
                let mut typed = TypedFilter::creature().properties(props);
                if let Some(c) = controller {
                    typed = typed.controller(c);
                }
                return Some(TargetFilter::Typed(typed));
            }
        }
    }

    None
}

/// CR 903.3d: Parse "commander(s) [you control | your opponents control]"
/// subject phrases into a `TargetFilter` carrying `FilterProp::IsCommander`.
/// "Commander" is the deck-construction designation (CR 903.3) — it is NOT
/// an MTG subtype, so it must not be routed through `parse_subtype` or the
/// capitalized-subtype fallback (which would synthesize `Subtype("Commander")`
/// and match zero objects at runtime).
///
/// Covers Codsworth, Falthis, Anara, Champions of Archery, Vexilus Praetor,
/// Guardian Augmenter, The Dilu Horse, Dancer's Chakrams ("other commanders
/// you control"), and analogous "[other] commander(s) [you control | your
/// opponents control]" subject phrases.
fn parse_commander_subject_filter(subject: &str) -> Option<TargetFilter> {
    type VE<'a> = nom_language::error::VerboseError<&'a str>;
    let lower = subject.trim().to_lowercase();
    let i = lower.as_str();

    // Optional leading "other " — emits FilterProp::Another.
    let (i, other) = opt(tag::<_, _, VE>("other ")).parse(i).ok()?;
    let has_other = other.is_some();

    // The bare commander token (singular or plural).
    let (i, _) = alt((tag::<_, _, VE>("commanders"), tag::<_, _, VE>("commander")))
        .parse(i)
        .ok()?;

    // Optional controller suffix.
    let (i, controller) = alt((
        value(Some(ControllerRef::You), tag::<_, _, VE>(" you control")),
        value(
            Some(ControllerRef::Opponent),
            tag::<_, _, VE>(" your opponents control"),
        ),
        value(None, tag::<_, _, VE>("")),
    ))
    .parse(i)
    .ok()?;

    if !i.trim().is_empty() {
        return None;
    }

    // CR 903.3d: a commander, when controlled, is a permanent on the battlefield.
    let mut props = vec![FilterProp::IsCommander];
    if has_other {
        props.push(FilterProp::Another);
    }
    let mut typed = TypedFilter::permanent().properties(props);
    if let Some(c) = controller {
        typed = typed.controller(c);
    }
    Some(TargetFilter::Typed(typed))
}

fn parse_creature_subject_filter(subject: &str) -> Option<TargetFilter> {
    let trimmed = subject.trim();
    let lower = trimmed.to_lowercase();
    let tp = TextPair::new(trimmed, &lower);

    let (subject_core, controller) = if let Some(prefix) = tp.original.strip_suffix(" you control")
    {
        (prefix.trim(), Some(ControllerRef::You))
    } else if let Some(prefix) = tp.original.strip_suffix(" your opponents control") {
        (prefix.trim(), Some(ControllerRef::Opponent))
    } else {
        (tp.original, None)
    };

    let subject_core_lower = subject_core.to_lowercase();
    let subject_core_tp = TextPair::new(subject_core, &subject_core_lower);
    let (descriptor_text, has_other) =
        if let Some(rest) = subject_core_tp.original.strip_prefix("Other ") {
            (rest.trim(), true)
        } else if let Some(rest) = subject_core_tp.original.strip_prefix("other ") {
            (rest.trim(), true)
        } else {
            (subject_core_tp.original.trim(), false)
        };

    let descriptor = if let Some(prefix) = descriptor_text.strip_suffix(" creatures") {
        prefix.trim()
    } else if !descriptor_text.contains(' ') && descriptor_text.to_lowercase().ends_with('s') {
        // CR 205.3m: Use parse_subtype for irregular plurals (Elves→Elf, Dwarves→Dwarf)
        if let Some((canonical, _)) = parse_subtype(descriptor_text) {
            let mut typed = TypedFilter::creature().subtype(canonical);
            if let Some(controller) = controller {
                typed = typed.controller(controller);
            }
            if has_other {
                typed = typed.properties(vec![FilterProp::Another]);
            }
            return Some(TargetFilter::Typed(typed));
        }
        descriptor_text.trim_end_matches('s').trim()
    } else {
        return None;
    };

    if descriptor.is_empty() {
        return None;
    }

    if let Some(color) = parse_named_color(descriptor) {
        let mut typed = TypedFilter::creature().properties(vec![FilterProp::HasColor { color }]);
        if let Some(controller) = controller {
            typed = typed.controller(controller);
        }
        if has_other {
            typed.properties.push(FilterProp::Another);
        }
        return Some(TargetFilter::Typed(typed));
    }

    if is_capitalized_words(descriptor) {
        let subtype = descriptor.to_string();
        let mut typed = TypedFilter::creature().subtype(subtype);
        if let Some(controller) = controller {
            typed = typed.controller(controller);
        }
        if has_other {
            typed.properties.push(FilterProp::Another);
        }
        return Some(TargetFilter::Typed(typed));
    }

    None
}

fn add_another_filter(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            typed.properties.push(FilterProp::Another);
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.into_iter().map(add_another_filter).collect(),
        },
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Another])),
            ],
        },
    }
}

/// Add a single `FilterProp` to an existing `TargetFilter`.
fn add_property(filter: TargetFilter, prop: FilterProp) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            typed.properties.push(prop);
            TargetFilter::Typed(typed)
        }
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter::default().properties(vec![prop])),
            ],
        },
    }
}

fn strip_rule_static_subject<'a>(text: &'a str, lower: &str) -> Option<(TargetFilter, &'a str)> {
    for marker in [
        " doesn't untap during ",
        " doesn't untap during ",
        " don't untap during ",
        " don't untap during ",
        " must attack each combat if able",
        " must attack if able",
        " attacks each combat if able",
        " attack each combat if able",
        " attacks each turn if able",
        " attack each turn if able",
        " must block each combat if able",
        " must block if able",
        " blocks each combat if able",
        " block each combat if able",
        " blocks each turn if able",
        " block each turn if able",
        " can block only creatures with flying",
        " has shroud",
        " have shroud",
        " has hexproof",
        " have hexproof",
        " has no maximum hand size",
        " have no maximum hand size",
        " may play an additional land",
        " may play up to ",
        " may look at the top card of your library",
        " loses all abilities",
        " lose all abilities",
    ] {
        let Some(subject_end) = lower.find(marker) else {
            continue;
        };
        let subject = text[..subject_end].trim();
        let predicate = text[subject_end + 1..].trim();
        let affected = parse_rule_static_subject_filter(subject)?;
        return Some((affected, predicate));
    }

    None
}

fn parse_rule_static_subject_filter(subject: &str) -> Option<TargetFilter> {
    let lower = subject.to_lowercase();
    let tp = TextPair::new(subject, &lower);

    if matches!(tp.lower, "~" | "this" | "it")
        || SELF_REF_PARSE_ONLY_PHRASES.contains(&tp.lower)
        || SELF_REF_TYPE_PHRASES.contains(&tp.lower)
    {
        return Some(TargetFilter::SelfRef);
    }

    if tp.lower == "you" {
        return Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ));
    }

    if matches!(tp.lower, "players" | "each player") {
        return Some(TargetFilter::Player);
    }

    if tp.lower == "enchanted creature" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ));
    }

    if tp.lower == "enchanted permanent" {
        return Some(TargetFilter::Typed(
            TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy]),
        ));
    }

    if tp.lower == "equipped creature" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
        ));
    }

    let (filter, rest) = parse_type_phrase(subject);
    if rest.trim().is_empty() {
        return Some(filter);
    }

    None
}

fn parse_rule_static_predicate(text: &str) -> Option<RuleStaticPredicate> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    if let Ok((rest, predicate)) = parse_rule_static_predicate_nom(tp.lower) {
        if rest.trim().is_empty() {
            return Some(predicate);
        }
    }

    if nom_tag_tp(&tp, "doesn't untap during").is_some()
        || nom_tag_tp(&tp, "doesn\u{2019}t untap during").is_some()
        || nom_tag_tp(&tp, "don't untap during").is_some()
        || nom_tag_tp(&tp, "don\u{2019}t untap during").is_some()
    {
        return Some(RuleStaticPredicate::CantUntap);
    }

    // CR 508.1d: A creature that "attacks if able" is a requirement on the declare attackers step.
    if matches!(
        tp.lower,
        "attack each combat if able"
            | "attack each combat if able."
            | "attacks each combat if able"
            | "attacks each combat if able."
            | "attack each turn if able"
            | "attack each turn if able."
            | "attacks each turn if able"
            | "attacks each turn if able."
            | "must attack each combat if able"
            | "must attack each combat if able."
            | "must attack if able"
            | "must attack if able."
    ) {
        return Some(RuleStaticPredicate::MustAttack);
    }

    // CR 509.1c: A creature that "blocks if able" is a requirement on the declare blockers step.
    if matches!(
        tp.lower,
        "block each combat if able"
            | "block each combat if able."
            | "blocks each combat if able"
            | "blocks each combat if able."
            | "block each turn if able"
            | "block each turn if able."
            | "blocks each turn if able"
            | "blocks each turn if able."
            | "must block each combat if able"
            | "must block each combat if able."
            | "must block if able"
            | "must block if able."
    ) {
        return Some(RuleStaticPredicate::MustBlock);
    }

    if matches!(
        tp.lower,
        "can block only creatures with flying" | "can block only creatures with flying."
    ) {
        return Some(RuleStaticPredicate::BlockOnlyCreaturesWithFlying);
    }

    if matches!(
        tp.lower,
        "has shroud" | "has shroud." | "have shroud" | "have shroud."
    ) {
        return Some(RuleStaticPredicate::Shroud);
    }

    // CR 702.11: Hexproof — player-scope hexproof ("You have hexproof.") mirrors
    // the shroud predicate wiring so the static is represented as a player-level
    // rule modification rather than a bogus AddKeyword on empty-typed objects.
    if matches!(
        tp.lower,
        "has hexproof" | "has hexproof." | "have hexproof" | "have hexproof."
    ) {
        return Some(RuleStaticPredicate::Hexproof);
    }

    if nom_tag_tp(&tp, "may look at the top card of your library").is_some() {
        return Some(RuleStaticPredicate::MayLookAtTopOfLibrary);
    }

    if matches!(
        tp.lower,
        "lose all abilities"
            | "lose all abilities."
            | "loses all abilities"
            | "loses all abilities."
    ) {
        return Some(RuleStaticPredicate::LoseAllAbilities);
    }

    if matches!(
        tp.lower,
        "has no maximum hand size"
            | "has no maximum hand size."
            | "have no maximum hand size"
            | "have no maximum hand size."
    ) {
        return Some(RuleStaticPredicate::NoMaximumHandSize);
    }

    if nom_tag_tp(&tp, "may play an additional land").is_some()
        || (nom_tag_tp(&tp, "may play up to ").is_some()
            && nom_primitives::scan_contains(tp.lower, "additional land"))
    {
        return Some(RuleStaticPredicate::MayPlayAdditionalLand);
    }

    None
}

fn parse_rule_static_predicate_nom(input: &str) -> OracleResult<'_, RuleStaticPredicate> {
    let (rest, predicate) = alt((
        parse_combat_rule_static_predicate_nom,
        value(
            RuleStaticPredicate::LoseAllAbilities,
            alt((tag("loses all abilities"), tag("lose all abilities"))),
        ),
    ))
    .parse(input)?;
    let (rest, _) = opt(tag(".")).parse(rest)?;
    Ok((rest, predicate))
}

fn parse_combat_rule_static_predicate_nom(input: &str) -> OracleResult<'_, RuleStaticPredicate> {
    alt((
        value(
            RuleStaticPredicate::CantAttackOrBlock,
            tag("can't attack or block"),
        ),
        value(RuleStaticPredicate::CantAttack, tag("can't attack")),
        value(RuleStaticPredicate::CantBlock, tag("can't block")),
    ))
    .parse(input)
}

fn lower_rule_static(
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
        RuleStaticPredicate::MustAttack => StaticDefinition::new(StaticMode::MustAttack)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::MustBlock => StaticDefinition::new(StaticMode::MustBlock)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::BlockOnlyCreaturesWithFlying => {
            StaticDefinition::new(StaticMode::BlockRestriction)
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::Shroud => StaticDefinition::new(StaticMode::Shroud)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::Hexproof => StaticDefinition::new(StaticMode::Hexproof)
            .affected(affected)
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

/// Determine player scope for "can't [verb]" patterns based on subject phrasing.
/// Handles "your opponents can't ...", "you can't ...", and "players can't ..." subjects.
fn parse_player_scope_filter(tp: &TextPair<'_>) -> TargetFilter {
    if nom_primitives::scan_contains(tp.lower, "your opponents")
        || nom_tag_tp(tp, "opponents").is_some()
    {
        TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
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
fn parse_life_total_scope_filter(lower: &str) -> TargetFilter {
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

/// Parse the subject of "X can't be countered" lines.
/// CR 101.2: Returns SelfRef for "~ can't be countered", or a typed filter for
/// "Green spells you control can't be countered", "Creature spells you control can't be countered", etc.
fn parse_cant_be_countered_subject(tp: &TextPair) -> TargetFilter {
    // Find the subject before "can't be countered"
    if let Some(pos) = tp.lower.find("can't be countered") {
        let subject = tp.lower[..pos].trim();
        // Self-referential: "~" or card name (handled by tp.contains matching the card name)
        if subject.is_empty() || subject == "~" || subject.ends_with(" ~") {
            return TargetFilter::SelfRef;
        }
        // "X spells you control" — parse color + type filter
        if let Some(before_yc) = subject.strip_suffix(" you control") {
            if let Some(before_spells) = before_yc
                .strip_suffix(" spells")
                .or_else(|| before_yc.strip_suffix(" spell"))
            {
                let mut properties = Vec::new();
                let mut card_types = Vec::new();

                // Split on " and " to handle compound types: "creature and enchantment"
                for part in before_spells.split(" and ") {
                    for raw_word in part.split_whitespace() {
                        let word = raw_word.trim_end_matches(',');
                        if let Some(color) = parse_named_color(word) {
                            properties.push(FilterProp::HasColor { color });
                        } else {
                            // Try as card type: "creature", "instant", "sorcery", etc.
                            match word {
                                "creature" => card_types.push(TypeFilter::Creature),
                                "instant" => card_types.push(TypeFilter::Instant),
                                "sorcery" => card_types.push(TypeFilter::Sorcery),
                                "enchantment" => card_types.push(TypeFilter::Enchantment),
                                "artifact" => card_types.push(TypeFilter::Artifact),
                                _ => {}
                            }
                        }
                    }
                }

                // CR 608.2b: Single type → direct filter; multiple types → AnyOf wrapper
                let type_filters = if card_types.len() > 1 {
                    vec![TypeFilter::AnyOf(card_types)]
                } else {
                    card_types
                };

                return TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller: Some(ControllerRef::You),
                    properties,
                });
            }
        }
    }
    TargetFilter::SelfRef
}

/// CR 605.1a: Parse the optional "unless they're mana abilities" suffix that
/// follows a `CantBeActivated` predicate. Returns `ActivationExemption::None`
/// (and the unconsumed input) when no suffix is present.
///
/// Composed from nom `tag()`/`alt()`/`value()`/`preceded`/`opt` so additional
/// exemption kinds can be added as one combinator branch when a real card needs
/// them — do not add variants speculatively.
fn parse_activation_exemption_suffix(input: &str) -> OracleResult<'_, ActivationExemption> {
    let mut parser = opt(preceded(
        tag(" unless they're "),
        value(ActivationExemption::ManaAbilities, tag("mana abilities")),
    ));
    let (rest, exemption) = parser.parse(input)?;
    Ok((rest, exemption.unwrap_or_default()))
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
fn parse_filter_scoped_cant_be_activated(
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
        tag::<_, _, nom_language::error::VerboseError<&str>>("sources with the chosen name"),
    ))
    .parse(rest_tp.lower)
    {
        if let Ok((after_predicate, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(" can't be activated")
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

    // Otherwise fall back to the type-list + controller-suffix form (Karn, Clarion).
    // Require the predicate ending "... can't be activated[.]" at the tail.
    let predicate_tp = rest_tp
        .strip_suffix(" can't be activated.")
        .or_else(|| rest_tp.strip_suffix(" can't be activated"))?;
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

/// CR 701.23 + CR 609.3: Parse "Spells and abilities <scope> can't cause their
/// controller to search their library" — Ashiok, Dream Render's first static.
///
/// The scope on the `cause` axis identifies whose spells/abilities are muzzled.
/// For Ashiok the phrasing is "your opponents control" so `cause = Opponents`.
fn parse_cant_search_library(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    let rest_tp = nom_tag_tp(tp, "spells and abilities ")?;
    // Strip the controller suffix — scope identifier rides on the possessive phrase.
    let (cause, predicate) = strip_controller_possessive_scope(rest_tp.original)?;
    // Verify the predicate continues with "can't cause their controller to search
    // their library" — require the exact full phrase so we don't match unrelated
    // "spells and abilities your opponents control can't ..." Oracle texts.
    let predicate_lower = predicate.to_lowercase();
    let trimmed_lower = predicate_lower.trim_end_matches('.').trim_end();
    if trimmed_lower != "can't cause their controller to search their library" {
        return None;
    }
    Some(
        StaticDefinition::new(StaticMode::CantSearchLibrary { cause })
            .description(text.to_string()),
    )
}

/// CR 603.2g + CR 603.6a + CR 700.4: Parse Torpor Orb / Hushbringer-class
/// "Creatures entering [the battlefield] [and dying] don't cause abilities to trigger."
///
/// The optional `and dying` clause toggles the `Dies` event in the event set.
/// Parser constructs events in canonical order `[EntersBattlefield, Dies]`.
fn parse_suppress_triggers(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
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
fn strip_controller_possessive_scope(tp: &str) -> Option<(ProhibitionScope, &str)> {
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
fn strip_casting_prohibition_subject(tp: &str) -> Option<(ProhibitionScope, &str)> {
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

/// CR 101.2 + CR 604.1: Parse per-turn casting limits from Oracle text.
/// Handles "Each player/opponent can't cast more than N [type] spell(s) each turn"
/// and the alternate phrasing "You can cast no more than N spells each turn."
fn parse_per_turn_cast_limit(tp: &str, text: &str) -> Option<StaticDefinition> {
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
            predicate.split_once(" and ").and_then(|(_, second)| {
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
        .strip_suffix(" each turn.")
        .or_else(|| rest.trim_start().strip_suffix(" each turn"))?;

    // 5. Extract optional spell type filter between count and "spell(s)"
    let type_text = before_each_turn
        .strip_suffix(" spells")
        .or_else(|| before_each_turn.strip_suffix(" spell"))
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

/// CR 101.2: Parse casting prohibition from Oracle text.
/// Handles multiple patterns:
/// - "[Subject] can't cast [type] spells" (Steel Golem, Hymn of the Wilds)
/// - "[Type] spells can't be cast" — passive voice (Aether Storm)
/// - "[Subject] can't cast spells with mana value N or less/greater" (Brisela)
/// - "[Subject] can't cast spells with the chosen name" (Alhammarret)
/// - "[Subject] can't cast spells of the chosen type" (Archon of Valor's Reach)
/// - "Enchanted creature's controller can't cast [type] spells" (Brand of Ill Omen)
fn parse_cant_cast_type_spells(tp: &str, text: &str) -> Option<StaticDefinition> {
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

    // --- "Each opponent who attacked [with a creature] this turn can't cast spells" ---
    // CR 101.2: Conditional subject with turn-scoped attack condition — approximate
    // as opponent-scoped prohibition since the condition is game-state dependent.
    if nom_tag_lower(tp, tp, "each opponent who attacked").is_some()
        && nom_primitives::scan_contains(tp, "can't cast spells")
    {
        return Some(
            StaticDefinition::new(StaticMode::CantBeCast {
                who: ProhibitionScope::Opponents,
            })
            .description(text.to_string()),
        );
    }

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
        .strip_suffix(" spells")
        .or_else(|| trimmed.strip_suffix(" spell"))?;

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
fn parse_passive_cant_be_cast(tp: &str, text: &str) -> Option<StaticDefinition> {
    // Look for "spells can't be cast" suffix
    let trimmed = tp.trim_end_matches('.');
    let before_cant = trimmed.strip_suffix(" can't be cast")?;

    // Check for "spells with mana value N or less/greater" pattern
    // E.g., "noncreature spells with mana value 4 or greater can't be cast"
    if let Some(pos) = before_cant.find(" spells with mana value ") {
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

    // Require " spells" at the end of the subject
    let type_text = before_cant.strip_suffix(" spells")?;

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
fn parse_temporal_prefix_cant_cast(tp: &str, text: &str) -> Option<StaticDefinition> {
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
        .strip_suffix(" spells")
        .or_else(|| trimmed.strip_suffix(" spell"))
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
fn parse_enchanted_controller_cant_cast(tp: &str, text: &str) -> Option<StaticDefinition> {
    let rest = nom_tag_lower(tp, tp, "enchanted creature's controller ")
        .or_else(|| nom_tag_lower(tp, tp, "enchanted creature\u{2019}s controller "))?;
    let after_cant_cast = nom_tag_lower(rest, rest, "can't cast ")
        .or_else(|| nom_tag_lower(rest, rest, "can\u{2019}t cast "))?;

    let trimmed = after_cant_cast.trim_end_matches('.');
    let before_spells = trimmed
        .strip_suffix(" spells")
        .or_else(|| trimmed.strip_suffix(" spell"))?;

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
fn parse_cant_cast_mana_value(
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
fn parse_per_turn_draw_limit(tp: &str, text: &str) -> Option<StaticDefinition> {
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
fn parse_cant_draw_cards(tp: &str, text: &str) -> Option<StaticDefinition> {
    type VE<'a> = nom_language::error::VerboseError<&'a str>;

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
fn parse_cant_enter_battlefield_subject(tp: &TextPair) -> TargetFilter {
    let mut card_type = None;
    let mut properties = Vec::new();

    if let Some(pos) = tp.lower.find("can't enter the battlefield") {
        let subject = tp.lower[..pos].trim();
        // "creature cards in graveyards and libraries" → card_type = Creature
        if let Some(type_part) = subject.split(" cards").next() {
            card_type = match type_part.trim() {
                "creature" => Some(TypeFilter::Creature),
                "artifact" => Some(TypeFilter::Artifact),
                "enchantment" => Some(TypeFilter::Enchantment),
                "instant" => Some(TypeFilter::Instant),
                "sorcery" => Some(TypeFilter::Sorcery),
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

/// Extract zone names referenced in Oracle text.
/// Handles "graveyards", "libraries", "exile" and their singular/plural forms.
fn parse_zone_names_from_tp(tp: &TextPair) -> Vec<Zone> {
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

/// Parse a color name from Oracle text, delegating to the shared nom color combinator.
///
/// Accepts leading/trailing whitespace and requires complete consumption (no trailing text
/// beyond whitespace). This preserves the original behavior of the match-based implementation.
fn parse_named_color(text: &str) -> Option<ManaColor> {
    let lower = text.trim().to_ascii_lowercase();
    let (rest, color) = nom_primitives::parse_color.parse(&lower).ok()?;
    if rest.is_empty() {
        Some(color)
    } else {
        None
    }
}

/// CR 613.1d + CR 205.1a: "Enchanted [permanent-type] is a/an [type] [with base P/T N/N]
/// [in addition to its other types]"
///
/// Handles type-changing aura effects like Ensoul Artifact, Imprisoned in the Moon,
/// and Darksteel Mutation. Reuses nom type-word and P/T combinators.
fn parse_enchanted_is_type(tp: &TextPair, description: &str) -> Option<StaticDefinition> {
    // Match "enchanted " prefix
    let rest_tp = nom_tag_tp(tp, "enchanted ")?;

    // Parse the enchanted permanent type using nom type-word combinator
    let (after_type, perm_tf) = nom_target::parse_type_filter_word(rest_tp.lower).ok()?;
    let after_type_lower = after_type.trim_start();

    // Must have " is a " or " is an " or " loses all abilities and is a "
    let mut modifications = Vec::new();
    type VE<'a> = nom_language::error::VerboseError<&'a str>;

    let is_rest_lower = if let Ok((r, _)) = alt((
        tag::<_, _, VE>("loses all abilities and is a "),
        tag::<_, _, VE>("loses all abilities and is an "),
    ))
    .parse(after_type_lower)
    {
        modifications.push(ContinuousModification::RemoveAllAbilities);
        r
    } else if let Ok((r, _)) =
        alt((tag::<_, _, VE>("is a "), tag::<_, _, VE>("is an "))).parse(after_type_lower)
    {
        r
    } else {
        return None;
    };

    let is_rest_lower = is_rest_lower.trim_end_matches('.');

    // Check for "in addition to its other types" suffix
    let (type_part, _is_additive) =
        if let Some(before) = is_rest_lower.strip_suffix(" in addition to its other types") {
            (before.trim(), true)
        } else {
            (is_rest_lower, false)
        };

    // Try to parse "base power and toughness N/N" suffix
    let (type_part, base_pt) = if let Some((before_pt, pt_part)) =
        type_part.rsplit_once(" with base power and toughness ")
    {
        if let Some((p, t)) = parse_pt_mod(pt_part) {
            (before_pt.trim(), Some((p, t)))
        } else {
            (type_part, None)
        }
    } else {
        (type_part, None)
    };

    // Parse "N/N [color] [type] [subtype]" patterns for Darksteel Mutation style
    // e.g., "0/1 green Insect creature"
    let (type_part, inline_pt) = if let Some((p, t)) = parse_pt_mod(type_part) {
        // parse_pt_mod trims and finds the slash — get remainder after P/T
        let slash_pos = type_part.find('/').unwrap_or(0);
        let after_slash = &type_part[slash_pos + 1..];
        let t_end = after_slash
            .find(|c: char| c.is_whitespace() || c == '.' || c == ',')
            .unwrap_or(after_slash.len());
        let rest = after_slash[t_end..].trim();
        (rest, Some((p, t)))
    } else {
        (type_part, None)
    };

    // Parse optional color
    let (type_part, opt_color) = if let Ok((rest, color)) = nom_primitives::parse_color(type_part) {
        (rest.trim(), Some(color))
    } else if let Ok((rest, _)) = tag::<_, _, VE>("colorless ").parse(type_part) {
        // "colorless" removes all colors — handled via SetColor([])
        (rest.trim(), None)
    } else {
        (type_part, None)
    };
    let is_colorless = nom_primitives::scan_contains(is_rest_lower, "colorless");

    // Parse the target type(s) — use parse_type_filter_word for the main type.
    // Handle "[Subtype] [type]" patterns (e.g., "insect creature") by trying the
    // first word as a subtype and the second as a type if direct parse fails.
    use crate::types::card_type::CoreType;

    let (parsed_type, subtype_word, remainder) =
        if let Ok((remainder, target_tf)) = nom_target::parse_type_filter_word(type_part) {
            (Some(target_tf), None, remainder.trim())
        } else if let Some(space_pos) = type_part.find(' ') {
            // First word might be a subtype — try the rest as a type
            let maybe_subtype = &type_part[..space_pos];
            let after_subtype = type_part[space_pos..].trim();
            if let Ok((remainder, target_tf)) = nom_target::parse_type_filter_word(after_subtype) {
                // Capitalize the subtype for canonical form
                let capitalized = {
                    let mut chars = maybe_subtype.chars();
                    match chars.next() {
                        Some(first) => {
                            let mut s = first.to_uppercase().collect::<String>();
                            s.push_str(chars.as_str());
                            s
                        }
                        None => maybe_subtype.to_string(),
                    }
                };
                (Some(target_tf), Some(capitalized), remainder.trim())
            } else {
                (None, None, type_part)
            }
        } else {
            (None, None, type_part)
        };

    if let Some(target_tf) = parsed_type {
        // Map TypeFilter → CoreType for AddType modification
        let core_type = match &target_tf {
            TypeFilter::Creature => Some(CoreType::Creature),
            TypeFilter::Artifact => Some(CoreType::Artifact),
            TypeFilter::Enchantment => Some(CoreType::Enchantment),
            TypeFilter::Land => Some(CoreType::Land),
            TypeFilter::Planeswalker => Some(CoreType::Planeswalker),
            _ => None,
        };

        if let Some(ct) = core_type {
            modifications.push(ContinuousModification::AddType { core_type: ct });
        }

        // Add subtype if parsed from "[Subtype] [type]" pattern
        if let Some(sub) = subtype_word {
            modifications.push(ContinuousModification::AddSubtype { subtype: sub });
        }

        // Parse any additional type words or subtypes from remainder
        // Handles "Insect artifact creature" where remainder = "creature" after parsing "artifact"
        let mut extra = remainder;
        while !extra.is_empty() {
            if let Ok((rest, extra_tf)) = nom_target::parse_type_filter_word(extra) {
                let extra_ct = match &extra_tf {
                    TypeFilter::Creature => Some(CoreType::Creature),
                    TypeFilter::Artifact => Some(CoreType::Artifact),
                    TypeFilter::Enchantment => Some(CoreType::Enchantment),
                    TypeFilter::Land => Some(CoreType::Land),
                    TypeFilter::Planeswalker => Some(CoreType::Planeswalker),
                    _ => None,
                };
                if let Some(ct) = extra_ct {
                    modifications.push(ContinuousModification::AddType { core_type: ct });
                }
                extra = rest.trim();
            } else if is_capitalized_words(extra) {
                modifications.push(ContinuousModification::AddSubtype {
                    subtype: extra.to_string(),
                });
                break;
            } else {
                break;
            }
        }

        // Add color modification
        if let Some(color) = opt_color {
            modifications.push(ContinuousModification::AddColor { color });
        } else if is_colorless {
            modifications.push(ContinuousModification::SetColor { colors: vec![] });
        }

        // Add base P/T from explicit "with base power and toughness" or inline "N/N"
        if let Some((p, t)) = base_pt.or(inline_pt) {
            modifications.push(ContinuousModification::SetPower { value: p });
            modifications.push(ContinuousModification::SetToughness { value: t });
        }

        if modifications.is_empty() {
            return None;
        }

        let affected = TargetFilter::Typed(
            TypedFilter::new(perm_tf).properties(vec![FilterProp::EnchantedBy]),
        );

        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(modifications)
                .description(description.to_string()),
        );
    }

    None
}

/// CR 614.1b: Parse a step name from Oracle text using nom combinators.
fn parse_step_name(input: &str) -> Option<Phase> {
    use nom_language::error::VerboseError;
    let result: Result<(&str, Phase), nom::Err<VerboseError<&str>>> = alt((
        value(Phase::Draw, tag("draw step")),
        value(Phase::Untap, tag("untap step")),
        value(Phase::Upkeep, tag("upkeep step")),
    ))
    .parse(input);
    result
        .ok()
        .and_then(|(rest, phase)| rest.is_empty().then_some(phase))
}

/// CR 205.2a: Check if a lowercase descriptor names a core card type that can modify
/// "creatures" (e.g., "artifact" in "artifact creatures"). Returns the TypeFilter if so.
/// Delegates to the existing nom type-word combinator for authoritative type recognition.
fn try_parse_core_type_descriptor(descriptor_lower: &str) -> Option<TypeFilter> {
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
fn typed_filter_for_subtype(subtype: &str) -> TypedFilter {
    use crate::types::ability::TypeFilter;
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

fn is_capitalized_words(s: &str) -> bool {
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
fn try_parse_thats_a_subtype_list(input: &str) -> Option<(TargetFilter, &str)> {
    use nom::multi::separated_list1;

    fn parse_subtype_word(
        input: &str,
    ) -> nom::IResult<&str, &str, nom_language::error::VerboseError<&str>> {
        use nom::bytes::complete::take_while1;
        let (rest, word) = take_while1(|c: char| c.is_alphabetic() || c == '-').parse(input)?;
        if !word.chars().next().is_some_and(|c| c.is_uppercase()) {
            return Err(nom::Err::Error(nom_language::error::VerboseError {
                errors: vec![(
                    input,
                    nom_language::error::VerboseErrorKind::Context("expected capitalized subtype"),
                )],
            }));
        }
        Ok((rest, word))
    }

    fn parse_conjunction(
        input: &str,
    ) -> nom::IResult<&str, &str, nom_language::error::VerboseError<&str>> {
        alt((tag(" or a "), tag(" and a "), tag(" or "), tag(" and "))).parse(input)
    }

    let (rest, words): (&str, Vec<&str>) = separated_list1(parse_conjunction, parse_subtype_word)
        .parse(input)
        .ok()?;
    // Predicate must follow a space
    let predicate = rest.strip_prefix(' ')?;
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
fn parse_can_attack_despite_defender(
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
        tag::<_, _, nom_language::error::VerboseError<&str>>("can attack as though").parse(i)
    })?;

    // Verify the rest of the phrase: " it didn't have defender" or
    // " they didn't have defender". Guards against "can attack as though
    // it had haste" reaching subject dispatch.
    type VE<'a> = nom_language::error::VerboseError<&'a str>;
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

/// Parse the predicate of an enchanted/equipped grant, handling:
/// - Non-standard keyword phrasings: "can attack as though it had haste", "can't be blocked"
/// - Conditional grants: "gets +1/+1 as long as you control a Wizard"
/// - Standard continuous grants: "gets +N/+M", "has keyword", "for each", "where X is"
///
/// CR 702.10 + CR 509.1b + CR 613.4c: Enchanted/equipped predicate dispatch.
fn parse_enchanted_equipped_predicate(
    predicate: &str,
    affected: TargetFilter,
    description: &str,
) -> Option<StaticDefinition> {
    let pred_lower = predicate.to_lowercase();

    // --- Non-standard keyword phrasings (check before continuous grants) ---

    // CR 702.10: "can attack as though it had haste" → AddKeyword(Haste)
    if nom_primitives::scan_contains(&pred_lower, "can attack as though it had haste") {
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste,
                }])
                .description(description.to_string()),
        );
    }

    // CR 702.3b: "can attack as though <pronoun> didn't have defender" →
    // CanAttackWithDefender. Accepts both pronoun forms so plural subjects
    // ("Creatures you control …they didn't…") routed through the
    // creatures-you-control prefix handler (line ~620) land here.
    type VE<'a> = nom_language::error::VerboseError<&'a str>;
    if alt((
        tag::<_, _, VE>("can attack as though it didn't have defender"),
        tag::<_, _, VE>("can attack as though they didn't have defender"),
    ))
    .parse(pred_lower.as_str())
    .is_ok()
    {
        return Some(
            StaticDefinition::new(StaticMode::CanAttackWithDefender)
                .affected(affected)
                .description(description.to_string()),
        );
    }

    // CR 509.1b: "can't be blocked" on enchanted/equipped creature
    if nom_tag_lower(&pred_lower, &pred_lower, "can't be blocked").is_some() {
        // "can't be blocked except by" → CantBeBlockedExceptBy
        if let Some(rest) = nom_tag_lower(&pred_lower, &pred_lower, "can't be blocked except by ") {
            let filter_text = rest.trim_end_matches('.');
            return Some(
                StaticDefinition::new(StaticMode::CantBeBlockedExceptBy {
                    filter: filter_text.to_string(),
                })
                .affected(affected)
                .description(description.to_string()),
            );
        }
        // CR 509.1b: "can't be blocked by <filter>" → CantBeBlockedBy
        if let Some(rest) = nom_tag_lower(&pred_lower, &pred_lower, "can't be blocked by ") {
            let filter_text = rest.trim_end_matches('.');
            let (filter, _) = parse_type_phrase(filter_text);
            if !matches!(filter, TargetFilter::Any) {
                return Some(
                    StaticDefinition::new(StaticMode::CantBeBlockedBy { filter })
                        .affected(affected)
                        .description(description.to_string()),
                );
            }
        }
        return Some(
            StaticDefinition::new(StaticMode::CantBeBlocked)
                .affected(affected)
                .description(description.to_string()),
        );
    }

    // --- Conditional grants: split "as long as" before passing to continuous parser ---
    // Handles both "gets +1/+1 as long as ..." and "has flying as long as ..."
    let pred_tp = TextPair::new(predicate, &pred_lower);
    if let Some((before_cond, after_cond)) = pred_tp.split_around(" as long as ") {
        let continuous_text = before_cond.original;
        let condition_text = after_cond.original.trim().trim_end_matches('.');
        if let Some(mut def) =
            parse_continuous_gets_has(continuous_text, affected.clone(), description)
        {
            let condition =
                parse_static_condition(condition_text).unwrap_or(StaticCondition::Unrecognized {
                    text: condition_text.to_string(),
                });
            def.condition = Some(condition);
            return Some(def);
        }
    }

    // --- Standard continuous grants (gets/has/for each/where X) ---
    parse_continuous_gets_has(predicate, affected, description)
}

/// Parse "gets +N/+M [and has {keyword}]" after the subject.
/// Also handles "gets +N/+M for each [clause]" dynamic P/T patterns.
fn parse_continuous_gets_has(
    text: &str,
    affected: TargetFilter,
    description: &str,
) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // CR 611.3a: Split "as long as [condition]" BEFORE "for each" — the condition applies
    // to the entire static, not to a quantity count. Mirrors parse_enchanted_equipped_predicate.
    if let Some((before_cond, after_cond)) = tp.split_around(" as long as ") {
        let continuous_text = before_cond.original;
        let condition_text = after_cond.original.trim().trim_end_matches('.');
        // Recursively parse the continuous part without the condition
        if let Some(mut def) =
            parse_continuous_gets_has(continuous_text, affected.clone(), description)
        {
            let condition =
                parse_static_condition(condition_text).unwrap_or(StaticCondition::Unrecognized {
                    text: condition_text.to_string(),
                });
            def.condition = Some(condition);
            return Some(def);
        }
    }

    // CR 613.4c: Handle "gets +N/+M for each [clause]" — dynamic P/T via ObjectCount.
    if let Some((before_for_each, after_for_each)) = tp.split_around("for each ") {
        let pt_text = before_for_each.original.trim();
        let raw_for_each = after_for_each.lower.trim_end_matches('.');
        // Strip a trailing keyword clause (" and has flying", " and gains haste",
        // etc.) so the for-each filter parser sees only its own clause. The
        // trailing keywords are picked up separately via `extract_keyword_clause`
        // on `description` below.
        let for_each_clause = strip_trailing_keyword_clause(raw_for_each);

        let pt_lower = pt_text.to_lowercase();
        let pt_source = nom_tag_lower(&pt_lower, &pt_lower, "gets ")
            .or_else(|| nom_tag_lower(&pt_lower, &pt_lower, "get "))
            .unwrap_or(&pt_lower);

        if let Some((p, t)) = parse_pt_mod(pt_source) {
            if let Some(quantity) =
                super::oracle_quantity::parse_for_each_clause_expr(for_each_clause)
            {
                let mut modifications = Vec::new();
                if p != 0 {
                    let value = if p.abs() == 1 {
                        if p > 0 {
                            quantity.clone()
                        } else {
                            QuantityExpr::Multiply {
                                factor: -1,
                                inner: Box::new(quantity.clone()),
                            }
                        }
                    } else {
                        QuantityExpr::Multiply {
                            factor: p,
                            inner: Box::new(quantity.clone()),
                        }
                    };
                    modifications.push(ContinuousModification::AddDynamicPower { value });
                }
                if t != 0 {
                    let value = if t.abs() == 1 {
                        if t > 0 {
                            quantity
                        } else {
                            QuantityExpr::Multiply {
                                factor: -1,
                                inner: Box::new(quantity),
                            }
                        }
                    } else {
                        QuantityExpr::Multiply {
                            factor: t,
                            inner: Box::new(quantity),
                        }
                    };
                    modifications.push(ContinuousModification::AddDynamicToughness { value });
                }
                if !modifications.is_empty() {
                    // Check for trailing "and has [keyword]" after the for-each clause
                    // e.g., "gets +1/+0 for each Mountain you control and has first strike"
                    if let Some(keyword_text) = extract_keyword_clause(description) {
                        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
                            if let Some(kw) = map_keyword(part.trim().trim_end_matches('.')) {
                                modifications
                                    .push(ContinuousModification::AddKeyword { keyword: kw });
                            }
                        }
                    }
                    return Some(
                        StaticDefinition::continuous()
                            .affected(affected)
                            .modifications(modifications)
                            .description(description.to_string()),
                    );
                }
            }
        }
    }

    let modifications = parse_continuous_modifications(text);

    if modifications.is_empty() {
        return None;
    }

    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(description.to_string()),
    )
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
    let tp = stripped_tp;
    let text_stripped = tp.original;
    let lower = tp.lower;
    let mut modifications = Vec::new();

    if nom_primitives::scan_contains(tp.lower, "lose all abilities") {
        modifications.push(ContinuousModification::RemoveAllAbilities);
    }

    if let Some(rest_tp) = nom_tag_tp(&tp, "gets ").or_else(|| nom_tag_tp(&tp, "get ")) {
        let after = rest_tp.original.trim();
        if let Some((p, t)) = parse_pt_mod(after) {
            modifications.push(ContinuousModification::AddPower { value: p });
            modifications.push(ContinuousModification::AddToughness { value: t });
        }
    }

    // CR 613.4c: Scan for "get +X/+X" / "gets +X/+X" anywhere in the text
    // for dynamic P/T modification (e.g., Craterhoof Behemoth)
    if let Some(dynamic_mods) = parse_dynamic_pt_in_text(lower, where_x_expression.as_deref()) {
        modifications.extend(dynamic_mods);
    }

    // CR 613.4b + CR 107.3m: "have base power and toughness X/X" — dynamic set
    // at layer 7b. Checked before the fixed-literal parser so X-bearing patterns
    // are not mis-parsed as literal integers.
    if let Some((power, toughness)) =
        parse_base_pt_dynamic(text_stripped, where_x_expression.as_deref())
    {
        modifications.push(ContinuousModification::SetPowerDynamic { value: power });
        modifications.push(ContinuousModification::SetToughnessDynamic { value: toughness });
    } else if let Some((power, toughness)) = parse_base_pt_mod(text_stripped) {
        modifications.push(ContinuousModification::SetPower { value: power });
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }
    if let Some(power) = parse_base_power_mod(text_stripped) {
        modifications.push(ContinuousModification::SetPower { value: power });
    }
    if let Some(toughness) = parse_base_toughness_mod(text_stripped) {
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }

    for modification in parse_quoted_ability_modifications(text_stripped) {
        modifications.push(modification);
    }

    if let Some(additive_modifications) = parse_additive_type_clause_modifications(text_stripped) {
        modifications.extend(additive_modifications);
    }

    // CR 702: Guard "can't have or gain [keyword]" from extract_keyword_clause —
    // "have" inside "can't have" must NOT produce AddKeyword.
    if nom_primitives::scan_contains(lower, "can't have")
        || nom_primitives::scan_contains(lower, "can't have or gain")
    {
        // Parse the keyword from "can't have or gain [keyword]" / "can't have [keyword]"
        let stripped_lower = lower.strip_suffix('.').unwrap_or(lower);
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
    } else if let Some(keyword_text) = extract_keyword_clause(text_stripped) {
        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
            let part_trimmed = part.trim().trim_end_matches('.');
            let part_lower = part_trimmed.to_lowercase();
            // CR 702: Check for dynamic "keyword X" with "where X is [qty]"
            if let Some(ref where_expr) = where_x_expression {
                if let Ok((_, kw_name)) = terminated(
                    alpha1::<_, nom_language::error::VerboseError<&str>>,
                    preceded(space1, tag_no_case("x")),
                )
                .parse(part_lower.as_str())
                {
                    if let Some(kind) =
                        crate::types::keywords::DynamicKeywordKind::from_name(kw_name)
                    {
                        if let Some(qty_ref) =
                            crate::parser::oracle_quantity::parse_quantity_ref(where_expr)
                        {
                            modifications.push(ContinuousModification::AddDynamicKeyword {
                                kind,
                                value: QuantityExpr::Ref { qty: qty_ref },
                            });
                            continue;
                        }
                    }
                }
            }
            // Fall through to normal AddKeyword
            if let Some(kw) = map_keyword(part_trimmed) {
                modifications.push(ContinuousModification::AddKeyword { keyword: kw });
            }
        }
    }

    // CR 702: "lose [keyword]" / "loses [keyword]" — keyword removal.
    if let Some(keyword_text) = extract_lose_keyword_clause(text_stripped) {
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
    modifications.extend(parse_becomes_type_addition_modifications(&tp));

    modifications
}

/// CR 613.4c: Scan text for "get(s) +X/+X" and resolve X via where_x_expression.
/// Returns AddDynamicPower + AddDynamicToughness modifications if found.
/// CR 613.4c: Parse a variable P/T modifier pattern like "+x/+x", "-x/-0", "+0/-x".
/// Returns (power_sign, power_is_x, toughness_sign, toughness_is_x) and remaining text.
fn parse_variable_pt_pattern(
    input: &str,
) -> nom::IResult<&str, (i32, bool, i32, bool), nom_language::error::VerboseError<&str>> {
    let (rest, p_sign) = alt((value(-1i32, tag("-")), value(1i32, tag("+")))).parse(input)?;
    let (rest, p_is_x) = alt((value(true, tag("x")), value(false, tag("0")))).parse(rest)?;
    let (rest, _) = tag("/").parse(rest)?;
    let (rest, t_sign) = alt((value(-1i32, tag("-")), value(1i32, tag("+")))).parse(rest)?;
    let (rest, t_is_x) = alt((value(true, tag("x")), value(false, tag("0")))).parse(rest)?;
    Ok((rest, (p_sign, p_is_x, t_sign, t_is_x)))
}

fn parse_dynamic_pt_in_text(
    lower: &str,
    where_x_expression: Option<&str>,
) -> Option<Vec<ContinuousModification>> {
    // Find "get " or "gets " followed by a variable P/T pattern via nom combinator
    let gets_pos = lower.find("gets ").or_else(|| lower.find("get "))?;
    let after_gets = &lower[gets_pos..];
    let after_verb = nom_tag_lower(after_gets, after_gets, "gets ")
        .or_else(|| nom_tag_lower(after_gets, after_gets, "get "))?;

    // CR 613.4c: Parse variable P/T pattern via nom combinator
    let (_, (p_sign, p_is_x, t_sign, t_is_x)) = parse_variable_pt_pattern(after_verb).ok()?;

    if !p_is_x && !t_is_x {
        return None; // No X variable — not a dynamic P/T pattern
    }

    let quantity = parse_cda_quantity(where_x_expression?)?;

    let mut mods = Vec::new();
    if p_is_x {
        let qty = if p_sign < 0 {
            QuantityExpr::Multiply {
                factor: -1,
                inner: Box::new(quantity.clone()),
            }
        } else {
            quantity.clone()
        };
        mods.push(ContinuousModification::AddDynamicPower { value: qty });
    }
    if t_is_x {
        let qty = if t_sign < 0 {
            QuantityExpr::Multiply {
                factor: -1,
                inner: Box::new(quantity),
            }
        } else {
            quantity
        };
        mods.push(ContinuousModification::AddDynamicToughness { value: qty });
    }

    Some(mods)
}

/// CR 205.1a + CR 205.2 + CR 205.3 + CR 613.1c: Scan text for a "becomes a
/// [subtype]* [core-type]+ in addition to its other types" descriptor and
/// decompose it into typed `ContinuousModification`s.
///
/// Uses nom combinators (`tag`, `alt`, `take_until`) to locate the descriptor
/// slice on the lowered text, then hands the original-cased slice to
/// [`super::oracle_effect::animation::parse_becomes_type_modifications`] which
/// reuses the existing animation type-sequence combinator for CR-205
/// token-by-token classification. One `AddType` per CR 205.2 core type and
/// one `AddSubtype` per CR 205.3 subtype are emitted; CR 205.4 supertypes are
/// recognized-and-discarded (animations don't grant supertypes).
fn parse_becomes_type_addition_modifications(tp: &TextPair<'_>) -> Vec<ContinuousModification> {
    type VE<'a> = nom_language::error::VerboseError<&'a str>;

    // Scan for the "becomes a"/"becomes an" phrase anywhere in the lowered
    // text, then locate the terminating "in addition to its other types"
    // clause. `scan_split_at_phrase` returns the lowered slice beginning at
    // the matched phrase.
    let Some((_, tail_lower)) = nom_primitives::scan_split_at_phrase(tp.lower, |i| {
        alt((
            tag::<_, _, VE>("becomes a "),
            tag::<_, _, VE>("becomes an "),
        ))
        .parse(i)
    }) else {
        return Vec::new();
    };
    let Ok::<_, nom::Err<VE<'_>>>((after_article_lower, _consumed)) =
        alt((tag("becomes a "), tag("becomes an "))).parse(tail_lower)
    else {
        return Vec::new();
    };

    // Extract the descriptor up to the first " in addition to" clause.
    let Ok::<_, nom::Err<VE<'_>>>((_, descriptor_lower)) =
        take_until(" in addition to")(after_article_lower)
    else {
        return Vec::new();
    };

    // Map the lowered descriptor back onto the original-cased text so the CR
    // 205.3 subtype grammar (which requires capitalized proper nouns) sees the
    // correct case.
    let start = tp.lower.len() - after_article_lower.len();
    let end = start + descriptor_lower.len();
    let descriptor_original = &tp.original[start..end];

    super::oracle_effect::animation::parse_becomes_type_modifications(descriptor_original)
}

fn parse_base_pt_mod(text: &str) -> Option<(i32, i32)> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let pt_text = tp.strip_after("base power and toughness ")?.original.trim();
    parse_pt_mod(pt_text)
}

/// One side of a dynamic base-P/T value token like `X/X` or `-X/2`.
/// Dynamic sides carry the sign (`+X` vs `-X`); fixed sides carry the literal.
#[derive(Clone, Copy)]
enum BasePtSide {
    Dynamic { sign: i32 },
    Fixed { value: i32 },
}

fn parse_base_pt_side(
    input: &str,
) -> nom::IResult<&str, BasePtSide, nom_language::error::VerboseError<&str>> {
    let (rest, sign) = opt(alt((value(-1i32, tag("-")), value(1i32, tag("+"))))).parse(input)?;
    let sign = sign.unwrap_or(1);
    if let Ok((rest2, _)) = tag::<_, _, nom_language::error::VerboseError<&str>>("x")(rest) {
        return Ok((rest2, BasePtSide::Dynamic { sign }));
    }
    let (rest, n) = nom_primitives::parse_number.parse(rest)?;
    Ok((
        rest,
        BasePtSide::Fixed {
            value: sign * (n as i32),
        },
    ))
}

/// CR 613.4b + CR 107.3: Parse "base power and toughness X/X" (dynamic form).
/// Returns a `(power_expr, toughness_expr)` pair when the P/T token contains X
/// on either side; otherwise returns `None` (literal N/N is handled by
/// `parse_base_pt_mod`). The X-ref is resolved via the provided
/// `where_x_expression` (for patterns like "base power and toughness X/X,
/// where X is the number of …"), falling back to `CostXPaid` for spell-cast
/// contexts where X is the cost X (e.g., Biomass Mutation).
fn parse_base_pt_dynamic(
    text: &str,
    where_x_expression: Option<&str>,
) -> Option<(QuantityExpr, QuantityExpr)> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let pt_tp = tp.strip_after("base power and toughness ")?;
    let (_, (p, _, t)) = (parse_base_pt_side, tag("/"), parse_base_pt_side)
        .parse(pt_tp.lower)
        .ok()?;
    match (p, t) {
        (BasePtSide::Fixed { .. }, BasePtSide::Fixed { .. }) => None,
        (p_side, t_side) => {
            let x_ref = resolve_base_pt_x_ref(where_x_expression)?;
            Some((
                base_pt_side_to_expr(p_side, &x_ref),
                base_pt_side_to_expr(t_side, &x_ref),
            ))
        }
    }
}

/// Build a `QuantityExpr` for one side of a dynamic base-P/T pattern.
fn base_pt_side_to_expr(side: BasePtSide, x_ref: &QuantityRef) -> QuantityExpr {
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
/// When a "where X is …" expression is present, parse it via `parse_quantity_ref`.
fn resolve_base_pt_x_ref(where_x_expression: Option<&str>) -> Option<QuantityRef> {
    if let Some(expr) = where_x_expression {
        return parse_quantity_ref(expr);
    }
    // CR 107.3m: In a spell-cast context, X refers to the value paid for {X}.
    Some(QuantityRef::CostXPaid)
}

fn parse_base_power_mod(text: &str) -> Option<i32> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    if nom_primitives::scan_contains(tp.lower, "base power and toughness") {
        return None;
    }
    let power_text = tp.strip_after("base power ")?.original.trim();
    parse_single_pt_value(power_text)
}

fn parse_base_toughness_mod(text: &str) -> Option<i32> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    if nom_primitives::scan_contains(tp.lower, "base power and toughness") {
        return None;
    }
    let toughness_text = tp.strip_after("base toughness ")?.original.trim();
    parse_single_pt_value(toughness_text)
}

fn parse_single_pt_value(text: &str) -> Option<i32> {
    let value = text
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | ','))
        .next()?;
    value.replace('+', "").parse::<i32>().ok()
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
                if !ability_text.is_empty() {
                    let lower = ability_text.to_lowercase();
                    // CR 603.1: Detect trigger prefixes to route to GrantTrigger.
                    if nom_tag_lower(&lower, &lower, "when ").is_some()
                        || nom_tag_lower(&lower, &lower, "whenever ").is_some()
                        || nom_tag_lower(&lower, &lower, "at the beginning of ").is_some()
                        || nom_tag_lower(&lower, &lower, "at the end of ").is_some()
                    {
                        let triggers =
                            super::oracle_trigger::parse_trigger_lines(ability_text, "~");
                        for trigger in triggers {
                            modifications.push(ContinuousModification::GrantTrigger {
                                trigger: Box::new(trigger),
                            });
                        }
                    } else {
                        // CR 702: Quoted text that is a keyword (e.g. "Ward—Pay 2 life") should be
                        // granted as AddKeyword, not wrapped in an AbilityDefinition.
                        if let Some(keyword) =
                            super::oracle_keyword::parse_keyword_from_oracle(&lower)
                        {
                            modifications.push(ContinuousModification::AddKeyword { keyword });
                        } else {
                            modifications.push(ContinuousModification::GrantAbility {
                                definition: Box::new(parse_quoted_ability(ability_text)),
                            });
                        }
                    }
                }
            } else {
                start = Some(idx);
            }
        }
    }

    modifications
}

/// Parse a single quoted ability string into a typed AbilityDefinition.
///
/// If the text contains a cost separator (e.g., `{T}: ...`), it's treated as an
/// activated ability with the cost parsed separately. Otherwise it's treated as
/// a spell-like effect.
fn parse_quoted_ability(text: &str) -> AbilityDefinition {
    let lower = text.to_lowercase();

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
        let mut def = parse_effect_chain(effect_text, AbilityKind::Activated);
        def.cost = Some(cost);
        def.description = Some(text.to_string());
        def
    } else {
        // No cost separator — treat as spell-like ability text
        let mut def = parse_effect_chain(text, AbilityKind::Spell);
        def.description = Some(text.to_string());
        def
    }
}

/// Find the position of the cost/effect separator colon in ability text.
///
/// Looks for `: ` or `:\n` that appears after cost-like content (mana symbols,
/// {T}, numeric loyalty, or text-based costs like "Sacrifice this token").
/// Returns the byte offset of the colon, or None.
fn find_cost_separator(text: &str) -> Option<usize> {
    // Walk through looking for ':' that follows a closing brace or known cost prefix
    for (idx, ch) in text.char_indices() {
        if ch == ':' && idx > 0 {
            let prefix = &text[..idx];
            // Must have cost-like content before the colon
            let trimmed_prefix = prefix.trim();
            let lower_prefix = trimmed_prefix.to_lowercase();
            let has_cost = prefix.contains('{')
                || trimmed_prefix.parse::<i32>().is_ok()
                || trimmed_prefix.strip_prefix('+').is_some()
                || trimmed_prefix.strip_prefix('\u{2212}').is_some() // minus sign for loyalty
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
fn is_text_based_cost_prefix(lower_prefix: &str) -> bool {
    type E<'a> = nom_language::error::VerboseError<&'a str>;

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

/// CR 702: Split a keyword list like "flying and first strike" into individual keywords.
pub(crate) fn split_keyword_list(text: &str) -> Vec<Cow<'_, str>> {
    let text = text.trim().trim_end_matches('.');
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

/// CR 613.4c: For "+N/+M for each X and has [keyword]" patterns, the for-each
/// filter clause ends at " and has " / " and gains " / " and have ". Returns
/// the input slice truncated at the first matching boundary, or unchanged if
/// no boundary is present. Mirrors the keyword recognition in
/// `extract_keyword_clause` but in the inverse direction (returns the
/// pre-boundary span instead of the post-boundary one).
fn strip_trailing_keyword_clause(clause: &str) -> &str {
    for needle in [" and gains ", " and gain ", " and has ", " and have "] {
        if let Some(pos) = clause.find(needle) {
            return &clause[..pos];
        }
    }
    clause
}

fn extract_keyword_clause(text: &str) -> Option<&str> {
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
fn extract_lose_keyword_clause(text: &str) -> Option<&str> {
    let lower = text.to_lowercase();

    for needle in [" and loses ", " and lose "] {
        if let Some(pos) = lower.find(needle) {
            let after = &text[pos + needle.len()..];
            // Stop before "and gains" to avoid consuming the gain clause
            let end = lower[pos + needle.len()..]
                .find(" and gain")
                .unwrap_or(after.len());
            return Some(&after[..end]);
        }
    }

    for prefix in ["loses ", "lose "] {
        if let Some(rest) = nom_tag_lower(&lower, &lower, prefix) {
            let after = &text[prefix.len()..];
            // Stop before "and gains"/"and gain" to avoid consuming the gain clause
            let end = rest.find(" and gain").unwrap_or(after.len());
            return Some(&after[..end]);
        }
    }

    None
}

/// Parse a P/T modifier like "+2/+3", "-1/-1", "+3/-2" from Oracle text.
///
/// Delegates to the shared nom P/T combinator for signed P/T values.
/// Falls back to manual parsing for unsigned values (e.g. "0/0") which the
/// nom combinator doesn't handle (it requires explicit +/- signs).
fn parse_pt_mod(text: &str) -> Option<(i32, i32)> {
    let text = text.trim();
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

/// Map a keyword text to a Keyword enum variant using the FromStr impl.
/// Returns None only for `Keyword::Unknown`.
fn map_keyword(text: &str) -> Option<Keyword> {
    let word = text.trim().trim_end_matches('.').trim();
    if word.is_empty() {
        return None;
    }
    if word.eq_ignore_ascii_case("flashback") {
        return Some(Keyword::Flashback(
            crate::types::keywords::FlashbackCost::Mana(ManaCost::SelfManaCost),
        ));
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
            super::oracle_keyword::parse_keyword_from_oracle(word)
        }
        Ok(kw) => Some(kw),
        Err(_) => None, // Infallible, but satisfy the compiler
    }
}

fn parse_landwalk_keyword(text: &str) -> Option<Keyword> {
    match text.trim().to_ascii_lowercase().as_str() {
        "plainswalk" => Some(Keyword::Landwalk("Plains".to_string())),
        "islandwalk" => Some(Keyword::Landwalk("Island".to_string())),
        "swampwalk" => Some(Keyword::Landwalk("Swamp".to_string())),
        "mountainwalk" => Some(Keyword::Landwalk("Mountain".to_string())),
        "forestwalk" => Some(Keyword::Landwalk("Forest".to_string())),
        _ => None,
    }
}

/// Parse CDA power/toughness equality patterns like:
/// - "~'s power and toughness are each equal to the number of creatures you control."
/// - "~'s power is equal to the number of card types among cards in all graveyards
///   and its toughness is equal to that number plus 1."
/// - "~'s toughness is equal to the number of cards in your hand."
fn parse_cda_pt_equality(lower: &str, text: &str) -> Option<StaticDefinition> {
    // Detect framing
    let both = nom_primitives::scan_contains(lower, "power and toughness are each equal to");
    let power_only = !both && nom_primitives::scan_contains(lower, "power is equal to");
    let toughness_only =
        !both && !power_only && nom_primitives::scan_contains(lower, "toughness is equal to");

    if !both && !power_only && !toughness_only {
        return None;
    }

    // Extract the quantity text after "equal to "
    let quantity_start = if both {
        lower
            .find("are each equal to ")
            .map(|p| p + "are each equal to ".len())
    } else if power_only {
        lower
            .find("power is equal to ")
            .map(|p| p + "power is equal to ".len())
    } else {
        lower
            .find("toughness is equal to ")
            .map(|p| p + "toughness is equal to ".len())
    };
    let quantity_text = &lower[quantity_start?..];

    // Strip trailing clause for split P/T ("and its toughness is equal to...")
    let quantity_text = quantity_text
        .split(" and its toughness")
        .next()
        .unwrap_or(quantity_text)
        .trim_end_matches('.');

    let qty = parse_cda_quantity(quantity_text)?;

    let mut modifications = Vec::new();

    if both {
        modifications.push(ContinuousModification::SetDynamicPower { value: qty.clone() });
        modifications.push(ContinuousModification::SetDynamicToughness { value: qty });
    } else if power_only {
        modifications.push(ContinuousModification::SetDynamicPower { value: qty.clone() });
        // Check for split P/T: "and its toughness is equal to that number plus N"
        if let Some(after_plus) = strip_after(lower, "that number plus ") {
            let n_str = after_plus
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap_or("0");
            let offset = n_str.parse::<i32>().unwrap_or(0);
            modifications.push(ContinuousModification::SetDynamicToughness {
                value: QuantityExpr::Offset {
                    inner: Box::new(qty),
                    offset,
                },
            });
        }
    } else {
        // toughness_only
        modifications.push(ContinuousModification::SetDynamicToughness { value: qty });
    }

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(modifications)
            .cda()
            .description(text.to_string()),
    )
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
fn try_parse_max_hand_size(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    type NomErr<'a> = nom_language::error::VerboseError<&'a str>;

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
fn try_parse_graveyard_cast_permission(text: &str, lower: &str) -> Option<StaticDefinition> {
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

    let (filter, _) = parse_type_phrase(&cleaned);

    // Parse optional alt-cost rider from the text after "from your graveyard".
    let rider_kind = parse_alt_cost_rider(trailing).ok().map(|(_, k)| k);

    let affected = if let Some(kind) = rider_kind {
        inject_keyword_kind_filter_prop(filter, kind)
    } else {
        filter
    };

    Some(
        StaticDefinition::new(StaticMode::GraveyardCastPermission {
            frequency,
            play_mode,
        })
        .affected(affected)
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
fn try_parse_top_of_library_cast_permission(text: &str, lower: &str) -> Option<StaticDefinition> {
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
        return Some(
            StaticDefinition::new(StaticMode::TopOfLibraryCastPermission {
                play_mode: CardPlayMode::Play,
                alt_cost,
            })
            .affected(TargetFilter::Any)
            .description(text.to_string()),
        );
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

    Some(
        StaticDefinition::new(StaticMode::TopOfLibraryCastPermission {
            play_mode,
            alt_cost,
        })
        .affected(filter)
        .description(text.to_string()),
    )
}

/// CR 118.9 + CR 119.4: Helper to parse the optional alt-cost rider that may
/// follow a top-of-library cast permission. Bolas's Citadel form: "If you
/// cast a spell this way, pay life equal to its mana value rather than pay
/// its mana cost." Scans for the rider's opening "if you cast" inside the
/// trailing text and the full line, slicing from that index forward so the
/// existing `try_parse_alt_cost_rider` (which expects the input to start at
/// the rider) sees a clean prefix.
fn parse_top_of_library_alt_cost_rider(
    trailing: &str,
    text: &str,
) -> Option<crate::types::ability::AbilityCost> {
    fn try_from(input: &str) -> Option<crate::types::ability::AbilityCost> {
        // Scan past any leading text (the "you may play ... library."
        // sentence) until the rider's opening anchor; pure-nom
        // `take_until + alt` keeps this on the combinator path. Both
        // anchors map to the same underlying rider parser.
        let lower = input.to_lowercase();
        type E<'a> = nom_language::error::VerboseError<&'a str>;
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
fn parse_alt_cost_rider(input: &str) -> OracleResult<'_, KeywordKind> {
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
fn inject_keyword_kind_filter_prop(filter: TargetFilter, kind: KeywordKind) -> TargetFilter {
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

/// CR 601.2b + CR 118.9a: Parse Omniscience-class restricted free-cast static
/// abilities — "you may cast [filter] [from your hand]? without paying [its|their]
/// mana cost[s]?" — covering Omniscience and the Tamiyo, Field Researcher emblem
/// (no filter, hand qualifier), Zaffai-and-the-Tempests (typed filter, hand
/// qualifier, once-per-turn frequency), and Dracogenesis (subtype filter, no
/// zone qualifier — implicit hand per CR 601.2: "To cast a spell is to take it
/// from where it is (usually the hand)..."). Continuous static — not a one-shot
/// effect.
fn try_parse_cast_free_permission(text: &str, lower: &str) -> Option<StaticDefinition> {
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

    // The zone qualifier "from your hand" is optional. CR 601.2 makes the hand
    // the implicit cast zone, so Dracogenesis's "you may cast Dragon spells
    // without paying their mana costs" carries the same semantics as Omniscience's
    // "you may cast spells from your hand without paying their mana costs".
    //
    // Both branches must terminate at " without paying" — that token is the
    // single anchor for the static. The qualified branch keeps a permissive
    // type-parse (warns on unconsumed remainder) for established Omniscience /
    // Zaffai / Expertise-cycle shapes; the unqualified branch is strict (rejects
    // unconsumed remainder) so complex filters like Fires of Invention's
    // "spells with mana value less than or equal to the number of lands you
    // control" decline cleanly instead of misparsing as `TargetFilter::Any`.
    let (filter_text, zone_qualified) = if let Ok((_, (before, hand_rest))) =
        nom_primitives::split_once_on(rest, " from your hand")
    {
        // "without paying" must follow "from your hand" — reject unusual word orders
        if !nom_primitives::scan_contains(hand_rest, "without paying") {
            return None;
        }
        (before, true)
    } else {
        let (_, (before, _)) = nom_primitives::split_once_on(rest, " without paying").ok()?;
        (before, false)
    };

    // Intentional: "spells" with no qualifier → Any filter (Omniscience) — no warning needed.
    if filter_text == "spells" {
        return Some(
            StaticDefinition::new(StaticMode::CastFromHandFree { frequency })
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
    if !remainder.trim().is_empty() {
        if !zone_qualified {
            // Unqualified branch is strict: an unconsumed remainder signals a
            // complex filter we don't yet model (e.g. Fires of Invention's
            // dynamic mana-value bound). Decline rather than emit a partial
            // `Any` filter that would be wrong in a different way.
            return None;
        }
        push_warning(format!(
            "ignored-remainder: '{}' after type parse in cast-free-permission",
            remainder.trim()
        ));
    }

    Some(
        StaticDefinition::new(StaticMode::CastFromHandFree { frequency })
            .affected(filter)
            .description(text.to_string()),
    )
}

fn parse_first_qualified_spell_filter(lower: &str) -> Option<TargetFilter> {
    let after_prefix = nom_tag_lower(lower, lower, "the first ")?;
    let qualifier = after_prefix
        .split_once(" you cast during each of your turns cost")
        .or_else(|| after_prefix.split_once(" you cast during each of your turns costs"))?
        .0
        .trim();

    let (filter, remainder) = parse_type_phrase(qualifier);
    if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
        Some(filter)
    } else {
        None
    }
}

fn first_qualified_spell_condition(filter: &TargetFilter) -> StaticCondition {
    StaticCondition::And {
        conditions: vec![
            StaticCondition::DuringYourTurn,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        filter: Some(filter.clone()),
                    },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            },
        ],
    }
}

/// CR 117.7 + CR 601.2f: Detect a self-spell cost-modification subject.
/// Matches the leading "this spell ", "this card ", or "~ " prefix used when
/// a spell reduces/raises its own cast cost (e.g., Tolarian Terror:
/// "This spell costs {1} less to cast for each instant and sorcery card in
/// your graveyard."). Callers use this to flag self-reference so the static
/// is emitted with `affected = SelfRef` and `active_zones = [Hand, Stack]`
/// instead of the default battlefield scope.
fn parse_self_spell_cost_subject(lower: &str) -> Option<()> {
    nom_on_lower(lower, lower, |i| {
        value((), alt((tag("this spell "), tag("this card "), tag("~ ")))).parse(i)
    })
    .map(|_| ())
}

/// CR 601.2f: Parse cost modification statics from Oracle text.
/// Handles all four sub-patterns:
/// 1. Type-filtered: "Creature spells you cast cost {1} less to cast"
/// 2. Color-filtered: "White spells your opponents cast cost {1} more to cast"
/// 3. Global taxing: "Noncreature spells cost {1} more to cast" (Thalia)
/// 4. Broad: "Spells you cast cost {1} less to cast"
/// 5. Self-spell: "This spell costs {N} less to cast for each ..." (Tolarian Terror)
///    — emitted with `affected = SelfRef`, `active_zones = [Hand, Stack]`.
///
/// Dynamic "for each" counts are extracted when present.
fn try_parse_cost_modification(text: &str, lower: &str) -> Option<StaticDefinition> {
    let is_raise = nom_primitives::scan_contains(lower, "more to cast")
        || nom_primitives::scan_contains(lower, "more to activate");
    let is_reduce = nom_primitives::scan_contains(lower, "less to cast")
        || nom_primitives::scan_contains(lower, "less to activate");
    if !is_raise && !is_reduce {
        return None;
    }

    // CR 601.2f + CR 117.7: Detect self-spell cost reduction ("this spell costs {N} less ...").
    // Distinct from battlefield cost modification (e.g., "creature spells you cast cost {1} less")
    // because the static must apply to the card while it is in hand (or on the stack during
    // casting), not once it has entered the battlefield. The caller wires this into
    // `active_zones = [Hand, Stack]` with `affected = SelfRef` so the casting-time scanner
    // finds it on the spell being cast.
    let is_self_spell = parse_self_spell_cost_subject(lower).is_some();

    let amount_is_variable_x = nom_primitives::scan_contains(lower, "{x}");

    // Extract the mana amount from the text (look for {N} pattern)
    let amount = if let Some(brace_start) = text.find('{') {
        let cost_fragment = &text[brace_start..];
        parse_mana_symbols(cost_fragment)
            .map(|(cost, _)| cost)
            .unwrap_or_else(|| ManaCost::generic(1))
    } else {
        ManaCost::generic(1)
    };

    // Determine player scope from "you cast", "your opponents cast", or bare
    let controller = if nom_primitives::scan_contains(lower, "your opponents cast")
        || nom_primitives::scan_contains(lower, "opponents cast")
    {
        Some(ControllerRef::Opponent)
    } else if nom_primitives::scan_contains(lower, "you cast") {
        Some(ControllerRef::You)
    } else {
        // Bare "spells cost more/less" — affects all players' spells.
        // For "Noncreature spells cost {1} more", both players are affected
        // in the casting check — no controller restriction on affected.
        None
    };

    let first_qualified_spell_filter = parse_first_qualified_spell_filter(lower);

    // Extract "from [zone(s)]" clause between player scope and "cost".
    // E.g., "cast from graveyards or from exile" → [Graveyard, Exile]
    // This must be extracted before type parsing so it doesn't pollute type_desc.
    let cast_from_zones: Vec<Zone> = {
        let mut zones = Vec::new();
        if let Some(cost_idx) = lower.find(" cost") {
            let prefix = &lower[..cost_idx];
            // Look for "from <zone> or from <zone>" or "from <zone>" after "cast".
            // Use the first " from " to capture compound patterns like
            // "from graveyards or from exile".
            if let Some(from_idx) = prefix.find(" from ") {
                let from_text = &prefix[from_idx..];
                // Skip "from anywhere other than" — that's a negation pattern
                // requiring a Not filter, not a direct zone match.
                if !nom_primitives::scan_contains(from_text, "anywhere other than") {
                    if nom_primitives::scan_contains(from_text, "graveyard") {
                        zones.push(Zone::Graveyard);
                    }
                    if nom_primitives::scan_contains(from_text, "exile") {
                        zones.push(Zone::Exile);
                    }
                    if nom_primitives::scan_contains(from_text, "hand") {
                        zones.push(Zone::Hand);
                    }
                    if nom_primitives::scan_contains(from_text, "command zone") {
                        zones.push(Zone::Command);
                    }
                }
            }
        }
        zones
    };

    // Extract spell type filter from the text before "cost"
    // E.g., "Creature spells you cast" → Creature, "Instant and sorcery spells" → AnyOf(Instant, Sorcery)
    let spell_filter = if let Some(filter) = first_qualified_spell_filter.clone() {
        Some(filter)
    } else if let Some(cost_idx) = lower.find(" cost") {
        let prefix = &lower[..cost_idx];
        // Strip "from [zones]" clause (only if zones were detected), player scope, then "spells"
        let without_from = if !cast_from_zones.is_empty() {
            if let Some(from_idx) = prefix.find(" from ") {
                &prefix[..from_idx]
            } else {
                prefix
            }
        } else {
            prefix
        };
        let type_desc = without_from
            .trim_end_matches(" you cast")
            .trim_end_matches(" your opponents cast")
            .trim_end_matches(" opponents cast")
            .trim_end_matches(" spells")
            .trim_end_matches(" spell")
            .trim();
        // "spells" alone means no type restriction (bare "Spells you cast cost...")
        if type_desc.is_empty() || type_desc == "spells" || type_desc == "spell" {
            None
        } else {
            // First try parse_type_phrase for standard type patterns
            let (filter, _) = parse_type_phrase(type_desc);
            match &filter {
                // Single type: "creature", "noncreature", "artifact"
                TargetFilter::Typed(tf)
                    if !tf.type_filters.is_empty() || !tf.properties.is_empty() =>
                {
                    Some(filter)
                }
                // Combined types: "instant and sorcery", "artifact or enchantment"
                TargetFilter::Or { filters } if !filters.is_empty() => Some(filter),
                _ => {
                    // Fallback: check for bare color names ("white", "blue", etc.)
                    parse_named_color(type_desc).map(|color| {
                        TargetFilter::Typed(
                            TypedFilter::card().properties(vec![FilterProp::HasColor { color }]),
                        )
                    })
                }
            }
        }
    } else {
        None
    };

    // Merge cast-from-zone restriction into the spell filter.
    // If zones were extracted, add InZone/InAnyZone to ensure the cost modification
    // only applies when the spell is being cast from the specified zone(s).
    let spell_filter = if !cast_from_zones.is_empty() {
        let zone_prop = if cast_from_zones.len() == 1 {
            FilterProp::InZone {
                zone: cast_from_zones[0],
            }
        } else {
            FilterProp::InAnyZone {
                zones: cast_from_zones,
            }
        };
        match spell_filter {
            Some(TargetFilter::Typed(mut tf)) => {
                tf.properties.push(zone_prop);
                Some(TargetFilter::Typed(tf))
            }
            Some(other) => {
                // Wrap non-Typed filters with an And that adds the zone constraint.
                Some(TargetFilter::And {
                    filters: vec![
                        other,
                        TargetFilter::Typed(TypedFilter::card().properties(vec![zone_prop])),
                    ],
                })
            }
            None => {
                // No type filter, just zone restriction (e.g., "Spells ... cast from exile cost more")
                Some(TargetFilter::Typed(
                    TypedFilter::card().properties(vec![zone_prop]),
                ))
            }
        }
    } else {
        spell_filter
    };

    // Detect dynamic "for each" count pattern
    // "for each artifact you control" → QuantityRef::ObjectCount
    let cost_tp = TextPair::new(text, lower);
    let mut dynamic_count = if let Some((_, after_for_each)) = cost_tp.split_around("for each ") {
        // Strip trailing period/punctuation
        let count_text = after_for_each.original.trim_end_matches('.');
        super::oracle_quantity::parse_for_each_clause(count_text)
            .or_else(|| {
                parse_cda_quantity(count_text).and_then(|expr| match expr {
                    QuantityExpr::Ref { qty } => Some(qty),
                    _ => None,
                })
            })
            .or_else(|| super::oracle_quantity::parse_quantity_ref(count_text))
            .or_else(|| {
                if let Some(prefixed) = count_text.strip_prefix("the number of ") {
                    super::oracle_quantity::parse_quantity_ref(prefixed)
                } else {
                    None
                }
            })
            .or_else(|| {
                let (count_filter, _) = parse_type_phrase(count_text);
                Some(QuantityRef::ObjectCount {
                    filter: count_filter,
                })
            })
    } else {
        None
    };

    if dynamic_count.is_none() && amount_is_variable_x {
        let (_, where_x_text) = super::oracle_effect::strip_trailing_where_x(cost_tp);
        if let Some(expression) = where_x_text {
            if let Some(QuantityExpr::Ref { qty }) = parse_cda_quantity(&expression) {
                dynamic_count = Some(qty);
            }
        }
    }

    let amount = if amount_is_variable_x {
        ManaCost::generic(1)
    } else {
        amount
    };

    let mode = if is_raise {
        StaticMode::RaiseCost {
            amount,
            spell_filter: spell_filter.clone(),
            dynamic_count: dynamic_count.clone(),
        }
    } else {
        StaticMode::ReduceCost {
            amount,
            spell_filter: spell_filter.clone(),
            dynamic_count: dynamic_count.clone(),
        }
    };

    // Build the affected filter for the static definition.
    // This controls which objects are "affected" — for cost modification statics,
    // this is the source permanent's controller scope (used by the registry).
    // CR 117.7: Self-spell cost reduction ("This spell costs {N} less ...") uses
    // SelfRef so the casting-time self-cost scanner matches it on the spell itself.
    let affected = if is_self_spell {
        TargetFilter::SelfRef
    } else {
        match controller {
            Some(ControllerRef::You) => {
                TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::You))
            }
            Some(ControllerRef::Opponent) => {
                TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::Opponent))
            }
            // CR 109.4: TargetPlayer has no defined semantics here (cost-modification
            // static scoping). Fall back to an untyped filter; the parser should not
            // emit this variant for cost statics.
            Some(ControllerRef::TargetPlayer) => TargetFilter::Typed(TypedFilter::card()),
            None => TargetFilter::Typed(TypedFilter::card()),
        }
    };

    let mut definition = StaticDefinition::new(mode)
        .affected(affected)
        .description(text.to_string());

    // CR 117.7 + CR 601.2f: A self-spell cost reduction must apply while the
    // card is in hand (pre-cast affordability checks) and on the stack (final
    // cost determination during casting). Without opting in via `active_zones`,
    // layer collection would ignore the static outside the battlefield, and
    // the card would never reduce its own cost.
    if is_self_spell {
        definition.active_zones = vec![Zone::Hand, Zone::Stack];
    }
    if let Some(filter) = first_qualified_spell_filter.as_ref() {
        definition.condition = Some(first_qualified_spell_condition(filter));
    }

    // Extract trailing "if [condition]" / "as long as [condition]" clause from
    // cost modification lines.
    // Patterns:
    // - "This spell costs {N} less to cast if you control a Wizard."
    // - "Spells you cast cost {1} less to cast as long as there are three or more Lesson cards in your graveyard."
    // Uses the shared nom condition combinator to handle the full class of conditions.
    if definition.condition.is_none() {
        if let Some((cond_pos, marker)) = [" as long as ", " if "]
            .into_iter()
            .filter_map(|marker| lower.rfind(marker).map(|pos| (pos, marker)))
            .max_by_key(|(pos, _)| *pos)
        {
            let cond_text = lower[cond_pos + marker.len()..]
                .trim()
                .trim_end_matches('.');
            if let Ok((rest, sc)) = nom_condition::parse_inner_condition(cond_text) {
                if rest.trim().is_empty() || rest.trim() == "." {
                    definition.condition = Some(sc);
                }
            }
        }
    }

    Some(definition)
}

/// Parse a basic land type name (case-insensitive) to its enum variant.
fn parse_basic_land_type(name: &str) -> Option<BasicLandType> {
    match name.to_ascii_lowercase().as_str() {
        "plains" => Some(BasicLandType::Plains),
        "island" => Some(BasicLandType::Island),
        "swamp" => Some(BasicLandType::Swamp),
        "mountain" => Some(BasicLandType::Mountain),
        "forest" => Some(BasicLandType::Forest),
        _ => None,
    }
}

/// Parse a basic land type name, accepting both singular and plural forms.
/// "Mountains" → Mountain, "Islands" → Island. "Plains" is already valid singular.
fn parse_basic_land_type_plural(name: &str) -> Option<BasicLandType> {
    parse_basic_land_type(name).or_else(|| name.strip_suffix('s').and_then(parse_basic_land_type))
}

/// CR 305.7: Parse a comma-and-separated list of basic land types.
/// "Mountain, Forest, and Plains" → [Mountain, Forest, Plains].
/// Also handles single types: "Island" → [Island].
fn parse_basic_land_type_list(text: &str) -> Option<Vec<BasicLandType>> {
    // Try single type first (most common case)
    if let Some(single) = parse_basic_land_type_plural(text) {
        return Some(vec![single]);
    }
    // Split on ", " and " and " for multi-type lists
    let mut types = Vec::new();
    for part in text.split(", ") {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("and ") {
            types.push(parse_basic_land_type(rest.trim())?);
        } else if let Some((before, after)) = part.split_once(" and ") {
            types.push(parse_basic_land_type(before.trim())?);
            types.push(parse_basic_land_type(after.trim())?);
        } else {
            types.push(parse_basic_land_type(part)?);
        }
    }
    if types.len() >= 2 {
        Some(types)
    } else {
        None
    }
}

/// CR 205.1a: Parse "All permanents are [type] in addition to their other types."
/// Handles global type-addition effects like Mycosynth Lattice ("artifacts") and
/// Enchanted Evening ("enchantments").
fn parse_all_permanents_are_type(tp: &TextPair<'_>, description: &str) -> Option<StaticDefinition> {
    let rest_tp = nom_tag_tp(tp, "all permanents are ")?;
    let rest = rest_tp.lower.trim_end_matches('.');
    let type_part = rest.strip_suffix(" in addition to their other types")?;
    // Map the type word to a CoreType
    let core_type = match type_part.trim() {
        "artifacts" => CoreType::Artifact,
        "enchantments" => CoreType::Enchantment,
        "creatures" => CoreType::Creature,
        "lands" => CoreType::Land,
        _ => return None,
    };
    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter::permanent()))
            .modifications(vec![ContinuousModification::AddType { core_type }])
            .description(description.to_string()),
    )
}

/// CR 613.1e + CR 105.1 / CR 105.2c / CR 105.3: Parse "All [subject] are [color(s)]."
/// — a global color-defining static ability (Layer 5).
///
/// - CR 105.1 enumerates the five colors.
/// - CR 105.2c: "A colorless object has no color." → empty color set.
/// - CR 105.3 authorizes color-changing effects (new color replaces previous
///   colors unless the effect says "in addition").
/// - CR 613.1e places color-changing effects in Layer 5.
///
/// Covers the class of "All X are Y" color-setting statics — Darkest Hour
/// ("All creatures are black."), Thran Lens ("All permanents are colorless."),
/// Ghostflame Sliver ("All Slivers are colorless."), and every future card
/// sharing this shape. Composes existing building blocks rather than writing
/// one-off string dispatch:
///
/// - `nom_target::parse_type_filter_word` recognizes every plural core-type
///   subject (creatures, permanents, lands, artifacts, enchantments,
///   planeswalkers, battles) AND every plural subtype in the shared subtype
///   table (Slivers, Elves, Treasures, Zombies, ...).
/// - `parse_color_predicate` composes a `tag("colorless")` combinator with
///   the shared `parse_color_list` (giving single colors, "X and Y", and
///   "X, Y, and Z" forms for free per CR 105.1).
/// - `typed_filter_for_subtype` routes artifact/land/enchantment subtypes to
///   their correct core type (e.g., Treasure → Artifact, not Creature).
///
/// Dispatch ordering constraints are documented at the call site in
/// `parse_static_line_inner` and pinned by three regression tests below.
fn parse_all_subject_are_color(tp: &TextPair<'_>, description: &str) -> Option<StaticDefinition> {
    let rest_tp = nom_tag_tp(tp, "all ")?;
    // Subject: single shared combinator for both core types and plural subtypes.
    let (after_subject, type_filter) = nom_target::parse_type_filter_word(rest_tp.lower).ok()?;
    // Copula — require " are " with surrounding whitespace so we never eat
    // words like "aren't" or "area".
    let after_verb = nom_tag_lower(after_subject, after_subject, " are ")?;
    // Strip the terminal period (structural cleanup on a post-combinator
    // chunk — the subject and copula have already been consumed), then the
    // predicate must fully parse as a color expression or follow-on clauses
    // route elsewhere.
    let predicate = after_verb.trim().trim_end_matches('.');
    let colors = parse_color_predicate(predicate)?;

    let affected = match type_filter {
        TypeFilter::Subtype(s) => TargetFilter::Typed(typed_filter_for_subtype(&s)),
        other => TargetFilter::Typed(TypedFilter::new(other)),
    };
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::SetColor { colors }])
            .description(description.to_string()),
    )
}

/// CR 105.1 / CR 105.2c: Parse a color expression terminating an
/// "All [subject] are ___" static. Accepts either the literal word "colorless"
/// (→ empty color set, CR 105.2c) or any color list recognized by
/// `parse_color_list` — single color, "X and Y", or "X, Y, and Z" (CR 105.1).
/// Input must be fully consumed by the combinator path; trailing content
/// returns `None` so the outer dispatcher falls through.
fn parse_color_predicate(text: &str) -> Option<Vec<ManaColor>> {
    if let Some(rest) = nom_tag_lower(text, text, "colorless") {
        if rest.is_empty() {
            return Some(Vec::new());
        }
    }
    parse_color_list(text)
}

/// CR 305.7: Parse "[Subject] lands are [type]" land type-changing static abilities.
/// Handles replacement ("Nonbasic lands are Mountains"), additive ("Each land is a
/// Swamp in addition to its other land types"), and all-basic-types ("Lands you control
/// are every basic land type in addition to their other types").
fn parse_land_type_change(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    let (subject_tp, rest_tp) = tp
        .split_around(" are ")
        .or_else(|| tp.split_around(" is a "))
        .or_else(|| tp.split_around(" is an "))
        .or_else(|| tp.split_around(" is "))?;
    let subject = subject_tp.original;
    let rest = rest_tp.original.trim().trim_end_matches('.');

    // Only proceed if subject is a land-type-change subject (avoids matching non-land patterns).
    let affected = parse_land_type_change_subject(subject)?;
    let lower_rest = rest.to_lowercase();

    // "every basic land type in addition to their other types"
    if nom_tag_lower(&lower_rest, &lower_rest, "every basic land type").is_some()
        && nom_primitives::scan_contains(&lower_rest, "in addition to")
    {
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddAllBasicLandTypes])
                .description(text.to_string()),
        );
    }

    // "[Type] in addition to {its/their} other {land }types" → AddSubtype (additive)
    if let Some(type_part) = strip_in_addition_suffix(&lower_rest) {
        let basic_type = parse_basic_land_type_plural(type_part.trim())?;
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddSubtype {
                    subtype: basic_type.as_subtype_str().to_string(),
                }])
                .description(text.to_string()),
        );
    }

    // CR 305.7: Replacement semantics — "[Type]" or "[Types]" → SetBasicLandType
    // Try multi-type list first: "Mountain, Forest, and Plains"
    if let Some(types) = parse_basic_land_type_list(rest.trim()) {
        if types.len() == 1 {
            return Some(
                StaticDefinition::continuous()
                    .affected(affected)
                    .modifications(vec![ContinuousModification::SetBasicLandType {
                        land_type: types[0],
                    }])
                    .description(text.to_string()),
            );
        }
        // CR 305.7: Multiple types — first SetBasicLandType clears old subtypes,
        // subsequent AddSubtype entries add the remaining types.
        let mut mods = vec![ContinuousModification::SetBasicLandType {
            land_type: types[0],
        }];
        for &lt in &types[1..] {
            mods.push(ContinuousModification::AddSubtype {
                subtype: lt.as_subtype_str().to_string(),
            });
        }
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(mods)
                .description(text.to_string()),
        );
    }

    None
}

/// Parse the subject of a land type-change line into a TargetFilter.
fn parse_land_type_change_subject(subject: &str) -> Option<TargetFilter> {
    match subject.to_lowercase().as_str() {
        "nonbasic lands" => Some(TargetFilter::Typed(TypedFilter::land().properties(vec![
            FilterProp::NotSupertype {
                value: Supertype::Basic,
            },
        ]))),
        "lands you control" => Some(TargetFilter::Typed(
            TypedFilter::land().controller(ControllerRef::You),
        )),
        "each land" | "all lands" => Some(TargetFilter::Typed(TypedFilter::land())),
        // CR 305.7: Aura enchantments that change the enchanted land's type.
        "enchanted land" => Some(TargetFilter::Typed(
            TypedFilter::land().properties(vec![FilterProp::EnchantedBy]),
        )),
        _ => None,
    }
}

/// CR 604.1: Strip turn-condition suffixes from predicate text.
///
/// Handles "during your turn" and "during turns other than yours" suffixes
/// on keyword/modification predicates. Returns the stripped predicate and
/// the corresponding `StaticCondition`, or the original text with `None`.
fn strip_suffix_turn_condition(text: &str) -> (String, Option<StaticCondition>) {
    let trimmed = text.trim_end_matches('.');
    if let Some(rest) = trimmed.strip_suffix(" during your turn") {
        (format!("{rest}."), Some(StaticCondition::DuringYourTurn))
    } else if let Some(rest) = trimmed.strip_suffix(" during turns other than yours") {
        (
            format!("{rest}."),
            Some(StaticCondition::Not {
                condition: Box::new(StaticCondition::DuringYourTurn),
            }),
        )
    } else {
        (text.to_string(), None)
    }
}

/// Strip "in addition to {its/their} other {land }types" suffix,
/// returning the type name before it.
fn strip_in_addition_suffix(text: &str) -> Option<&str> {
    [
        " in addition to its other land types",
        " in addition to its other types",
        " in addition to their other land types",
        " in addition to their other types",
    ]
    .iter()
    .find_map(|suffix| text.strip_suffix(suffix))
}

/// CR 502.3: Extract a trailing condition from a "doesn't untap during [untap step]" clause.
/// Handles patterns like:
/// - "doesn't untap during your untap step as long as [condition]"
/// - "doesn't untap during your untap step if [condition]"
fn extract_cant_untap_condition(lower: &str) -> Option<StaticCondition> {
    // Find the end of the "untap step" phrase
    let untap_phrases = [
        "its controller's untap step",
        "its controller\u{2019}s untap step",
        "their controllers' untap steps",
        "your untap step",
    ];
    let mut after_untap = None;
    for phrase in &untap_phrases {
        if let Some(pos) = lower.find(phrase) {
            let end = pos + phrase.len();
            after_untap = Some(lower[end..].trim().trim_end_matches('.'));
            break;
        }
    }
    let remaining = after_untap?;
    if remaining.is_empty() {
        return None;
    }
    // Strip "as long as" or "if" prefix
    let condition_text = nom_tag_lower(remaining, remaining, "as long as ")
        .or_else(|| nom_tag_lower(remaining, remaining, "if "))?;
    parse_static_condition(condition_text).or_else(|| {
        Some(StaticCondition::Unrecognized {
            text: condition_text.to_string(),
        })
    })
}

/// CR 508.1d / CR 509.1c: Parse subject-scoped "attack/block each combat if able" patterns.
///
/// Handles "All creatures attack each combat if able", "Creatures you control attack each
/// combat if able", "Creatures your opponents control attack each combat if able", and the
/// combined "attacks or blocks each combat if able" variant.
fn try_parse_scoped_must_attack_block(lower: &str, text: &str) -> Option<Vec<StaticDefinition>> {
    // Strip trailing period for matching.
    let clean = lower.trim_end_matches('.');
    let clean_text = text.trim_end_matches('.');

    // Try to extract the verb phrase suffix and determine the mode(s).
    let (subject_lower, modes) =
        if let Some(subj) = clean.strip_suffix(" attack each combat if able") {
            (subj, vec![StaticMode::MustAttack])
        } else if let Some(subj) = clean.strip_suffix(" attacks each combat if able") {
            (subj, vec![StaticMode::MustAttack])
        } else if let Some(subj) = clean.strip_suffix(" attack each turn if able") {
            (subj, vec![StaticMode::MustAttack])
        } else if let Some(subj) = clean.strip_suffix(" block each combat if able") {
            (subj, vec![StaticMode::MustBlock])
        } else if let Some(subj) = clean.strip_suffix(" blocks each combat if able") {
            (subj, vec![StaticMode::MustBlock])
        } else if let Some(subj) = clean.strip_suffix(" block each turn if able") {
            (subj, vec![StaticMode::MustBlock])
        } else if let Some(subj) = clean.strip_suffix(" attacks or blocks each combat if able") {
            (subj, vec![StaticMode::MustAttack, StaticMode::MustBlock])
        } else {
            let subj = clean.strip_suffix(" attack or block each combat if able")?;
            (subj, vec![StaticMode::MustAttack, StaticMode::MustBlock])
        };
    let subject = &clean_text[..subject_lower.len()];

    // Determine the affected filter from the subject phrase.
    let affected = match subject_lower {
        "all creatures" | "each creature" => TargetFilter::Typed(TypedFilter::creature()),
        "creatures you control" => {
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
        }
        "creatures your opponents control" => {
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent))
        }
        // Self-ref: "this creature" / card name — handled by the existing
        // RuleStaticPredicate path, so we skip here.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{CountScope, TypeFilter, ZoneRef};

    /// CR 205.1a + CR 205.2 + CR 205.3 + CR 613.1c: "becomes a [subtype]*
    /// [core-type]+ in addition to its other types" must decompose into
    /// typed `AddType`/`AddSubtype` modifications. Jump Scare regression.
    #[test]
    fn continuous_mods_decompose_becomes_compound_type_phrase() {
        let mods = parse_continuous_modifications(
            "get +2/+2, gains flying, and becomes a Horror enchantment creature in addition to its other types",
        );
        assert!(
            mods.contains(&ContinuousModification::AddSubtype {
                subtype: "Horror".into()
            }),
            "expected AddSubtype(Horror) in {mods:?}"
        );
        assert!(
            mods.contains(&ContinuousModification::AddType {
                core_type: CoreType::Enchantment
            }),
            "expected AddType(Enchantment) in {mods:?}"
        );
        assert!(
            mods.contains(&ContinuousModification::AddType {
                core_type: CoreType::Creature
            }),
            "expected AddType(Creature) in {mods:?}"
        );
        // Must not regress to the single-string whole-phrase subtype.
        assert!(
            !mods.contains(&ContinuousModification::AddSubtype {
                subtype: "Horror enchantment creature".into()
            }),
            "must not emit whole-phrase AddSubtype"
        );
    }

    #[test]
    fn static_merfolk_lord() {
        let def = parse_static_line("Other Merfolk you control get +1/+1.").unwrap();
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
    }

    #[test]
    fn static_bonesplitter() {
        let def = parse_static_line("Equipped creature gets +2/+0.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 0 }));
    }

    #[test]
    fn static_rancor() {
        let def = parse_static_line("Enchanted creature gets +2/+0 and has trample.").unwrap();
        assert!(def.modifications.len() >= 3); // +2, +0, trample
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample
            }));
    }

    #[test]
    fn static_cant_be_blocked_by_power_le() {
        // CR 509.1b: Questing Beast — can't be blocked by creatures with power 2 or less
        let def =
            parse_static_line("Questing Beast can't be blocked by creatures with power 2 or less.")
                .unwrap();
        assert!(
            matches!(
                &def.mode,
                StaticMode::CantBeBlockedBy { filter }
                if matches!(filter, TargetFilter::Typed(tf) if tf.properties.contains(&FilterProp::PowerLE { value: QuantityExpr::Fixed { value: 2 } }))
            ),
            "Expected CantBeBlockedBy with PowerLE(2), got {:?}",
            def.mode
        );
    }

    #[test]
    fn static_cant_be_blocked_by_power_ge() {
        // CR 509.1b: April O'Neil — can't be blocked by creatures with power 3 or greater
        let def = parse_static_line(
            "April O'Neil can't be blocked by creatures with power 3 or greater.",
        )
        .unwrap();
        assert!(
            matches!(
                &def.mode,
                StaticMode::CantBeBlockedBy { filter }
                if matches!(filter, TargetFilter::Typed(tf) if tf.properties.contains(&FilterProp::PowerGE { value: QuantityExpr::Fixed { value: 3 } }))
            ),
            "Expected CantBeBlockedBy with PowerGE(3), got {:?}",
            def.mode
        );
    }

    #[test]
    fn static_cant_be_blocked_by_greater_power() {
        // CR 509.1b: Prehistoric Pet — can't be blocked by creatures with greater power
        let def =
            parse_static_line("This creature can't be blocked by creatures with greater power.")
                .unwrap();
        assert!(
            matches!(
                &def.mode,
                StaticMode::CantBeBlockedBy { filter }
                if matches!(filter, TargetFilter::Typed(tf) if tf.properties.contains(&FilterProp::PowerGTSource))
            ),
            "Expected CantBeBlockedBy with PowerGTSource, got {:?}",
            def.mode
        );
    }

    #[test]
    fn static_creatures_you_control() {
        let def = parse_static_line("Creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                ..
            }))
        ));
    }

    // --- New pattern tests ---

    #[test]
    fn static_self_referential_has_keyword() {
        let def = parse_static_line("Phage the Untouchable has deathtouch.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Deathtouch,
            }));
    }

    #[test]
    fn static_enchanted_permanent() {
        let def = parse_static_line("Enchanted permanent has hexproof.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(ref tf)) if tf.type_filters.contains(&TypeFilter::Permanent)
        ));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }));
    }

    #[test]
    fn static_all_creatures() {
        let def = parse_static_line("All creatures get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(ref tf)) if tf.type_filters.contains(&TypeFilter::Creature) && tf.controller.is_none()
        ));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
    }

    #[test]
    fn static_subtype_creatures_you_control() {
        let def = parse_static_line("Elf creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(ref tf))
                if tf.type_filters.contains(&TypeFilter::Creature)
                    && tf.type_filters.contains(&TypeFilter::Subtype("Elf".to_string()))
                    && tf.controller == Some(ControllerRef::You)
        ));
    }

    #[test]
    fn static_color_creatures_you_control() {
        let def = parse_static_line("White creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(ref tf))
                if tf.type_filters.contains(&TypeFilter::Creature)
                    && tf.get_subtype().is_none()
                    && tf.controller == Some(ControllerRef::You)
                    && tf.properties == vec![FilterProp::HasColor { color: ManaColor::White }]
        ));
    }

    #[test]
    fn static_other_subtype_you_control() {
        let def = parse_static_line("Other Zombies you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
    }

    #[test]
    fn static_cant_block() {
        let def = parse_static_line("Ragavan can't block.").unwrap();
        assert_eq!(def.mode, StaticMode::CantBlock);
        assert!(def.modifications.is_empty());
        assert!(def.description.is_some());
    }

    #[test]
    fn static_doesnt_untap() {
        let def =
            parse_static_line("Darksteel Sentinel doesn't untap during your untap step.").unwrap();
        assert_eq!(def.mode, StaticMode::CantUntap);
        assert!(def.description.is_some());
    }

    #[test]
    fn static_cant_be_countered() {
        // CR 101.2: "can't be countered" emits CantBeCountered, not CantBeCast
        let def = parse_static_line("Carnage Tyrant can't be countered.").unwrap();
        assert_eq!(def.mode, StaticMode::CantBeCountered);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def.description.is_some());
    }

    #[test]
    fn static_this_spell_cant_be_copied() {
        // CR 707.10: "This spell can't be copied." — Choreographed Sparks-class.
        // "this spell" is a SELF_REF_PARSE_ONLY phrase (not normalized to ~),
        // so the parser must recognize it as a self-ref static directly.
        let def = parse_static_line("This spell can't be copied.").unwrap();
        assert_eq!(def.mode, StaticMode::CantBeCopied);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def.description.is_some());
    }

    #[test]
    fn static_cant_be_countered_typed_subject() {
        // Allosaurus Shepherd: "Green spells you control can't be countered."
        let def = parse_static_line("Green spells you control can't be countered.").unwrap();
        assert_eq!(def.mode, StaticMode::CantBeCountered);
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.properties.iter().any(
                    |p| matches!(p, FilterProp::HasColor { color } if *color == ManaColor::Green)
                ),
                "Expected HasColor Green, got {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {:?}", def.affected);
        }
    }

    /// CR 117.7 + CR 601.2f: "This spell costs {N} less ..." must parse into a
    /// self-scoped static — affected = SelfRef, active_zones = [Hand, Stack] —
    /// so the cast-time scanner finds it on the spell itself (not on the
    /// battlefield). Regression guard for Tolarian Terror class.
    #[test]
    fn static_this_spell_cost_less_self_scoped_in_hand_and_stack() {
        let def = parse_static_line(
            "This spell costs {1} less to cast for each instant and sorcery card in your graveyard.",
        )
        .unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::ReduceCost {
                amount: ManaCost::Cost { generic: 1, .. },
                dynamic_count: Some(_),
                ..
            }
        ));
        assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
        assert_eq!(def.active_zones, vec![Zone::Hand, Zone::Stack]);
    }

    #[test]
    fn static_spells_cost_less() {
        let def = parse_static_line("Spells you cast cost {1} less to cast.").unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::ReduceCost {
                spell_filter: None,
                dynamic_count: None,
                ..
            }
        ));
        // Verify amount is generic 1 (avoid assert_eq! on complex types — SIGABRT risk)
        assert!(matches!(
            def.mode,
            StaticMode::ReduceCost {
                amount: ManaCost::Cost { generic: 1, .. },
                ..
            }
        ));
    }

    #[test]
    fn static_opponent_spells_cost_more() {
        let def = parse_static_line("Spells your opponents cast cost {1} more to cast.").unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::RaiseCost {
                spell_filter: None,
                dynamic_count: None,
                ..
            }
        ));
        assert!(matches!(
            def.mode,
            StaticMode::RaiseCost {
                amount: ManaCost::Cost { generic: 1, .. },
                ..
            }
        ));
    }

    #[test]
    fn static_opponent_spells_from_zones_cost_more() {
        // Aven Interrupter: "Spells your opponents cast from graveyards or from exile cost {2} more to cast."
        let def = parse_static_line(
            "Spells your opponents cast from graveyards or from exile cost {2} more to cast.",
        )
        .unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::RaiseCost {
                amount: ManaCost::Cost { generic: 2, .. },
                ..
            }
        ));
        if let StaticMode::RaiseCost {
            ref spell_filter, ..
        } = def.mode
        {
            let filter = spell_filter
                .as_ref()
                .expect("Expected spell_filter with zone constraint");
            match filter {
                TargetFilter::Typed(tf) => {
                    assert!(
                        tf.properties.iter().any(|p| matches!(
                            p,
                            FilterProp::InAnyZone { zones }
                                if zones.contains(&Zone::Graveyard) && zones.contains(&Zone::Exile)
                        )),
                        "Expected InAnyZone with Graveyard and Exile, got {:?}",
                        tf.properties
                    );
                }
                _ => panic!("Expected Typed filter, got {:?}", filter),
            }
        }
        // Affected should scope to opponents
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            }
            other => panic!("Expected Typed affected with Opponent, got {:?}", other),
        }
    }

    #[test]
    fn static_spells_from_exile_cost_less() {
        // "Spells you cast from exile this turn cost {X} less to cast" (without "this turn" dynamic)
        let def = parse_static_line("Spells you cast from exile cost {1} less to cast.").unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::ReduceCost {
                amount: ManaCost::Cost { generic: 1, .. },
                ..
            }
        ));
        if let StaticMode::ReduceCost {
            ref spell_filter, ..
        } = def.mode
        {
            let filter = spell_filter
                .as_ref()
                .expect("Expected spell_filter with zone constraint");
            match filter {
                TargetFilter::Typed(tf) => {
                    assert!(
                        tf.properties
                            .iter()
                            .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Exile })),
                        "Expected InZone Exile, got {:?}",
                        tf.properties
                    );
                }
                _ => panic!("Expected Typed filter"),
            }
        }
    }

    #[test]
    fn static_creature_spells_cost_less() {
        // Goblin Electromancer-style: "Creature spells you cast cost {1} less to cast."
        let def = parse_static_line("Creature spells you cast cost {1} less to cast.").unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::ReduceCost {
                amount: ManaCost::Cost { generic: 1, .. },
                ..
            }
        ));
        if let StaticMode::ReduceCost {
            ref spell_filter, ..
        } = def.mode
        {
            let filter = spell_filter.as_ref().expect("Expected spell_filter");
            match filter {
                TargetFilter::Typed(tf) => {
                    assert!(
                        tf.type_filters
                            .iter()
                            .any(|t| matches!(t, TypeFilter::Creature)),
                        "Expected Creature type filter"
                    );
                }
                _ => panic!("Expected Typed filter"),
            }
        }
    }

    #[test]
    fn static_instant_sorcery_spells_cost_less() {
        // Goblin Electromancer: "Instant and sorcery spells you cast cost {1} less to cast."
        let def = parse_static_line("Instant and sorcery spells you cast cost {1} less to cast.");
        assert!(
            def.is_some(),
            "parse returned None for instant/sorcery cost reduction"
        );
        let def = def.unwrap();
        assert!(
            matches!(def.mode, StaticMode::ReduceCost { .. }),
            "Expected ReduceCost mode"
        );
        if let StaticMode::ReduceCost {
            ref spell_filter, ..
        } = def.mode
        {
            assert!(
                spell_filter.is_some(),
                "Expected spell_filter for instant/sorcery"
            );
            let filter = spell_filter.as_ref().unwrap();
            // parse_type_phrase("instant and sorcery") → TargetFilter::Or { [Typed(Instant), Typed(Sorcery)] }
            fn contains_type(f: &TargetFilter, expected: TypeFilter) -> bool {
                match f {
                    TargetFilter::Typed(tf) => tf.type_filters.contains(&expected),
                    TargetFilter::Or { filters } => filters
                        .iter()
                        .any(|inner| contains_type(inner, expected.clone())),
                    _ => false,
                }
            }
            assert!(
                contains_type(filter, TypeFilter::Instant),
                "Expected Instant in filter"
            );
            assert!(
                contains_type(filter, TypeFilter::Sorcery),
                "Expected Sorcery in filter"
            );
        }
    }

    #[test]
    fn static_white_spells_cost_more() {
        // "White spells your opponents cast cost {1} more to cast."
        let def =
            parse_static_line("White spells your opponents cast cost {1} more to cast.").unwrap();
        assert!(matches!(def.mode, StaticMode::RaiseCost { .. }));
        if let StaticMode::RaiseCost {
            ref spell_filter, ..
        } = def.mode
        {
            let filter = spell_filter.as_ref().expect("Expected spell_filter");
            match filter {
                TargetFilter::Typed(tf) => {
                    assert!(
                        tf.properties.iter().any(|p| matches!(
                            p,
                            FilterProp::HasColor { color } if *color == ManaColor::White
                        )),
                        "Expected HasColor White"
                    );
                }
                _ => panic!("Expected Typed filter"),
            }
        }
    }

    #[test]
    fn static_noncreature_spells_cost_more_thalia() {
        // Thalia: "Noncreature spells cost {1} more to cast."
        let def = parse_static_line("Noncreature spells cost {1} more to cast.").unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::RaiseCost {
                amount: ManaCost::Cost { generic: 1, .. },
                ..
            }
        ));
        if let StaticMode::RaiseCost {
            ref spell_filter, ..
        } = def.mode
        {
            let filter = spell_filter.as_ref().expect("Expected spell_filter");
            match filter {
                TargetFilter::Typed(tf) => {
                    // Noncreature → TypeFilter::Non(Creature)
                    assert!(
                        tf.type_filters.iter().any(|t| matches!(
                            t,
                            TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Creature)
                        )),
                        "Expected Non(Creature) type filter"
                    );
                }
                _ => panic!("Expected Typed filter"),
            }
        }
    }

    #[test]
    fn static_first_qualified_spell_costs_less_has_filter_and_condition() {
        let def = parse_static_line(
            "The first non-Lemur creature spell with flying you cast during each of your turns costs {1} less to cast.",
        )
        .unwrap();

        assert!(matches!(def.mode, StaticMode::ReduceCost { .. }));
        let StaticMode::ReduceCost {
            ref spell_filter, ..
        } = def.mode
        else {
            unreachable!();
        };
        let filter = spell_filter.as_ref().expect("expected spell filter");
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected typed spell filter, got {filter:?}");
        };
        assert!(filter.type_filters.contains(&TypeFilter::Creature));
        assert!(filter.type_filters.iter().any(|entry| matches!(
            entry,
            TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Subtype(ref subtype) if subtype == "Lemur")
        )));
        assert!(filter.properties.iter().any(|prop| matches!(
            prop,
            FilterProp::WithKeyword { value } if *value == Keyword::Flying
        )));

        let condition = def.condition.expect("expected first-spell condition");
        let StaticCondition::And { conditions } = condition else {
            panic!("expected And condition");
        };
        assert!(conditions.contains(&StaticCondition::DuringYourTurn));
        assert!(conditions.iter().any(|condition| matches!(
            condition,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn { filter: Some(inner) },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            } if inner == spell_filter.as_ref().unwrap()
        )));
    }

    #[test]
    fn static_spells_cost_x_less_where_x_is_your_speed() {
        let def = parse_static_line(
            "Noncreature spells you cast cost {X} less to cast, where X is your speed.",
        )
        .unwrap();
        let StaticMode::ReduceCost {
            amount,
            dynamic_count,
            ..
        } = def.mode
        else {
            panic!("expected ReduceCost");
        };
        assert_eq!(amount, ManaCost::generic(1));
        assert_eq!(dynamic_count, Some(QuantityRef::Speed));
    }

    #[test]
    fn static_noncreature_spells_cost_less_as_long_as_lesson_threshold() {
        let def = parse_static_line(
            "Noncreature spells you cast cost {1} less to cast as long as there are three or more Lesson cards in your graveyard.",
        )
        .unwrap();

        assert!(matches!(def.mode, StaticMode::ReduceCost { .. }));
        assert!(matches!(
            def.condition,
            Some(StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ZoneCardCount {
                            zone: ZoneRef::Graveyard,
                            ref card_types,
                            scope: CountScope::Controller,
                        },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            }) if card_types == &vec![TypeFilter::Subtype("Lesson".to_string())]
        ));
    }

    // NOTE: static_enters_with_counters test moved to oracle_replacement tests —
    // "enters with counters" is now parsed as a Moved replacement effect.

    #[test]
    fn static_as_long_as_chosen_color() {
        let def = parse_static_line(
            "As long as the chosen color is blue, enchanted creature has flying.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.condition,
            Some(StaticCondition::ChosenColorIs {
                color: crate::types::mana::ManaColor::Blue
            })
        ));
    }

    #[test]
    fn static_as_long_as_hand_size_gt_life() {
        use crate::types::ability::{Comparator, QuantityExpr, QuantityRef};
        let def = parse_static_line(
            "As long as the number of cards in your hand is greater than your life total, enchanted creature has trample.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.condition,
            Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: crate::types::ability::PlayerScope::Controller
                    }
                },
                comparator: Comparator::GT,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: crate::types::ability::PlayerScope::Controller
                    }
                },
            })
        ));
    }

    #[test]
    fn static_keen_eyed_curator_condition_parses() {
        use crate::types::ability::{Comparator, QuantityExpr, QuantityRef};

        let def = parse_static_line(
            "As long as there are four or more card types among cards exiled with this creature, it gets +4/+4 and has trample.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 4 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 4 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }));
        assert!(matches!(
            def.condition,
            Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::DistinctCardTypesExiledBySource,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            })
        ));
    }

    #[test]
    fn static_as_long_as_unrecognized_condition() {
        // Conditions the parser cannot yet decompose fall through to Unrecognized.
        // The whole "As long as X, Y" string is captured permissively so the effect still fires.
        let def = parse_static_line(
            "As long as you cast this spell from exile, enchanted creature gets +1/+1.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.condition,
            Some(StaticCondition::Unrecognized { .. })
        ));
    }

    #[test]
    fn static_has_keyword_as_long_as() {
        let def =
            parse_static_line("Tarmogoyf has trample as long as a land card is in a graveyard.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }));
        assert!(matches!(
            def.condition,
            Some(StaticCondition::Unrecognized { .. })
        ));
    }

    #[test]
    fn static_erebos_god_of_the_dead_type_removal() {
        // CR 613.1d: Layer-4 type-removal with an attached devotion condition.
        // Inverted form — clause splitter rewrites to canonical form and the
        // "~ isn't a creature" branch now attaches the condition.
        let def = parse_static_line(
            "As long as your devotion to black is less than five, ~ isn't a creature.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::RemoveType {
                core_type: CoreType::Creature,
            }]
        );
        // The condition is "devotion < 5" which the existing static-condition
        // parser renders as Not{DevotionGE{Black, 5}}.
        assert!(def.condition.is_some(), "condition must be extracted");
        assert!(
            !matches!(def.condition, Some(StaticCondition::Unrecognized { .. })),
            "condition must be typed, not Unrecognized"
        );
    }

    #[test]
    fn static_type_removal_with_nondevotion_condition() {
        // The Warring Triad: non-devotion condition path. We don't assert the
        // condition variant (may or may not type via parse_static_condition),
        // but modifications MUST be non-empty regardless.
        let def = parse_static_line(
            "As long as there are fewer than eight cards in your graveyard, ~ isn't a creature.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::RemoveType {
                core_type: CoreType::Creature,
            }]
        );
        assert!(def.condition.is_some(), "condition must be extracted");
    }

    #[test]
    fn static_can_attack_despite_defender_self_unconditional() {
        // CR 702.3b: bare ~ subject, no condition.
        let def = parse_static_line("~ can attack as though it didn't have defender.").unwrap();
        assert_eq!(def.mode, StaticMode::CanAttackWithDefender);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def.condition.is_none());
    }

    #[test]
    fn static_can_attack_despite_defender_self_conditional() {
        // CR 702.3b + CR 611.3a: ~ subject + "as long as" condition
        // (Bristlepack Sentry pattern).
        let def = parse_static_line(
            "As long as you control a creature with power 4 or greater, ~ can attack as though it didn't have defender.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::CanAttackWithDefender);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def.condition.is_some(), "condition must be attached");
        assert!(
            !matches!(def.condition, Some(StaticCondition::Unrecognized { .. })),
            "condition must be typed, not Unrecognized"
        );
    }

    #[test]
    fn static_can_attack_despite_defender_creatures_you_control_they() {
        // CR 702.3b: plural subject + "they" pronoun (High Alert pattern).
        let def = parse_static_line(
            "Creatures you control can attack as though they didn't have defender.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::CanAttackWithDefender);
        assert!(matches!(def.affected, Some(TargetFilter::Typed(_))));
    }

    #[test]
    fn static_can_attack_despite_defender_modified_creatures_they() {
        // CR 700.9 + CR 702.3b: "modified creatures you control" subject
        // (Guardians of Oboro). Previously misparsed as Subtype("Modified");
        // now correctly maps to FilterProp::Modified.
        let def = parse_static_line(
            "Modified creatures you control can attack as though they didn't have defender.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::CanAttackWithDefender);
        match def.affected {
            Some(TargetFilter::Typed(ref tf)) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(
                    tf.properties.contains(&FilterProp::Modified),
                    "expected FilterProp::Modified in {:?}",
                    tf.properties
                );
                assert!(
                    !tf.type_filters.iter().any(|t| matches!(
                        t,
                        TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("modified")
                    )),
                    "must not emit Subtype(\"Modified\") (CR 205.3m — not a subtype)"
                );
            }
            _ => panic!("expected TargetFilter::Typed"),
        }
    }

    #[test]
    fn static_can_attack_despite_defender_enchanted_creature() {
        // Enchanted-creature subject (Animate Wall pattern) — routed through
        // parse_enchanted_equipped_predicate which now accepts both pronouns.
        let def =
            parse_static_line("Enchanted creature can attack as though it didn't have defender.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::CanAttackWithDefender);
    }

    #[test]
    fn static_life_more_than_starting_conditional() {
        let def = parse_static_line(
            "As long as you have at least 7 life more than your starting life total, creatures you control get +2/+2.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(ref tf))
                if tf.type_filters.contains(&TypeFilter::Creature)
                    && tf.controller == Some(ControllerRef::You)
        ));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 2 }));
        assert_eq!(
            def.condition,
            Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeAboveStarting
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 7 },
            })
        );
    }

    #[test]
    fn static_devotion_condition() {
        use crate::types::mana::ManaColor;
        // CR 110.4b: "less than five" → Not(DevotionGE { threshold: 5 })
        let def = parse_static_line(
            "As long as your devotion to black is less than five, Erebos isn't a creature.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.condition,
            Some(StaticCondition::Not {
                condition: Box::new(StaticCondition::DevotionGE {
                    colors: vec![ManaColor::Black],
                    threshold: 5,
                }),
            })
        );
    }

    #[test]
    fn static_devotion_multicolor_condition() {
        use crate::types::mana::ManaColor;
        // CR 110.4b: "less than seven" → Not(DevotionGE { threshold: 7 })
        let def = parse_static_line(
            "As long as your devotion to white and black is less than seven, Athreos isn't a creature.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.condition,
            Some(StaticCondition::Not {
                condition: Box::new(StaticCondition::DevotionGE {
                    colors: vec![ManaColor::White, ManaColor::Black],
                    threshold: 7,
                }),
            })
        );
    }

    #[test]
    fn static_during_your_turn_condition() {
        let def =
            parse_static_line("As long as it's your turn, Triumphant Adventurer has first strike.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
    }

    #[test]
    fn static_control_presence_condition() {
        let def =
            parse_static_line("As long as you control a artifact, Toolcraft Exemplar gets +2/+1.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.condition,
            Some(StaticCondition::IsPresent { filter: Some(_) })
        ));
    }

    #[test]
    fn static_control_creature_with_power_ge() {
        // "creature with power 4 or greater" — digit form
        let def = parse_static_line(
            "As long as you control a creature with power 4 or greater, Inspiring Commander gets +1/+1.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.condition,
            Some(StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(_))
            })
        ));
        // Modifications should include PT buff
        assert!(def
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddPower { value: 1 })));
    }

    #[test]
    fn static_control_creature_with_power_ge_word() {
        // "creature with power four or greater" — English word form via parse_number
        let def = parse_static_line(
            "As long as you control a creature with power four or greater, Target gets +2/+0.",
        )
        .unwrap();
        assert!(matches!(
            def.condition,
            Some(StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(_))
            })
        ));
    }

    #[test]
    fn static_control_creature_with_power_le() {
        // "creature with power 2 or less"
        let def = parse_static_line(
            "As long as you control a creature with power 2 or less, Target gets -1/-0.",
        )
        .unwrap();
        assert!(matches!(
            def.condition,
            Some(StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(_))
            })
        ));
    }

    #[test]
    fn static_lands_you_control_have() {
        let def = parse_static_line("Lands you control have 'Forests'.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddSubtype {
                subtype: "Forests".to_string(),
            }));
    }

    #[test]
    fn static_cant_be_the_target() {
        let def = parse_static_line(
            "Sphinx of the Final Word can't be the target of spells or abilities your opponents control.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::CantBeTargeted);
    }

    #[test]
    fn static_cant_be_sacrificed() {
        // CR 701.21: Self-referential sacrifice prohibition emits the canonical
        // `StaticMode::Other("CantBeSacrificed")` so the runtime guard in
        // `game::sacrifice` (`object_has_static_other(id, "CantBeSacrificed")`)
        // can observe it.
        let def = parse_static_line("Sigarda, Host of Herons can't be sacrificed.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantBeSacrificed".to_string()));
        assert!(def.description.is_some());
    }

    #[test]
    fn map_keyword_uses_fromstr() {
        // Test that map_keyword handles all standard keywords via FromStr
        assert_eq!(map_keyword("flying"), Some(Keyword::Flying));
        assert_eq!(map_keyword("first strike"), Some(Keyword::FirstStrike));
        assert_eq!(map_keyword("double strike"), Some(Keyword::DoubleStrike));
        assert_eq!(map_keyword("trample"), Some(Keyword::Trample));
        assert_eq!(map_keyword("deathtouch"), Some(Keyword::Deathtouch));
        assert_eq!(map_keyword("lifelink"), Some(Keyword::Lifelink));
        assert_eq!(map_keyword("vigilance"), Some(Keyword::Vigilance));
        assert_eq!(map_keyword("haste"), Some(Keyword::Haste));
        assert_eq!(map_keyword("reach"), Some(Keyword::Reach));
        assert_eq!(map_keyword("menace"), Some(Keyword::Menace));
        assert_eq!(map_keyword("hexproof"), Some(Keyword::Hexproof));
        assert_eq!(map_keyword("indestructible"), Some(Keyword::Indestructible));
        assert_eq!(map_keyword("defender"), Some(Keyword::Defender));
        assert_eq!(map_keyword("shroud"), Some(Keyword::Shroud));
        assert_eq!(map_keyword("flash"), Some(Keyword::Flash));
        assert_eq!(map_keyword("prowess"), Some(Keyword::Prowess));
        assert_eq!(map_keyword("fear"), Some(Keyword::Fear));
        assert_eq!(map_keyword("intimidate"), Some(Keyword::Intimidate));
        assert_eq!(map_keyword("wither"), Some(Keyword::Wither));
        assert_eq!(map_keyword("infect"), Some(Keyword::Infect));
        // Unknown returns None
        assert_eq!(map_keyword("notakeyword"), None);
    }

    #[test]
    fn static_multiple_keywords() {
        let def = parse_static_line("Enchanted creature has flying, trample, and haste.").unwrap();
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }));
    }

    #[test]
    fn static_self_gets_pt() {
        let def = parse_static_line("Tarmogoyf gets +1/+2.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 2 }));
    }

    #[test]
    fn static_have_keyword() {
        let def = parse_static_line("Creatures you control have vigilance.").unwrap();
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Vigilance,
            }));
    }

    #[test]
    fn during_your_turn_has_lifelink() {
        let def = parse_static_line("During your turn, this creature has lifelink.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Lifelink,
            }));
    }

    #[test]
    fn suffix_during_your_turn_has_first_strike() {
        // Razorkin Needlehead: "This creature has first strike during your turn."
        let def = parse_static_line("This creature has first strike during your turn.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::FirstStrike,
            }));
    }

    #[test]
    fn suffix_during_turns_other_than_yours() {
        let def =
            parse_static_line("This creature has hexproof during turns other than yours.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.condition,
            Some(StaticCondition::Not {
                condition: Box::new(StaticCondition::DuringYourTurn),
            })
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }));
    }

    #[test]
    fn this_land_is_the_chosen_type() {
        let def = parse_static_line("This land is the chosen type.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::BasicLandType,
            }]
        );
    }

    #[test]
    fn this_creature_is_the_chosen_type() {
        let def =
            parse_static_line("This creature is the chosen type in addition to its other types.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::CreatureType,
            }]
        );
    }

    #[test]
    fn static_tarmogoyf_cda() {
        let def = parse_static_line(
            "Tarmogoyf's power is equal to the number of card types among cards in all graveyards and its toughness is equal to that number plus 1.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def.characteristic_defining);
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetDynamicPower {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::DistinctCardTypesInZone {
                        zone: ZoneRef::Graveyard,
                        scope: CountScope::All,
                    },
                },
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetDynamicToughness {
                value: QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::DistinctCardTypesInZone {
                            zone: ZoneRef::Graveyard,
                            scope: CountScope::All,
                        },
                    }),
                    offset: 1,
                },
            }));
    }

    #[test]
    fn static_enchanted_creature_doesnt_untap() {
        let def = parse_static_line(
            "Enchanted creature doesn't untap during its controller's untap step.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::CantUntap);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
    }

    #[test]
    fn static_creatures_with_counters_dont_untap() {
        let def = parse_static_line(
            "Creatures with ice counters on them don't untap during their controllers' untap steps.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::CantUntap);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter::creature().properties(
                vec![FilterProp::CountersGE {
                    counter_type: crate::types::counter::CounterType::Generic("ice".to_string()),
                    count: QuantityExpr::Fixed { value: 1 },
                },]
            )))
        );
    }

    #[test]
    fn static_this_creature_attacks_each_combat_if_able() {
        let def = parse_static_line("This creature attacks each combat if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustAttack);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_enchanted_creature_attacks_each_combat_if_able() {
        let def = parse_static_line("Enchanted creature attacks each combat if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustAttack);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
    }

    #[test]
    fn static_this_creature_can_block_only_creatures_with_flying() {
        let def = parse_static_line("This creature can block only creatures with flying.").unwrap();
        assert_eq!(def.mode, StaticMode::BlockRestriction);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_you_have_shroud() {
        let def = parse_static_line("You have shroud.").unwrap();
        assert_eq!(def.mode, StaticMode::Shroud);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
        );
    }

    /// CR 702.11: "You have hexproof." (Crystal Barricade) must produce a
    /// player-scope `StaticMode::Hexproof`, not a bogus
    /// `ContinuousModification::AddKeyword(Hexproof)` on an empty-typed
    /// controller-scoped filter (which would wrongly grant hexproof to every
    /// permanent you control instead of to the player).
    #[test]
    fn static_you_have_hexproof() {
        let def = parse_static_line("You have hexproof.").unwrap();
        assert_eq!(def.mode, StaticMode::Hexproof);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
        );
    }

    #[test]
    fn static_you_have_no_maximum_hand_size() {
        let def = parse_static_line("You have no maximum hand size.").unwrap();
        assert_eq!(def.mode, StaticMode::NoMaximumHandSize);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
        );
    }

    #[test]
    fn static_each_player_may_play_an_additional_land() {
        let def =
            parse_static_line("Each player may play an additional land on each of their turns.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::MayPlayAdditionalLand);
        assert_eq!(def.affected, Some(TargetFilter::Player));
    }

    #[test]
    fn static_you_may_choose_not_to_untap_self() {
        let def =
            parse_static_line("You may choose not to untap this creature during your untap step.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::MayChooseNotToUntap);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_you_may_look_at_top_card_of_library() {
        let def =
            parse_static_line("You may look at the top card of your library any time.").unwrap();
        assert_eq!(def.mode, StaticMode::MayLookAtTopOfLibrary);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
        );
    }

    #[test]
    fn static_cards_in_graveyards_lose_all_abilities() {
        let def = parse_static_line("Cards in graveyards lose all abilities.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::InZone {
                    zone: crate::types::zones::Zone::Graveyard,
                },
            ])))
        );
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::RemoveAllAbilities]
        );
    }

    #[test]
    fn static_black_creatures_get_plus_one_plus_one() {
        let def = parse_static_line("Black creatures get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter::creature().properties(
                vec![FilterProp::HasColor {
                    color: ManaColor::Black,
                }]
            )))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
    }

    #[test]
    fn static_creatures_you_control_with_mana_value_filter() {
        let def = parse_static_line("Creatures you control with mana value 3 or less get +1/+0.")
            .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Cmc {
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 3 },
                    }]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 0 }));
    }

    #[test]
    fn static_creatures_you_control_with_flying_filter() {
        let def = parse_static_line("Creatures you control with flying get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::WithKeyword {
                        value: Keyword::Flying,
                    }]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
    }

    #[test]
    fn static_other_zombie_creatures_have_swampwalk() {
        let def = parse_static_line("Other Zombie creatures have swampwalk.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .subtype("Zombie".to_string())
                    .properties(vec![FilterProp::Another]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Landwalk("Swamp".to_string()),
            }));
    }

    #[test]
    fn static_creature_tokens_you_control_lose_all_abilities_and_have_base_pt() {
        let def = parse_static_line(
            "Creature tokens you control lose all abilities and have base power and toughness 3/3.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Token]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::RemoveAllAbilities));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetPower { value: 3 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetToughness { value: 3 }));
    }

    #[test]
    fn static_target_subject_can_set_base_power_without_toughness() {
        let modifications = parse_continuous_modifications("has base power 3 until end of turn");
        assert_eq!(
            modifications,
            vec![ContinuousModification::SetPower { value: 3 }]
        );
    }

    #[test]
    fn static_enchanted_land_has_quoted_ability() {
        let def = parse_static_line("Enchanted land has \"{T}: Add two mana of any one color.\"")
            .unwrap();
        // Should produce a GrantAbility with a typed activated AbilityDefinition
        let grant = def
            .modifications
            .iter()
            .find(|m| matches!(m, ContinuousModification::GrantAbility { .. }));
        assert!(
            grant.is_some(),
            "should contain a GrantAbility modification"
        );
        if let ContinuousModification::GrantAbility { definition } = grant.unwrap() {
            assert_eq!(definition.kind, AbilityKind::Activated);
            assert!(definition.cost.is_some());
        }
    }

    #[test]
    fn quoted_ability_sacrifice_cost_separator() {
        // CR 118.12: "Sacrifice this token: Add {C}." should parse as an activated ability
        // with sacrifice cost and mana effect, not a spell-like sacrifice effect.
        let def = parse_quoted_ability("Sacrifice this token: Add {C}.");
        assert_eq!(def.kind, AbilityKind::Activated);
        assert!(def.cost.is_some(), "should have a cost");
        assert!(
            !matches!(
                *def.effect,
                crate::types::ability::Effect::Unimplemented { .. }
            ),
            "effect should not be Unimplemented, got {:?}",
            def.effect
        );
    }

    #[test]
    fn static_other_tapped_creatures_you_control_have_indestructible() {
        let def =
            parse_static_line("Other tapped creatures you control have indestructible.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Tapped, FilterProp::Another]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Indestructible,
            }));
    }

    #[test]
    fn static_attacking_creatures_you_control_have_double_strike() {
        let def = parse_static_line("Attacking creatures you control have double strike.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Attacking]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::DoubleStrike,
            }));
    }

    #[test]
    fn static_during_your_turn_creatures_you_control_have_hexproof() {
        let def =
            parse_static_line("During your turn, creatures you control have hexproof.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }));
    }

    #[test]
    fn static_during_your_turn_equipped_creatures_you_control_have_double_strike() {
        let def = parse_static_line(
            "During your turn, equipped creatures you control have double strike and haste.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::EquippedBy]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::DoubleStrike,
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }));
    }

    #[test]
    fn parse_compound_static_kaito_animation() {
        let text = "During your turn, as long as ~ has one or more loyalty counters on him, he's a 3/4 Ninja creature and has hexproof.";
        let def = parse_static_line(text).unwrap();

        // Verify compound condition
        assert!(matches!(
            def.condition,
            Some(StaticCondition::And { ref conditions })
            if conditions.len() == 2
        ));
        if let Some(StaticCondition::And { ref conditions }) = def.condition {
            assert!(matches!(conditions[0], StaticCondition::DuringYourTurn));
            assert!(matches!(
                conditions[1],
                StaticCondition::HasCounters {
                    counters: CounterMatch::OfType(crate::types::counter::CounterType::Loyalty),
                    minimum: 1,
                    ..
                }
            ));
        }

        // Verify self-referencing
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));

        // Verify modifications
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetPower { value: 3 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetToughness { value: 4 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Creature,
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddSubtype {
                subtype: "Ninja".to_string(),
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }));
    }

    // ── New static routing tests (Steps 4-5) ─────────────────────────────

    #[test]
    fn static_must_be_blocked_if_able() {
        // CR 509.1b: "must be blocked if able"
        let def = parse_static_line("Darksteel Myr must be blocked if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustBeBlocked);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_opponents_cant_gain_life() {
        // CR 119.7: Lifegain prevention — opponent scope
        let def = parse_static_line("Your opponents can't gain life.").unwrap();
        assert_eq!(def.mode, StaticMode::CantGainLife);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::Opponent),
                ..
            }))
        ));
    }

    #[test]
    fn static_you_cant_gain_life() {
        // CR 119.7: Lifegain prevention — self scope
        let def = parse_static_line("You can't gain life.").unwrap();
        assert_eq!(def.mode, StaticMode::CantGainLife);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                ..
            }))
        ));
    }

    #[test]
    fn static_players_cant_gain_life() {
        // CR 119.7: Lifegain prevention — all players
        let def = parse_static_line("Players can't gain life.").unwrap();
        assert_eq!(def.mode, StaticMode::CantGainLife);
        // No controller restriction — affects all
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                controller: None,
                ..
            }))
        ));
    }

    #[test]
    fn static_cast_as_though_flash() {
        // CR 702.8a: Flash-granting static
        let def =
            parse_static_line("You may cast creature spells as though they had flash.").unwrap();
        assert_eq!(def.mode, StaticMode::CastWithFlash);
    }

    #[test]
    fn static_can_block_additional_creature() {
        let def = parse_static_line("Palace Guard can block an additional creature each combat.")
            .unwrap();
        assert_eq!(def.mode, StaticMode::ExtraBlockers { count: Some(1) });
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_can_block_any_number() {
        let def =
            parse_static_line("Hundred-Handed One can block any number of creatures.").unwrap();
        assert_eq!(def.mode, StaticMode::ExtraBlockers { count: None });
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_play_two_additional_lands() {
        // "play two additional lands" — not handled by the subject-predicate parser
        let def =
            parse_static_line("You may play two additional lands on each of your turns.").unwrap();
        assert_eq!(def.mode, StaticMode::AdditionalLandDrop { count: 2 });
    }

    #[test]
    fn parse_compound_static_counter_minimum_variants() {
        // "a" counter variant
        let text =
            "During your turn, as long as ~ has a loyalty counter on it, it's a 2/2 Ninja creature and has hexproof.";
        let def = parse_static_line(text).unwrap();
        if let Some(StaticCondition::And { ref conditions }) = def.condition {
            assert!(matches!(
                conditions[1],
                StaticCondition::HasCounters { minimum: 1, .. }
            ));
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetPower { value: 2 }));
    }

    // ── CR 510.1c: AssignDamageFromToughness (Doran-class) ─────────────

    #[test]
    fn static_assigns_damage_from_toughness_basic() {
        // CR 510.1c: "Each creature you control assigns combat damage equal to its toughness"
        let def = parse_static_line(
            "Each creature you control assigns combat damage equal to its toughness rather than its power.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AssignDamageFromToughness));
    }

    #[test]
    fn static_assigns_damage_from_toughness_with_defender() {
        // CR 510.1c: "Each creature you control with defender assigns combat damage..."
        let def = parse_static_line(
            "Each creature you control with defender assigns combat damage equal to its toughness rather than its power.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::WithKeyword {
                        value: Keyword::Defender,
                    }]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AssignDamageFromToughness));
    }

    #[test]
    fn static_assigns_damage_from_toughness_gt_power() {
        // CR 510.1c: "Each creature you control with toughness greater than its power..."
        let def = parse_static_line(
            "Each creature you control with toughness greater than its power assigns combat damage equal to its toughness rather than its power.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::ToughnessGTPower]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AssignDamageFromToughness));
    }

    // --- Conditional counter-based keyword grants (CR 613.7) ---

    #[test]
    fn static_each_creature_with_counter_has_trample() {
        let def =
            parse_static_line("Each creature you control with a +1/+1 counter on it has trample.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(ref tf))
                if tf.type_filters.contains(&TypeFilter::Creature)
                    && tf.controller == Some(ControllerRef::You) =>
            {
                let properties = &tf.properties;
                assert!(properties.iter().any(|p| matches!(
                    p,
                    FilterProp::CountersGE {
                        counter_type: crate::types::counter::CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 1 },
                    }
                )));
            }
            other => panic!("Expected Typed creature filter, got {:?}", other),
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample
            }));
    }

    #[test]
    fn static_creatures_with_counters_have_haste() {
        let def =
            parse_static_line("Creatures you control with +1/+1 counters on them have haste.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(ref tf))
                if tf.type_filters.contains(&TypeFilter::Creature)
                    && tf.controller == Some(ControllerRef::You) =>
            {
                let properties = &tf.properties;
                assert!(properties.iter().any(|p| matches!(
                    p,
                    FilterProp::CountersGE {
                        counter_type: crate::types::counter::CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 1 },
                    }
                )));
            }
            other => panic!("Expected Typed creature filter, got {:?}", other),
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Haste
            }));
    }

    #[test]
    fn static_creatures_with_counter_get_pump() {
        let def = parse_static_line("Creatures you control with a +1/+1 counter on it gets +2/+2.")
            .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                properties,
                ..
            })) => {
                assert!(properties.iter().any(|p| matches!(
                    p,
                    FilterProp::CountersGE {
                        counter_type: crate::types::counter::CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 1 },
                    }
                )));
            }
            other => panic!("Expected Typed creature filter, got {:?}", other),
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }));
    }

    // --- split_keyword_list protection-awareness tests ---

    /// Helper: collect split results as owned strings for easy comparison.
    fn kw_list(text: &str) -> Vec<String> {
        split_keyword_list(text)
            .into_iter()
            .map(|c| c.into_owned())
            .collect()
    }

    #[test]
    fn split_keyword_list_two_color_protections() {
        assert_eq!(
            kw_list("protection from black and from red"),
            vec!["protection from black", "protection from red"]
        );
    }

    #[test]
    fn split_keyword_list_non_protection_and() {
        assert_eq!(
            kw_list("flying and first strike"),
            vec!["flying", "first strike"]
        );
    }

    #[test]
    fn split_keyword_list_mixed_keywords_and_protection() {
        // expand_protection_parts lowercases protection fragments
        assert_eq!(
            kw_list("flying, protection from Demons and from Dragons, and first strike"),
            vec![
                "flying",
                "protection from demons",
                "protection from dragons",
                "first strike"
            ]
        );
    }

    #[test]
    fn split_keyword_list_three_way_inline_protection() {
        assert_eq!(
            kw_list("protection from red and from blue and from green"),
            vec![
                "protection from red",
                "protection from blue",
                "protection from green"
            ]
        );
    }

    #[test]
    fn split_keyword_list_comma_continuation_protection() {
        // expand_protection_parts lowercases protection fragments
        assert_eq!(
            kw_list("protection from Vampires, from Werewolves, and from Zombies"),
            vec![
                "protection from vampires",
                "protection from werewolves",
                "protection from zombies"
            ]
        );
    }

    #[test]
    fn split_keyword_list_protection_from_everything_no_split() {
        assert_eq!(
            kw_list("protection from everything"),
            vec!["protection from everything"]
        );
    }

    #[test]
    fn continuous_mods_protection_from_two_colors() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;
        let mods = parse_continuous_modifications("has protection from black and from red");
        let prot_keywords: Vec<_> = mods
            .iter()
            .filter_map(|m| match m {
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Protection(pt),
                } => Some(pt.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            prot_keywords,
            vec![
                ProtectionTarget::Color(ManaColor::Black),
                ProtectionTarget::Color(ManaColor::Red),
            ]
        );
    }

    // --- Graveyard cast permission tests ---

    #[test]
    fn graveyard_cast_permission_lurrus() {
        let text = "Once during each of your turns, you may cast a permanent spell with mana value 2 or less from your graveyard.";
        let def = parse_static_line(text).expect("should parse Lurrus text");
        assert!(matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::OncePerTurn,
                play_mode: CardPlayMode::Cast,
            }
        ));
        let filter = def.affected.expect("should have affected filter");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Permanent));
            assert!(
                tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Cmc {
                        comparator: Comparator::LE,
                        ..
                    }
                )),
                "Expected CmcLE property, got: {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got: {filter:?}");
        }
    }

    #[test]
    fn graveyard_cast_permission_karador() {
        let def = parse_static_line(
            "Once during each of your turns, you may cast a creature spell from your graveyard.",
        )
        .unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::OncePerTurn,
                play_mode: CardPlayMode::Cast,
            }
        ));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("Expected Typed creature filter for Karador, got {other:?}"),
        }
    }

    #[test]
    fn graveyard_cast_permission_kess() {
        let def = parse_static_line(
            "Once during each of your turns, you may cast an instant or sorcery spell from your graveyard."
        ).unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::OncePerTurn,
                play_mode: CardPlayMode::Cast,
            }
        ));
        // Should parse as a union or typed filter covering instant/sorcery
        assert!(def.affected.is_some());
    }

    #[test]
    fn graveyard_cast_permission_gisa_geralf() {
        let text = "Once during each of your turns, you may cast a Zombie creature spell from your graveyard.";
        let lower = text.to_lowercase();
        let def = try_parse_graveyard_cast_permission(text, &lower)
            .expect("should parse Gisa+Geralf text");
        assert!(matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::OncePerTurn,
                play_mode: CardPlayMode::Cast,
            }
        ));
        // "zombie creature" → parse_type_phrase recognizes "zombie" as subtype.
        // card_type may be None (subtype alone) or Creature depending on parser —
        // either is functionally correct since Zombie is exclusively a creature subtype.
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert_eq!(tf.get_subtype(), Some("Zombie"));
        } else {
            panic!("Expected Typed filter with Zombie subtype");
        }
    }

    #[test]
    fn graveyard_keyword_grant_clause_flashback() {
        let (filter, kind) = try_parse_graveyard_keyword_grant_clause(
            "Each instant and sorcery card in your graveyard has flashback.",
        )
        .expect("should parse flashback grant clause");
        assert_eq!(kind, GraveyardGrantedKeywordKind::Flashback);
        match filter {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 2);
                for branch in filters {
                    let TargetFilter::Typed(tf) = branch else {
                        panic!("expected typed branch, got {branch:?}");
                    };
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                    assert!(tf.properties.contains(&FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }));
                }
            }
            other => panic!("expected instant/sorcery graveyard filter, got {other:?}"),
        }
    }

    #[test]
    fn graveyard_keyword_grant_clause_escape() {
        let (filter, kind) = try_parse_graveyard_keyword_grant_clause(
            "Each nonland card in your graveyard has escape.",
        )
        .expect("should parse escape grant clause");
        assert_eq!(kind, GraveyardGrantedKeywordKind::Escape);
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected typed graveyard filter");
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(
            tf.properties.contains(&FilterProp::InZone {
                zone: Zone::Graveyard
            }),
            "missing graveyard zone: {:?}",
            tf.properties
        );
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))),
            "missing nonland type filter: {:?}",
            tf.type_filters
        );
    }

    #[test]
    fn graveyard_keyword_grant_clause_rejects_non_you_scope() {
        let clause = try_parse_graveyard_keyword_grant_clause(
            "Each nonland card in their graveyard has escape.",
        );
        assert!(
            clause.is_none(),
            "only your graveyard scope is currently supported"
        );
    }

    // --- Graveyard play permission tests (Crucible of Worlds / Icetill Explorer) ---

    #[test]
    fn graveyard_play_permission_crucible() {
        let text = "You may play lands from your graveyard.";
        let def = parse_static_line(text).expect("should parse Crucible text");
        assert!(matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Play,
            }
        ));
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
        } else {
            panic!(
                "Expected Typed filter with Land type, got: {:?}",
                def.affected
            );
        }
    }

    #[test]
    fn graveyard_cast_permission_conduit_of_worlds() {
        let text = "You may cast permanent spells from your graveyard.";
        let def = parse_static_line(text).expect("should parse Conduit text");
        assert!(matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Cast,
            }
        ));
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert!(tf.type_filters.contains(&TypeFilter::Permanent));
        } else {
            panic!(
                "Expected Typed filter with Permanent type, got: {:?}",
                def.affected
            );
        }
    }

    // --- Alt-cost rider tests (Ninja Teen et al., CR 118.9 / CR 702.190a) ---

    #[test]
    fn graveyard_cast_permission_ninja_teen_sneak_rider() {
        // Ninja Teen Level 3 rider: grants GY-cast permission gated on Sneak.
        let text = "You may cast creature spells from your graveyard using their sneak abilities.";
        let def = parse_static_line(text).expect("should parse Ninja Teen rider");
        assert!(matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Cast,
            }
        ));
        let filter = def.affected.expect("should have affected filter");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got: {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::HasKeywordKind {
                    value: KeywordKind::Sneak
                }
            )),
            "expected HasKeywordKind{{Sneak}} in properties, got: {:?}",
            tf.properties
        );
    }

    #[test]
    fn graveyard_cast_permission_self_ref_rider_all_keywords() {
        // Self-referential riders on the 5 shipping cards (Brokkos/Mutate,
        // Phoenix/Bestow, Sabin+Underdog/Blitz, Timeline Culler/Warp).
        let cases = [
            ("mutate", KeywordKind::Mutate),
            ("bestow", KeywordKind::Bestow),
            ("blitz", KeywordKind::Blitz),
            ("warp", KeywordKind::Warp),
        ];
        for (name, expected_kind) in cases {
            let text =
                format!("You may cast this card from your graveyard using its {name} ability.");
            let def = parse_static_line(&text)
                .unwrap_or_else(|| panic!("should parse self-ref rider for {name}"));
            let filter = def
                .affected
                .unwrap_or_else(|| panic!("missing affected filter for {name}"));
            let has_kind = match filter {
                TargetFilter::Typed(tf) => tf.properties.iter().any(|p| {
                    matches!(
                        p,
                        FilterProp::HasKeywordKind { value } if *value == expected_kind
                    )
                }),
                TargetFilter::And { filters } => filters.iter().any(|f| {
                    matches!(f, TargetFilter::Typed(tf)
                        if tf.properties.iter().any(|p| matches!(
                            p,
                            FilterProp::HasKeywordKind { value } if *value == expected_kind
                        ))
                    )
                }),
                _ => false,
            };
            assert!(
                has_kind,
                "missing HasKeywordKind{{{expected_kind:?}}} for {name}"
            );
        }
    }

    #[test]
    fn graveyard_cast_permission_no_rider_leaves_filter_clean() {
        // Lurrus / Muldrotha / Karador / Conduit / Yawgmoth's Will regression:
        // permissions without a rider must not carry any HasKeywordKind prop.
        let cases = [
            "Once during each of your turns, you may cast a permanent spell with mana value 2 or less from your graveyard.",
            "Once during each of your turns, you may cast a creature spell from your graveyard.",
            "You may cast permanent spells from your graveyard.",
        ];
        for text in cases {
            let def = parse_static_line(text)
                .unwrap_or_else(|| panic!("should parse no-rider text: {text:?}"));
            if let Some(TargetFilter::Typed(tf)) = def.affected {
                assert!(
                    !tf.properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::HasKeywordKind { .. })),
                    "unexpected HasKeywordKind in {text:?}: {:?}",
                    tf.properties
                );
            }
        }
    }

    // --- Hand cast free permission tests (Omniscience) ---

    #[test]
    fn hand_cast_free_omniscience() {
        let text = "You may cast spells from your hand without paying their mana costs.";
        let def = parse_static_line(text).expect("should parse Omniscience text");
        assert_eq!(
            def.mode,
            StaticMode::CastFromHandFree {
                frequency: CastFrequency::Unlimited,
            }
        );
        assert_eq!(def.affected, Some(TargetFilter::Any));
    }

    #[test]
    fn hand_cast_free_rejects_without_free() {
        // "you may cast ... from your hand" without "without paying" is not a free-cast static
        let text = "You may cast a spell from your hand.";
        let lower = text.to_lowercase();
        assert!(try_parse_cast_free_permission(text, &lower).is_none());
    }

    /// CR 601.2b: Zaffai and the Tempests — once-per-turn cast-from-hand-free.
    #[test]
    fn hand_cast_free_zaffai_once_per_turn() {
        let text = "Once during each of your turns, you may cast an instant or sorcery spell from your hand without paying its mana cost.";
        let def = parse_static_line(text).expect("should parse Zaffai text");
        assert!(
            matches!(
                def.mode,
                StaticMode::CastFromHandFree {
                    frequency: CastFrequency::OncePerTurn,
                }
            ),
            "expected CastFromHandFree {{ OncePerTurn }}, got: {:?}",
            def.mode
        );
        // Affected filter must reject non-instant/sorcery hand spells.
        let filter = def.affected.expect("should have affected filter");
        match filter {
            TargetFilter::Or { .. } | TargetFilter::Typed(_) => {
                // Either an Or { Instant, Sorcery } union or a Typed filter whose
                // type_filters cover instant/sorcery — both are structurally valid.
            }
            other => panic!("unexpected filter for Zaffai: {other:?}"),
        }
    }

    /// CR 601.2b: Zaffai parser must NOT be intercepted by the graveyard-cast
    /// permission branch when the zone is "from your hand".
    #[test]
    fn hand_cast_free_zaffai_not_intercepted_by_graveyard_branch() {
        let text = "Once during each of your turns, you may cast an instant or sorcery spell from your hand without paying its mana cost.";
        let lower = text.to_lowercase();
        // Graveyard branch must decline (zone is hand, not graveyard).
        assert!(try_parse_graveyard_cast_permission(text, &lower).is_none());
        // Hand-free branch must succeed.
        assert!(try_parse_cast_free_permission(text, &lower).is_some());
    }

    // CR 601.2 + CR 118.9a: B10 Dracogenesis — Omniscience-class static with
    // the zone qualifier omitted ("you may cast Dragon spells without paying
    // their mana costs"). Implicit cast zone defaults to hand per CR 601.2.
    #[test]
    fn cast_free_dracogenesis_no_zone_qualifier() {
        let text = "You may cast Dragon spells without paying their mana costs.";
        let def = parse_static_line(text).expect("should parse Dracogenesis text");
        assert_eq!(
            def.mode,
            StaticMode::CastFromHandFree {
                frequency: CastFrequency::Unlimited,
            }
        );
        // Dragon subtype filter must survive.
        let filter = def.affected.expect("should have affected filter");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.get_subtype(), Some("Dragon"));
            }
            other => panic!("expected Typed[Subtype: Dragon] for Dracogenesis, got {other:?}"),
        }
    }

    // CR 601.2 + CR 119.3: Unqualified branch now accepts dynamic mana-value
    // filters whose RHS is any `parse_quantity_ref` phrase (Fires of Invention
    // class). Earlier the comparator only matched the trigger-anaphoric
    // `that <type>` form, so this filter fell through to a partial parse and
    // the test asserted the rejection (better-decline-than-overgrant). The
    // comparator was extended to delegate the RHS to the shared
    // `parse_quantity_ref` building block, so the filter now fully types as
    // `CmcLE { value: Ref { ObjectCount { Land, You } } }` and the cast-free
    // permission can carry it. The test is inverted: it now asserts the
    // typed filter is preserved end-to-end.
    #[test]
    fn cast_free_unqualified_accepts_dynamic_mv_filter() {
        use crate::types::ability::{FilterProp, QuantityExpr, QuantityRef, TargetFilter};
        let text = "You may cast spells with mana value less than or equal to the number of lands you control without paying their mana costs.";
        let lower = text.to_lowercase();
        let def = try_parse_cast_free_permission(text, &lower)
            .expect("dynamic-MV filter should parse end-to-end");
        let filter = def.affected.expect("affected filter must be present");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter for Fires-of-Invention class");
        };
        let has_dynamic_cmc_le = tf.properties.iter().any(|p| {
            matches!(
                p,
                FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. }
                    }
                }
            )
        });
        assert!(
            has_dynamic_cmc_le,
            "expected CmcLE with dynamic ObjectCount RHS, got {:?}",
            tf.properties
        );
    }

    // Negative test: text without "without paying" must not match the
    // free-cast combinator under either zone-qualifier branch.
    #[test]
    fn cast_free_rejects_text_without_without_paying() {
        let text = "You may cast Dragon spells from your hand.";
        let lower = text.to_lowercase();
        assert!(try_parse_cast_free_permission(text, &lower).is_none());

        let text2 = "You may cast Dragon spells.";
        let lower2 = text2.to_lowercase();
        assert!(try_parse_cast_free_permission(text2, &lower2).is_none());
    }

    // ── Fix 1: Irregular plural subtype normalization ──

    #[test]
    fn static_elves_you_control_uses_elf_subtype() {
        // CR 205.3m: "Elves" must normalize to "Elf", not "Elve"
        let def = parse_static_line("Other Elves you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::And { filters }) = &def.affected {
            let has_elf = filters
                .iter()
                .any(|f| matches!(f, TargetFilter::Typed(tf) if tf.get_subtype() == Some("Elf")));
            assert!(has_elf, "Expected Elf subtype, got {:?}", def.affected);
        } else if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert_eq!(tf.get_subtype(), Some("Elf"));
        } else {
            panic!("Expected filter with Elf subtype, got {:?}", def.affected);
        }
    }

    #[test]
    fn static_dwarves_you_control_uses_dwarf_subtype() {
        // CR 205.3m: "Dwarves" must normalize to "Dwarf", not "Dwarve"
        let def = parse_static_line("Dwarves you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert_eq!(tf.get_subtype(), Some("Dwarf"));
        } else {
            panic!(
                "Expected Typed filter with Dwarf subtype, got {:?}",
                def.affected
            );
        }
    }

    #[test]
    fn parse_creature_subject_filter_irregular_plurals() {
        // Single-word plural subtypes should resolve via parse_subtype
        let filter = super::parse_creature_subject_filter("Elves").unwrap();
        if let TargetFilter::Typed(tf) = &filter {
            assert_eq!(tf.get_subtype(), Some("Elf"));
        } else {
            panic!("Expected Typed filter with Elf subtype, got {:?}", filter);
        }

        let filter = super::parse_creature_subject_filter("Wolves").unwrap();
        if let TargetFilter::Typed(tf) = &filter {
            assert_eq!(tf.get_subtype(), Some("Wolf"));
        } else {
            panic!("Expected Typed filter with Wolf subtype, got {:?}", filter);
        }
    }

    #[test]
    fn static_unblocked_attacking_ninjas_you_control_have_lifelink() {
        let def =
            parse_static_line("Unblocked attacking Ninjas you control have lifelink.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert_eq!(tf.get_subtype(), Some("Ninja"));
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.contains(&FilterProp::Unblocked));
            assert!(tf.properties.contains(&FilterProp::Attacking));
        } else {
            panic!(
                "Expected Typed filter with Ninja subtype, got {:?}",
                def.affected
            );
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Lifelink,
            }));
    }

    #[test]
    fn static_attacking_ninjas_you_control_have_deathtouch() {
        let def = parse_static_line("Attacking Ninjas you control have deathtouch.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert_eq!(tf.get_subtype(), Some("Ninja"));
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.contains(&FilterProp::Attacking));
            assert!(!tf.properties.contains(&FilterProp::Unblocked));
        } else {
            panic!(
                "Expected Typed filter with Ninja subtype, got {:?}",
                def.affected
            );
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Deathtouch,
            }));
    }

    #[test]
    fn static_other_ninja_and_rogue_creatures_you_control_get_plus1() {
        let def =
            parse_static_line("Other Ninja and Rogue creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Or { filters }) = &def.affected {
            assert_eq!(filters.len(), 2);
            for f in filters {
                if let TargetFilter::Typed(tf) = f {
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                    assert!(tf.properties.contains(&FilterProp::Another));
                    assert!(tf.get_subtype() == Some("Ninja") || tf.get_subtype() == Some("Rogue"));
                } else {
                    panic!("Expected Typed filter in Or, got {f:?}");
                }
            }
        } else {
            panic!("Expected Or filter, got {:?}", def.affected);
        }
    }

    #[test]
    fn static_elf_or_warrior_creatures_you_control_have_trample() {
        let def = parse_static_line("Elf or Warrior creatures you control have trample.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Or { filters }) = &def.affected {
            assert_eq!(filters.len(), 2);
        } else {
            panic!("Expected Or filter, got {:?}", def.affected);
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }));
    }

    #[test]
    fn static_parse_for_each_attached_to_self_kellan() {
        // CR 301.5 + CR 303.4: Kellan, the Fae-Blooded — "Other creatures you
        // control get +1/+0 for each Aura and Equipment attached to ~." The
        // multiplier was previously dropped (boost frozen at +1/+0); now the
        // for-each clause emits an `AddDynamicPower` over an `ObjectCount`
        // filtered by `AttachedToSource` so the boost scales with attachments.
        let result = parse_static_line(
            "Other creatures you control get +1/+0 for each Aura and Equipment attached to ~.",
        );
        let def = result.expect("Kellan static must parse");
        assert_eq!(def.mode, StaticMode::Continuous);
        let dynamic_power = def
            .modifications
            .iter()
            .find_map(|m| match m {
                ContinuousModification::AddDynamicPower { value } => Some(value),
                _ => None,
            })
            .expect("expected AddDynamicPower");
        match dynamic_power {
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            } => match filter {
                TargetFilter::Typed(tf) => {
                    assert!(
                        tf.properties.contains(&FilterProp::AttachedToSource),
                        "filter must carry AttachedToSource, got {:?}",
                        tf.properties
                    );
                }
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("expected ObjectCount Ref, got {other:?}"),
        }
    }

    #[test]
    fn static_parse_for_each_clause_other_creature() {
        // Verify parse_for_each_clause handles "other creature you control"
        let result =
            crate::parser::oracle_quantity::parse_for_each_clause("other creature you control");
        assert!(
            result.is_some(),
            "parse_for_each_clause should handle 'other creature you control'"
        );
        assert!(
            matches!(result.unwrap(), QuantityRef::ObjectCount { .. }),
            "Expected ObjectCount"
        );
    }

    #[test]
    fn static_self_gets_dynamic_power_for_each_creature() {
        // CR 613.4c: "~ gets +1/+0 for each other creature you control"
        let result = parse_static_line("~ gets +1/+0 for each other creature you control.");
        assert!(result.is_some(), "Should parse 'gets +N/+M for each'");
        let def = result.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(
            def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicPower { .. })),
            "Expected AddDynamicPower, got {:?}",
            def.modifications
        );
        // Should NOT have AddDynamicToughness since toughness is +0
        assert!(
            !def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. })),
            "Should not have AddDynamicToughness for +0"
        );
    }

    #[test]
    fn static_reduce_ability_cost_ninjutsu() {
        // CR 601.2f: "Ninjutsu abilities you activate cost {1} less to activate"
        let def = parse_static_line("Ninjutsu abilities you activate cost {1} less to activate.")
            .expect("should parse ReduceAbilityCost");
        assert!(
            matches!(
                def.mode,
                StaticMode::ReduceAbilityCost {
                    ref keyword,
                    amount: 1,
                } if keyword == "ninjutsu"
            ),
            "Expected ReduceAbilityCost {{ keyword: ninjutsu, amount: 1 }}, got {:?}",
            def.mode
        );
    }

    // --- Phase 33-01: Conditional, dynamic, and non-standard enchanted/equipped patterns ---

    #[test]
    fn static_enchanted_creature_has_keyword_as_long_as_control() {
        // Conditional grant: "enchanted creature has flying as long as you control a Wizard"
        let def =
            parse_static_line("Enchanted creature has flying as long as you control a Wizard.")
                .expect("should parse conditional enchanted grant");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying,
                }),
            "Expected AddKeyword(Flying), got {:?}",
            def.modifications
        );
        assert!(
            matches!(def.condition, Some(StaticCondition::IsPresent { .. })),
            "Expected IsPresent condition, got {:?}",
            def.condition
        );
    }

    #[test]
    fn static_as_long_as_equipped_creature_is_legendary_grants_to_equipped_creature() {
        let def = parse_static_line("As long as equipped creature is legendary, it has hexproof.")
            .expect("should parse attached-subject inverted grant");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter::creature().properties(
                vec![
                    FilterProp::EquippedBy,
                    FilterProp::HasSupertype {
                        value: Supertype::Legendary,
                    },
                ]
            )))
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Hexproof,
                }),
            "Expected AddKeyword(Hexproof), got {:?}",
            def.modifications
        );
        assert_eq!(def.condition, None);
    }

    #[test]
    fn static_enchanted_creature_gets_pt_as_long_as() {
        // Conditional grant: "enchanted creature gets +1/+1 as long as you control a Wizard"
        let def =
            parse_static_line("Enchanted creature gets +1/+1 as long as you control a Wizard.")
                .expect("should parse conditional enchanted P/T grant");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddPower { value: 1 }),
            "Expected AddPower(1)"
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddToughness { value: 1 }),
            "Expected AddToughness(1)"
        );
        assert!(
            matches!(def.condition, Some(StaticCondition::IsPresent { .. })),
            "Expected IsPresent condition, got {:?}",
            def.condition
        );
    }

    #[test]
    fn static_enchanted_creature_dynamic_for_each() {
        // Dynamic grant: "enchanted creature gets +1/+1 for each creature you control"
        let def = parse_static_line("Enchanted creature gets +1/+1 for each creature you control.")
            .expect("should parse dynamic enchanted P/T grant");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(
            def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicPower { .. })),
            "Expected AddDynamicPower, got {:?}",
            def.modifications
        );
        assert!(
            def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. })),
            "Expected AddDynamicToughness, got {:?}",
            def.modifications
        );
    }

    #[test]
    fn static_strong_back_attached_to_recipient_emits_attached_to_recipient_prop() {
        // CR 301.5 + CR 303.4 + CR 613.4c: Strong Back's third static —
        // "Enchanted creature gets +2/+2 for each Aura and Equipment attached
        // to it." The pronoun "it" is anaphoric on the enchanted creature
        // (the per-recipient affected of the boost), not on the Aura source.
        // The static must therefore lower to a `QuantityRef::ObjectCount`
        // whose filter carries `FilterProp::AttachedToRecipient`, NOT
        // `FilterProp::AttachedToSource`. The legacy bug was a flat
        // `AddPower(2) + AddToughness(2)` because the for-each clause did not
        // recognize "attached to it" and the parser fell through to the
        // fixed-P/T fallback.
        let def = parse_static_line(
            "Enchanted creature gets +2/+2 for each Aura and Equipment attached to it.",
        )
        .expect("Strong Back static must parse");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );

        // Capture the dynamic-power modification's QuantityExpr for inspection.
        let dyn_pow = def
            .modifications
            .iter()
            .find_map(|m| match m {
                ContinuousModification::AddDynamicPower { value } => Some(value),
                _ => None,
            })
            .expect("expected AddDynamicPower for the for-each scaling");

        // The factor-2 multiplier wraps an ObjectCount whose filter carries
        // AttachedToRecipient — confirming the per-recipient referent.
        let inner = match dyn_pow {
            QuantityExpr::Multiply { factor, inner } => {
                assert_eq!(*factor, 2);
                inner.as_ref()
            }
            other => panic!("expected QuantityExpr::Multiply, got {other:?}"),
        };
        match inner {
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            } => match filter {
                TargetFilter::Typed(TypedFilter { properties, .. }) => {
                    assert!(
                        properties.contains(&FilterProp::AttachedToRecipient),
                        "filter must carry AttachedToRecipient, got {properties:?}"
                    );
                    assert!(
                        !properties.contains(&FilterProp::AttachedToSource),
                        "filter must NOT carry AttachedToSource (would point at the Aura)"
                    );
                }
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("expected ObjectCount ref, got {other:?}"),
        }

        // Negative regression: ensure the parser is not also producing a
        // bogus flat `AddPower(2)` alongside the dynamic version. (Layered
        // application would otherwise grant +2 *plus* +2/attached, which is
        // a different bug from the original 0-multiplier symptom but equally
        // wrong.)
        assert!(
            !def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddPower { .. })),
            "must not emit a flat AddPower alongside AddDynamicPower; got {:?}",
            def.modifications
        );
    }

    #[test]
    fn static_enchanted_creature_dynamic_where_x() {
        // Dynamic grant: "enchanted creature gets +X/+X, where X is the number of cards in your hand"
        let def = parse_static_line(
            "Enchanted creature gets +X/+X, where X is the number of cards in your hand.",
        )
        .expect("should parse dynamic enchanted where-X grant");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(
            def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicPower { .. })),
            "Expected AddDynamicPower, got {:?}",
            def.modifications
        );
        assert!(
            def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. })),
            "Expected AddDynamicToughness, got {:?}",
            def.modifications
        );
    }

    #[test]
    fn static_enchanted_creature_can_attack_as_though_haste() {
        // Non-standard keyword: "enchanted creature can attack as though it had haste"
        // CR 702.10: Haste-equivalent for aura-granted attack permission.
        let def = parse_static_line("Enchanted creature can attack as though it had haste.")
            .expect("should parse 'can attack as though it had haste'");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste,
                }),
            "Expected AddKeyword(Haste), got {:?}",
            def.modifications
        );
    }

    #[test]
    fn static_enchanted_creature_cant_be_blocked() {
        // Non-standard: "enchanted creature can't be blocked"
        // CR 509.1b: Unblockable via aura.
        let def = parse_static_line("Enchanted creature can't be blocked.")
            .expect("should parse enchanted can't be blocked");
        assert_eq!(def.mode, StaticMode::CantBeBlocked);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
    }

    // --- MustAttack / MustBlock combat requirement pattern tests ---

    #[test]
    fn static_must_attack_each_combat_if_able() {
        let def = parse_static_line("This creature must attack each combat if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustAttack);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_attacks_each_turn_if_able() {
        let def = parse_static_line("Enchanted creature attacks each turn if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustAttack);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
    }

    #[test]
    fn static_equipped_creature_regression() {
        // Regression: existing equipped creature pattern still works.
        let def = parse_static_line("Equipped creature has first strike and lifelink.")
            .expect("should parse equipped creature keywords");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
            ))
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::FirstStrike,
                }),
            "Expected AddKeyword(FirstStrike)"
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Lifelink,
                }),
            "Expected AddKeyword(Lifelink)"
        );
    }

    #[test]
    fn static_enchanted_creature_gets_pt_regression() {
        // Regression: basic enchanted creature P/T pattern still works.
        let def = parse_static_line("Enchanted creature gets +2/+2.")
            .expect("should parse enchanted creature P/T");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 2 }));
    }

    // --- Lord pattern tests (Plan 33-02) ---

    #[test]
    fn lord_bare_creatures_have_keyword() {
        // "Creatures you control have vigilance" (e.g., Brave the Sands)
        let result = parse_static_line("Creatures you control have vigilance.");
        assert!(result.is_some(), "should parse bare keyword lord");
        let def = result.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        // Verify affected filter is creature + controller You
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            _ => panic!("Expected Typed creature filter with controller You"),
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Vigilance,
            }));
    }

    #[test]
    fn lord_other_creatures_have_keyword() {
        // CR 613.7: "Other creatures you control have hexproof" (e.g., Shalai, Voice of Plenty)
        // Must produce Continuous with AddKeyword(Hexproof) and Another filter to exclude self.
        let result = parse_static_line("Other creatures you control have hexproof.");
        assert!(
            result.is_some(),
            "should parse other creatures keyword lord"
        );
        let def = result.unwrap();
        assert!(matches!(def.mode, StaticMode::Continuous), "not continuous");
        let has_hexproof = def.modifications.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Hexproof
                }
            )
        });
        assert!(has_hexproof, "no hexproof keyword");
        // CR 613.7: "Other" means the static excludes the source permanent itself.
        let has_another = match &def.affected {
            Some(TargetFilter::Typed(tf)) => tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::Another)),
            _ => false,
        };
        assert!(has_another, "no Another property for 'other' lord");
    }

    #[test]
    fn lord_subtype_creatures_have_keyword() {
        // "Pirate creatures you control have menace" (e.g., Dire Fleet Neckbreaker variant)
        let result = parse_static_line("Pirate creatures you control have menace.");
        assert!(result.is_some(), "should parse subtype keyword lord");
        let def = result.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Menace,
            }));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Subtype("Pirate".to_string())));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            _ => panic!("Expected Typed filter"),
        }
    }

    #[test]
    fn lord_conditional_as_long_as_control() {
        // "As long as you control a Wizard, creatures you control get +1/+1"
        // (e.g., Adeliz, the Cinder Wind variant)
        let result =
            parse_static_line("As long as you control a Wizard, creatures you control get +1/+1.");
        assert!(result.is_some(), "should parse conditional lord");
        let def = result.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
        assert!(def.condition.is_some(), "Expected a StaticCondition");
        match def.condition {
            Some(StaticCondition::IsPresent { .. }) => {}
            _ => panic!("Expected IsPresent condition"),
        }
    }

    #[test]
    fn lord_each_creature_with_keyword() {
        // "Each creature you control with flying gets +1/+1"
        // (e.g., Favorable Winds, Empyrean Eagle)
        let result = parse_static_line("Each creature you control with flying gets +1/+1.");
        assert!(result.is_some(), "should parse keyword-filtered lord");
        let def = result.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
        // Should have a filter with WithKeyword for flying
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.properties.contains(&FilterProp::WithKeyword {
                    value: Keyword::Flying,
                }));
            }
            _ => panic!("Expected Typed filter with keyword property"),
        }
    }

    #[test]
    fn lord_other_zombie_creatures_regression() {
        // Regression: "Other Zombie creatures you control get +1/+1" still works
        let result = parse_static_line("Other Zombie creatures you control get +1/+1.");
        assert!(result.is_some(), "should parse other subtype lord");
        let def = result.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Subtype("Zombie".to_string())));
                assert!(tf.properties.contains(&FilterProp::Another));
            }
            _ => panic!("Expected Typed filter"),
        }
    }

    #[test]
    fn enchanted_land_is_a_mountain_produces_set_basic_land_type() {
        let def = parse_static_line("Enchanted land is a Mountain.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.modifications.as_slice(),
            [ContinuousModification::SetBasicLandType { land_type }]
            if *land_type == BasicLandType::Mountain
        ));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert!(tf.properties.contains(&FilterProp::EnchantedBy));
            }
            _ => panic!("Expected Typed land filter with EnchantedBy"),
        }
    }

    #[test]
    fn enchanted_land_is_a_plains_produces_set_basic_land_type() {
        let def = parse_static_line("Enchanted land is a Plains.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.modifications.as_slice(),
            [ContinuousModification::SetBasicLandType { land_type }]
            if *land_type == BasicLandType::Plains
        ));
    }

    #[test]
    fn enchanted_land_is_a_forest_in_addition_produces_add_subtype() {
        let def = parse_static_line("Enchanted land is a Forest in addition to its other types.")
            .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddSubtype {
                subtype: "Forest".to_string(),
            }]
        );
    }

    #[test]
    fn enchanted_land_is_a_swamp_in_addition_produces_add_subtype() {
        let def =
            parse_static_line("Enchanted land is a Swamp in addition to its other types.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddSubtype {
                subtype: "Swamp".to_string(),
            }]
        );
    }

    // --- Land type-changing statics (CR 305.7) ---

    #[test]
    fn nonbasic_lands_are_mountains_blood_moon() {
        let def = parse_static_line("Nonbasic lands are Mountains.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.modifications.as_slice(),
            [ContinuousModification::SetBasicLandType { land_type }]
            if *land_type == BasicLandType::Mountain
        ));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert!(tf.properties.contains(&FilterProp::NotSupertype {
                    value: Supertype::Basic,
                }));
            }
            _ => panic!("Expected Typed nonbasic land filter"),
        }
    }

    #[test]
    fn nonbasic_lands_are_islands_harbinger() {
        let def = parse_static_line("Nonbasic lands are Islands.").unwrap();
        assert!(matches!(
            def.modifications.as_slice(),
            [ContinuousModification::SetBasicLandType { land_type }]
            if *land_type == BasicLandType::Island
        ));
    }

    #[test]
    fn lands_you_control_are_plains_celestial_dawn() {
        let def = parse_static_line("Lands you control are Plains.").unwrap();
        assert!(matches!(
            def.modifications.as_slice(),
            [ContinuousModification::SetBasicLandType { land_type }]
            if *land_type == BasicLandType::Plains
        ));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            _ => panic!("Expected Typed land filter with you-control"),
        }
    }

    #[test]
    fn each_land_is_a_swamp_in_addition_urborg() {
        let def =
            parse_static_line("Each land is a Swamp in addition to its other land types.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddSubtype {
                subtype: "Swamp".to_string(),
            }]
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert!(tf.controller.is_none());
            }
            _ => panic!("Expected Typed land filter (all lands)"),
        }
    }

    #[test]
    fn all_lands_are_islands_in_addition_stormtide() {
        let def =
            parse_static_line("All lands are Islands in addition to their other types.").unwrap();
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddSubtype {
                subtype: "Island".to_string(),
            }]
        );
    }

    #[test]
    fn lands_you_control_every_basic_land_type_prismatic_omen() {
        let def = parse_static_line(
            "Lands you control are every basic land type in addition to their other types.",
        )
        .unwrap();
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddAllBasicLandTypes]
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            _ => panic!("Expected Typed land filter with you-control"),
        }
    }

    // --- CantCastDuring: turn/phase-scoped casting prohibitions ---

    #[test]
    fn static_cant_cast_opponents_during_your_turn() {
        // CR 101.2: Teferi, Time Raveler — "Your opponents can't cast spells during your turn."
        let def = parse_static_line("Your opponents can't cast spells during your turn.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantCastDuring {
                who: ProhibitionScope::Opponents,
                when: CastingProhibitionCondition::DuringYourTurn,
            }
        );
    }

    #[test]
    fn static_cant_cast_players_during_combat() {
        // CR 101.2: "Players can't cast spells during combat."
        let def = parse_static_line("Players can't cast spells during combat.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantCastDuring {
                who: ProhibitionScope::AllPlayers,
                when: CastingProhibitionCondition::DuringCombat,
            }
        );
    }

    #[test]
    fn static_cant_cast_from_still_works() {
        // Regression: CantCastFrom (zone-based) must not be affected
        let def =
            parse_static_line("Players can't cast spells from graveyards or libraries.").unwrap();
        assert_eq!(def.mode, StaticMode::CantCastFrom);
    }

    #[test]
    fn static_cant_cast_during_serde_roundtrip() {
        let mode = StaticMode::CantCastDuring {
            who: ProhibitionScope::Opponents,
            when: CastingProhibitionCondition::DuringYourTurn,
        };
        let json = serde_json::to_string(&mode).unwrap();
        let deserialized: StaticMode = serde_json::from_str(&json).unwrap();
        assert_eq!(mode, deserialized);
    }

    #[test]
    fn static_cant_cast_during_display_roundtrip() {
        let mode = StaticMode::CantCastDuring {
            who: ProhibitionScope::Opponents,
            when: CastingProhibitionCondition::DuringYourTurn,
        };
        let s = mode.to_string();
        assert_eq!(StaticMode::from_str(&s).unwrap(), mode);

        let mode2 = StaticMode::CantCastDuring {
            who: ProhibitionScope::AllPlayers,
            when: CastingProhibitionCondition::DuringCombat,
        };
        let s2 = mode2.to_string();
        assert_eq!(StaticMode::from_str(&s2).unwrap(), mode2);
    }

    // --- PerTurnCastLimit tests ---

    #[test]
    fn per_turn_cast_limit_all_players() {
        // CR 101.2 + CR 604.1: Rule of Law — "Each player can't cast more than one spell each turn."
        let def =
            parse_static_line("Each player can't cast more than one spell each turn.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::PerTurnCastLimit {
                who: ProhibitionScope::AllPlayers,
                max: 1,
                spell_filter: None,
            }
        );
    }

    #[test]
    fn per_turn_cast_limit_opponents() {
        let def =
            parse_static_line("Each opponent can't cast more than one spell each turn.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::PerTurnCastLimit {
                who: ProhibitionScope::Opponents,
                max: 1,
                spell_filter: None,
            }
        );
    }

    #[test]
    fn per_turn_cast_limit_controller() {
        let def = parse_static_line("You can't cast more than one spell each turn.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::PerTurnCastLimit {
                who: ProhibitionScope::Controller,
                max: 1,
                spell_filter: None,
            }
        );
    }

    #[test]
    fn per_turn_cast_limit_noncreature_filter() {
        // Deafening Silence: "Each player can't cast more than one noncreature spell each turn."
        let def =
            parse_static_line("Each player can't cast more than one noncreature spell each turn.")
                .unwrap();
        let StaticMode::PerTurnCastLimit {
            who,
            max,
            spell_filter,
        } = &def.mode
        else {
            panic!("expected PerTurnCastLimit");
        };
        assert_eq!(*who, ProhibitionScope::AllPlayers);
        assert_eq!(*max, 1);
        // Filter should be Non(Creature)
        let Some(TargetFilter::Typed(tf)) = spell_filter else {
            panic!("expected typed spell filter, got {spell_filter:?}");
        };
        assert_eq!(
            tf.type_filters,
            vec![TypeFilter::Non(Box::new(TypeFilter::Creature))]
        );
    }

    #[test]
    fn per_turn_cast_limit_max_two() {
        // Fires of Invention (standalone clause): "You can cast no more than two spells each turn."
        let def = parse_static_line("You can cast no more than two spells each turn.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::PerTurnCastLimit {
                who: ProhibitionScope::Controller,
                max: 2,
                spell_filter: None,
            }
        );
    }

    #[test]
    fn per_turn_cast_limit_compound_clause() {
        // Fires of Invention: compound "and" clause with per-turn limit in second half
        let def = parse_static_line(
            "You can cast spells only during your turn and you can cast no more than two spells each turn.",
        );
        assert!(def.is_some(), "expected Some for compound clause");
        let def = def.unwrap();
        assert_eq!(
            def.mode,
            StaticMode::PerTurnCastLimit {
                who: ProhibitionScope::Controller,
                max: 2,
                spell_filter: None,
            }
        );
    }

    #[test]
    fn only_during_your_turn_standalone() {
        // CR 117.1a + CR 604.1: "You can cast spells only during your turn."
        let def = parse_static_line("You can cast spells only during your turn.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantCastDuring {
                who: ProhibitionScope::Controller,
                when: CastingProhibitionCondition::NotDuringYourTurn,
            }
        );
    }

    #[test]
    fn per_turn_cast_limit_does_not_affect_cant_cast_during() {
        // Regression: CantCastDuring must still parse correctly
        let def = parse_static_line("Your opponents can't cast spells during your turn.").unwrap();
        assert!(matches!(def.mode, StaticMode::CantCastDuring { .. }));
    }

    #[test]
    fn per_turn_cast_limit_does_not_affect_cant_cast_from() {
        // Regression: CantCastFrom must still parse correctly
        let def =
            parse_static_line("Players can't cast spells from graveyards or libraries.").unwrap();
        assert_eq!(def.mode, StaticMode::CantCastFrom);
    }

    // --- MustAttack / MustBlock additional combat requirement tests ---

    #[test]
    fn static_must_attack_if_able() {
        let def = parse_static_line("This creature must attack if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustAttack);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_must_block_each_combat_if_able() {
        let def = parse_static_line("This creature must block each combat if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustBlock);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_blocks_each_combat_if_able() {
        let def = parse_static_line("Enchanted creature blocks each combat if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustBlock);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
    }

    #[test]
    fn static_must_block_if_able() {
        let def = parse_static_line("This creature must block if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustBlock);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_blocks_each_turn_if_able() {
        let def = parse_static_line("This creature blocks each turn if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustBlock);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_unrelated_text_not_must_attack() {
        // "gets +1/+1" should not produce MustAttack
        let def = parse_static_line("This creature gets +1/+1.").unwrap();
        assert_ne!(def.mode, StaticMode::MustAttack);
        assert_ne!(def.mode, StaticMode::MustBlock);
    }

    #[test]
    fn map_keyword_all_creature_types_returns_changeling() {
        // CR 702.73a: "all creature types" is the Changeling CDA effect.
        assert_eq!(map_keyword("all creature types"), Some(Keyword::Changeling));
        assert_eq!(map_keyword("All Creature Types"), Some(Keyword::Changeling));
    }

    #[test]
    fn gain_all_creature_types_produces_add_keyword_changeling() {
        let mods = parse_continuous_modifications("gain all creature types");
        assert!(
            mods.iter().any(|m| matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Changeling
                }
            )),
            "Should produce AddKeyword(Changeling), got: {mods:?}"
        );
    }

    #[test]
    fn static_condition_source_in_graveyard() {
        let cond = parse_static_condition("this card is in your graveyard");
        assert!(
            matches!(
                cond,
                Some(StaticCondition::SourceInZone {
                    zone: Zone::Graveyard
                })
            ),
            "Expected SourceInZone(Graveyard), got: {cond:?}"
        );
    }

    #[test]
    fn static_condition_source_in_hand() {
        let cond = parse_static_condition("~ is in your hand");
        assert!(
            matches!(
                cond,
                Some(StaticCondition::SourceInZone { zone: Zone::Hand })
            ),
            "Expected SourceInZone(Hand), got: {cond:?}"
        );
    }

    #[test]
    fn static_condition_compound_and() {
        let cond =
            parse_static_condition("this card is in your graveyard and you control a Mountain");
        assert!(
            matches!(cond, Some(StaticCondition::And { ref conditions }) if conditions.len() == 2),
            "Expected And with 2 conditions, got: {cond:?}"
        );
    }

    #[test]
    fn static_condition_no_false_split_noun_phrase() {
        // "artifacts and creatures you control" is NOT a compound condition
        let cond = parse_static_condition("artifacts and creatures you control");
        assert!(
            !matches!(cond, Some(StaticCondition::And { .. })),
            "Should not split noun phrase, got: {cond:?}"
        );
    }

    // --- Task 1: as-long-as condition splitting in parse_continuous_gets_has ---

    #[test]
    fn static_self_ref_gets_as_long_as_control_forest() {
        // Kird Ape: "~ gets +1/+2 as long as you control a Forest"
        let def = parse_static_line("Kird Ape gets +1/+2 as long as you control a Forest.");
        assert!(def.is_some(), "Should parse 'gets +1/+2 as long as' static");
        let def = def.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(
            def.condition.is_some(),
            "Expected non-null condition for 'as long as' static, got None"
        );
    }

    #[test]
    fn static_self_ref_gets_as_long_as_regression_for_each() {
        // "for each" split must still work after adding "as long as" split
        let def = parse_static_line("~ gets +1/+1 for each creature you control.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        // Should have dynamic P/T modifications, not a condition
        assert!(def.condition.is_none());
    }

    #[test]
    fn static_self_ref_gets_without_condition_regression() {
        // Plain "gets +2/+2" without condition must still work
        let def = parse_static_line("~ gets +2/+2.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def.condition.is_none());
    }

    #[test]
    fn static_condition_you_have_n_or_more_life() {
        // "you have 5 or more life" should parse as a QuantityComparison
        let cond = parse_static_condition("you have 5 or more life");
        assert!(
            matches!(
                cond,
                Some(StaticCondition::QuantityComparison {
                    comparator: Comparator::GE,
                    ..
                })
            ),
            "Expected QuantityComparison with GE, got: {cond:?}"
        );
    }

    #[test]
    fn static_conditional_cant_untap_with_if() {
        // "~ doesn't untap during your untap step if enchanted creature is blue"
        // Should produce CantUntap with a condition populated
        let def = parse_static_line(
            "~ doesn't untap during your untap step as long as enchanted creature is tapped.",
        );
        // For now, just check it parses as CantUntap (condition handling is new)
        assert!(def.is_some(), "Should parse conditional CantUntap");
        let def = def.unwrap();
        assert_eq!(def.mode, StaticMode::CantUntap);
    }

    #[test]
    fn control_enchanted_creature() {
        // CR 303.4e + CR 613.2: "You control enchanted creature" (Control Magic pattern)
        let def = parse_static_line("You control enchanted creature.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::ChangeController));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert!(tf.properties.contains(&FilterProp::EnchantedBy));
            }
            _ => panic!("expected Typed filter"),
        }
        // Also works without trailing period
        let def2 = parse_static_line("You control enchanted creature").unwrap();
        assert_eq!(def2.mode, StaticMode::Continuous);
    }

    #[test]
    fn control_enchanted_permanent() {
        let def = parse_static_line("You control enchanted permanent.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Permanent));
                assert!(tf.properties.contains(&FilterProp::EnchantedBy));
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn control_enchanted_land() {
        let def = parse_static_line("You control enchanted land.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert!(tf.properties.contains(&FilterProp::EnchantedBy));
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn control_enchanted_artifact() {
        let def = parse_static_line("You control enchanted artifact.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Artifact));
                assert!(tf.properties.contains(&FilterProp::EnchantedBy));
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn control_enchanted_planeswalker() {
        // Not yet in Oracle text but structurally valid — the generic pattern should handle it
        let def = parse_static_line("You control enchanted planeswalker.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Planeswalker));
                assert!(tf.properties.contains(&FilterProp::EnchantedBy));
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn core_type_creature_filter() {
        // CR 205.2a: "Artifact creatures you control get +1/+1" → Creature + Artifact
        let def = parse_static_line("Artifact creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert!(tf.type_filters.contains(&TypeFilter::Artifact));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            _ => panic!("expected Typed filter with Creature + Artifact"),
        }
    }

    #[test]
    fn other_enchantment_creatures() {
        // "Other enchantment creatures you control get +1/+1"
        let def = parse_static_line("Other enchantment creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert!(tf.type_filters.contains(&TypeFilter::Enchantment));
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::Another));
            }
            _ => panic!("expected Typed filter with Creature + Enchantment + Another"),
        }
    }

    #[test]
    fn creatures_you_control_that_are_enchanted() {
        // CR 613.1: "Creatures you control that are enchanted get +1/+1"
        let def = parse_static_line("Creatures you control that are enchanted get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::EnchantedBy));
            }
            _ => panic!("expected Typed filter with enchanted property"),
        }
    }

    #[test]
    fn negative_dynamic_power() {
        // CR 613.4c: "gets -X/-0, where X is the number of creatures you control"
        let def = parse_static_line(
            "Enchanted creature gets -X/-0, where X is the number of creatures you control.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        // Should have AddDynamicPower with Multiply(-1, ...) but NOT AddDynamicToughness
        let has_neg_power = def.modifications.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::AddDynamicPower {
                    value: QuantityExpr::Multiply { factor: -1, .. },
                }
            )
        });
        assert!(
            has_neg_power,
            "Expected negative dynamic power: {:?}",
            def.modifications
        );
        let has_dynamic_toughness = def
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. }));
        assert!(
            !has_dynamic_toughness,
            "Should NOT have dynamic toughness for -X/-0"
        );
    }

    #[test]
    fn skip_draw_step() {
        let def = parse_static_line("Skip your draw step.").unwrap();
        assert_eq!(def.mode, StaticMode::SkipStep { step: Phase::Draw });
    }

    #[test]
    fn skip_untap_step() {
        let def = parse_static_line("Skip your untap step.").unwrap();
        assert_eq!(def.mode, StaticMode::SkipStep { step: Phase::Untap });
    }

    #[test]
    fn skip_upkeep_step() {
        let def = parse_static_line("Skip your upkeep step.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::SkipStep {
                step: Phase::Upkeep
            }
        );
    }

    #[test]
    fn positive_dynamic_pt() {
        // CR 613.4c: "gets +X/+X, where X is the number of creatures you control"
        let def = parse_static_line(
            "Enchanted creature gets +X/+X, where X is the number of creatures you control.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        let has_power = def
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddDynamicPower { .. }));
        let has_toughness = def
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. }));
        assert!(has_power, "Expected dynamic power");
        assert!(has_toughness, "Expected dynamic toughness");
    }

    #[test]
    fn dynamic_keyword_annihilator_x() {
        // "~ has annihilator X, where X is the number of +1/+1 counters on it."
        let def = parse_static_line(
            "~ has annihilator X, where X is the number of +1/+1 counters on it.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        let has_dynamic_keyword = def.modifications.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::AddDynamicKeyword {
                    kind: crate::types::keywords::DynamicKeywordKind::Annihilator,
                    ..
                }
            )
        });
        assert!(
            has_dynamic_keyword,
            "Expected AddDynamicKeyword(Annihilator), got {:?}",
            def.modifications
        );
    }

    #[test]
    fn cant_be_blocked_unconditional() {
        let def = parse_static_line("This creature can't be blocked.").unwrap();
        assert_eq!(def.mode, StaticMode::CantBeBlocked);
        assert!(def.condition.is_none());
    }

    #[test]
    fn cant_be_blocked_as_long_as_defending_controls() {
        // CR 509.1a: "can't be blocked as long as defending player controls an artifact"
        let def = parse_static_line(
            "This creature can't be blocked as long as defending player controls an artifact.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::CantBeBlocked);
        assert!(
            matches!(
                &def.condition,
                Some(StaticCondition::DefendingPlayerControls { .. })
            ),
            "Expected DefendingPlayerControls condition, got: {:?}",
            def.condition
        );
    }

    #[test]
    fn cant_be_blocked_attacking_alone() {
        // CR 506.5: "can't be blocked as long as it's attacking alone"
        let def =
            parse_static_line("This creature can't be blocked as long as it's attacking alone.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::CantBeBlocked);
        assert_eq!(def.condition, Some(StaticCondition::SourceAttackingAlone));
    }

    #[test]
    fn enchanted_artifact_is_creature_with_base_pt() {
        // CR 613.1d: Ensoul Artifact pattern
        let def = parse_static_line(
            "Enchanted artifact is a creature with base power and toughness 5/5 in addition to its other types.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Creature,
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetPower { value: 5 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetToughness { value: 5 }));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Artifact));
                assert!(tf.properties.contains(&FilterProp::EnchantedBy));
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn enchanted_permanent_loses_abilities_becomes_land() {
        // CR 613.1d: Imprisoned in the Moon pattern
        let def =
            parse_static_line("Enchanted permanent loses all abilities and is a colorless land.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::RemoveAllAbilities));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Land,
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetColor { colors: vec![] }));
    }

    #[test]
    fn enchanted_creature_loses_abilities_becomes_insect() {
        // CR 613.1d: Darksteel Mutation pattern
        let def = parse_static_line(
            "Enchanted creature loses all abilities and is a 0/1 green Insect creature.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::RemoveAllAbilities));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetPower { value: 0 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetToughness { value: 1 }));
    }

    // --- CantBeCast (blanket casting prohibition) tests ---

    #[test]
    fn cant_cast_creature_spells() {
        // CR 101.2: Steel Golem — "You can't cast creature spells."
        let def = parse_static_line("You can't cast creature spells.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::Controller,
            }
        );
    }

    #[test]
    fn cant_cast_instant_or_sorcery_spells() {
        // CR 101.2: Hymn of the Wilds — "You can't cast instant or sorcery spells."
        let def = parse_static_line("You can't cast instant or sorcery spells.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::Controller,
            }
        );
    }

    #[test]
    fn cant_cast_noncreature_spells() {
        // CR 101.2: Generic noncreature prohibition
        let def = parse_static_line("You can't cast noncreature spells.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::Controller,
            }
        );
    }

    // --- "don't lose the game" ---

    #[test]
    fn dont_lose_the_game() {
        // CR 104.3b: Phyrexian Unlife — "You don't lose the game for having 0 or less life."
        let def = parse_static_line("You don't lose the game for having 0 or less life.").unwrap();
        assert_eq!(def.mode, StaticMode::CantLoseTheGame);
    }

    // --- PerTurnDrawLimit tests ---

    #[test]
    fn per_turn_draw_limit_all_players() {
        // CR 101.2: Spirit of the Labyrinth
        let def =
            parse_static_line("Each player can't draw more than one card each turn.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::PerTurnDrawLimit {
                who: ProhibitionScope::AllPlayers,
                max: 1,
            }
        );
    }

    #[test]
    fn per_turn_draw_limit_opponents() {
        // CR 101.2: Narset, Parter of Veils
        let def =
            parse_static_line("Each opponent can't draw more than one card each turn.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::PerTurnDrawLimit {
                who: ProhibitionScope::Opponents,
                max: 1,
            }
        );
    }

    #[test]
    fn cant_draw_all_players() {
        let def = parse_static_line("Players can't draw cards.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantDraw {
                who: ProhibitionScope::AllPlayers,
            }
        );
    }

    #[test]
    fn cant_draw_controller() {
        let def = parse_static_line("You can't draw cards.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantDraw {
                who: ProhibitionScope::Controller,
            }
        );
    }

    #[test]
    fn cant_draw_opponents() {
        let def = parse_static_line("Your opponents can't draw cards.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantDraw {
                who: ProhibitionScope::Opponents,
            }
        );
    }

    #[test]
    fn spell_cost_reduction_uses_card_types_in_graveyard_quantity() {
        let def = parse_static_line(
            "This spell costs {1} less to cast for each card type among cards in your graveyard.",
        )
        .unwrap();
        match def.mode {
            StaticMode::ReduceCost {
                dynamic_count:
                    Some(QuantityRef::DistinctCardTypesInZone {
                        zone: ZoneRef::Graveyard,
                        scope,
                    }),
                ..
            } => assert_eq!(scope, CountScope::Controller),
            other => panic!("expected card-types-in-graveyard cost reduction, got {other:?}"),
        }
    }

    // --- Expanded CantBeCast pattern tests ---

    #[test]
    fn cant_cast_passive_voice_creature_spells() {
        // Aether Storm: "Creature spells can't be cast."
        let def = parse_static_line("Creature spells can't be cast.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::AllPlayers,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("Expected Typed filter with Creature, got {other:?}"),
        }
    }

    #[test]
    fn cant_cast_spells_with_mana_value_or_less() {
        // Brisela: "Your opponents can't cast spells with mana value 3 or less."
        let def = parse_static_line("Your opponents can't cast spells with mana value 3 or less.")
            .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::Opponents,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Cmc {
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 3 }
                    }
                )));
            }
            other => panic!("Expected Typed filter with CmcLE, got {other:?}"),
        }
    }

    #[test]
    fn cant_cast_spells_with_chosen_name() {
        // Alhammarret: "Your opponents can't cast spells with the chosen name."
        let def =
            parse_static_line("Your opponents can't cast spells with the chosen name.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::Opponents,
            }
        );
        assert_eq!(def.affected, Some(TargetFilter::HasChosenName));
    }

    #[test]
    fn cant_cast_spells_with_chosen_name_parenthetical() {
        // Alhammarret full text with parenthetical condition
        let def = parse_static_line(
            "Your opponents can't cast spells with the chosen name (as long as this creature is on the battlefield).",
        )
        .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::Opponents,
            }
        );
        assert_eq!(def.affected, Some(TargetFilter::HasChosenName));
    }

    // CR 201.3 / CR 113.6: Petrified Hamlet — "Lands with the chosen name
    // have \"{T}: Add {C}.\"" grants a quoted mana ability to every land
    // whose name matches the CardName persisted on the source by the
    // preceding ETB choose-a-land-card-name trigger.
    #[test]
    fn lands_with_chosen_name_grant_quoted_ability() {
        let def = parse_static_line("Lands with the chosen name have \"{T}: Add {C}.\"").unwrap();
        match &def.affected {
            Some(TargetFilter::And { filters }) => {
                assert_eq!(filters.len(), 2);
                assert!(
                    matches!(
                        &filters[0],
                        TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Land)
                    ),
                    "expected land typed filter, got {:?}",
                    filters[0]
                );
                assert_eq!(filters[1], TargetFilter::HasChosenName);
            }
            other => panic!("expected And[Typed(Land), HasChosenName], got {other:?}"),
        }
        assert_eq!(def.modifications.len(), 1);
        assert!(
            matches!(
                &def.modifications[0],
                ContinuousModification::GrantAbility { .. }
            ),
            "expected GrantAbility, got {:?}",
            def.modifications[0]
        );
    }

    #[test]
    fn cant_cast_spells_of_chosen_type() {
        // Archon of Valor's Reach: "Players can't cast spells of the chosen type."
        let def = parse_static_line("Players can't cast spells of the chosen type.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::AllPlayers,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf
                    .properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::IsChosenCardType)));
            }
            other => panic!("Expected Typed filter with IsChosenCardType, got {other:?}"),
        }
    }

    #[test]
    fn enchanted_controller_cant_cast_creature_spells() {
        // Brand of Ill Omen: "Enchanted creature's controller can't cast creature spells."
        let def = parse_static_line("Enchanted creature's controller can't cast creature spells.")
            .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::EnchantedCreatureController,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("Expected Typed filter with Creature, got {other:?}"),
        }
    }

    #[test]
    fn cant_cast_mana_value_or_greater() {
        // Angel of Eternal Dawn pattern: "can't cast spells with mana value 5 or greater"
        let def =
            parse_static_line("Your opponents can't cast spells with mana value 5 or greater.")
                .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::Opponents,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Cmc {
                        comparator: Comparator::GE,
                        value: QuantityExpr::Fixed { value: 5 }
                    }
                )));
            }
            other => panic!("Expected Typed filter with CmcGE, got {other:?}"),
        }
    }

    #[test]
    fn cant_cast_opponents_creature_spells() {
        // "Your opponents can't cast creature spells." — existing pattern with opponent scope
        let def = parse_static_line("Your opponents can't cast creature spells.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::Opponents,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("Expected Typed filter with Creature, got {other:?}"),
        }
    }

    // --- MaximumHandSize tests ---

    #[test]
    fn max_hand_size_set_to_two() {
        let def = parse_static_line("Your maximum hand size is two.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::MaximumHandSize {
                modification: HandSizeModification::SetTo(2),
            }
        );
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                ..
            }))
        ));
    }

    #[test]
    fn max_hand_size_set_to_twenty() {
        let def = parse_static_line("Your maximum hand size is twenty.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::MaximumHandSize {
                modification: HandSizeModification::SetTo(20),
            }
        );
    }

    #[test]
    fn max_hand_size_increased_by_one() {
        let def = parse_static_line("Your maximum hand size is increased by one.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::MaximumHandSize {
                modification: HandSizeModification::AdjustedBy(1),
            }
        );
    }

    #[test]
    fn max_hand_size_reduced_by_three() {
        let def = parse_static_line("Your maximum hand size is reduced by three.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::MaximumHandSize {
                modification: HandSizeModification::AdjustedBy(-3),
            }
        );
    }

    #[test]
    fn max_hand_size_opponent_reduced_by_one() {
        let def =
            parse_static_line("Each opponent's maximum hand size is reduced by one.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::MaximumHandSize {
                modification: HandSizeModification::AdjustedBy(-1),
            }
        );
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::Opponent),
                ..
            }))
        ));
    }

    #[test]
    fn max_hand_size_set_to_five() {
        let def = parse_static_line("Your maximum hand size is five.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::MaximumHandSize {
                modification: HandSizeModification::SetTo(5),
            }
        );
    }

    // --- Group A: AssignDamageFromToughness global and self-referential variants ---

    #[test]
    fn static_assigns_damage_from_toughness_all_creatures() {
        // CR 510.1c: Global variant without "you control" — affects all creatures.
        let def = parse_static_line(
            "Each creature assigns combat damage equal to its toughness rather than its power.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter::creature()))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AssignDamageFromToughness));
    }

    #[test]
    fn static_assigns_damage_from_toughness_self() {
        // CR 510.1c: Self-referential variant — "This creature assigns..."
        let def = parse_static_line(
            "This creature assigns combat damage equal to its toughness rather than its power.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AssignDamageFromToughness));
    }

    #[test]
    fn static_assign_damage_as_though_unblocked_self() {
        let def = parse_static_line(
            "You may have this creature assign its combat damage as though it weren't blocked.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AssignDamageAsThoughUnblocked));
    }

    #[test]
    fn static_assign_damage_as_though_unblocked_enchanted_controller() {
        let def = parse_static_line(
            "Enchanted creature's controller may have it assign its combat damage as though it weren't blocked.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AssignDamageAsThoughUnblocked));
    }

    // --- Group C: Casting prohibition variants ---

    #[test]
    fn cant_cast_during_your_turn_opponents() {
        // CR 101.2: Temporal-prefix pattern — "During your turn, your opponents can't cast spells"
        let def = parse_static_line(
            "During your turn, your opponents can't cast spells or activate abilities of artifacts, creatures, or enchantments.",
        )
        .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantCastDuring {
                who: ProhibitionScope::Opponents,
                when: CastingProhibitionCondition::DuringYourTurn,
            }
        );
    }

    #[test]
    fn cant_cast_opponents_same_name() {
        // CR 101.2: "can't cast spells with the same name as" — approximate prohibition
        let def = parse_static_line(
            "Your opponents can't cast spells with the same name as a card exiled with Dragonlord Dromoka.",
        )
        .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::Opponents,
            }
        );
    }

    #[test]
    fn cant_cast_noncreature_mv4_or_greater() {
        // CR 101.2: Passive voice with mana value filter
        let def =
            parse_static_line("Noncreature spells with mana value 4 or greater can't be cast.")
                .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::AllPlayers,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Creature))));
                assert!(tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Cmc {
                        comparator: Comparator::GE,
                        value: QuantityExpr::Fixed { value: 4 }
                    }
                )));
            }
            other => panic!("Expected Typed filter with Noncreature + CmcGE, got {other:?}"),
        }
    }

    #[test]
    fn cant_cast_enchanted_player_per_turn_limit() {
        // CR 101.2 + CR 303.4e: "Enchanted player can't cast more than one spell each turn."
        let def = parse_static_line("Enchanted player can't cast more than one spell each turn.")
            .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::PerTurnCastLimit {
                who: ProhibitionScope::EnchantedCreatureController,
                max: 1,
                spell_filter: None,
            }
        );
    }

    #[test]
    fn cant_cast_during_combat_instants() {
        // CR 101.2: Temporal-prefix — "During combat, players can't cast instant spells..."
        let def = parse_static_line(
            "During combat, players can't cast instant spells or activate abilities that aren't mana abilities.",
        )
        .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantCastDuring {
                who: ProhibitionScope::AllPlayers,
                when: CastingProhibitionCondition::DuringCombat,
            }
        );
        // Should have instant spell filter
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Instant));
            }
            other => panic!("Expected Typed filter with Instant, got {other:?}"),
        }
    }

    #[test]
    fn cant_cast_spells_of_chosen_color() {
        // CR 101.2: "can't cast spells of the chosen color"
        let def =
            parse_static_line("Your opponents can't cast spells of the chosen color.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::Opponents,
            }
        );
    }

    #[test]
    fn cant_cast_spells_with_even_mana_values() {
        // CR 101.2: "can't cast spells with even mana values"
        let def =
            parse_static_line("Your opponents can't cast spells with even mana values.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::Opponents,
            }
        );
    }

    #[test]
    fn cant_cast_by_paying_alternative_costs() {
        // CR 101.2: "can't cast spells by paying alternative costs"
        let def =
            parse_static_line("Players can't cast spells by paying alternative costs.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::AllPlayers,
            }
        );
    }

    #[test]
    fn cant_cast_opponent_attacked_this_turn() {
        // CR 101.2: "Each opponent who attacked this turn can't cast spells"
        let def = parse_static_line(
            "Each opponent who attacked with a creature this turn can't cast spells.",
        )
        .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::Opponents,
            }
        );
    }

    // --- Group A: Enchanted land type changes ---

    #[test]
    fn enchanted_land_is_island() {
        // CR 305.7: "Enchanted land is an Island." — replacement semantics via "is an"
        let def = parse_static_line("Enchanted land is an Island.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(ref tf)) = def.affected {
            assert!(
                tf.type_filters.contains(&TypeFilter::Land),
                "Expected Land type filter"
            );
            assert!(
                tf.properties.contains(&FilterProp::EnchantedBy),
                "Expected EnchantedBy property"
            );
        } else {
            panic!(
                "Expected Typed filter with Land + EnchantedBy, got {:?}",
                def.affected
            );
        }
        assert!(
            def.modifications
                .contains(&ContinuousModification::SetBasicLandType {
                    land_type: BasicLandType::Island,
                }),
            "Expected SetBasicLandType Island, got {:?}",
            def.modifications
        );
    }

    #[test]
    fn enchanted_land_every_basic_land_type() {
        // CR 305.7: "Enchanted land is every basic land type in addition to its other types."
        let def = parse_static_line(
            "Enchanted land is every basic land type in addition to its other types.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(ref tf)) = def.affected {
            assert!(tf.properties.contains(&FilterProp::EnchantedBy));
        } else {
            panic!("Expected Typed filter with EnchantedBy");
        }
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddAllBasicLandTypes),
            "Expected AddAllBasicLandTypes, got {:?}",
            def.modifications
        );
    }

    #[test]
    fn enchanted_land_multiple_types() {
        // CR 305.7: "Enchanted land is a Mountain, Forest, and Plains." — multi-type replacement
        let def = parse_static_line("Enchanted land is a Mountain, Forest, and Plains.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(ref tf)) = def.affected {
            assert!(tf.properties.contains(&FilterProp::EnchantedBy));
        } else {
            panic!("Expected Typed filter with EnchantedBy");
        }
        // First type is SetBasicLandType (clears old subtypes), rest are AddSubtype
        assert!(
            def.modifications
                .contains(&ContinuousModification::SetBasicLandType {
                    land_type: BasicLandType::Mountain,
                }),
            "Expected SetBasicLandType Mountain"
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddSubtype {
                    subtype: "Forest".to_string(),
                }),
            "Expected AddSubtype Forest"
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddSubtype {
                    subtype: "Plains".to_string(),
                }),
            "Expected AddSubtype Plains"
        );
    }

    // --- Group B: Colorless/Multicolored/Snow lord pump ---

    #[test]
    fn static_other_colorless_creatures_get_plus() {
        // CR 105.2b: "Other colorless creatures you control get +1/+1."
        let def = parse_static_line("Other colorless creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(ref tf)) = def.affected {
            assert!(tf.properties.contains(&FilterProp::Colorless));
            assert!(tf.properties.contains(&FilterProp::Another));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!("Expected Typed filter, got {:?}", def.affected);
        }
    }

    #[test]
    fn static_ygra_additive_food_artifact_grants_food_ability() {
        let def = parse_static_line(
            "Other creatures are Food artifacts in addition to their other types and have \"{2}, {T}, Sacrifice this permanent: You gain 3 life.\"",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Another]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddSubtype {
                subtype: "Food".to_string(),
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddType {
                core_type: CoreType::Artifact,
            }));
        let grant = def
            .modifications
            .iter()
            .find(|m| matches!(m, ContinuousModification::GrantAbility { .. }));
        assert!(grant.is_some(), "expected granted activated Food ability");
        if let Some(ContinuousModification::GrantAbility { definition }) = grant {
            assert_eq!(definition.kind, AbilityKind::Activated);
            assert!(definition.cost.is_some());
        }
    }

    #[test]
    fn static_kudo_adds_bear_subtype_alongside_base_pt() {
        let def = parse_static_line(
            "Other creatures have base power and toughness 2/2 and are Bears in addition to their other types.",
        )
        .unwrap();
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetPower { value: 2 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetToughness { value: 2 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddSubtype {
                subtype: "Bear".to_string(),
            }));
    }

    #[test]
    fn static_other_multicolored_creatures_get_plus() {
        // CR 105.2: "Other multicolored creatures you control get +1/+0."
        let def = parse_static_line("Other multicolored creatures you control get +1/+0.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(ref tf)) = def.affected {
            assert!(tf.properties.contains(&FilterProp::Multicolored));
            assert!(tf.properties.contains(&FilterProp::Another));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!("Expected Typed filter, got {:?}", def.affected);
        }
    }

    #[test]
    fn static_other_snow_zombie_creatures_get_plus() {
        // CR 205.4a: "Other snow and Zombie creatures you control get +1/+1."
        let def =
            parse_static_line("Other snow and Zombie creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(ref tf)) = def.affected {
            assert!(
                tf.properties.contains(&FilterProp::HasSupertype {
                    value: Supertype::Snow,
                }),
                "Expected HasSupertype Snow, got {:?}",
                tf.properties
            );
            assert!(
                tf.type_filters
                    .contains(&TypeFilter::Subtype("Zombie".to_string())),
                "Expected Zombie subtype, got {:?}",
                tf.type_filters
            );
            assert!(tf.properties.contains(&FilterProp::Another));
        } else {
            panic!("Expected Typed filter, got {:?}", def.affected);
        }
    }

    // --- Group C: All permanents are [type] ---

    #[test]
    fn static_all_permanents_are_artifacts() {
        // CR 205.1a: "All permanents are artifacts in addition to their other types."
        let def =
            parse_static_line("All permanents are artifacts in addition to their other types.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(ref tf)) = def.affected {
            assert!(
                tf.type_filters.contains(&TypeFilter::Permanent),
                "Expected Permanent type filter"
            );
        } else {
            panic!("Expected Typed filter with Permanent");
        }
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddType {
                    core_type: crate::types::card_type::CoreType::Artifact,
                }),
            "Expected AddType Artifact, got {:?}",
            def.modifications
        );
    }

    #[test]
    fn static_all_permanents_are_enchantments() {
        // CR 205.1a: "All permanents are enchantments in addition to their other types."
        let def =
            parse_static_line("All permanents are enchantments in addition to their other types.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddType {
                    core_type: crate::types::card_type::CoreType::Enchantment,
                }),
            "Expected AddType Enchantment"
        );
    }

    // --- Group C2: All [subject] are [color] (global color-defining statics) ---

    #[test]
    fn static_all_creatures_are_black() {
        // CR 613.1e + CR 105.1: Darkest Hour — "All creatures are black."
        let def = parse_static_line("All creatures are black.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(ref tf)) = def.affected {
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "Expected Creature type filter, got {:?}",
                tf.type_filters
            );
        } else {
            panic!("Expected Typed filter, got {:?}", def.affected);
        }
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::SetColor {
                colors: vec![ManaColor::Black]
            }]
        );
    }

    #[test]
    fn static_all_permanents_are_colorless() {
        // CR 613.1e + CR 105.2c: Thran Lens — "All permanents are colorless."
        let def = parse_static_line("All permanents are colorless.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(ref tf)) = def.affected {
            assert!(
                tf.type_filters.contains(&TypeFilter::Permanent),
                "Expected Permanent type filter, got {:?}",
                tf.type_filters
            );
        } else {
            panic!("Expected Typed filter, got {:?}", def.affected);
        }
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::SetColor { colors: vec![] }]
        );
    }

    #[test]
    fn static_all_slivers_are_colorless() {
        // CR 613.1e + CR 105.2c: Ghostflame Sliver — "All Slivers are colorless."
        // Plural subtype path: parse_subtype canonicalizes "Slivers" → "Sliver".
        let def = parse_static_line("All Slivers are colorless.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(ref tf)) = def.affected {
            assert!(
                tf.type_filters
                    .contains(&TypeFilter::Subtype("Sliver".to_string())),
                "Expected Sliver subtype filter, got {:?}",
                tf.type_filters
            );
        } else {
            panic!("Expected Typed filter, got {:?}", def.affected);
        }
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::SetColor { colors: vec![] }]
        );
    }

    #[test]
    fn static_all_subject_are_color_does_not_eat_get_plus_lines() {
        // Regression guard: "All creatures get +1/+1." must still reach the
        // gets_has branch, not be swallowed by the color-set handler.
        let def = parse_static_line("All creatures get +1/+1.").unwrap();
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
        for m in &def.modifications {
            assert!(
                !matches!(m, ContinuousModification::SetColor { .. }),
                "Unexpected SetColor in gets-pump line, got {:?}",
                def.modifications
            );
        }
    }

    #[test]
    fn static_all_subject_are_color_rejects_in_addition_type_form() {
        // Regression guard: "All permanents are artifacts in addition to ..."
        // must route to parse_all_permanents_are_type (AddType), not be mis-parsed
        // here. parse_color_predicate rejects the trailing " in addition..." suffix
        // because it's not a bare color word.
        let def =
            parse_static_line("All permanents are artifacts in addition to their other types.")
                .unwrap();
        for m in &def.modifications {
            assert!(
                !matches!(m, ContinuousModification::SetColor { .. }),
                "Unexpected SetColor for type-addition line, got {:?}",
                def.modifications
            );
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Artifact,
            }));
    }

    #[test]
    fn static_all_elves_are_green() {
        // CR 613.1e + CR 105.1: non-black, non-colorless color on a plural
        // creature subtype — exercises the parse_color_list single-color path
        // plus typed_filter_for_subtype routing.
        let def = parse_static_line("All Elves are green.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(ref tf)) = def.affected {
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "Expected Creature type filter (Elves route via typed_filter_for_subtype), \
                 got {:?}",
                tf.type_filters
            );
            assert!(
                tf.type_filters
                    .contains(&TypeFilter::Subtype("Elf".to_string())),
                "Expected Elf subtype filter, got {:?}",
                tf.type_filters
            );
        } else {
            panic!("Expected Typed filter, got {:?}", def.affected);
        }
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::SetColor {
                colors: vec![ManaColor::Green]
            }]
        );
    }

    #[test]
    fn static_all_treasures_are_colorless() {
        // CR 613.1e + CR 105.2c: artifact-subtype subject — `typed_filter_for_subtype`
        // must route Treasure → Artifact core type, not default to Creature.
        let def = parse_static_line("All Treasures are colorless.").unwrap();
        if let Some(TargetFilter::Typed(ref tf)) = def.affected {
            assert!(
                tf.type_filters.contains(&TypeFilter::Artifact),
                "Expected Artifact core type for Treasures, got {:?}",
                tf.type_filters
            );
            assert!(
                tf.type_filters
                    .contains(&TypeFilter::Subtype("Treasure".to_string())),
                "Expected Treasure subtype filter, got {:?}",
                tf.type_filters
            );
        } else {
            panic!("Expected Typed filter, got {:?}", def.affected);
        }
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::SetColor { colors: vec![] }]
        );
    }

    #[test]
    fn static_all_creatures_are_white_and_blue() {
        // CR 105.1: multi-color predicate via parse_color_list. Verifies the
        // predicate path is not limited to single colors.
        let def = parse_static_line("All creatures are white and blue.").unwrap();
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::SetColor {
                colors: vec![ManaColor::White, ManaColor::Blue]
            }]
        );
    }

    #[test]
    fn static_all_subject_are_color_falls_through_to_land_type_change() {
        // Regression guard: "All lands are Plains." has a non-color predicate,
        // so parse_color_predicate must reject and allow the outer dispatcher
        // to continue through to parse_land_type_change. Expect SetBasicLandType
        // (or equivalent land-type machinery) — not SetColor.
        let def = parse_static_line("All lands are Plains.").unwrap();
        assert!(
            !def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::SetColor { .. })),
            "land type-change line must not produce SetColor, got {:?}",
            def.modifications
        );
        assert!(
            def.modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::SetBasicLandType { .. }
                    | ContinuousModification::AddSubtype { .. }
            )),
            "expected a land-type modification, got {:?}",
            def.modifications
        );
    }

    // --- Group A: Chosen color/type creature pump ---

    #[test]
    fn static_chosen_color_pump() {
        // Hall of Triumph: "Creatures you control of the chosen color get +1/+1."
        let def =
            parse_static_line("Creatures you control of the chosen color get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(
                    tf.properties.contains(&FilterProp::IsChosenColor),
                    "Expected IsChosenColor property"
                );
            }
            other => panic!("Expected Some(Typed filter), got {other:?}"),
        }
    }

    #[test]
    fn static_chosen_type_pump() {
        // "Creatures of the chosen type your opponents control get -1/-1."
        let def =
            parse_static_line("Creatures of the chosen type your opponents control get -1/-1.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                assert!(
                    tf.properties.contains(&FilterProp::IsChosenCreatureType),
                    "Expected IsChosenCreatureType property"
                );
            }
            other => panic!("Expected Some(Typed filter), got {other:?}"),
        }
    }

    // --- Group B: Generic activated ability cost reduction ---

    #[test]
    fn static_reduce_activated_ability_cost_generic() {
        // Training Grounds: "Activated abilities of creatures you control cost {2} less to activate."
        let def = parse_static_line(
            "Activated abilities of creatures you control cost {2} less to activate.",
        )
        .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::ReduceAbilityCost {
                keyword: "activated".to_string(),
                amount: 2,
            }
        );
    }

    // --- Group C: Spells you cast have keyword ---

    #[test]
    fn static_creature_spells_have_convoke() {
        // "Creature spells you cast have convoke."
        let def = parse_static_line("Creature spells you cast have convoke.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CastWithKeyword {
                keyword: Keyword::Convoke,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(
                    tf.type_filters.contains(&TypeFilter::Creature),
                    "Expected Creature type filter"
                );
            }
            other => panic!("Expected Some(Typed filter), got {other:?}"),
        }
    }

    #[test]
    fn static_noncreature_spells_have_convoke() {
        // "Noncreature spells you cast have convoke."
        let def = parse_static_line("Noncreature spells you cast have convoke.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CastWithKeyword {
                keyword: Keyword::Convoke,
            }
        );
    }

    #[test]
    fn static_spells_from_exile_have_convoke() {
        // "Spells you cast from exile have convoke."
        let def = parse_static_line("Spells you cast from exile have convoke.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CastWithKeyword {
                keyword: Keyword::Convoke,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(
                    tf.properties
                        .contains(&FilterProp::InZone { zone: Zone::Exile }),
                    "Expected InZone(Exile) property"
                );
            }
            other => panic!("Expected Some(Typed filter), got {other:?}"),
        }
    }

    #[test]
    fn static_spells_with_mana_value_ge_have_cascade() {
        // Imoti, Celebrant of Bounty: "Spells you cast with mana value 6 or greater have cascade."
        let def = parse_static_line("Spells you cast with mana value 6 or greater have cascade.")
            .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CastWithKeyword {
                keyword: Keyword::Cascade,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(
                    tf.properties.contains(&FilterProp::Cmc {
                        comparator: Comparator::GE,
                        value: QuantityExpr::Fixed { value: 6 },
                    }),
                    "Expected CmcGE(6) property, got {:?}",
                    tf.properties
                );
            }
            other => panic!("Expected Some(Typed filter), got {other:?}"),
        }
    }

    #[test]
    fn static_creature_spells_with_mana_value_ge_have_keyword() {
        // Type-prefixed + MV qualifier: confirms the type filter and the
        // CmcGE prop coexist on the same affected filter.
        let def = parse_static_line(
            "Creature spells you cast with mana value 4 or greater have trample.",
        )
        .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CastWithKeyword {
                keyword: Keyword::Trample,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(
                    tf.type_filters.contains(&TypeFilter::Creature),
                    "Expected Creature type filter, got {:?}",
                    tf.type_filters
                );
                assert!(
                    tf.properties.contains(&FilterProp::Cmc {
                        comparator: Comparator::GE,
                        value: QuantityExpr::Fixed { value: 4 },
                    }),
                    "Expected CmcGE(4), got {:?}",
                    tf.properties
                );
            }
            other => panic!("Expected Some(Typed filter), got {other:?}"),
        }
    }

    #[test]
    fn static_spells_from_exile_with_mana_value_ge_have_keyword() {
        // Combined zone + MV qualifier — both should land on the same filter.
        let def = parse_static_line(
            "Spells you cast from exile with mana value 4 or greater have cascade.",
        )
        .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CastWithKeyword {
                keyword: Keyword::Cascade,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(
                    tf.properties
                        .contains(&FilterProp::InZone { zone: Zone::Exile }),
                    "Expected InZone(Exile), got {:?}",
                    tf.properties
                );
                assert!(
                    tf.properties.contains(&FilterProp::Cmc {
                        comparator: Comparator::GE,
                        value: QuantityExpr::Fixed { value: 4 },
                    }),
                    "Expected CmcGE(4), got {:?}",
                    tf.properties
                );
            }
            other => panic!("Expected Some(Typed filter), got {other:?}"),
        }
    }

    #[test]
    fn static_creature_spells_have_convoke_no_mv_regression() {
        // Regression: bare "have keyword" without an MV/zone qualifier still
        // parses cleanly (the cursor walk must not require any qualifier).
        let def = parse_static_line("Creature spells you cast have convoke.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CastWithKeyword {
                keyword: Keyword::Convoke,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(
                    !tf.properties.iter().any(|p| matches!(
                        p,
                        FilterProp::Cmc {
                            comparator: Comparator::GE,
                            ..
                        } | FilterProp::Cmc {
                            comparator: Comparator::LE,
                            ..
                        }
                    )),
                    "Did not expect any Cmc property, got {:?}",
                    tf.properties
                );
            }
            other => panic!("Expected Some(Typed filter), got {other:?}"),
        }
    }

    #[test]
    fn static_creature_cards_not_on_battlefield_have_flash() {
        // Leyline of Anticipation variant: "Creature cards you own that aren't on the battlefield have flash."
        let def =
            parse_static_line("Creature cards you own that aren't on the battlefield have flash.")
                .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CastWithKeyword {
                keyword: Keyword::Flash,
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(
                    tf.type_filters.contains(&TypeFilter::Creature),
                    "Expected Creature type filter"
                );
            }
            other => panic!("Expected Some(Typed filter), got {other:?}"),
        }
    }

    // --- Group: Prohibition-family statics (CR 305.1, 701.21, 701.27, 702.5, 702.6) ---
    // Each test proves that `parse_static_line` / `parse_static_line_multi` emits the
    // canonical `StaticMode::Other("...")` name so the corresponding runtime guard in
    // the engine (e.g., `object_has_static_other(id, "CantBeSacrificed")`) can observe it.

    #[test]
    fn static_cant_be_sacrificed_self_ref() {
        // CR 701.21: Hithlain Rope — "This artifact can't be sacrificed."
        let def = parse_static_line("This artifact can't be sacrificed.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantBeSacrificed".to_string()));
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_cant_be_enchanted_self_ref() {
        // CR 702.5: Anti-Magic Aura variant — "This creature can't be enchanted by other Auras."
        let def = parse_static_line("This creature can't be enchanted by other Auras.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantBeEnchanted".to_string()));
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_cant_be_equipped_self_ref() {
        // CR 702.6: Goblin Brawler — "This creature can't be equipped."
        let def = parse_static_line("This creature can't be equipped.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantBeEquipped".to_string()));
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_life_total_cant_change_emits_both_locks_self_scope() {
        // CR 119.7 + CR 119.8: Platinum Emperion — "Your life total can't change."
        // Must emit BOTH CantGainLife and CantLoseLife scoped to controller.
        let defs = parse_static_line_multi("Your life total can't change.");
        let modes: Vec<_> = defs.iter().map(|d| d.mode.clone()).collect();
        assert_eq!(modes.len(), 2, "expected exactly 2 statics, got {modes:?}");
        assert!(modes.contains(&StaticMode::CantGainLife));
        assert!(modes.contains(&StaticMode::CantLoseLife));
        for def in &defs {
            assert!(matches!(
                def.affected,
                Some(TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::You),
                    ..
                }))
            ));
        }
    }

    #[test]
    fn static_life_total_cant_change_opponent_scope() {
        // CR 119.7 + CR 119.8: "Your opponents' life totals can't change."
        let defs = parse_static_line_multi("Your opponents' life totals can't change.");
        let modes: Vec<_> = defs.iter().map(|d| d.mode.clone()).collect();
        assert_eq!(modes.len(), 2);
        assert!(modes.contains(&StaticMode::CantGainLife));
        assert!(modes.contains(&StaticMode::CantLoseLife));
        for def in &defs {
            assert!(matches!(
                def.affected,
                Some(TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::Opponent),
                    ..
                }))
            ));
        }
    }

    #[test]
    fn static_life_total_cannot_change_alt_spelling() {
        // "cannot" alternative phrasing should also work.
        let defs = parse_static_line_multi("Your life total cannot change.");
        let modes: Vec<_> = defs.iter().map(|d| d.mode.clone()).collect();
        assert_eq!(modes.len(), 2);
        assert!(modes.contains(&StaticMode::CantGainLife));
        assert!(modes.contains(&StaticMode::CantLoseLife));
    }

    #[test]
    fn static_cant_be_equipped_or_enchanted_compound_multi() {
        // CR 701.3 + CR 702.5 + CR 702.6: The compound phrase must emit BOTH
        // CantBeEquipped and CantBeEnchanted. Fortifications are excluded by wording,
        // so CantBeAttached must NOT be emitted.
        let defs = parse_static_line_multi("This creature can't be equipped or enchanted.");
        let modes: Vec<_> = defs.iter().map(|d| d.mode.clone()).collect();
        assert!(
            modes.contains(&StaticMode::Other("CantBeEquipped".to_string())),
            "expected CantBeEquipped in {modes:?}"
        );
        assert!(
            modes.contains(&StaticMode::Other("CantBeEnchanted".to_string())),
            "expected CantBeEnchanted in {modes:?}"
        );
        assert!(
            !modes.contains(&StaticMode::Other("CantBeAttached".to_string())),
            "CantBeAttached is a superset and must not be emitted"
        );
    }

    #[test]
    fn static_enchanted_creature_loses_abilities_and_cant_attack_or_block() {
        let defs = parse_static_line_multi(
            "Enchanted creature loses all abilities and can't attack or block.",
        );
        assert_eq!(defs.len(), 2, "expected two statics, got {defs:?}");
        assert!(defs.iter().any(|def| {
            def.mode == StaticMode::Continuous
                && def
                    .modifications
                    .contains(&ContinuousModification::RemoveAllAbilities)
        }));
        assert!(defs
            .iter()
            .any(|def| def.mode == StaticMode::CantAttackOrBlock));
        for def in defs {
            assert_eq!(
                def.affected,
                Some(TargetFilter::Typed(
                    TypedFilter::creature().properties(vec![FilterProp::EnchantedBy])
                ))
            );
        }
    }

    #[test]
    fn static_enchanted_creature_cant_attack_or_block_uses_enchanted_subject() {
        let def = parse_static_line("Enchanted creature can't attack or block.").unwrap();
        assert_eq!(def.mode, StaticMode::CantAttackOrBlock);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy])
            ))
        );
    }

    #[test]
    fn static_cant_transform_self_ref() {
        // CR 701.27: Immerwolf-style "non-Human Werewolves you control can't transform"
        // after subject-stripping reduces to the self-ref form in parse_static_line.
        let def = parse_static_line("This creature can't transform.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantTransform".to_string()));
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_cant_play_lands_you() {
        // CR 305.1: Aggressive Mining — "You can't play lands."
        let def = parse_static_line("You can't play lands.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantPlayLand".to_string()));
        assert!(
            def.affected.is_some(),
            "affected player-scope filter required"
        );
    }

    #[test]
    fn static_cant_play_lands_players() {
        // CR 305.1: Worms of the Earth — "Players can't play lands."
        let def = parse_static_line("Players can't play lands.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantPlayLand".to_string()));
        assert!(
            def.affected.is_some(),
            "affected player-scope filter required"
        );
    }

    // --- CR 602.5 + CR 603.2a: Global filter-scoped CantBeActivated (Clarion/Karn class) ---

    #[test]
    fn cant_be_activated_self_ref_preserves_legacy_semantics() {
        // CR 602.5: Self-reference form (Chalice-of-Life class) must emit the
        // unit-default shape: `who = AllPlayers, source_filter = SelfRef`.
        let def = parse_static_line("Its activated abilities can't be activated.").unwrap();
        match def.mode {
            StaticMode::CantBeActivated {
                who,
                source_filter,
                exemption,
            } => {
                assert_eq!(who, ProhibitionScope::AllPlayers);
                assert_eq!(source_filter, TargetFilter::SelfRef);
                // CR 605.1a: Self-ref form has no exemption suffix.
                assert_eq!(exemption, ActivationExemption::None);
            }
            other => panic!("expected CantBeActivated, got {other:?}"),
        }
    }

    #[test]
    fn cant_be_activated_clarion_multi_type_filter() {
        // CR 602.5 + CR 603.2a: Clarion Conqueror — "Activated abilities of artifacts,
        // creatures, and planeswalkers your opponents control can't be activated."
        // The activator axis is AllPlayers; opponent-ness rides on the filter's
        // `ControllerRef::Opponent`. `parse_type_phrase` emits an `Or`-disjunction of
        // `Typed` filters when a comma-separated type list is present — each variant
        // inherits the shared controller suffix via the post-process pass.
        let def = parse_static_line(
            "Activated abilities of artifacts, creatures, and planeswalkers your opponents control can't be activated.",
        )
        .expect("Clarion Conqueror Oracle text should parse");
        match def.mode {
            StaticMode::CantBeActivated {
                who,
                source_filter: TargetFilter::Or { filters },
                exemption,
            } => {
                assert_eq!(who, ProhibitionScope::AllPlayers);
                assert_eq!(exemption, ActivationExemption::None);
                assert_eq!(filters.len(), 3, "three type variants expected");
                // Each disjunct should be a Typed filter with opponent controller and
                // one of the three expected type filters.
                let mut seen_types: Vec<TypeFilter> = Vec::new();
                for f in &filters {
                    match f {
                        TargetFilter::Typed(tf) => {
                            assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                            assert_eq!(tf.type_filters.len(), 1);
                            seen_types.push(tf.type_filters[0].clone());
                        }
                        other => panic!("expected Typed variant, got {other:?}"),
                    }
                }
                assert!(seen_types.iter().any(|t| matches!(t, TypeFilter::Artifact)));
                assert!(seen_types.iter().any(|t| matches!(t, TypeFilter::Creature)));
                assert!(seen_types
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Planeswalker)));
            }
            other => panic!("expected CantBeActivated with Or-disjunction filter, got {other:?}"),
        }
    }

    #[test]
    fn cant_be_activated_karn_single_type_filter() {
        // CR 602.5 + CR 603.2a: Karn, the Great Creator — "Activated abilities of
        // artifacts your opponents control can't be activated."
        let def = parse_static_line(
            "Activated abilities of artifacts your opponents control can't be activated.",
        )
        .expect("Karn Oracle text should parse");
        match def.mode {
            StaticMode::CantBeActivated {
                who,
                source_filter: TargetFilter::Typed(tf),
                exemption,
            } => {
                assert_eq!(who, ProhibitionScope::AllPlayers);
                assert_eq!(exemption, ActivationExemption::None);
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                assert_eq!(tf.type_filters, vec![TypeFilter::Artifact]);
            }
            other => panic!("expected CantBeActivated, got {other:?}"),
        }
    }

    #[test]
    fn cant_be_activated_pithing_needle_chosen_name_with_mana_exemption() {
        // CR 605.1a + CR 602.5 + CR 603.2a: Pithing Needle —
        // "Activated abilities of sources with the chosen name can't be activated
        // unless they're mana abilities."
        // Source filter binds to `HasChosenName`; exemption captures the mana-ability suffix.
        let def = parse_static_line(
            "Activated abilities of sources with the chosen name can't be activated unless they're mana abilities.",
        )
        .expect("Pithing Needle Oracle text should parse");
        match def.mode {
            StaticMode::CantBeActivated {
                who,
                source_filter,
                exemption,
            } => {
                assert_eq!(who, ProhibitionScope::AllPlayers);
                assert_eq!(source_filter, TargetFilter::HasChosenName);
                assert_eq!(exemption, ActivationExemption::ManaAbilities);
            }
            other => panic!("expected CantBeActivated, got {other:?}"),
        }
    }

    #[test]
    fn cant_be_activated_phyrexian_revoker_chosen_name_no_exemption_suffix() {
        // CR 602.5 + CR 603.2a: Phyrexian Revoker — MTGJSON Oracle text omits the
        // "unless they're mana abilities" suffix on this card. Same source filter
        // shape as Pithing Needle, but `ActivationExemption::None`. The parser must
        // produce the same `HasChosenName` AST shape regardless of exemption suffix —
        // demonstrating the optional suffix combinator works in both branches.
        let def = parse_static_line(
            "Activated abilities of sources with the chosen name can't be activated.",
        )
        .expect("Phyrexian Revoker Oracle text should parse");
        match def.mode {
            StaticMode::CantBeActivated {
                who,
                source_filter,
                exemption,
            } => {
                assert_eq!(who, ProhibitionScope::AllPlayers);
                assert_eq!(source_filter, TargetFilter::HasChosenName);
                assert_eq!(exemption, ActivationExemption::None);
            }
            other => panic!("expected CantBeActivated, got {other:?}"),
        }
    }

    #[test]
    fn cant_be_activated_sorcerous_spyglass_chosen_name_with_mana_exemption() {
        // CR 605.1a + CR 602.5: Sorcerous Spyglass — identical static on an artifact
        // that reveals an opponent's hand on ETB. Exercises composability: the static
        // parses identically regardless of the surrounding ETB shape.
        let def = parse_static_line(
            "Activated abilities of sources with the chosen name can't be activated unless they're mana abilities.",
        )
        .expect("Sorcerous Spyglass Oracle text should parse");
        match def.mode {
            StaticMode::CantBeActivated {
                source_filter,
                exemption,
                ..
            } => {
                assert_eq!(source_filter, TargetFilter::HasChosenName);
                assert_eq!(exemption, ActivationExemption::ManaAbilities);
            }
            other => panic!("expected CantBeActivated, got {other:?}"),
        }
    }

    // --- CR 701.23 + CR 609.3: CantSearchLibrary (Ashiok class) ---

    #[test]
    fn cant_search_library_ashiok() {
        // CR 701.23 + CR 609.3: Ashiok, Dream Render — "Spells and abilities your
        // opponents control can't cause their controller to search their library."
        let def = parse_static_line(
            "Spells and abilities your opponents control can't cause their controller to search their library.",
        )
        .expect("Ashiok Oracle text should parse");
        assert_eq!(
            def.mode,
            StaticMode::CantSearchLibrary {
                cause: ProhibitionScope::Opponents,
            }
        );
    }

    #[test]
    fn cant_search_library_controller_variant() {
        // Building-block coverage: `you control` should map to Controller scope.
        let def = parse_static_line(
            "Spells and abilities you control can't cause their controller to search their library.",
        )
        .expect("controller-scoped variant should parse");
        assert_eq!(
            def.mode,
            StaticMode::CantSearchLibrary {
                cause: ProhibitionScope::Controller,
            }
        );
    }

    // --- CR 603.2g + CR 603.6a + CR 700.4: SuppressTriggers (Torpor Orb / Hushbringer) ---

    #[test]
    fn suppress_triggers_torpor_orb_etb_only() {
        use crate::types::statics::SuppressedTriggerEvent;

        // CR 603.2g + CR 603.6a: Torpor Orb — "Creatures entering the battlefield
        // don't cause abilities to trigger." Event set is [EntersBattlefield] only.
        let def = parse_static_line(
            "Creatures entering the battlefield don't cause abilities to trigger.",
        )
        .expect("Torpor Orb Oracle text should parse");
        match def.mode {
            StaticMode::SuppressTriggers {
                source_filter: TargetFilter::Typed(tf),
                events,
            } => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                assert_eq!(events, vec![SuppressedTriggerEvent::EntersBattlefield]);
            }
            other => panic!("expected SuppressTriggers, got {other:?}"),
        }
    }

    #[test]
    fn suppress_triggers_torpor_orb_etb_without_the_battlefield() {
        use crate::types::statics::SuppressedTriggerEvent;

        // Errata variant: some printings drop "the battlefield" and just say
        // "Creatures entering don't cause abilities to trigger." — same semantics.
        let def = parse_static_line("Creatures entering don't cause abilities to trigger.")
            .expect("Short-form Oracle should parse");
        match def.mode {
            StaticMode::SuppressTriggers { events, .. } => {
                assert_eq!(events, vec![SuppressedTriggerEvent::EntersBattlefield]);
            }
            other => panic!("expected SuppressTriggers, got {other:?}"),
        }
    }

    #[test]
    fn suppress_triggers_hushbringer_accepts_and_dying_variant() {
        use crate::types::statics::SuppressedTriggerEvent;

        // CR 603.2g + CR 700.4: The "and dying" phrasing is also accepted for
        // defensive parsing of errata/near-variants. Same event set as "or dying".
        let def = parse_static_line(
            "Creatures entering the battlefield and dying don't cause abilities to trigger.",
        )
        .expect("'and dying' variant should parse");
        match def.mode {
            StaticMode::SuppressTriggers { events, .. } => {
                assert_eq!(
                    events,
                    vec![
                        SuppressedTriggerEvent::EntersBattlefield,
                        SuppressedTriggerEvent::Dies,
                    ]
                );
            }
            other => panic!("expected SuppressTriggers, got {other:?}"),
        }
    }

    #[test]
    fn suppress_triggers_hushbringer_etb_and_dies() {
        use crate::types::statics::SuppressedTriggerEvent;

        // CR 603.2g + CR 603.6a + CR 700.4: Hushbringer's actual MTGJSON Oracle
        // text is "Creatures entering or dying don't cause abilities to trigger."
        // Event set is [EntersBattlefield, Dies] in canonical order.
        let def =
            parse_static_line("Creatures entering or dying don't cause abilities to trigger.")
                .expect("Hushbringer Oracle text should parse");
        match def.mode {
            StaticMode::SuppressTriggers {
                source_filter: TargetFilter::Typed(tf),
                events,
            } => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                assert_eq!(
                    events,
                    vec![
                        SuppressedTriggerEvent::EntersBattlefield,
                        SuppressedTriggerEvent::Dies,
                    ]
                );
            }
            other => panic!("expected SuppressTriggers, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------------
    // Inverted "As long as <cond>, <effect>" rewrite tests (CR 611.3a)
    // ------------------------------------------------------------------------

    fn rewrite(text: &str) -> Option<String> {
        let stripped = strip_reminder_text(text);
        let lower = stripped.to_lowercase();
        let tp = TextPair::new(&stripped, &lower);
        try_split_inverted_as_long_as(&tp).map(|s| s.canonical)
    }

    fn split_condition(text: &str) -> Option<String> {
        let stripped = strip_reminder_text(text);
        let lower = stripped.to_lowercase();
        let tp = TextPair::new(&stripped, &lower);
        try_split_inverted_as_long_as(&tp).map(|s| s.condition_text)
    }

    #[test]
    fn inverted_rewrites_auriok_shape() {
        let got = rewrite(
            "As long as ~ is equipped, each creature you control that's a Soldier or a Knight gets +1/+1.",
        )
        .expect("auriok shape must rewrite");
        assert_eq!(
            got,
            "each creature you control that's a Soldier or a Knight gets +1/+1 as long as ~ is equipped"
        );
    }

    #[test]
    fn inverted_rewrites_watchdog_shape() {
        let got = rewrite("As long as ~ is untapped, all creatures attacking you get -1/-0.")
            .expect("watchdog shape must rewrite");
        assert_eq!(
            got,
            "all creatures attacking you get -1/-0 as long as ~ is untapped"
        );
    }

    #[test]
    fn inverted_preserves_original_case() {
        let got = rewrite("As long as ~ is attacking, defending player can't cast spells.")
            .expect("should rewrite");
        assert!(got.contains("defending player can't cast spells"));
        assert!(got.ends_with("as long as ~ is attacking"));
    }

    #[test]
    fn inverted_returns_none_without_commas() {
        let got = rewrite("As long as ~ is red with no trailing clause at all without commas");
        assert!(got.is_none());
    }

    #[test]
    fn inverted_liu_bei_internal_commas_without_effect_subject() {
        // Liu Bei, Lord of Shu: "you control a permanent named Guan Yu, Sainted Warrior or a
        // permanent named Zhang Fei, Fierce Warrior" — commas are inside the condition and
        // no trailing effect clause starts with a recognized subject, so the scanner must
        // not split (returns None).
        let got = rewrite(
            "As long as you control a permanent named Guan Yu, Sainted Warrior or a permanent named Zhang Fei, Fierce Warrior",
        );
        assert!(
            got.is_none(),
            "must not split on condition-internal commas without effect subject; got {got:?}"
        );
    }

    #[test]
    fn inverted_handles_trailing_period() {
        let got = rewrite("As long as ~ is equipped, it gets +1/+1.").expect("must rewrite");
        assert!(!got.ends_with('.'));
        assert_eq!(got, "it gets +1/+1 as long as ~ is equipped");
    }

    #[test]
    fn effect_subject_prefix_word_boundary() {
        assert!(parse_effect_subject_prefix("it gets +1/+1").is_ok());
        // Word boundary: "its mana value" must NOT match via "it ".
        assert!(parse_effect_subject_prefix("its mana value is 4").is_err());
        assert!(parse_effect_subject_prefix("each creature you control gets +1/+1").is_ok());
        assert!(parse_effect_subject_prefix("eachother").is_err());
    }

    #[test]
    fn inverted_splits_auriok_condition_cleanly() {
        // The primary success criterion: the condition is separated from the effect clause.
        // Whether the effect clause parses into modifications depends on downstream
        // subject-phrase support, which is separate work.
        let cond = split_condition(
            "As long as ~ is equipped, each creature you control that's a Soldier or a Knight gets +1/+1.",
        )
        .expect("must split");
        assert_eq!(cond, "~ is equipped");
    }

    #[test]
    fn inverted_splits_watchdog_condition_cleanly() {
        let cond =
            split_condition("As long as ~ is untapped, all creatures attacking you get -1/-0.")
                .expect("must split");
        assert_eq!(cond, "~ is untapped");
    }

    #[test]
    fn inverted_end_to_end_auriok_no_effect_bleed() {
        // End-to-end: the returned StaticDefinition must have a condition text that is
        // ONLY the condition (no effect-clause bleed-through). Modifications may remain
        // empty if downstream subject-phrase parsing doesn't yet handle the effect,
        // but that is a separate issue (and explicitly out-of-scope per task spec).
        let def = parse_static_line(
            "As long as ~ is equipped, each creature you control that's a Soldier or a Knight gets +1/+1.",
        )
        .expect("must parse");
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.condition {
            Some(StaticCondition::Unrecognized { text }) => {
                assert_eq!(text, "~ is equipped", "condition must be cleanly split");
                assert!(
                    !text.contains("gets +1/+1"),
                    "effect clause bled into condition text: {text:?}"
                );
            }
            Some(other) => {
                // Typed condition recognized — also acceptable, just confirm it's not
                // the bleed-through fallback.
                eprintln!("auriok: got typed condition {other:?}");
            }
            None => panic!("condition must be set"),
        }
        assert_eq!(
            def.description.as_deref(),
            Some(
                "As long as ~ is equipped, each creature you control that's a Soldier or a Knight gets +1/+1."
            ),
            "description must equal the original printed oracle text"
        );
    }

    #[test]
    fn inverted_end_to_end_watchdog_no_effect_bleed() {
        let def =
            parse_static_line("As long as ~ is untapped, all creatures attacking you get -1/-0.")
                .expect("must parse");
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.condition {
            Some(StaticCondition::Unrecognized { text }) => {
                assert_eq!(text, "~ is untapped");
                assert!(
                    !text.contains("get -1/-0"),
                    "effect clause bled into condition text: {text:?}"
                );
            }
            Some(_) => {}
            None => panic!("condition must be set"),
        }
        assert_eq!(
            def.description.as_deref(),
            Some("As long as ~ is untapped, all creatures attacking you get -1/-0.")
        );
    }

    #[test]
    fn inverted_falls_through_when_no_effect_subject_found() {
        // With no recognized effect-subject prefix after any comma, behavior must equal
        // today's generic fallback: a Continuous static with Unrecognized condition text
        // (the old bleed-through behavior is preserved as a strict non-regression baseline).
        let def = parse_static_line(
            "As long as you control a permanent named Guan Yu, Sainted Warrior or a permanent named Zhang Fei, Fierce Warrior.",
        )
        .expect("fallback must still match");
        assert_eq!(def.mode, StaticMode::Continuous);
        match def.condition {
            Some(StaticCondition::Unrecognized { .. }) => {}
            other => panic!("expected Unrecognized condition via fallback, got {other:?}"),
        }
    }

    // --- Hand-zone keyword grant statics (CR 702.94a + CR 400.3) ---

    /// CR 702.94a: "Each instant and sorcery card in your hand has miracle {2}"
    /// (Lorehold, the Historian) must parse as a Continuous static whose
    /// affected filter carries `InZone { zone: Hand }` and whose modification
    /// is `AddKeyword(Miracle({2}))`.
    #[test]
    fn hand_grant_lorehold_miracle() {
        let text = "Each instant and sorcery card in your hand has miracle {2}.";
        let def = parse_static_line(text).expect("Lorehold text must parse");
        assert_eq!(def.mode, StaticMode::Continuous);
        let affected = def.affected.expect("should have affected filter");
        assert!(
            affected.extract_in_zone() == Some(Zone::Hand),
            "affected filter should carry InZone: Hand, got {affected:?}"
        );
        assert!(
            def.modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: crate::types::keywords::Keyword::Miracle(_)
                }
            )),
            "modifications should include AddKeyword(Miracle), got {:?}",
            def.modifications,
        );
    }

    /// CR 400.3: "Sliver cards in your hand have warp {3}" (Sliver Weftwinder)
    /// — single-subtype hand-grant keyword. Confirms the parser covers the
    /// typed-subtype class beyond Lorehold's instant/sorcery pair.
    #[test]
    fn hand_grant_sliver_weftwinder_warp() {
        let text = "Sliver cards in your hand have warp {3}.";
        let defs = parse_static_line_multi(text);
        assert!(
            !defs.is_empty(),
            "parse_static_line_multi returned empty for: {text}"
        );
        let def = defs
            .into_iter()
            .find(|d| {
                d.mode == StaticMode::Continuous
                    && d.affected
                        .as_ref()
                        .map(|a| a.extract_in_zone() == Some(Zone::Hand))
                        .unwrap_or(false)
            })
            .expect("expected a hand-zone Continuous static in output");
        assert!(
            def.modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: crate::types::keywords::Keyword::Warp(_)
                }
            )),
            "modifications should include AddKeyword(Warp), got {:?}",
            def.modifications,
        );
    }

    // ---------------------------------------------------------------------
    // Combat-tax static family — class-level parser coverage.
    // CR 508.1d + CR 508.1h + CR 118.12a: "[subject] can't attack/block unless
    // [controller] pays [cost] [per-creature qualifier]" produces a typed
    // `StaticCondition::UnlessPay` with the correct `UnlessPayScaling` variant.
    // ---------------------------------------------------------------------

    use crate::types::ability::UnlessPayScaling;

    /// Helper: extract the `UnlessPay { cost, scaling }` from a parsed combat-tax static.
    fn extract_unless_pay(def: &StaticDefinition) -> (ManaCost, UnlessPayScaling) {
        match def
            .condition
            .as_ref()
            .expect("combat-tax static must carry a condition")
        {
            StaticCondition::UnlessPay { cost, scaling } => (cost.clone(), scaling.clone()),
            other => panic!("expected UnlessPay, got {other:?}"),
        }
    }

    /// CR 508.1h: Ghostly Prison / Propaganda — fixed per-attacker mana.
    /// Parses to `CantAttack` + opponents'-creature filter + `PerAffectedCreature` scaling.
    #[test]
    fn combat_tax_ghostly_prison_per_affected_creature() {
        let def = parse_static_line(
            "Creatures can't attack you unless their controller pays {2} for each creature they control that's attacking you.",
        )
        .expect("Ghostly Prison should parse");
        assert_eq!(def.mode, StaticMode::CantAttack);
        let (cost, scaling) = extract_unless_pay(&def);
        assert_eq!(cost.mana_value(), 2);
        assert!(matches!(scaling, UnlessPayScaling::PerAffectedCreature));
    }

    /// CR 508.1h + CR 202.3e: Sphere of Safety — dynamic {X} per attacker where X
    /// is a battlefield count. Parses to `PerAffectedAndQuantityRef`.
    #[test]
    fn combat_tax_sphere_of_safety_per_affected_and_ref() {
        let def = parse_static_line(
            "Creatures can't attack you or planeswalkers you control unless their controller pays {X} for each of those creatures, where X is the number of enchantments you control.",
        )
        .expect("Sphere of Safety should parse");
        assert_eq!(def.mode, StaticMode::CantAttack);
        let (_cost, scaling) = extract_unless_pay(&def);
        assert!(matches!(
            scaling,
            UnlessPayScaling::PerAffectedAndQuantityRef { .. }
        ));
    }

    /// CR 118.12a + CR 202.3e: Nils, Discipline Enforcer — counter-gated subject
    /// ("Each creature with one or more counters on it") with per-attacker-resolved
    /// scaling ({X} = counters on THAT creature). Parses to `PerAffectedWithRef`
    /// with `QuantityRef::AnyCountersOnTarget`, using a creature filter with
    /// `FilterProp::HasAnyCounter`.
    #[test]
    fn combat_tax_nils_per_affected_with_ref() {
        let def = parse_static_line(
            "Each creature with one or more counters on it can't attack you or planeswalkers you control unless its controller pays {X}, where X is the number of counters on that creature.",
        )
        .expect("Nils, Discipline Enforcer should parse");
        assert_eq!(def.mode, StaticMode::CantAttack);

        // Affected filter gates on counter presence.
        let affected = def.affected.as_ref().expect("affected filter must be set");
        let TargetFilter::Typed(tf) = affected else {
            panic!("expected TypedFilter, got {affected:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf.properties.contains(&FilterProp::HasAnyCounter));

        let (_cost, scaling) = extract_unless_pay(&def);
        match scaling {
            UnlessPayScaling::PerAffectedWithRef { quantity } => {
                assert!(matches!(
                    quantity,
                    QuantityRef::CountersOn {
                        scope: ObjectScope::Target,
                        counter_type: None
                    }
                ));
            }
            other => panic!("expected PerAffectedWithRef, got {other:?}"),
        }
    }

    /// CR 508.1d: Brainwash-class aura form — "Enchanted creature can't attack
    /// unless its controller pays {3}." Verifies the aura subject branch emits
    /// `FilterProp::EnchantedBy` and flat scaling.
    #[test]
    fn combat_tax_brainwash_flat_aura() {
        let def =
            parse_static_line("Enchanted creature can't attack unless its controller pays {3}.")
                .expect("Brainwash-style aura should parse");
        assert_eq!(def.mode, StaticMode::CantAttack);
        let (cost, scaling) = extract_unless_pay(&def);
        assert_eq!(cost.mana_value(), 3);
        assert!(matches!(scaling, UnlessPayScaling::Flat));
        let affected = def.affected.as_ref().expect("affected filter");
        let TargetFilter::Typed(tf) = affected else {
            panic!("expected TypedFilter");
        };
        assert!(tf.properties.contains(&FilterProp::EnchantedBy));
    }

    /// CR 113.6 + CR 113.6b: Anger (Onslaught / Incarnation cycle). The static
    /// "As long as this card is in your graveyard and you control a Mountain,
    /// creatures you control have haste" must parse with
    /// `active_zones = [Graveyard]` so the layers pipeline collects it from
    /// the graveyard. Also verifies the compound condition combines
    /// `SourceInZone(Graveyard)` AND `IsPresent(Mountain you control)`.
    #[test]
    fn anger_incarnation_static_declares_graveyard_active_zone() {
        let def = parse_static_line(
            "As long as this card is in your graveyard and you control a Mountain, \
             creatures you control have haste.",
        )
        .expect("Anger static should parse");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.active_zones,
            vec![crate::types::zones::Zone::Graveyard],
            "Anger must declare Graveyard in active_zones (CR 113.6b opt-in), got {:?}",
            def.active_zones,
        );
        // Compound condition: source-in-graveyard AND controller-has-Mountain.
        let Some(StaticCondition::And { conditions }) = def.condition.as_ref() else {
            panic!("expected compound And condition, got {:?}", def.condition);
        };
        assert_eq!(conditions.len(), 2);
        assert!(conditions.iter().any(|c| matches!(
            c,
            StaticCondition::SourceInZone { zone } if *zone == crate::types::zones::Zone::Graveyard
        )));
        assert!(conditions
            .iter()
            .any(|c| matches!(c, StaticCondition::IsPresent { .. })));
        // Grants Haste to creatures you control.
        assert!(def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }
        )));
    }

    /// Statics with no zone-location condition keep `active_zones` empty so
    /// they remain battlefield-only (CR 113.6 default).
    #[test]
    fn ordinary_static_keeps_empty_active_zones() {
        let def = parse_static_line("Creatures you control get +1/+1.")
            .expect("anthem static should parse");
        assert!(
            def.active_zones.is_empty(),
            "plain anthem must remain battlefield-default, got {:?}",
            def.active_zones,
        );
    }

    /// CR 613.4b + CR 107.3m: "have base power and toughness X/X" produces
    /// dynamic set-P/T at layer 7b (not static layer 7a CDA, and not pump 7c).
    /// Biomass Mutation shape. With no "where X is" clause, X binds to
    /// `CostXPaid` (the spell's {X} cost value).
    #[test]
    fn base_pt_dynamic_x_x_emits_set_power_dynamic() {
        let mods =
            parse_continuous_modifications("have base power and toughness X/X until end of turn");
        let has_p = mods.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::SetPowerDynamic {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::CostXPaid
                    }
                }
            )
        });
        let has_t = mods.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::SetToughnessDynamic {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::CostXPaid
                    }
                }
            )
        });
        assert!(has_p, "missing SetPowerDynamic(CostXPaid) in {mods:?}");
        assert!(has_t, "missing SetToughnessDynamic(CostXPaid) in {mods:?}");
        assert_eq!(
            mods.iter()
                .filter(|m| matches!(
                    m,
                    ContinuousModification::SetPower { .. }
                        | ContinuousModification::SetToughness { .. }
                ))
                .count(),
            0,
            "literal SetPower/SetToughness must not be emitted for X/X"
        );
    }

    // CR 700.9: "Modified creatures you control have <keyword>" class.
    // Previously misparsed as Subtype("Modified") (see commit body).
    #[test]
    fn static_modified_creatures_you_control_have_menace() {
        let def = parse_static_line("Modified creatures you control have menace.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match def.affected {
            Some(TargetFilter::Typed(ref tf)) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::Modified));
                assert!(
                    !tf.type_filters.iter().any(|t| matches!(
                        t,
                        TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("modified")
                    )),
                    "Modified must not be emitted as a subtype"
                );
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            _ => panic!("expected TargetFilter::Typed"),
        }
    }

    // CR 700.9: Ondu Knotmaster-style "other modified creature you control".
    #[test]
    fn parse_modified_creature_subject_other_variant() {
        let filter = parse_modified_creature_subject_filter("other modified creature you control")
            .expect("other modified creature you control must parse");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::Modified));
                assert!(tf.properties.contains(&FilterProp::Another));
            }
            _ => panic!("expected TargetFilter::Typed"),
        }
    }

    // CR 700.9: Bare "modified creature" with no controller scope.
    #[test]
    fn parse_modified_creature_subject_unscoped() {
        let filter = parse_modified_creature_subject_filter("modified creature")
            .expect("modified creature must parse");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, None);
                assert!(tf.properties.contains(&FilterProp::Modified));
                assert!(!tf.properties.contains(&FilterProp::Another));
            }
            _ => panic!("expected TargetFilter::Typed"),
        }
    }

    // CR 903.3d: "Commanders you control have <keyword>" — Codsworth, Falthis,
    // Vexilus Praetor class. Must produce IsCommander, NOT a bogus
    // Subtype("Commander") (Commander is not an MTG subtype per CR 903.3).
    #[test]
    fn parse_commanders_you_control_have_keyword() {
        let def = parse_static_line("Commanders you control have ward {2}.")
            .expect("should parse Commanders-you-control");
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(
                    tf.properties.contains(&FilterProp::IsCommander),
                    "must carry IsCommander, got {:?}",
                    tf.properties
                );
                // Must NOT synthesize a Commander subtype.
                assert!(
                    !tf.type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Commander")),
                    "must not emit Subtype(\"Commander\") (CR 903.3 — not a subtype)"
                );
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    // CR 903.3d + CR 700.4: "Other commanders you control" — must include Another.
    #[test]
    fn parse_other_commanders_you_control_have_keyword() {
        let def = parse_static_line("Other commanders you control have menace.")
            .expect("should parse other-commanders-you-control");
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.properties.contains(&FilterProp::IsCommander));
                assert!(tf.properties.contains(&FilterProp::Another));
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    // CR 903.3d: "Commander creatures you control" — Guardian Augmenter class.
    // The "Commander" adjective on a creature subject is the commander
    // designation, not a subtype.
    #[test]
    fn parse_commander_creatures_you_control() {
        let def = parse_static_line("Commander creatures you control get +2/+2.")
            .expect("should parse Commander-creatures-you-control");
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert!(tf.properties.contains(&FilterProp::IsCommander));
                assert!(
                    !tf.type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Commander")),
                    "must not emit Subtype(\"Commander\")"
                );
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    // CR 903.3d: parse_commander_subject_filter as a raw subject helper.
    // Unblocks subject-continuous-static dispatch (the secondary path).
    #[test]
    fn parse_commander_subject_filter_basic_variants() {
        let f = parse_commander_subject_filter("commanders you control")
            .expect("commanders you control");
        match f {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::IsCommander));
                assert!(tf.type_filters.contains(&TypeFilter::Permanent));
            }
            _ => panic!("expected Typed"),
        }

        let f = parse_commander_subject_filter("other commander you control")
            .expect("other commander you control");
        match f {
            TargetFilter::Typed(tf) => {
                assert!(tf.properties.contains(&FilterProp::IsCommander));
                assert!(tf.properties.contains(&FilterProp::Another));
            }
            _ => panic!("expected Typed"),
        }

        // Bare "commander" (no controller) — used by `parse_subject_continuous_static`
        // when an enclosing clause supplies the controller.
        let f = parse_commander_subject_filter("commanders").expect("bare commanders");
        match f {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, None);
                assert!(tf.properties.contains(&FilterProp::IsCommander));
            }
            _ => panic!("expected Typed"),
        }

        // Negative: must not match subtype-like words.
        assert!(parse_commander_subject_filter("zombies you control").is_none());
        assert!(parse_commander_subject_filter("commander spirits").is_none());
    }

    /// CR 401.5 + CR 118.9: Realmwalker's "You may cast creature spells of the
    /// chosen type from the top of your library." should lower to a
    /// `TopOfLibraryCastPermission { play_mode: Cast }` static with the
    /// chosen-creature-type filter, NOT to an imperative `Effect::CastFromZone`
    /// (which would exile the card via the impulse-draw resolver).
    #[test]
    fn top_of_library_cast_permission_realmwalker() {
        let text = "You may cast creature spells of the chosen type from the top of your library.";
        let lower = text.to_lowercase();
        let def = try_parse_top_of_library_cast_permission(text, &lower)
            .expect("Realmwalker static must parse");
        match def.mode {
            StaticMode::TopOfLibraryCastPermission {
                play_mode,
                ref alt_cost,
            } => {
                assert_eq!(play_mode, CardPlayMode::Cast);
                assert!(alt_cost.is_none());
            }
            other => panic!("expected TopOfLibraryCastPermission, got {other:?}"),
        }
        // The chosen-creature-type filter must be carried on `affected`.
        let affected = def.affected.expect("affected filter set");
        match affected {
            TargetFilter::Typed(tf) => {
                assert!(tf
                    .type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Creature)));
                assert!(tf
                    .properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::IsChosenCreatureType)));
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    /// CR 401.5: Future Sight / Magus of the Future — compound "you may play
    /// lands and cast spells from the top of your library" collapses to a
    /// single `Play` permission with `affected: Any`.
    #[test]
    fn top_of_library_cast_permission_future_sight_compound() {
        let text = "You may play lands and cast spells from the top of your library.";
        let lower = text.to_lowercase();
        let def = try_parse_top_of_library_cast_permission(text, &lower)
            .expect("Future Sight static must parse");
        match def.mode {
            StaticMode::TopOfLibraryCastPermission {
                play_mode,
                ref alt_cost,
            } => {
                assert_eq!(play_mode, CardPlayMode::Play);
                assert!(alt_cost.is_none());
            }
            other => panic!("expected TopOfLibraryCastPermission, got {other:?}"),
        }
        assert!(matches!(def.affected, Some(TargetFilter::Any)));
    }

    /// CR 118.9 + CR 119.4: Bolas's Citadel — compound permission line carrying
    /// a same-line alt-cost rider must lower with `alt_cost: Some(PayLife {
    /// SelfManaValue })`. Verifies the rider scanner correctly slices into the
    /// "If you cast a spell this way, ..." sentence inside the same line.
    #[test]
    fn top_of_library_cast_permission_bolas_alt_cost() {
        let text = "You may play lands and cast spells from the top of your library. \
                    If you cast a spell this way, pay life equal to its mana value rather \
                    than pay its mana cost.";
        let lower = text.to_lowercase();
        let def = try_parse_top_of_library_cast_permission(text, &lower)
            .expect("Bolas's Citadel static must parse");
        match def.mode {
            StaticMode::TopOfLibraryCastPermission {
                play_mode,
                alt_cost: Some(crate::types::ability::AbilityCost::PayLife { amount }),
            } => {
                assert_eq!(play_mode, CardPlayMode::Play);
                assert_eq!(
                    amount,
                    crate::types::ability::QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::SelfManaValue
                    }
                );
            }
            other => panic!("expected PayLife alt_cost, got {other:?}"),
        }
    }

    /// Negative: lines without "from the top of your library" must NOT match —
    /// the existing impulse-draw / graveyard / hand-permission paths must
    /// still own those lines.
    #[test]
    fn top_of_library_cast_permission_rejects_other_anchors() {
        // Graveyard form — owned by `try_parse_graveyard_cast_permission`.
        assert!(try_parse_top_of_library_cast_permission(
            "You may cast a creature spell from your graveyard.",
            "you may cast a creature spell from your graveyard.",
        )
        .is_none());
        // Hand-free form — owned by `try_parse_cast_free_permission`.
        assert!(try_parse_top_of_library_cast_permission(
            "You may cast spells from your hand without paying their mana costs.",
            "you may cast spells from your hand without paying their mana costs.",
        )
        .is_none());
        // Imperative form (Discover-class) — owned by `try_parse_cast_effect`.
        assert!(try_parse_top_of_library_cast_permission(
            "Cast that card without paying its mana cost.",
            "cast that card without paying its mana cost.",
        )
        .is_none());
    }
}
