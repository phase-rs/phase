use crate::parser::oracle_nom::error::{OracleError, OracleResult};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until, take_while};
use nom::character::complete::multispace0;
use nom::combinator::{all_consuming, opt, recognize, value};
use nom::multi::many1;
use nom::sequence::{preceded, terminated};
use nom::Parser;
use serde::{Deserialize, Serialize};

use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AbilityTag,
    ActivationRestriction, AdditionalCost, CastTimingPermission, CastingRestriction, ChoiceType,
    ChosenSubtypeKind, ContinuousModification, DelayedTriggerCondition, Effect, ManaProduction,
    ModalChoice, ParsedCondition, QuantityExpr, ReplacementDefinition, SolveCondition,
    SpellCastingOption, StaticCondition, StaticDefinition, TargetFilter, TriggerCondition,
    TriggerDefinition, TypedFilter,
};
use crate::types::format::DeckCopyLimit;
use crate::types::keywords::{ActivationCadence, FlashbackCost, Keyword, KeywordKind};
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::oracle_nom::bridge::{nom_on_lower, split_once_on_lower};
use super::oracle_nom::condition::parse_inner_condition;
use super::oracle_nom::primitives::parse_number as nom_parse_number;
use super::oracle_nom::primitives::scan_contains;

use super::oracle_attraction::parse_attraction_visit_triggers;
use super::oracle_casting::{
    parse_additional_cost_line, parse_casting_restriction_line, parse_spell_casting_option_line,
};
use super::oracle_class::parse_class_oracle_text;
use super::oracle_classifier::{
    has_roll_die_pattern, has_trigger_prefix, is_ability_activate_cost_static,
    is_cant_win_lose_compound, is_compound_turn_limit, is_defiler_cost_pattern,
    is_enters_tapped_cant_untap_compound, is_enters_with_counter_trigger,
    is_flashback_equal_mana_cost, is_granted_static_line, is_instead_replacement_line,
    is_opening_hand_begin_game, is_replacement_pattern, is_spells_alternative_cost_pattern,
    is_static_pattern, is_vehicle_tier_line, lower_starts_with, should_defer_spell_to_effect,
};
use super::oracle_condition::parse_restriction_condition;
use super::oracle_cost::{parse_oracle_cost, try_parse_cost_reduction};
use super::oracle_dispatch::{dispatch_line_nom, make_unimplemented_with_effect};
use super::oracle_effect::{
    lower_effect_chain_ir, parse_effect_chain, parse_effect_chain_with_context,
    try_parse_temporal_delayed_trigger_ability,
};
use super::oracle_ir::context::ParseContext;
use super::oracle_ir::diagnostic::OracleDiagnostic;
use super::oracle_ir::doc::{OracleDocIr, OracleItemIr};
pub use super::oracle_keyword::keyword_display_name;
use super::oracle_keyword::{
    extract_keyword_line, is_keyword_cost_line, parse_keyword_from_oracle,
    parse_kicker_additional_cost_line,
};
use super::oracle_level::parse_level_blocks;
use super::oracle_modal::{
    extract_ability_word_reminder_body, lower_oracle_block, parse_oracle_block, strip_ability_word,
    strip_ability_word_with_name,
};
use super::oracle_replacement::{
    find_copy_verb_present, lower_replacement_ir, parse_replacement_line,
};
use super::oracle_saga::{is_saga_chapter, parse_saga_chapters};
use super::oracle_spacecraft::parse_spacecraft_threshold_lines;
use super::oracle_special::{
    attach_die_result_branches_to_chain, normalize_self_refs_for_static,
    parse_cumulative_upkeep_keyword, parse_defiler_cost_reduction, parse_escape_keyword,
    parse_harmonize_keyword, parse_solve_condition, try_parse_die_roll_table,
};
use super::oracle_static::{
    lower_static_ir, parse_chosen_creature_type_static_prefix,
    parse_every_creature_type_static_prefix, parse_spells_alternative_cost,
    parse_static_line_multi, try_parse_graveyard_keyword_grant_clause, GraveyardGrantedKeywordKind,
};
use super::oracle_trigger::{lower_trigger_ir, parse_trigger_lines_at_index};
use super::oracle_util::{
    normalize_card_name_refs, parse_mana_symbols, parse_number, split_same_is_true_static_tail,
    strip_reminder_text, TextPair,
};

/// Collected parsed abilities from Oracle text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedAbilities {
    pub abilities: Vec<AbilityDefinition>,
    pub triggers: Vec<TriggerDefinition>,
    pub statics: Vec<StaticDefinition>,
    pub replacements: Vec<ReplacementDefinition>,
    /// Keywords extracted from Oracle text keyword-only lines (e.g. "Protection from multicolored").
    /// Merged with MTGJSON keywords in the loader to form the complete keyword set.
    pub extracted_keywords: Vec<Keyword>,
    /// Modal spell metadata, set when Oracle text begins with "Choose one —" etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal: Option<ModalChoice>,
    /// Additional casting cost parsed from "As an additional cost..." text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_cost: Option<AdditionalCost>,
    /// Spell-casting restrictions parsed from Oracle text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub casting_restrictions: Vec<CastingRestriction>,
    /// Spell-casting options parsed from Oracle text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub casting_options: Vec<SpellCastingOption>,
    /// CR 719.1: Solve condition for Case enchantments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solve_condition: Option<SolveCondition>,
    /// CR 207.2c + CR 601.2f: Strive per-target surcharge cost.
    /// "This spell costs {X} more to cast for each target beyond the first."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strive_cost: Option<ManaCost>,
    /// Typed diagnostic warnings from silent fallback patterns during parsing (D-12).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parse_warnings: Vec<OracleDiagnostic>,
}

fn merge_kicker_additional_cost(slot: &mut Option<AdditionalCost>, incoming: AdditionalCost) {
    match incoming {
        AdditionalCost::Kicker {
            costs: incoming_costs,
            repeatable: false,
        } => {
            if let Some(AdditionalCost::Kicker {
                costs,
                repeatable: false,
            }) = slot.as_mut()
            {
                costs.extend(incoming_costs);
            } else {
                *slot = Some(AdditionalCost::Kicker {
                    costs: incoming_costs,
                    repeatable: false,
                });
            }
        }
        incoming => *slot = Some(incoming),
    }
}

fn definition_grants_flashback(def: &AbilityDefinition) -> bool {
    let grants_here = match &*def.effect {
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities.iter().any(|static_def| {
            static_def.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    crate::types::ability::ContinuousModification::AddKeyword { keyword }
                        if keyword.kind() == KeywordKind::Flashback
                )
            })
        }),
        _ => false,
    };

    grants_here
        || def
            .sub_ability
            .as_deref()
            .is_some_and(definition_grants_flashback)
}

fn parse_commander_permission_sentence(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    let (input, subject) = take_until(" can be your commander").parse(input)?;
    if subject.trim().is_empty() {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let (input, _) = tag(" can be your commander").parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    Ok((input, ()))
}

/// Deck-construction permission text has no runtime ability to resolve.
pub(crate) fn is_commander_permission_sentence(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    let parsed = all_consuming(parse_commander_permission_sentence)
        .parse(lower.as_str())
        .is_ok();
    parsed
}

fn parse_replacement_sentence_sequence(
    line: &str,
    card_name: &str,
) -> Option<Vec<ReplacementDefinition>> {
    // CR 614.1c: Effects that read "[This permanent] enters with ...",
    // "As [this permanent] enters ...", or "[This permanent] enters as ..."
    // are replacement effects.
    // CR 614.12: Some replacement effects modify how a permanent enters the battlefield.
    let (_, sentences) = parse_replacement_sentences(line).ok()?;
    if sentences.len() < 2 {
        return None;
    }

    let mut replacements = Vec::with_capacity(sentences.len());
    for sentence in sentences {
        if !is_replacement_pattern(&sentence.to_lowercase()) {
            return None;
        }
        replacements.push(parse_replacement_line(sentence, card_name)?);
    }
    Some(replacements)
}

fn parse_replacement_sentences(input: &str) -> OracleResult<'_, Vec<&str>> {
    all_consuming(many1(parse_replacement_sentence)).parse(input)
}

fn parse_replacement_sentence(input: &str) -> OracleResult<'_, &str> {
    preceded(
        multispace0,
        recognize(terminated(take_until("."), tag("."))),
    )
    .parse(input)
}

// CR 100.2a / CR 903.5b: Deck-construction overrides like "A deck can have
// any number of cards named X." (Tempest Hawk, Rat Colony, Relentless Rats,
// Persistent Petitioners, Shadowborn Apostle, etc.) and bounded variants like
// "A deck can have up to seven cards named Seven Dwarves." (also Nazgûl → 9)
// are deck-construction metadata that override CR 100.2a's four-of limit and
// the CR 903.5b Commander singleton rule. They have no runtime effect to
// resolve. The same combinator both extracts the typed `DeckCopyLimit` (for
// deck validation) and recognizes the line so it does not fall through to
// `Effect::Unimplemented { name: "static_structure", .. }`.

/// Consume the trailing card-name subject of a deck-construction sentence.
///
/// Rejects an empty subject so "... named ." cannot match. The predicate
/// accepts the raw card name, the engine's normalized self-reference "~", and
/// Unicode letters (Rust `char::is_alphanumeric` accepts "û" in "Nazgûl").
fn parse_deck_limit_subject(input: &str) -> OracleResult<'_, &str> {
    let (rest, subject) = take_while(|c: char| {
        c.is_alphanumeric() || c == ' ' || c == '\'' || c == ',' || c == '-' || c == '~'
    })
    .parse(input)?;
    if subject.trim().is_empty() {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((rest, subject))
}

/// Consume " card named " / " cards named " (plural tried first; `tag` is
/// all-or-nothing so the singular cannot shadow the plural).
fn parse_card_s_named(input: &str) -> OracleResult<'_, ()> {
    value((), alt((tag(" cards named "), tag(" card named ")))).parse(input)
}

/// CR 100.2a / CR 903.5b: Parse a single deck-construction copy-limit sentence
/// into a typed [`DeckCopyLimit`]. Accepts the optional "DCI ruling — " /
/// "DCI ruling - " prefix (Once More with Feeling). The caller wraps this in
/// `all_consuming` so the subject must be fully consumed (no trailing remainder
/// regresses the card to Unimplemented).
fn parse_deck_copy_limit(input: &str) -> OracleResult<'_, DeckCopyLimit> {
    let (input, _) = opt(alt((tag("dci ruling \u{2014} "), tag("dci ruling - ")))).parse(input)?;
    let (input, limit) = alt((
        // Variant 1: "a deck can have any number of cards named X" — Unlimited.
        (
            tag("a deck can have any number of cards named "),
            parse_deck_limit_subject,
        )
            .map(|_| DeckCopyLimit::Unlimited),
        // Variants 2/3/4: "a deck can have {up to|only} N card(s) named X" — UpTo(N).
        preceded(
            tag("a deck can have "),
            (
                alt((value((), tag("up to ")), value((), tag("only ")))),
                nom_parse_number,
                parse_card_s_named,
                parse_deck_limit_subject,
            ),
        )
        .map(|(_, n, _, _)| DeckCopyLimit::UpTo(n)),
        // Variant 5: Megalegendary reminder body — singleton, no subject.
        value(
            DeckCopyLimit::UpTo(1),
            tag("your deck can have only one copy of this card"),
        ),
    ))
    .parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    Ok((input, limit))
}

/// CR 100.2a / CR 903.5b: Run the copy-limit combinator over a single
/// lowercased fragment, tolerating leading prose by trying each sentence within
/// it. The deck-limit sentence is sometimes its own line ("...\nA deck can
/// have...") and sometimes the tail sentence of a multi-sentence line
/// ("...you control. A deck can have..."), so both must be reachable.
fn copy_limit_from_fragment(fragment: &str) -> Option<DeckCopyLimit> {
    let lower = fragment.trim().to_ascii_lowercase();
    // Each ". "-separated sentence is a candidate; the combinator's trailing
    // `opt(".")` absorbs a present period and tolerates its absence.
    for sentence in lower.split(". ") {
        if let Ok((_, limit)) =
            all_consuming(parse_deck_copy_limit).parse(sentence.trim_end_matches('.').trim())
        {
            return Some(limit);
        }
    }
    None
}

/// CR 100.2a / CR 903.5b: Extract the deck-construction copy limit from a card's
/// full Oracle text, scanning each line AND each parenthesized reminder-text
/// body (Vazal, the Compleat's Megalegendary limit lives only in the reminder
/// body). The first match wins.
pub(crate) fn compute_deck_copy_limit_from_text(text: &str) -> Option<DeckCopyLimit> {
    for line in text.lines() {
        if let Some(limit) = copy_limit_from_fragment(line) {
            return Some(limit);
        }
        // Reminder-text bodies, e.g. "Megalegendary (Your deck can have ...)".
        let mut rest = line;
        while let Some(open) = rest.find('(') {
            let after = &rest[open + 1..];
            let Some(close) = after.find(')') else { break };
            if let Some(limit) = copy_limit_from_fragment(&after[..close]) {
                return Some(limit);
            }
            rest = &after[close + 1..];
        }
    }
    None
}

/// Recognizer for deck-construction copy-limit sentences — deck-construction
/// text consumed silently by the parser so it does not fall through to
/// `Effect::Unimplemented { name: "static_structure", .. }`. Also matches the
/// bare "Megalegendary" keyword line, whose copy limit lives in the reminder
/// body of the same logical line (handled by `compute_deck_copy_limit_from_text`).
pub(crate) fn is_deck_construction_copy_limit_sentence(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    all_consuming(parse_deck_copy_limit)
        .parse(lower.as_str())
        .is_ok()
        || lower.trim() == "megalegendary"
}

/// Whether Oracle text explicitly permits this card to be a commander.
pub fn oracle_text_allows_commander(oracle_text: &str, card_name: &str) -> bool {
    let normalized = normalize_card_name_refs(oracle_text, card_name);
    normalized.lines().any(is_commander_permission_sentence)
        || scan_contains(&oracle_text.to_ascii_lowercase(), "can be your commander")
}

/// CR 103.5b: "Any time you could mulligan and ~ is in your hand, you may ..."
/// (Serum Powder, No-Regrets Egret). Classified as `AbilityKind::Mulligan` —
/// the runtime path lives in `mulligan.rs`, never the stack resolver. The
/// inner effect is parsed via the normal effect-chain path so coverage / debug
/// tooling can read the shape of the action; the resolution guard in
/// `effects/mod.rs` skips it during stack resolution regardless of what the
/// inner effect happens to be.
fn try_parse_mulligan_time_ability(line: &str, lower: &str) -> Option<AbilityDefinition> {
    let (_, rest) = nom_on_lower(line, lower, |input| {
        let (input, _) = tag("any time you could mulligan and ").parse(input)?;
        let (input, _) = alt((
            tag("~ is in your hand, you may "),
            tag("this card is in your hand, you may "),
        ))
        .parse(input)?;
        Ok((input, ()))
    })?;

    let mut def = parse_effect_chain(rest, AbilityKind::Mulligan).description(line.to_string());
    def.optional = true;
    Some(def)
}

fn try_parse_opening_hand_reveal_delayed_trigger(
    line: &str,
    lower: &str,
) -> Option<AbilityDefinition> {
    let (condition, rest) = nom_on_lower(line, lower, |input| {
        let (input, _) =
            tag("you may reveal this card from your opening hand. if you do, ").parse(input)?;
        let (input, condition) = alt((
            value(
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::Upkeep,
                    player: PlayerId(0),
                },
                tag("at the beginning of your first upkeep, "),
            ),
            value(
                DelayedTriggerCondition::AtNextPhase {
                    phase: Phase::Upkeep,
                },
                tag("at the beginning of the first upkeep, "),
            ),
        ))
        .parse(input)?;
        Ok((input, condition))
    })?;

    let effect = parse_effect_chain(rest, AbilityKind::Spell);
    if has_unimplemented(&effect) {
        return None;
    }

    let delayed = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CreateDelayedTrigger {
            condition,
            effect: Box::new(effect),
            uses_tracked_set: false,
        },
    );

    let mut def = AbilityDefinition::new(
        AbilityKind::BeginGame,
        Effect::Reveal {
            target: TargetFilter::SelfRef,
        },
    )
    .sub_ability(delayed)
    .description(line.to_string());
    def.optional = true;
    Some(def)
}

/// CR 103.6 / CR 103.6a: Parse an "opening hand, begin the game with ~ on the
/// battlefield" line into a `BeginGame` `AbilityDefinition`.
///
/// This is the sole detector for the begin-game class — the parser IS the
/// detector. It is built entirely from nom combinators; the preamble is matched
/// with explicit `alt`/`tag` over its known forms (never `take_until`, which
/// would skip arbitrary text and weaken the detector).
///
/// Two pieces of text the previous hardcoded branch dropped are now captured:
///   1. CR 122.1: an optional "with [N] [type] counter(s) on it" clause →
///      populates `Effect::ChangeZone::enter_with_counters`.
///   2. An optional "If you do, [effect]" follow-up sentence → becomes a
///      `sub_ability` gated by `AbilityCondition::effect_performed()`, so the dependent
///      effect only fires when the player accepts the begin-game opt-in.
///
/// Mirrors `try_parse_opening_hand_reveal_delayed_trigger` end-to-end shape and
/// is near-isomorphic to Forsaken City's `optional: true` + `IfYouDo`
/// sub-ability layout (Forsaken City proves `parse_effect_chain` handles the
/// "exile a card from your hand" tail).
fn parse_begin_game_clause(line: &str, lower: &str) -> Option<AbilityDefinition> {
    // Closure consumes the structural prefix on the lowercased view. It returns
    // the parsed entry counters; the original-case remainder (mapped back by
    // `nom_on_lower`) is the "If you do, [effect]" tail — empty when absent.
    let (enter_with_counters, effect_text) = nom_on_lower(line, lower, |input| {
        // Preamble — explicit known forms, each ending in "you may ".
        // CR 103.6a (begin the game with that card on the battlefield);
        // Gemstone Caverns additionally gates on not being the starting player.
        let (input, _) = alt((
            tag(
                "if this card is in your opening hand and you're not the starting player, you may ",
            ),
            tag("if this card is in your opening hand, you may "),
            tag("if ~ is in your opening hand, you may "),
        ))
        .parse(input)?;
        let (input, _) = tag("begin the game with ").parse(input)?;
        // Self-reference: `~` after normalization, or an object pronoun.
        let (input, _) =
            alt((tag("~"), tag("it"), tag("him"), tag("her"), tag("them"))).parse(input)?;
        let (input, _) = tag(" on the battlefield").parse(input)?;

        // Optional "with [N] [type] counter(s) on it" clause (CR 122.1).
        let (input, counters) = opt(parse_begin_game_counter_clause).parse(input)?;

        // First sentence terminator.
        let (input, _) = tag(".").parse(input)?;

        // Optional "If you do, " follow-up prefix. When present, the remainder
        // is the dependent effect text; when absent, the remainder is empty.
        let (input, _) = opt(alt((tag(" if you do, "), tag(" if you do ")))).parse(input)?;

        Ok((input, counters.unwrap_or_default()))
    })?;

    let mut def = AbilityDefinition::new(
        AbilityKind::BeginGame,
        // CR 103.6a: the card is put onto the battlefield from the opening hand.
        Effect::ChangeZone {
            destination: Zone::Battlefield,
            target: TargetFilter::SelfRef,
            origin: Some(Zone::Hand),
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            // CR 122.1: entry counters parsed from "with [N] [type] counter(s) on it".
            enter_with_counters,
            face_down_profile: None,
        },
    )
    .description(line.to_string());
    def.optional = true;

    // Optional "If you do, [effect]" dependent sub-ability. A non-empty
    // remainder means the line carried a follow-up sentence.
    let effect_text = effect_text.trim().trim_end_matches('.').trim();
    if !effect_text.is_empty() {
        // CR 701.13a: "exile a card from your hand" resolves to a player-choice
        // exile via `parse_effect_chain` (proven by Forsaken City's identical
        // tail). The `IfYouDo` condition gates it so it only fires when the
        // player accepted the begin-game opt-in.
        let sub = parse_effect_chain(effect_text, AbilityKind::Spell);
        if has_unimplemented(&sub) {
            return None;
        }
        def = def.sub_ability(sub.condition(AbilityCondition::effect_performed()));
    }

    Some(def)
}

/// Parse the "with [N] [type] counter(s) on it" sub-clause of a begin-game line.
///
/// CR 122.1: counters placed on the permanent as it enters. The count defaults
/// to 1 ("a"/"an") and the type word is canonicalized through
/// `types::counter::parse_counter_type` (single authority).
fn parse_begin_game_counter_clause(
    input: &str,
) -> super::oracle_nom::error::OracleResult<
    '_,
    Vec<(crate::types::counter::CounterType, QuantityExpr)>,
> {
    use nom::bytes::complete::take_while1;
    use nom::character::complete::{char as nom_char, digit1};

    let (input, _) = tag(" with ").parse(input)?;
    // Count: a number, or the article "a"/"an" (→ 1).
    let (input, count) = alt((
        nom::combinator::map_res(digit1, |d: &str| d.parse::<u32>()),
        value(1u32, alt((tag("an "), tag("a ")))),
    ))
    .parse(input)?;
    let (input, _) = opt(nom_char(' ')).parse(input)?;
    // Counter type word (e.g. "luck"). Canonicalized by the single authority.
    let (input, type_word) =
        take_while1(|c: char| c.is_ascii_alphabetic() || c == '-').parse(input)?;
    let (input, _) = alt((tag(" counters"), tag(" counter"))).parse(input)?;
    let (input, _) = tag(" on it").parse(input)?;

    let counter_type = crate::types::counter::parse_counter_type(type_word);
    Ok((
        input,
        vec![(
            counter_type,
            QuantityExpr::Fixed {
                value: count as i32,
            },
        )],
    ))
}

fn parsed_result_recently_granted_flashback(result: &ParsedAbilities) -> bool {
    result
        .abilities
        .last()
        .is_some_and(definition_grants_flashback)
        || result.triggers.last().is_some_and(|trigger| {
            trigger
                .execute
                .as_deref()
                .is_some_and(definition_grants_flashback)
        })
        || result.statics.last().is_some_and(|static_def| {
            static_def.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    crate::types::ability::ContinuousModification::AddKeyword { keyword }
                        if keyword.kind() == KeywordKind::Flashback
                )
            })
        })
}

fn parse_graveyard_keyword_continuation(
    text: &str,
    kind: GraveyardGrantedKeywordKind,
) -> Option<Keyword> {
    fn continuation_fully_consumed(rest: &str) -> bool {
        rest.trim().trim_end_matches('.').trim().is_empty()
    }

    let lower = text.to_lowercase();

    match kind {
        GraveyardGrantedKeywordKind::Flashback => {
            let (_, rest) = nom_on_lower(text, &lower, |i| {
                value((), tag("the flashback cost is equal to ")).parse(i)
            })?;
            let rest_lower = rest.to_lowercase();
            let (_, rest) = nom_on_lower(rest, &rest_lower, |i| {
                value(
                    (),
                    alt((
                        tag("that card's mana cost"),
                        tag("the card's mana cost"),
                        tag("its mana cost"),
                    )),
                )
                .parse(i)
            })?;
            if !continuation_fully_consumed(rest) {
                return None;
            }
            Some(Keyword::Flashback(FlashbackCost::Mana(
                ManaCost::SelfManaCost,
            )))
        }
        GraveyardGrantedKeywordKind::Escape => {
            let (_, rest) = nom_on_lower(text, &lower, |i| {
                value((), tag("the escape cost is equal to ")).parse(i)
            })?;
            let rest_lower = rest.to_lowercase();
            let (_, rest) = nom_on_lower(rest, &rest_lower, |i| {
                value(
                    (),
                    alt((
                        tag("that card's mana cost plus exile "),
                        tag("the card's mana cost plus exile "),
                        tag("its mana cost plus exile "),
                    )),
                )
                .parse(i)
            })?;
            let (exile_count, rest) = parse_number(rest)?;
            let rest_lower = rest.to_lowercase();
            let (_, rest) = nom_on_lower(rest, &rest_lower, |i| {
                value((), tag("other cards from your graveyard")).parse(i)
            })?;
            if !continuation_fully_consumed(rest) {
                return None;
            }
            Some(Keyword::Escape {
                cost: ManaCost::SelfManaCost,
                exile_count,
            })
        }
    }
}

fn try_parse_graveyard_keyword_static_with_continuation(line: &str) -> Option<StaticDefinition> {
    let lower = line.to_lowercase();
    let (prefix, continuation) = split_once_on_lower(line, &lower, ". ")?;
    let (affected, kind) = try_parse_graveyard_keyword_grant_clause(prefix)?;
    let keyword = parse_graveyard_keyword_continuation(continuation, kind)?;
    kind.matches_keyword(&keyword).then_some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::AddKeyword { keyword }])
            .description(line.to_string()),
    )
}

/// Returns every `StaticDefinition` produced by `line`, with the
/// graveyard-keyword-continuation front door checked first (CR 702.99 etc.)
/// and then delegating to `parse_static_line_multi` so compound forms
/// (e.g., cross-mode conjunctions) emit all their constituent statics
/// rather than silently dropping the extras.
fn parse_static_line_with_graveyard_keyword_continuation(line: &str) -> Vec<StaticDefinition> {
    if let Some(def) = try_parse_graveyard_keyword_static_with_continuation(line) {
        return vec![def];
    }
    parse_static_line_multi(line)
}

/// CR 607.2d: Reconcile self-chosen type statics with the source's linked
/// persisted choice.
fn reconcile_self_chosen_type_statics(result: &mut ParsedAbilities, types: &[String]) {
    let Some(chosen_kind) = chosen_subtype_kind_from_persisted_choice(result)
        .or_else(|| chosen_kind_from_card_types(types))
    else {
        return;
    };

    for static_def in &mut result.statics {
        let is_self_chosen_type_static = static_def.affected == Some(TargetFilter::SelfRef)
            && static_def
                .description
                .as_deref()
                .is_some_and(is_self_chosen_type_description);
        if !is_self_chosen_type_static {
            continue;
        }
        for modification in &mut static_def.modifications {
            if let ContinuousModification::AddChosenSubtype { kind } = modification {
                *kind = chosen_kind.clone();
            }
        }
    }
}

fn chosen_kind_from_card_types(types: &[String]) -> Option<ChosenSubtypeKind> {
    if types.iter().any(|card_type| card_type == "Creature") {
        Some(ChosenSubtypeKind::CreatureType)
    } else if types.iter().any(|card_type| card_type == "Land") {
        Some(ChosenSubtypeKind::BasicLandType)
    } else {
        None
    }
}

fn chosen_subtype_kind_from_persisted_choice(
    result: &ParsedAbilities,
) -> Option<ChosenSubtypeKind> {
    result
        .replacements
        .iter()
        .filter_map(|replacement| replacement.execute.as_deref())
        .find_map(chosen_subtype_kind_from_ability)
        .or_else(|| {
            result
                .abilities
                .iter()
                .find_map(chosen_subtype_kind_from_ability)
        })
        .or_else(|| {
            result
                .triggers
                .iter()
                .filter_map(|trigger| trigger.execute.as_deref())
                .find_map(chosen_subtype_kind_from_ability)
        })
}

fn chosen_subtype_kind_from_ability(def: &AbilityDefinition) -> Option<ChosenSubtypeKind> {
    match def.effect.as_ref() {
        Effect::Choose {
            choice_type: ChoiceType::CreatureType,
            persist: true,
        } => Some(ChosenSubtypeKind::CreatureType),
        Effect::Choose {
            choice_type: ChoiceType::BasicLandType,
            persist: true,
        } => Some(ChosenSubtypeKind::BasicLandType),
        _ => def
            .sub_ability
            .as_deref()
            .and_then(chosen_subtype_kind_from_ability),
    }
}

fn is_self_chosen_type_description(description: &str) -> bool {
    let lower = description.to_lowercase();
    let parsed = alt((
        tag::<_, _, OracleError<'_>>("~ is"),
        tag("this creature is"),
        tag("this land is"),
        tag("this permanent is"),
    ))
    .parse(lower.as_str())
    .and_then(|(rest, _)| tag(" the chosen type").parse(rest));
    parsed.is_ok()
}

fn push_same_is_true_static_tail<F>(
    result: &mut ParsedAbilities,
    line: &str,
    lower: &str,
    parse_modeled_sentence: F,
) -> bool
where
    F: for<'i> FnMut(&'i str) -> OracleResult<'i, ()>,
{
    if let Some((modeled_sentence, unmodeled_tail)) =
        split_same_is_true_static_tail(line, lower, parse_modeled_sentence)
    {
        result
            .statics
            .extend(parse_static_line_with_graveyard_keyword_continuation(
                modeled_sentence,
            ));
        result.abilities.push(make_unimplemented(unmodeled_tail));
        return true;
    }

    false
}

use crate::parser::oracle_ir::ast::ActivatedConstraintAst;

/// CR 614.1a / CR 614.15: Pre-strip an "instead" replacement clause from effect text.
/// The "instead" keyword signals a cross-line self-replacement pattern (CR 614.15 —
/// "the text can be a separate ability, particularly when preceded by an ability
/// word").
///
/// Three word orders are recognised:
/// 1. "if [condition], instead [effect]" — condition FIRST (Arrow Storm, Lightning Surge)
/// 2. "[effect] instead if [condition]" — mid-line "instead", condition AFTER
/// 3. "[effect] instead" — trailing "instead"
///
/// Any extracted "if [condition]" clause is parsed through the shared condition
/// grammar (`parse_inner_condition`) and composed with any ability-word condition
/// at the caller.
fn strip_instead_clause(
    text: &str,
    ctx: &mut ParseContext,
) -> (String, Option<AbilityCondition>, bool) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Pattern: "if [condition], instead [effect]" — leading-conditional word order.
    // Ordered FIRST: more specific (requires a leading "if " before a ", instead "
    // split). The `", instead "` needle (with surrounding spaces) cannot match the
    // "instead of" compound, so no extra compound guard is needed here.
    if let Some((before, after)) = tp.split_around(", instead ") {
        if let Ok((cond_text, ())) =
            value::<_, _, OracleError<'_>, _>((), tag("if ")).parse(before.lower.trim_start())
        {
            if let Some(condition) = parse_inner_condition(cond_text.trim())
                .ok()
                .and_then(|(rest, condition)| rest.trim().is_empty().then_some(condition))
                .and_then(|condition| ability_word_to_ability_condition(&Some(condition), ctx))
            {
                return (after.original.trim().to_string(), Some(condition), true);
            }
        }
    }

    // Pattern: " instead if [condition]" — mid-line "instead" followed by condition
    if let Some((before, after)) = tp.rsplit_around(" instead if ") {
        let condition_text = after.lower.trim().trim_end_matches('.');
        let condition = parse_inner_condition(condition_text)
            .ok()
            .and_then(|(rest, condition)| rest.trim().is_empty().then_some(condition))
            .and_then(|condition| ability_word_to_ability_condition(&Some(condition), ctx));
        return (before.original.trim().to_string(), condition, true);
    }

    // Pattern: "[effect] instead" — trailing "instead" (with optional period)
    if let Some((before, after)) = tp.rsplit_around(" instead") {
        // Guard: "instead" must be at end of text (not "instead of" compound)
        let remainder = after.lower.trim().trim_end_matches('.');
        if remainder.is_empty() {
            // CR 608.2c guard: Only treat as a cross-line "instead" replacement when
            // the "instead" clause covers the whole effect line (i.e., the remaining
            // text is a single conditional sentence). When there is a prior sentence
            // in the same line (Rite of Replication, Saproling Migration: "Create X.
            // If kicked, create Y instead."), the "instead" is an intra-chain override
            // and must be handled by `strip_additional_cost_conditional` inside the
            // chain parser to produce `AdditionalCostPaidInstead` on the sub-ability.
            let before_trim = before.original.trim().trim_end_matches('.');
            if !before_trim.contains('.') {
                return (before.original.trim().to_string(), None, true);
            }
        }
    }

    (text.to_string(), None, false)
}

#[derive(Debug, Clone)]
struct SpellResolutionLine {
    line: String,
    effect_text: String,
    ability_word_condition: Option<StaticCondition>,
    has_ability_word_prefix: bool,
    min_x_value: u32,
}

fn prepare_spell_resolution_line(raw_line: &str) -> Option<SpellResolutionLine> {
    let raw_line = raw_line.trim();
    if raw_line.is_empty() {
        return None;
    }

    let reminder_body_owned = extract_ability_word_reminder_body(raw_line);
    let raw_line = reminder_body_owned.as_deref().unwrap_or(raw_line);
    let line_with_reminder_stripped = strip_reminder_text(raw_line);
    let min_x_value = x_annotation_min_value(&line_with_reminder_stripped);
    let line = strip_x_cant_be_zero_suffix(&line_with_reminder_stripped);
    if line.is_empty() {
        return None;
    }

    let (ability_word_condition, effect_text, has_ability_word_prefix) =
        if let Some((aw_name, effect_text)) = strip_ability_word_with_name(&line) {
            (ability_word_to_condition(&aw_name), effect_text, true)
        } else {
            (None, line.clone(), false)
        };

    Some(SpellResolutionLine {
        line,
        effect_text,
        ability_word_condition,
        has_ability_word_prefix,
        min_x_value,
    })
}

fn is_self_exile_cleanup_line(line: &str, card_name: &str) -> bool {
    let normalized = normalize_card_name_refs(line, card_name);
    let normalized_lower = normalized.to_lowercase();

    nom_on_lower(&normalized, &normalized_lower, |i| {
        value(
            (),
            (
                tag::<_, _, OracleError<'_>>("exile "),
                tag::<_, _, OracleError<'_>>("~"),
                opt(tag::<_, _, OracleError<'_>>(".")),
            ),
        )
        .parse(i)
    })
    .is_some()
}

fn starts_with_until_duration(line: &str) -> bool {
    let lower = line.to_lowercase();
    nom_on_lower(line, &lower, |i| {
        value(
            (),
            alt((
                tag("until your next turn, "),
                tag("until the end of your next turn, "),
                tag("until end of turn, "),
            )),
        )
        .parse(i)
    })
    .is_some()
}

fn ends_with_quoted_activated_ability(line: &str) -> bool {
    let trimmed = line.trim_end();
    if !matches!(trimmed.chars().next_back(), Some('"')) {
        return false;
    }

    let mut quote_positions = trimmed
        .char_indices()
        .filter_map(|(idx, ch)| (ch == '"').then_some(idx))
        .rev();
    let Some(close_quote) = quote_positions.next() else {
        return false;
    };
    let Some(open_quote) = quote_positions.next() else {
        return false;
    };
    find_activated_colon(&trimmed[open_quote + 1..close_quote]).is_some()
}

fn is_standalone_spell_keyword_action_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    let parsed = all_consuming(value(
        (),
        (
            tag::<_, _, OracleError<'_>>("time travel"),
            opt(tag::<_, _, OracleError<'_>>(".")),
        ),
    ))
    .parse(lower.as_str())
    .is_ok();
    parsed
}

fn is_semicolon_keyword_line(line: &str, mtgjson_keyword_names: &[String]) -> bool {
    let mut saw_multiple_parts = false;
    let mut parts = line
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty());
    let Some(first) = parts.next() else {
        return false;
    };

    if extract_keyword_line(first, mtgjson_keyword_names).is_none() {
        return false;
    }

    for part in parts {
        saw_multiple_parts = true;
        if extract_keyword_line(part, mtgjson_keyword_names).is_none() {
            return false;
        }
    }

    saw_multiple_parts
}

fn is_spell_resolution_instruction_line(
    prepared: &SpellResolutionLine,
    card_name: &str,
    mtgjson_keyword_names: &[String],
    parsed_so_far: &ParsedAbilities,
    ctx: &mut ParseContext,
) -> bool {
    let line = &prepared.line;
    let lower = line.to_lowercase();

    if is_semicolon_keyword_line(line, mtgjson_keyword_names) {
        return false;
    }

    if lower == "start your engines!" || lower == "start your engines" {
        return false;
    }

    if lower == "your speed can increase beyond 4." || lower == "your speed can increase beyond 4" {
        return false;
    }

    if lower_starts_with(&lower, "equip")
        && !lower_starts_with(&lower, "equipped")
        && try_parse_equip(line).is_some()
    {
        return false;
    }

    if !is_ability_activate_cost_static(&lower)
        && extract_keyword_line(line, mtgjson_keyword_names).is_some()
    {
        return false;
    }

    if lower_starts_with(&lower, "enchant ") && !lower_starts_with(&lower, "enchanted ") {
        return false;
    }

    let loyalty_snap = ctx.diagnostics.len();
    let is_loyalty = try_parse_loyalty_line(line, ctx).is_some();
    ctx.diagnostics.truncate(loyalty_snap);
    if is_commander_permission_sentence(line)
        || is_deck_construction_copy_limit_sentence(line)
        || is_loyalty
    {
        return false;
    }

    if is_granted_static_line(&lower) {
        return false;
    }

    if nom_on_lower(line, &lower, |i| {
        value((), alt((tag("to solve \u{2014} "), tag("to solve -- ")))).parse(i)
    })
    .is_some()
    {
        return false;
    }

    if nom_on_lower(line, &lower, |i| {
        value((), alt((tag("solved \u{2014} "), tag("solved -- ")))).parse(i)
    })
    .is_some()
    {
        return false;
    }

    if nom_on_lower(line, &lower, |i| {
        value((), alt((tag("channel \u{2014} "), tag("channel -- ")))).parse(i)
    })
    .is_some()
    {
        return false;
    }

    // CR 702.142: Boast is a keyword ability with "Boast — Cost: Effect" structure.
    if nom_on_lower(line, &lower, |i| {
        value((), alt((tag("boast \u{2014} "), tag("boast -- ")))).parse(i)
    })
    .is_some()
    {
        return false;
    }

    if find_activated_colon(line).is_some() {
        return false;
    }

    let effect_lower = prepared.effect_text.to_lowercase();
    if has_trigger_prefix(&effect_lower) {
        return false;
    }

    if is_static_pattern(&effect_lower) && !should_defer_spell_to_effect(&effect_lower) {
        return false;
    }

    if is_replacement_pattern(&effect_lower)
        && !(scan_contains(&effect_lower, "prevent") && scan_contains(&effect_lower, "damage"))
        && parse_replacement_line(line, card_name).is_some()
    {
        return false;
    }

    if is_opening_hand_begin_game(&lower) || lower_starts_with(&lower, "as an additional cost") {
        return false;
    }

    if parsed_so_far.strive_cost.is_some() {
        if let Some(effect_text) = strip_ability_word(line) {
            let effect_lower = effect_text.to_lowercase();
            if lower_starts_with(&effect_lower, "this spell costs ")
                && scan_contains(
                    &effect_lower,
                    "more to cast for each target beyond the first",
                )
            {
                return false;
            }
        }
    }

    if parse_casting_restriction_line(line).is_some()
        || parse_spell_casting_option_line(line, card_name).is_some()
    {
        return false;
    }

    if is_saga_chapter(&lower)
        || is_flashback_equal_mana_cost(&lower)
        || lower_starts_with(&lower, "commander ninjutsu ")
        || lower_starts_with(&lower, "escape")
        || lower_starts_with(&lower, "cumulative upkeep")
        || is_keyword_cost_line(&lower)
        || is_vehicle_tier_line(&lower)
        || lower_starts_with(&lower, "activate ")
        || lower_starts_with(&lower, "suspend ")
        || lower_starts_with(&lower, "harmonize ")
        || lower_starts_with(&lower, "flashback")
        || lower_starts_with(&lower, "buyback")
        || lower_starts_with(&lower, "this spell costs ")
        || alt((
            tag::<_, _, OracleError<'_>>("kicker"),
            tag("multikicker"),
            tag("replicate"),
            tag("mayhem"),
        ))
        .parse(lower.as_str())
        .is_ok()
    {
        return false;
    }

    let snapshot = ctx.diagnostics.len();
    let parsed = parse_effect_chain_with_context(&prepared.effect_text, AbilityKind::Spell, ctx);
    ctx.diagnostics.truncate(snapshot);
    !has_unimplemented(&parsed)
}

/// Map a known ability word name to a typed `StaticCondition`.
/// Returns `None` for unrecognized ability words (Landfall, Constellation, etc.
/// don't have implicit conditions — their trigger text encodes the condition).
///
/// Covers:
/// - Threshold: 7+ cards in graveyard
/// - Metalcraft: 3+ artifacts you control
/// - Delirium: 4+ card types in graveyard
/// - Spell mastery: 2+ instant/sorcery in graveyard
/// - Revolt: a permanent you controlled left the battlefield this turn
/// - Ferocious: you control a creature with power 4 or greater
fn ability_word_to_condition(word: &str) -> Option<crate::types::ability::StaticCondition> {
    use crate::types::ability::{
        CardTypeSetSource, Comparator, ControllerRef, CountScope, FilterProp, PlayerScope, PtStat,
        PtValueScope, QuantityExpr, QuantityRef, StaticCondition, TargetFilter, TypeFilter,
        TypedFilter, ZoneRef,
    };

    match word {
        "threshold" => Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::GraveyardSize {
                    player: PlayerScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 7 },
        }),
        "metalcraft" => Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
                    ),
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 3 },
        }),
        "delirium" => Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::DistinctCardTypes {
                    source: CardTypeSetSource::Zone {
                        zone: ZoneRef::Graveyard,
                        scope: CountScope::Controller,
                    },
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 4 },
        }),
        "spell mastery" => Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Graveyard,
                    card_types: vec![TypeFilter::Instant, TypeFilter::Sorcery],
                    scope: CountScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 2 },
        }),
        "revolt" => {
            // Revolt: "a permanent you controlled left the battlefield this turn"
            // Uses the per-turn zone-change tracking on GameState.
            // Mapped to a QuantityComparison checking permanents_left_battlefield > 0.
            // The tracking field already exists as part of the general zone-change tracking.
            Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneChangeCountThisTurn {
                        from: Some(Zone::Battlefield),
                        to: None,
                        filter: TargetFilter::Typed(
                            TypedFilter::permanent().controller(ControllerRef::You),
                        ),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
        }
        // allow-noncombinator: semantic mapping after ability-word parser has classified the word
        // CR 702.x: Ferocious — "you control a creature with power 4 or greater".
        // The `InZone { Battlefield }` property is emitted explicitly so this
        // ability-word condition is structurally identical to the literal
        // "you control a creature with power 4 or greater" clause parsed by
        // `parse_inner_condition`, letting `merge_ability_condition` dedup the
        // two when a card prints both (e.g. Feed the Clan's "Ferocious — …
        // instead if you control a creature with power 4 or greater").
        "ferocious" => Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![
                                FilterProp::PtComparison {
                                    stat: PtStat::Power,
                                    scope: PtValueScope::Current,
                                    comparator: Comparator::GE,
                                    value: QuantityExpr::Fixed { value: 4 },
                                },
                                FilterProp::InZone {
                                    zone: Zone::Battlefield,
                                },
                            ]),
                    ),
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        }),
        "max speed" => Some(StaticCondition::HasMaxSpeed),
        _ => None,
    }
}

/// Convert an ability-word `StaticCondition` to an `AbilityCondition` for spell effects.
/// CR 608.2c: Bridge an ability-word / "instead if" `StaticCondition` to its
/// effect-resolution `AbilityCondition` form. Delegates to the single
/// authoritative bridge (`static_condition_to_ability_condition`) so every
/// `StaticCondition` variant — including compound `Or`/`And`, `WasStartingPlayer`,
/// and `SpellCastWithVariantThisTurn` — is handled uniformly rather than via a
/// narrow per-call duplicate.
fn ability_word_to_ability_condition(
    cond: &Option<crate::types::ability::StaticCondition>,
    ctx: &mut ParseContext,
) -> Option<crate::types::ability::AbilityCondition> {
    crate::parser::oracle_effect::conditions::static_condition_to_ability_condition(
        cond.as_ref()?,
        ctx,
    )
}

/// Single-authority merge for composing a freshly-parsed `AbilityCondition` onto an
/// existing one on an `AbilityDefinition`.
///
/// CR 608.2c: Compound condition — a spell's resolution gate is the conjunction of
/// every condition that applies. Two independent parser paths can emit the same
/// condition (e.g. the "Delirium —" ability-word prefix and the literal
/// "If there are four or more card types..." phrase both yield the same
/// `QuantityCheck`). Structural dedup keeps the AST flat and prevents
/// `And(X, X)` wrappers that would be semantically identical but waste work.
///
/// Invariants:
/// - Structural equality (`==`) is the dedup criterion.
/// - Results never nest: `And` children are always leaves, never `And`.
/// - Empty-conjunction not produced — at least one operand is always retained.
fn merge_ability_condition(
    existing: Option<crate::types::ability::AbilityCondition>,
    incoming: crate::types::ability::AbilityCondition,
) -> crate::types::ability::AbilityCondition {
    use crate::types::ability::AbilityCondition;
    match existing {
        None => incoming,
        Some(existing) if existing == incoming => existing,
        Some(AbilityCondition::And { mut conditions }) => {
            // Flatten: if incoming is itself an And, absorb its children.
            let new_children: Vec<AbilityCondition> = match incoming {
                AbilityCondition::And { conditions: inner } => inner,
                other => vec![other],
            };
            for child in new_children {
                if !conditions.contains(&child) {
                    conditions.push(child);
                }
            }
            // If dedup collapsed everything to a single child, unwrap.
            if conditions.len() == 1 {
                conditions.into_iter().next().unwrap()
            } else {
                AbilityCondition::And { conditions }
            }
        }
        Some(existing) => match incoming {
            AbilityCondition::And { mut conditions } => {
                // Existing is a leaf; prepend it to the incoming And (deduped).
                if !conditions.contains(&existing) {
                    conditions.insert(0, existing);
                }
                if conditions.len() == 1 {
                    conditions.into_iter().next().unwrap()
                } else {
                    AbilityCondition::And { conditions }
                }
            }
            other => AbilityCondition::And {
                conditions: vec![existing, other],
            },
        },
    }
}

/// Convert an ability-word condition to a `TriggerCondition`.
/// All known ability words use `StaticCondition::QuantityComparison`, which maps
/// directly to `TriggerCondition::QuantityComparison`.
fn ability_word_to_trigger_condition(
    word: &str,
) -> Option<crate::types::ability::TriggerCondition> {
    use crate::types::ability::{StaticCondition, TriggerCondition};
    match ability_word_to_condition(word)? {
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => Some(TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }),
        StaticCondition::HasMaxSpeed => Some(TriggerCondition::HasMaxSpeed),
        _ => None,
    }
}

fn parse_flash_cleanup_sacrifice_casting_option(
    line: &str,
) -> Option<(SpellCastingOption, TriggerDefinition)> {
    let lower = line.trim().to_ascii_lowercase();
    let (rest, _) =
        tag::<_, _, OracleError<'_>>("you may cast this spell as though it had flash. ")
            .parse(lower.as_str())
            .ok()?;
    let (rest, _) =
        tag::<_, _, OracleError<'_>>("if you cast it any time a sorcery couldn't have been cast, ")
            .parse(rest)
            .ok()?;
    all_consuming(tag::<_, _, OracleError<'_>>(
        "the controller of the permanent it becomes sacrifices it at the beginning of the next cleanup step.",
    ))
    .parse(rest)
    .ok()?;

    let sacrifice = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
    );
    let delayed = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase {
                phase: Phase::Cleanup,
            },
            effect: Box::new(sacrifice),
            uses_tracked_set: false,
        },
    );
    let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .destination(Zone::Battlefield)
        .valid_card(TargetFilter::SelfRef)
        .condition(TriggerCondition::CastTimingPermission {
            permission: CastTimingPermission::AsThoughHadFlash,
        })
        .execute(delayed)
        .description(line.to_string());

    Some((SpellCastingOption::as_though_had_flash(), trigger))
}

/// Lower an `OracleDocIr` into the final `ParsedAbilities` via exhaustive match
/// on each `OracleItemIr` variant.
///
/// Core IR variants are lowered through their dedicated lowering functions.
/// PreLowered variants are identity-lowered (pushed directly to the result).
pub(crate) fn lower_oracle_ir(ir: &OracleDocIr) -> ParsedAbilities {
    let mut result = ParsedAbilities {
        abilities: Vec::new(),
        triggers: Vec::new(),
        statics: Vec::new(),
        replacements: Vec::new(),
        extracted_keywords: Vec::new(),
        modal: None,
        additional_cost: None,
        casting_restrictions: Vec::new(),
        casting_options: Vec::new(),
        solve_condition: None,
        strive_cost: None,
        parse_warnings: Vec::new(),
    };
    for item in &ir.items {
        match item {
            OracleItemIr::Spell(effect_ir) => {
                result.abilities.push(lower_effect_chain_ir(effect_ir));
            }
            OracleItemIr::Trigger(trigger_ir) => {
                result.triggers.push(lower_trigger_ir(trigger_ir));
            }
            OracleItemIr::Static(static_ir) => {
                result.statics.push(lower_static_ir(static_ir));
            }
            OracleItemIr::Replacement(replacement_ir) => {
                result
                    .replacements
                    .push(lower_replacement_ir(replacement_ir));
            }
            OracleItemIr::Keyword(kw) => {
                result.extracted_keywords.push(kw.clone());
            }
            OracleItemIr::Modal(modal) => {
                result.modal = Some(modal.clone());
            }
            OracleItemIr::AdditionalCost(cost) => {
                result.additional_cost = Some(cost.clone());
            }
            OracleItemIr::CastingRestriction(restriction) => {
                result.casting_restrictions.push(restriction.clone());
            }
            OracleItemIr::CastingOption(option) => {
                result.casting_options.push(option.clone());
            }
            OracleItemIr::SolveCondition(condition) => {
                result.solve_condition = Some(condition.clone());
            }
            OracleItemIr::StriveCost(cost) => {
                result.strive_cost = Some(cost.clone());
            }
            OracleItemIr::PreLoweredTrigger(def) => {
                result.triggers.push(def.clone());
            }
            OracleItemIr::PreLoweredStatic(def) => {
                result.statics.push(def.clone());
            }
            OracleItemIr::PreLoweredReplacement(def) => {
                result.replacements.push(def.clone());
            }
            OracleItemIr::PreLoweredSpell(def) => {
                result.abilities.push(def.clone());
            }
        }
    }
    result.parse_warnings = ir.diagnostics.clone();
    // CR 607.1 + CR 610.3: Two-trigger exile-return synthesis. Cards like
    // Journey to Nowhere and Oblivion Ring use a two-trigger design:
    //   Line 1 (ETB): "When ~ enters, exile target creature."
    //   Line 2 (LTB): "When ~ leaves the battlefield, return the exiled card
    //                  to the battlefield under its owner's control."
    // The ETB exile produces no duration (the oracle text has no "until" clause),
    // so no ExileLink::UntilSourceLeaves is created and the exiled card is
    // never returned. Fix: when we detect this paired pattern, set
    // Duration::UntilHostLeavesPlay on the ETB exile's execute ability so the
    // existing exile-link mechanism handles the return correctly. The LTB
    // trigger stays registered as-is (its TrackedSet target gracefully resolves
    // to nothing when the exile link has already returned the card).
    synthesize_etb_exile_ltb_return_pair(&mut result.triggers);
    result
}

/// CR 607.1 + CR 610.3: Detect an (ETB exile, LTB return) trigger pair and
/// upgrade the ETB exile to `Duration::UntilHostLeavesPlay` so the
/// `ExileLink::UntilSourceLeaves` mechanism returns the exiled card when the
/// source leaves. Covers Journey to Nowhere, Oblivion Ring, and the broader
/// "exile target X … LTB return" two-trigger class.
fn synthesize_etb_exile_ltb_return_pair(triggers: &mut [TriggerDefinition]) {
    let has_ltb_return = triggers.iter().any(|t| {
        t.mode == TriggerMode::LeavesBattlefield
            && t.execute.as_deref().is_some_and(|ex| {
                matches!(
                    ex.effect.as_ref(),
                    Effect::ChangeZone {
                        destination: Zone::Battlefield,
                        target: TargetFilter::TrackedSet { .. },
                        ..
                    }
                )
            })
    });

    if !has_ltb_return {
        return;
    }

    for trig in triggers.iter_mut() {
        if trig.mode != TriggerMode::ChangesZone || trig.destination != Some(Zone::Battlefield) {
            continue;
        }
        let Some(execute) = trig.execute.as_deref_mut() else {
            continue;
        };
        if !matches!(
            execute.effect.as_ref(),
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ) {
            continue;
        }
        if execute.duration.is_none() {
            execute.duration = Some(crate::types::ability::Duration::UntilHostLeavesPlay);
        }
    }
}

/// Produce an `OracleDocIr` from Oracle text — the IR-production half of the
/// parse/lower split (Phase 49, Plan 03).
///
/// Contains all pre-processing (saga, class, leveler, modal, spacecraft, strive)
/// and the full per-line dispatch loop. Parsed items are wrapped in `OracleItemIr`
/// variants. Pre-processors and complex dispatch paths use `PreLowered*` variants
/// carrying already-assembled engine types; future phases will incrementally
/// migrate these to proper IR types.
pub(crate) fn parse_oracle_ir(
    oracle_text: &str,
    card_name: &str,
    mtgjson_keyword_names: &[String],
    types: &[String],
    subtypes: &[String],
) -> OracleDocIr {
    let is_spell = types.iter().any(|t| t == "Instant" || t == "Sorcery");

    let mut result = ParsedAbilities {
        abilities: Vec::new(),
        triggers: Vec::new(),
        statics: Vec::new(),
        replacements: Vec::new(),
        extracted_keywords: Vec::new(),
        modal: None,
        additional_cost: None,
        casting_restrictions: Vec::new(),
        casting_options: Vec::new(),
        solve_condition: None,
        strive_cost: None,
        parse_warnings: Vec::new(),
    };

    let mut ctx = ParseContext {
        card_name: Some(card_name.to_string()),
        ..Default::default()
    };

    // CR 303.4 + CR 702.103: When the card being parsed is an Aura or has the
    // Bestow keyword, it can be attached to a permanent. A "that creature"
    // anaphor inside such a card's ability body (e.g. Springheart Nantuko's
    // landfall "create a token that's a copy of that creature") refers to the
    // enchanted host, not a chosen target. Expose the typed host self-reference
    // so the token-copy parser can remap a generic-parser `ParentTarget` to
    // `TargetFilter::AttachedTo`. Left `None` for non-Aura cards so
    // `ParentTarget` keeps its chosen-target meaning (Twinflame Strike).
    if subtypes.iter().any(|s| s.eq_ignore_ascii_case("Aura"))
        || mtgjson_keyword_names
            .iter()
            .any(|k| k.eq_ignore_ascii_case("bestow"))
    {
        ctx.host_self_reference = Some(crate::types::ability::TargetFilter::AttachedTo);
    }

    // CR 201.4b: A card's Oracle text uses its name to refer to itself.
    // Normalize self-references to `~` once, at the single parser entry point,
    // so every downstream block parser (saga, class, leveler, modal, trigger,
    // static, effect, replacement, spacecraft) receives already-normalized
    // text. The `pub fn` wrappers retained for test-facing API re-invoke
    // `normalize_card_name_refs` on this pre-normalized text; strategies 1-4
    // find nothing to replace and strategy 5 is short-circuited by its
    // `!result.contains('~')` guard, making re-entry an idempotent no-op.
    let oracle_text_owned = normalize_card_name_refs(oracle_text, card_name);
    let lines: Vec<&str> = oracle_text_owned.split('\n').collect();

    // CR 714 / CR 717: Pre-parse Saga chapters and Attraction visit lines.
    let mut preparsed_consumed = if subtypes.iter().any(|s| s == "Saga") {
        let (chapter_triggers, etb_replacement, consumed) = parse_saga_chapters(&lines, card_name);
        result.triggers.extend(chapter_triggers);
        result.replacements.push(etb_replacement);
        consumed
    } else {
        std::collections::HashSet::new()
    };
    if subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Attraction"))
    {
        let (visit_triggers, consumed) = parse_attraction_visit_triggers(&lines, card_name);
        result.triggers.extend(visit_triggers);
        preparsed_consumed.extend(consumed);
    }

    // CR 716: Pre-parse Class level sections into level-gated abilities.
    if subtypes.iter().any(|s| s == "Class") {
        let class_result =
            parse_class_oracle_text(&lines, card_name, mtgjson_keyword_names, result);
        return parsed_abilities_to_doc_ir(class_result, oracle_text, card_name, &mut ctx);
    }

    // CR 711: Pre-parse leveler LEVEL blocks into counter-gated static abilities.
    let (level_statics, level_consumed, level_ability_lines) =
        parse_level_blocks(&lines, card_name);
    if !level_statics.is_empty() {
        result.statics.extend(level_statics);
    }
    // CR 711.2a + CR 711.2b: Re-parse ability lines found within LEVEL blocks through
    // the normal trigger/activated/static pipeline, then attach the level counter condition.
    for (ability_text, level_condition) in &level_ability_lines {
        let (minimum, maximum) = match level_condition {
            StaticCondition::HasCounters {
                minimum, maximum, ..
            } => (*minimum, *maximum),
            _ => continue,
        };

        // CR 711.2a + CR 711.2b: Activated abilities within LEVEL blocks get a LevelCounterRange restriction.
        if let Some(colon_pos) = find_activated_colon(ability_text) {
            let cost_text = ability_text[..colon_pos].trim();
            let effect_text = ability_text[colon_pos + 1..].trim();
            let (effect_text, constraints) = strip_activated_constraints(effect_text);
            let normalized_cost_text = normalize_self_refs_for_static(cost_text, card_name);
            let cost = parse_oracle_cost(&normalized_cost_text);

            ctx.subject = None;
            ctx.actor = None;
            let mut def =
                parse_effect_chain_with_context(&effect_text, AbilityKind::Activated, &mut ctx);
            if has_unimplemented(&def) {
                let normalized_effect = normalize_self_refs_for_static(&effect_text, card_name);
                if normalized_effect != effect_text {
                    let alt = parse_effect_chain_with_context(
                        &normalized_effect,
                        AbilityKind::Activated,
                        &mut ctx,
                    );
                    if !has_unimplemented(&alt) {
                        def = alt;
                    }
                }
            }
            def.cost = Some(cost);
            def.description = Some(ability_text.to_string());
            if constraints.sorcery_speed() {
                def.sorcery_speed = true;
            }
            let mut restrictions = constraints.restrictions;
            restrictions.push(ActivationRestriction::LevelCounterRange { minimum, maximum });
            def.activation_restrictions = restrictions;
            extract_cost_reduction_from_chain(&mut def);
            result.abilities.push(def);
            continue;
        }

        // CR 711.2a + CR 711.2b: Triggered abilities within LEVEL blocks get a HasCounters condition.
        // (Static abilities are now parsed directly in oracle_level.rs with the level condition attached.)
        let trigger_condition = TriggerCondition::HasCounters {
            counters: crate::types::counter::CounterMatch::OfType(
                crate::types::counter::CounterType::Generic("level".to_string()),
            ),
            minimum,
            maximum,
        };
        // CR 707.9a: Thread the running trigger count as the base index so
        // any "and it has this ability" except clause inside a leveler trigger
        // body resolves to the correct printed-trigger slot.
        let mut triggers = parse_trigger_lines_at_index(
            ability_text,
            card_name,
            Some(result.triggers.len()),
            &mut ctx,
        );
        for trigger in &mut triggers {
            trigger.condition = Some(trigger_condition.clone());
        }
        result.triggers.extend(triggers);
    }

    // CR 702.184a + CR 721.2: Pre-parse Spacecraft "N+ | body" threshold lines
    // into charge-counter-gated statics / triggers / activated abilities. The
    // `Station` reminder-text paragraph is handled independently: the keyword
    // itself comes from MTGJSON, and the creature-shift at the highest symbol
    // (CR 721.2b) is synthesized post-parse in `database::synthesis::synthesize_station`
    // where `face.power` / `face.toughness` are available for the base P/T.
    let spacecraft_consumed = if subtypes.iter().any(|s| s == "Spacecraft") {
        // CR 707.9a: Pass the running trigger count so any "has this ability"
        // retain modification inside a Spacecraft threshold trigger body
        // resolves to the correct printed-trigger slot.
        let (sc_statics, sc_triggers, sc_abilities, consumed) =
            parse_spacecraft_threshold_lines(&lines, card_name, result.triggers.len());
        result.statics.extend(sc_statics);
        result.triggers.extend(sc_triggers);
        for mut def in sc_abilities {
            extract_cost_reduction_from_chain(&mut def);
            result.abilities.push(def);
        }
        consumed
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
    } else {
        std::collections::HashSet::new()
    };

    // CR 207.2c + CR 601.2f: Pre-parse Strive ability word cost before main loop.
    // Strive lines have the form: "Strive — This spell costs {X} more to cast for each
    // target beyond the first." — extract the per-target surcharge cost.
    for raw in &lines {
        let stripped = strip_reminder_text(raw.trim());
        if let Some(effect_text) = strip_ability_word(&stripped) {
            let effect_lower = effect_text.to_lowercase();
            if let Some(((), rest_original)) = nom_on_lower(&effect_text, &effect_lower, |i| {
                value((), tag("this spell costs ")).parse(i)
            }) {
                if let Some((mana_part, _)) =
                    rest_original.split_once(" more to cast for each target beyond the first")
                {
                    if let Some((cost, _)) = parse_mana_symbols(mana_part) {
                        result.strive_cost = Some(cost);
                        break;
                    }
                }
            }
        }
    }

    let mut i = 0;

    while i < lines.len() {
        // CR 711: Skip lines already consumed by the leveler pre-parser.
        if level_consumed.contains(&i) {
            i += 1;
            continue;
        }
        // CR 714 / CR 717: Skip lines consumed by saga/attraction pre-parsers.
        if preparsed_consumed.contains(&i) {
            i += 1;
            continue;
        }
        // CR 702.184a + CR 721: Skip Spacecraft threshold lines already consumed.
        if spacecraft_consumed.contains(&i) {
            i += 1;
            continue;
        }

        let raw_line = lines[i].trim();
        if raw_line.is_empty() {
            i += 1;
            continue;
        }

        // CR 207.2c: Ability words have no rules meaning. For the Increment-class
        // pattern (`<ability-word> (<body>)`) where the printed reminder text IS
        // the rules body — e.g., SOS Increment / Opus / Repartee / Converge —
        // extract the parenthesized body and dispatch it as if it were the line
        // itself. Without this, `strip_reminder_text` (next line) would erase
        // the entire body and leave only the bare ability-word name, producing
        // zero parsed abilities for these cards.
        let reminder_body_owned = extract_ability_word_reminder_body(raw_line);
        let raw_line: &str = reminder_body_owned.as_deref().unwrap_or(raw_line);

        let line = strip_reminder_text(raw_line);
        let ability_cant_be_copied = x_annotation_marks_ability_uncopyable(&line);
        let min_x_value = x_annotation_min_value(&line);
        // Strip "X can't be 0." casting constraint suffix — annotation only, not an ability.
        let line = strip_x_cant_be_zero_suffix(&line);
        if line.is_empty() {
            if min_x_value > 0 {
                if let Some(previous) = result.abilities.last_mut() {
                    previous.min_x_value = previous.min_x_value.max(min_x_value);
                }
            }
            // Priority 14: entirely parenthesized reminder text
            i += 1;
            continue;
        }

        // Priority 0: Semicolon-separated keyword lines (e.g., "Defender; reach").
        // Oracle text uses semicolons exclusively to separate keywords on a single line.
        // The colon guard prevents splitting activated ability lines like "{T}: Draw a card".
        if line.contains(';') && !line.contains(':') {
            let parts: Vec<&str> = line
                .split(';')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            if parts.len() > 1 {
                let all_keywords = parts
                    .iter()
                    .all(|part| extract_keyword_line(part, mtgjson_keyword_names).is_some());
                if all_keywords {
                    for part in &parts {
                        if let Some(extracted) = extract_keyword_line(part, mtgjson_keyword_names) {
                            result.extracted_keywords.extend(extracted);
                        }
                    }
                    i += 1;
                    continue;
                }
            }
        }

        // Priority 1: Modal block (standard "Choose one —" + modes, or Spree + modes).
        // Must run before keyword extraction so "Spree" header + follow-on `+` lines
        // are consumed as a modal block, not swallowed as a keyword-only line.
        if let Some((block, next_i)) = parse_oracle_block(&lines, i) {
            lower_oracle_block(
                block,
                card_name,
                ctx.host_self_reference.clone(),
                &mut result,
            );
            i = next_i;
            continue;
        }

        let lower = line.to_lowercase();

        // Pre-keyword activated ability: "Equip {cost}" / "Equip — {cost}"
        // (but not "Equipped ...").
        // This must run before keyword-only extraction because MTGJSON keyword
        // names can match exact printed equip costs, but equip is an activated
        // ability and still needs the synthesized activation body.
        if lower_starts_with(&lower, "equip") && !lower_starts_with(&lower, "equipped") {
            if let Some(ability) = try_parse_equip(&line) {
                result.abilities.push(ability);
                i += 1;
                continue;
            }
        }

        // CR 702.122 + CR 602.5b: Crew with a trailing "Activate only once each
        // turn." cadence sentence. Must run before the generic keyword-only
        // extraction below — that path parses "Crew N" via `parse_keyword_from_oracle`
        // and would consume the line, dropping the cadence sentence.
        if lower_starts_with(&lower, "crew ") {
            if let Some(crew_kw) = parse_crew_keyword(&lower) {
                result.extracted_keywords.push(crew_kw);
                i += 1;
                continue;
            }
        }

        // Priority 1b: keyword-only line — extract any keywords for the union set
        // Guard: "{Keyword} abilities you activate cost {N} less" is a static ability,
        // not a keyword line. Don't let keyword extraction consume it.
        let is_ability_cost_static = is_ability_activate_cost_static(&lower);
        if !is_ability_cost_static {
            if let Some(extracted) = extract_keyword_line(&line, mtgjson_keyword_names) {
                if let Some(cost) = parse_kicker_additional_cost_line(&line, &lower) {
                    merge_kicker_additional_cost(&mut result.additional_cost, cost);
                }
                result.extracted_keywords.extend(extracted);
                i += 1;
                continue;
            }
        }

        // Normalize card self-references for static parsing (replace card name with ~)
        let static_line = normalize_self_refs_for_static(&line, card_name);
        let static_line_lower = static_line.to_lowercase();
        if push_same_is_true_static_tail(
            &mut result,
            &static_line,
            &static_line_lower,
            parse_chosen_creature_type_static_prefix,
        ) {
            i += 1;
            continue;
        }
        if push_same_is_true_static_tail(
            &mut result,
            &static_line,
            &static_line_lower,
            parse_every_creature_type_static_prefix,
        ) {
            i += 1;
            continue;
        }
        if let Some(next_raw_line) = lines.get(i + 1).map(|next| next.trim()) {
            if !next_raw_line.is_empty() {
                let next_line = strip_x_cant_be_zero_suffix(&strip_reminder_text(next_raw_line));
                if !next_line.is_empty() {
                    let next_static_line = normalize_self_refs_for_static(&next_line, card_name);
                    let combined_static_line = format!("{static_line} {next_static_line}");
                    if let Some(static_def) =
                        try_parse_graveyard_keyword_static_with_continuation(&combined_static_line)
                    {
                        result.statics.push(static_def);
                        i += 2;
                        continue;
                    }
                }
            }
        }

        // CR 604.3 + CR 604.3a + CR 105.2c: Some instants/sorceries carry
        // self color-defining characteristic-defining abilities (e.g.,
        // "~ is colorless.") that define the source's own color in all zones.
        // Intercept only this narrow class before spell-effect lowering.
        //
        // Intercept only that narrow class so we do not steal ordinary spell
        // instruction lines that happen to have static-like phrasing.
        if is_spell {
            let defs = parse_static_line_with_graveyard_keyword_continuation(&static_line);
            let is_self_color_cda = defs.len() == 1
                && defs[0].characteristic_defining
                && defs[0].affected == Some(TargetFilter::SelfRef)
                && defs[0].modifications.len() == 1
                && matches!(
                    defs[0].modifications[0],
                    ContinuousModification::SetColor { .. }
                );
            if is_self_color_cda {
                result.statics.extend(defs);
                i += 1;
                continue;
            }
        }

        if lower == "start your engines!" || lower == "start your engines" {
            result.extracted_keywords.push(Keyword::StartYourEngines);
            i += 1;
            continue;
        }

        if lower == "your speed can increase beyond 4."
            || lower == "your speed can increase beyond 4"
        {
            let defs = parse_static_line_with_graveyard_keyword_continuation(&static_line);
            if !defs.is_empty() {
                result.statics.extend(defs);
                i += 1;
                continue;
            }
        }

        // Priority 2: "Enchant {filter}" — skip (handled externally)
        if lower_starts_with(&lower, "enchant ") && !lower_starts_with(&lower, "enchanted ") {
            i += 1;
            continue;
        }

        if is_commander_permission_sentence(&line) {
            i += 1;
            continue;
        }

        if is_deck_construction_copy_limit_sentence(&line) {
            i += 1;
            continue;
        }

        // CR 702.6: Named equip variant — "<Flavor Name> — Equip {cost}"
        let tp = TextPair::new(&line, &lower);
        if let Some(idx) = tp.find(" \u{2014} equip").or_else(|| tp.find(" - equip")) {
            let equip_part = tp
                .split_at(idx)
                .1
                .original
                .trim_start_matches(" \u{2014} ")
                .trim_start_matches(" - ");
            if let Some(ability) = try_parse_equip(equip_part) {
                result.abilities.push(ability);
                i += 1;
                continue;
            }
        }
        // Priority 11: Planeswalker loyalty abilities: +N:, −N:, 0:, [+N]:, [−N]:, [0]:
        if let Some(ability) = try_parse_loyalty_line(&line, &mut ctx) {
            result.abilities.push(ability);
            i += 1;
            continue;
        }

        if is_granted_static_line(&lower) {
            // B20: Handle compound "can't win/lose" lines by splitting
            if is_cant_win_lose_compound(&lower) {
                for clause in static_line.split(" and ") {
                    let trimmed = clause.trim().trim_end_matches('.');
                    if !trimmed.is_empty() {
                        let clause_dot = format!("{trimmed}.");
                        result.statics.extend(
                            parse_static_line_with_graveyard_keyword_continuation(&clause_dot),
                        );
                    }
                }
                i += 1;
                continue;
            }
            // Compound detection (CR 602.5 can't-be-activated, cross-mode conjunctions,
            // life-total locks, etc.) is already owned by `parse_static_line_multi`,
            // which the wrapper below delegates to.
            let defs = parse_static_line_with_graveyard_keyword_continuation(&static_line);
            if !defs.is_empty() {
                result.statics.extend(defs);
                i += 1;
                continue;
            }
        }

        // Priority 3b: Case "To solve — {condition}" line (CR 719.1)
        if let Some(((), rest_original)) = nom_on_lower(&line, &lower, |i| {
            value((), alt((tag("to solve \u{2014} "), tag("to solve -- ")))).parse(i)
        }) {
            let rest_lower = rest_original.to_lowercase();
            result.solve_condition = Some(parse_solve_condition(&rest_lower));
            i += 1;
            continue;
        }

        // CR 719.3c: Case "Solved — {cost}: {effect}" activated ability.
        if let Some(((), rest)) = nom_on_lower(&line, &lower, |i| {
            value((), alt((tag("solved \u{2014} "), tag("solved -- ")))).parse(i)
        }) {
            if let Some(colon_pos) = find_activated_colon(rest) {
                let cost_text = rest[..colon_pos].trim();
                let effect_text = rest[colon_pos + 1..].trim();
                let (effect_text, constraints) = strip_activated_constraints(effect_text);
                let cost = parse_oracle_cost(cost_text);

                ctx.subject = None;
                ctx.actor = None;
                let mut def =
                    parse_effect_chain_with_context(&effect_text, AbilityKind::Activated, &mut ctx);
                def.cost = Some(cost);
                def.description = Some(line.to_string());
                // CR 719.3c: Solved abilities only activate while Case is solved.
                def.activation_restrictions
                    .push(ActivationRestriction::IsSolved);
                if constraints.sorcery_speed() {
                    def.sorcery_speed = true;
                }
                // CR 602.5d: `constraints.restrictions` already contains
                // `AsSorcery` when the source text said "Activate only as a
                // sorcery"; extend preserves it so the legality gate fires.
                if !constraints.restrictions.is_empty() {
                    def.activation_restrictions.extend(constraints.restrictions);
                }
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 3c: Channel — "Channel — {cost}, Discard this card: {effect}" (CR 207.2c + CR 602.1)
        if let Some(((), rest_original)) = nom_on_lower(&line, &lower, |i| {
            value((), alt((tag("channel \u{2014} "), tag("channel -- ")))).parse(i)
        }) {
            let rest_lower = rest_original.to_lowercase();
            if let Some(colon_pos) = find_activated_colon(&rest_lower) {
                let prefix_len = line.len() - rest_original.len();
                let cost_text = line[prefix_len..prefix_len + colon_pos].trim();
                let effect_text = line[prefix_len + colon_pos + 1..].trim();
                let (effect_text, constraints) = strip_activated_constraints(effect_text);
                let cost = parse_oracle_cost(cost_text);
                ctx.subject = None;
                ctx.actor = None;
                let mut def =
                    parse_effect_chain_with_context(&effect_text, AbilityKind::Activated, &mut ctx);
                def.cost = Some(cost);
                // CR 207.2c: Channel is an ability word; the underlying ability activates from hand.
                def.activation_zone = Some(Zone::Hand);
                def.description = Some(line.to_string());
                if constraints.sorcery_speed() {
                    def.sorcery_speed = true;
                }
                if !constraints.restrictions.is_empty() {
                    def.activation_restrictions = constraints.restrictions;
                }
                // CR 601.2f: Extract self-referential cost reduction from the terminal
                // sub_ability in the chain (it may be several levels deep).
                extract_cost_reduction_from_chain(&mut def);
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 3d: Boast — "Boast — {cost}: {effect}" (CR 702.142a)
        // Boast is a keyword ability (not an ability word per CR 207.2c) that grants
        // an activated ability with implicit restrictions: "Activate only if this
        // creature attacked this turn and only once each turn."
        if let Some(((), rest_original)) = nom_on_lower(&line, &lower, |i| {
            value((), alt((tag("boast \u{2014} "), tag("boast -- ")))).parse(i)
        }) {
            let rest_lower = rest_original.to_lowercase();
            if let Some(colon_pos) = find_activated_colon(&rest_lower) {
                let prefix_len = line.len() - rest_original.len();
                let cost_text = line[prefix_len..prefix_len + colon_pos].trim();
                let effect_text = line[prefix_len + colon_pos + 1..].trim();
                let (effect_text, constraints) = strip_activated_constraints(effect_text);
                let cost = parse_oracle_cost(cost_text);
                ctx.subject = None;
                ctx.actor = None;
                let mut def =
                    parse_effect_chain_with_context(&effect_text, AbilityKind::Activated, &mut ctx);
                def.cost = Some(cost);
                def.description = Some(line.to_string());
                if constraints.sorcery_speed() {
                    def.sorcery_speed = true;
                }
                def.activation_restrictions.extend(constraints.restrictions);
                // CR 702.142a: "Activate only if this creature attacked this turn
                // and only once each turn."
                def.activation_restrictions
                    .push(ActivationRestriction::OnlyOnceEachTurn);
                def.activation_restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        condition: Some(ParsedCondition::SourceAttackedThisTurn),
                    });
                // CR 702.142b: Tag this ability as originating from Boast so
                // effects can reference "boast abilities" as a class.
                def.ability_tag = Some(AbilityTag::Boast);
                extract_cost_reduction_from_chain(&mut def);
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 3e: Exhaust — "Exhaust — {cost}: {effect}" (CR 702.177a)
        // Exhaust is a keyword ability that grants an activated ability with
        // the implicit activation restriction "Activate only once."
        if let Some(((), rest_original)) = nom_on_lower(&line, &lower, |i| {
            value((), alt((tag("exhaust \u{2014} "), tag("exhaust -- ")))).parse(i)
        }) {
            let rest_lower = rest_original.to_lowercase();
            if let Some(colon_pos) = find_activated_colon(&rest_lower) {
                let prefix_len = line.len() - rest_original.len();
                let cost_text = line[prefix_len..prefix_len + colon_pos].trim();
                let effect_text = line[prefix_len + colon_pos + 1..].trim();
                let (effect_text, constraints) = strip_activated_constraints(effect_text);
                let cost = parse_oracle_cost(cost_text);
                ctx.subject = None;
                ctx.actor = None;
                let mut def =
                    parse_effect_chain_with_context(&effect_text, AbilityKind::Activated, &mut ctx);
                def.cost = Some(cost);
                def.description = Some(line.to_string());
                if constraints.sorcery_speed() {
                    def.sorcery_speed = true;
                }
                def.activation_restrictions.extend(constraints.restrictions);
                def.activation_restrictions
                    .push(ActivationRestriction::OnlyOnce);
                def.ability_tag = Some(AbilityTag::Exhaust);
                extract_cost_reduction_from_chain(&mut def);
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 4: Activated ability — contains ":" with cost-like prefix
        if let Some(colon_pos) = find_activated_colon(&line) {
            let cost_text = line[..colon_pos].trim();
            let effect_text = line[colon_pos + 1..].trim();
            let (mut def, effect_text) = parse_activated_ability_definition(
                cost_text,
                effect_text,
                &line,
                card_name,
                &mut ctx,
            );
            if ability_cant_be_copied {
                def.cant_be_copied = true;
            }
            def.min_x_value = min_x_value;
            i += 1;
            // CR 706: If the activated ability ends with "roll a dN", consume
            // subsequent d20 table lines and attach them as die result branches.
            if has_roll_die_pattern(&effect_text.to_lowercase()) {
                i = attach_die_result_branches_to_chain(&mut def, &lines, i);
            }
            result.abilities.push(def);
            continue;
        }

        // Priority 5-pre: trigger-framed "… enters with [counters] on it" lines
        // are CR 614.1c replacement effects, not triggered abilities — despite
        // the "whenever"/"when" framing. Intercept before the generic trigger
        // dispatch routes them through the SpellCast / ChangesZone matcher.
        // Applies to Wildgrowth Archaic and cousin cards (Runadi, Boreal
        // Outrider, Torgal, Dragon Broodmother, …). `parse_replacement_line`
        // handles all the compositional variants (fixed / X / "where X is …").
        //
        // CR 603.2 exclusion: an ETB-with-counter TRIGGER ("… enters with a
        // counter on it, <consequence>") watches for ANY (untyped) counter and
        // is a real triggered ability (Murderous Redcap Avatar class). The
        // typed/counted enters-with forms ("a +1/+1 counter", "X +1/+1
        // counters", "an additional loyalty counter") are CR 614.1c
        // replacements. `is_enters_with_counter_trigger` recognizes the untyped
        // trigger and excludes it from this replacement interceptor.
        if has_trigger_prefix(&lower)
            && !is_enters_with_counter_trigger(&lower)
            && scan_contains(&lower, "enters with")
        {
            if let Some(rep_def) = parse_replacement_line(&line, card_name) {
                result.replacements.push(rep_def);
                i += 1;
                continue;
            }
        }

        // CR 603.7a-b: Instant/sorcery text like "Whenever [event] this turn, ..."
        // creates a delayed triggered ability during resolution. It is not a
        // permanent's printed triggered ability, so spell cards must get one
        // chance to route trigger-shaped temporal text through the effect parser
        // before generic trigger dispatch.
        if is_spell && has_trigger_prefix(&lower) && scan_contains(&lower, "this turn") {
            if let Some(def) = try_parse_temporal_delayed_trigger_ability(&line, AbilityKind::Spell)
            {
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 5-6: Triggered abilities — starts with When/Whenever/At
        // CR 603.2: Compound triggers ("When X and when Y, effect") produce
        // multiple TriggerDefinitions sharing the same execute effect.
        if has_trigger_prefix(&lower) {
            // CR 707.9a: Pass the running trigger count as the base index so
            // any "and it has this ability" except clause in this trigger's
            // body resolves to the correct printed-trigger slot.
            let mut triggers = parse_trigger_lines_at_index(
                &line,
                card_name,
                Some(result.triggers.len()),
                &mut ctx,
            );
            i += 1;
            // CR 706: If the trigger's effect ends with "roll a dN", consume
            // subsequent d20 table lines and attach them as die result branches.
            if has_roll_die_pattern(&lower) {
                if let Some(last) = triggers.last_mut() {
                    if let Some(ref mut execute) = last.execute {
                        i = attach_die_result_branches_to_chain(execute, &lines, i);
                    }
                }
            }
            result.triggers.extend(triggers);
            continue;
        }

        // Priority 6b: Ability-word-prefixed activated abilities/triggers (e.g.,
        // "Threshold — {T}: ...", "Heroic — Whenever ..."). Must intercept BEFORE
        // is_static_pattern and is_replacement_pattern checks, which would otherwise
        // match on keywords like "gets" or "prevent" in the effect text and misroute
        // the line.
        if let Some((aw_name, effect_text)) = strip_ability_word_with_name(&line) {
            let effect_lower = effect_text.to_lowercase();
            let aw_condition = ability_word_to_condition(&aw_name);
            if aw_condition.is_some() {
                if let Some(colon_pos) = find_activated_colon(&effect_text) {
                    let cost_text = effect_text[..colon_pos].trim();
                    let activated_effect_text = effect_text[colon_pos + 1..].trim();
                    let (def, _) = parse_activated_ability_definition(
                        cost_text,
                        activated_effect_text,
                        &line,
                        card_name,
                        &mut ctx,
                    );
                    result.abilities.push(def);
                    i += 1;
                    continue;
                }
            }
            if has_trigger_prefix(&effect_lower) {
                // CR 707.9a: Thread the running trigger count as the base index.
                let mut triggers = parse_trigger_lines_at_index(
                    &effect_text,
                    card_name,
                    Some(result.triggers.len()),
                    &mut ctx,
                );
                // B7: Attach ability-word condition as fallback when extract_if_condition
                // doesn't recognize the intervening-if pattern.
                for trigger in &mut triggers {
                    if trigger.condition.is_none() {
                        trigger.condition = ability_word_to_trigger_condition(&aw_name);
                    }
                }
                i += 1;
                if has_roll_die_pattern(&effect_lower) {
                    if let Some(last) = triggers.last_mut() {
                        if let Some(ref mut execute) = last.execute {
                            i = attach_die_result_branches_to_chain(execute, &lines, i);
                        }
                    }
                }
                result.triggers.extend(triggers);
                continue;
            }
        }

        // CR 701.43d: "You may exert [creature] as it attacks" — optional attack cost.
        // Must intercept BEFORE Priority 7 (static patterns) because the "When you do"
        // linked effect often contains "gets +N/+M" which is_static_pattern would match.
        // Standalone: skip (separate "Whenever you exert" trigger line follows).
        // Compound: produce an Exerted trigger with the linked effect.
        if let Some(((), rest_original)) = nom_on_lower(&line, &lower, |i| {
            value(
                (),
                alt((
                    tag("you may exert this creature as it attacks"),
                    tag("you may exert ~ as it attacks"),
                    tag("you may exert it as it attacks"),
                )),
            )
            .parse(i)
        }) {
            // Check for linked "When you do, [effect]" in same sentence
            let rest_trimmed = rest_original.trim().trim_start_matches('.').trim_start();
            let rest_lower = rest_trimmed.to_lowercase();
            if let Some(((), effect_rest)) = nom_on_lower(rest_trimmed, &rest_lower, |i| {
                value((), tag("when you do, ")).parse(i)
            }) {
                ctx.subject = None;
                ctx.actor = None;
                let effect_def = parse_effect_chain_with_context(
                    effect_rest.trim(),
                    AbilityKind::Spell,
                    &mut ctx,
                );
                let trigger = TriggerDefinition::new(TriggerMode::Exerted)
                    .valid_card(TargetFilter::SelfRef)
                    .trigger_zones(vec![Zone::Battlefield])
                    .execute(effect_def)
                    .description(line.to_string());
                result.triggers.push(trigger);
            }
            i += 1;
            continue;
        }
        // CR 701.43d: Variant with card name — "You may exert {Name} as {he/she/it/they} attacks"
        if nom_on_lower(&line, &lower, |i| value((), tag("you may exert ")).parse(i)).is_some()
            && scan_contains(&lower, "as ")
            && scan_contains(&lower, "attacks")
        {
            if let Some((_, effect_text)) = split_once_on_lower(&line, &lower, ". when you do, ") {
                ctx.subject = None;
                ctx.actor = None;
                let effect_def = parse_effect_chain_with_context(
                    effect_text.trim(),
                    AbilityKind::Spell,
                    &mut ctx,
                );
                let trigger = TriggerDefinition::new(TriggerMode::Exerted)
                    .valid_card(TargetFilter::SelfRef)
                    .trigger_zones(vec![Zone::Battlefield])
                    .execute(effect_def)
                    .description(line.to_string());
                result.triggers.push(trigger);
            }
            i += 1;
            continue;
        }
        // CR 701.43d: Conditional exert — "If [creature] hasn't been exerted this turn, you may exert it"
        if nom_on_lower(&line, &lower, |i| value((), tag("if ")).parse(i)).is_some()
            && scan_contains(&lower, "you may exert")
            && scan_contains(&lower, "attacks")
        {
            if let Some((_, effect_text)) = split_once_on_lower(&line, &lower, ". when you do, ") {
                ctx.subject = None;
                ctx.actor = None;
                let effect_def = parse_effect_chain_with_context(
                    effect_text.trim(),
                    AbilityKind::Spell,
                    &mut ctx,
                );
                let trigger = TriggerDefinition::new(TriggerMode::Exerted)
                    .valid_card(TargetFilter::SelfRef)
                    .trigger_zones(vec![Zone::Battlefield])
                    .execute(effect_def)
                    .description(line.to_string());
                result.triggers.push(trigger);
            }
            i += 1;
            continue;
        }

        // Priority 6c-defiler: "As an additional cost to cast [color] permanent spells,
        // you may pay N life. Those spells cost {C} less to cast if you paid life this way."
        // This is a static ability on the permanent, not a self-cost for this spell.
        if is_defiler_cost_pattern(&lower) {
            if let Some((static_def, consumes_next_line)) =
                parse_defiler_cost_reduction(&lower, i + 1 < lines.len(), || {
                    lines.get(i + 1).map(|l| l.to_lowercase())
                })
            {
                result.statics.push(static_def);
                i += if consumes_next_line { 2 } else { 1 };
                continue;
            }
        }

        // Priority 6c-altcost: CR 118.9 — "You may pay X rather than pay the mana
        // cost for [filter] spells you cast." Mana-cost-alternative-grant static
        // (Rooftop Storm, Fist of Suns, Jodah). Must run before Priority 7
        // because `is_static_pattern` does not classify this shape, so the line
        // would otherwise fall through to the imperative parser as
        // Effect::PayCost.
        if is_spells_alternative_cost_pattern(&lower) {
            if let Some(static_def) = parse_spells_alternative_cost(&line) {
                result.statics.push(static_def);
                i += 1;
                continue;
            }
        }

        // Priority 6d: Compound "[~] enters tapped and doesn't untap during your
        // untap step." carries TWO independent rules in one sentence — an
        // ETB-tapped replacement (CR 614.1c) and a CantUntap static (CR 502.3).
        // The "doesn't untap" substring makes Priority 7's `is_static_pattern`
        // fire and consume the line, dropping the ETB-tapped half. Decompose so
        // both parsers run.
        // Corpus: Traxos, Scourge of Kroog; Grimgrin, Corpse-Born; Leviathan.
        if is_enters_tapped_cant_untap_compound(&lower) {
            let mut consumed = false;
            if let Some(rep_def) = parse_replacement_line(&line, card_name) {
                result.replacements.push(rep_def);
                consumed = true;
            }
            let defs = parse_static_line_with_graveyard_keyword_continuation(&static_line);
            if !defs.is_empty() {
                result.statics.extend(defs);
                consumed = true;
            }
            if consumed {
                i += 1;
                continue;
            }
        }

        if let Some((option, trigger)) = parse_flash_cleanup_sacrifice_casting_option(&line) {
            result.casting_options.push(option);
            result.triggers.push(trigger);
            i += 1;
            continue;
        }

        // Priority 7: Static/continuous patterns
        // CR 611.2a + CR 611.3a: On permanents, "creatures you control get +1/+1"
        // is a static ability (CR 611.3a). On instants/sorceries, lines with an
        // explicit duration ("until end of turn", "this turn") are one-shot
        // continuous effects from spell resolution (CR 611.2a) and must reach the
        // effect parser at Priority 9. Damage-verb lines are also deferred because
        // parse_effect_chain handles embedded statics via split_clause_sequence.
        if is_static_pattern(&lower) {
            // CR 614.1c / CR 707.9: Lines that are both static-shaped (e.g.
            // trailing "doesn't untap during…" from a reflexive "When you do"
            // clause) and a copy-replacement ("enter as a copy of") must route
            // to the replacement parser first — Wall of Stolen Identity class.
            // The copy-verb gate keeps static / prevent lines (Anthem of Rakdos,
            // Pollen Lullaby, Subdue, Mikey & Don Party Planners) out of the
            // replacement parsers; the legacy `as long as` precondition still
            // routes the duration-gated replacement fallback.
            if find_copy_verb_present(&lower) {
                if let Some(rep_defs) = parse_replacement_sentence_sequence(&line, card_name) {
                    result.replacements.extend(rep_defs);
                    i += 1;
                    continue;
                }
                if let Some(rep_def) = parse_replacement_line(&line, card_name) {
                    result.replacements.push(rep_def);
                    i += 1;
                    continue;
                }
            } else if lower_starts_with(&lower, "as long as ") && is_replacement_pattern(&lower) {
                if let Some(rep_def) = parse_replacement_line(&line, card_name) {
                    result.replacements.push(rep_def);
                    i += 1;
                    continue;
                }
            }
            // Guard: ability-word-prefixed trigger lines (e.g., "Flurry — Whenever...")
            // handled above at Priority 6b. The check below is kept as a defensive
            // guard for any edge cases that reach Priority 7.
            let is_ability_word_trigger = strip_ability_word(&line).is_some_and(|stripped| {
                let sl = stripped.to_lowercase();
                has_trigger_prefix(&sl)
            });
            let defer_to_effect_parser =
                is_ability_word_trigger || (is_spell && should_defer_spell_to_effect(&lower));
            if !defer_to_effect_parser {
                // B7: Ability-word-prefixed static lines — strip prefix and attach condition.
                // Must happen here (Priority 7) because Priority 9 (spell catch-all) would
                // otherwise consume the line before Priority 14 for instants/sorceries.
                if let Some((aw_name, effect_text)) = strip_ability_word_with_name(&line) {
                    let effect_static = normalize_self_refs_for_static(&effect_text, card_name);
                    let mut defs =
                        parse_static_line_with_graveyard_keyword_continuation(&effect_static);
                    if !defs.is_empty() {
                        if let Some(cond) = ability_word_to_condition(&aw_name) {
                            for def in &mut defs {
                                if def.condition.is_none() {
                                    def.condition = Some(cond.clone());
                                }
                            }
                        }
                        for def in &mut defs {
                            def.description = Some(line.to_string());
                        }
                        result.statics.extend(defs);
                        i += 1;
                        continue;
                    }
                }
                // B20: Handle compound "can't win/lose" lines by splitting
                // at " and " so both CantWinTheGame and CantLoseTheGame emit.
                // CR 104.3a / CR 104.3b: Both restrictions must be independent statics.
                if is_cant_win_lose_compound(&lower) {
                    for clause in static_line.split(" and ") {
                        let trimmed = clause.trim().trim_end_matches('.');
                        if !trimmed.is_empty() {
                            let clause_dot = format!("{trimmed}.");
                            result.statics.extend(
                                parse_static_line_with_graveyard_keyword_continuation(&clause_dot),
                            );
                        }
                    }
                    i += 1;
                    continue;
                }
                // Compound clause: casting time restriction + per-turn limit joined by " and "
                // E.g., Fires of Invention: "You can cast spells only during your turn and
                // you can cast no more than two spells each turn."
                // CR 117.1a + CR 604.1: Both restrictions are independent statics.
                if is_compound_turn_limit(&lower) {
                    for clause in static_line.split(" and ") {
                        let trimmed = clause.trim().trim_end_matches('.');
                        if !trimmed.is_empty() {
                            let clause_dot = format!("{trimmed}.");
                            result.statics.extend(
                                parse_static_line_with_graveyard_keyword_continuation(&clause_dot),
                            );
                        }
                    }
                    i += 1;
                    continue;
                }
                // Compound detection (CR 602.5 can't-be-activated, cross-mode conjunctions,
                // "attacks or blocks each combat if able" → MustAttack + MustBlock, life-total
                // locks, etc.) is already owned by `parse_static_line_multi`, which the wrapper
                // delegates to.
                let defs = parse_static_line_with_graveyard_keyword_continuation(&static_line);
                if !defs.is_empty() {
                    result.statics.extend(defs);
                    i += 1;
                    continue;
                }
            }
        }

        // CR 615 + CR 105.1: "Prevent all damage that sources of the color of your choice
        // would deal this turn." → Choose(Color) → PreventDamage chain.
        // Must run before Priority 8 (replacement) to avoid being caught as a passive shield.
        if is_spell
            && scan_contains(&lower, "prevent")
            && scan_contains(&lower, "damage")
            && scan_contains(&lower, "color of your choice")
        {
            use crate::types::ability::{
                ChoiceType, FilterProp, PreventionAmount, PreventionScope,
            };
            // CR 615 + CR 105.1: Build a source filter using IsChosenColor —
            // at resolution time the resolver reads ChosenAttribute::Color from
            // the source object and converts to a concrete HasColor filter.
            let mut source_filter = TypedFilter::default();
            source_filter.properties.push(FilterProp::IsChosenColor);
            let def = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Choose {
                    choice_type: ChoiceType::color(),
                    persist: true,
                },
            )
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PreventDamage {
                    amount: PreventionAmount::All,
                    amount_dynamic: None,
                    target: TargetFilter::Any,
                    scope: PreventionScope::AllDamage,
                    damage_source_filter: Some(TargetFilter::Typed(source_filter)),
                },
            ))
            .description(line.to_string());
            result.abilities.push(def);
            i += 1;
            continue;
        }

        // Instant/sorcery prevention text creates a resolving spell effect,
        // not a standing replacement definition. Let the effect-chain parser
        // preserve any preceding clauses ("You gain 1 life for each ...")
        // before the replacement classifier sees the prevention marker.
        //
        // CR 614.15: Exclude ability-word self-replacement lines whose body is
        // "if <cond>, instead <effect> ... the damage can't be prevented."
        // (Arrow Storm, Lightning Surge). For these, the prevention clause is a
        // sub-effect of the conditional override, not the line's primary effect —
        // routing the whole line through `parse_effect_chain_with_context` here
        // would swallow the leading conditional and drop the `instead` composition.
        // They must reach Priority 9, where `strip_instead_clause` extracts the
        // condition and the existing block composes a `ConditionInstead` sub-ability.
        let prevention_effect_text = strip_ability_word_with_name(&line)
            .map(|(_, effect)| effect)
            .unwrap_or_else(|| line.clone());
        if is_spell
            && scan_contains(&lower, "prevent")
            && scan_contains(&lower, "damage")
            && !is_instead_replacement_line(&prevention_effect_text)
        {
            ctx.subject = None;
            ctx.actor = None;
            let def = parse_effect_chain_with_context(&line, AbilityKind::Spell, &mut ctx);
            if !has_unimplemented(&def) {
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 8: Replacement patterns
        if is_replacement_pattern(&lower) {
            // CR 614.1c: Effects that read "[This permanent] enters with ...",
            // "As [this permanent] enters ...", or "[This permanent] enters as ..."
            // are replacement effects.
            // CR 614.12: Some replacement effects modify how a permanent enters the battlefield.
            // A single Oracle paragraph can contain multiple independent ETB
            // replacement sentences. Parse each replacement sentence instead of
            // letting the first successful parser drop sibling modifiers.
            if let Some(rep_defs) = parse_replacement_sentence_sequence(&line, card_name) {
                result.replacements.extend(rep_defs);
                i += 1;
                continue;
            }
            if let Some(rep_def) = parse_replacement_line(&line, card_name) {
                result.replacements.push(rep_def);
                i += 1;
                continue;
            }
        }

        if let Some(def) = try_parse_opening_hand_reveal_delayed_trigger(&line, &lower) {
            result.abilities.push(def);
            i += 1;
            continue;
        }

        // CR 103.5b: "Any time you could mulligan and ~ is in your hand, you may ..."
        // (Serum Powder, No-Regrets Egret). Mulligan-time abilities never resolve
        // through the stack — see `AbilityKind::Mulligan` and the guard in
        // `effects/mod.rs`. Runtime dispatch lives in `mulligan.rs`.
        if let Some(def) = try_parse_mulligan_time_ability(&line, &lower) {
            result.abilities.push(def);
            i += 1;
            continue;
        }

        // Priority 8c: "If this card is in your opening hand, you may begin the game with it on the battlefield"
        // CR 103.6: The Leyline rule — opt-in at game start, never compelled.
        // `parse_begin_game_clause` is the sole detector — the parser IS the
        // detector; there is no string pre-filter. It also captures the
        // optional "with [counters] on it" clause and the optional "If you do,
        // [effect]" dependent sub-ability.
        if let Some(def) = parse_begin_game_clause(&line, &lower) {
            result.abilities.push(def);
            i += 1;
            continue;
        }

        // Priority 8b: "As an additional cost to cast this spell"
        if lower_starts_with(&lower, "as an additional cost") {
            result.additional_cost = parse_additional_cost_line(&lower, &line);
            i += 1;
            continue;
        }

        // Priority 8c-strive: Skip strive lines (cost already extracted in pre-parse above).
        // Must run before Priority 9 (spell imperative catch-all) which would otherwise
        // consume the entire "Strive — This spell costs..." line as an unimplemented ability.
        if result.strive_cost.is_some() {
            if let Some(effect_text) = strip_ability_word(&line) {
                let effect_lower = effect_text.to_lowercase();
                if lower_starts_with(&effect_lower, "this spell costs ")
                    && effect_lower.contains("more to cast for each target beyond the first")
                {
                    i += 1;
                    continue;
                }
            }
        }

        // CR 601.3: "Cast this spell only [condition]" — applies to any card type, not just instants/sorceries.
        if let Some(restrictions) = parse_casting_restriction_line(&line) {
            result.casting_restrictions.extend(restrictions);
            i += 1;
            continue;
        }

        if let Some(option) = parse_spell_casting_option_line(&line, card_name) {
            result.casting_options.push(option);
            i += 1;
            continue;
        }

        // CR 706: Die roll table — "Roll a dN" followed by "min—max | effect" lines.
        // Consumes the header + all table lines and produces a single RollDie ability.
        if let Some((def, next_i)) = try_parse_die_roll_table(
            &lines,
            i,
            &line,
            if is_spell {
                AbilityKind::Spell
            } else {
                AbilityKind::Activated
            },
        ) {
            result.abilities.push(def);
            i = next_i;
            continue;
        }

        // CR 702.62a: Suspend N—{cost} — parse count and cost from Oracle text.
        // Must run before the spell imperative catch-all (priority 9) so the line
        // is intercepted as a keyword, not parsed as an Unimplemented ability.
        // Spells (instants/sorceries) with Suspend would otherwise be caught by
        // the is_spell branch and produce an Unimplemented effect.
        if lower_starts_with(&lower, "suspend ") {
            if let Some(kw) = parse_keyword_from_oracle(&lower) {
                result.extracted_keywords.push(kw);
                i += 1;
                continue;
            }
        }

        // Digital-only Specialize: "specialize {cost}" — MTGJSON may omit the keyword
        // when it appears as a standalone rules line; intercept before dispatch fallback.
        if lower_starts_with(&lower, "specialize ") {
            if let Some(kw) = parse_keyword_from_oracle(&lower) {
                result.extracted_keywords.push(kw);
                i += 1;
                continue;
            }
        }

        // Harmonize {cost} — parse mana cost from Oracle text.
        // Must run before the spell imperative catch-all (priority 9) so the line
        // is intercepted as a keyword, not parsed as an effect.
        // MTGJSON keywords array only says "Harmonize" (no cost), so we extract cost here.
        // Format: "Harmonize {cost} (reminder text)" — space-separated.
        // Note: When MTGJSON provides "Harmonize" in keywords, extract_keyword_line at
        // priority 1b already handles this. This is a fallback for test/edge cases.
        if lower_starts_with(&lower, "harmonize ") {
            if let Some(harmonize_kw) = parse_harmonize_keyword(&line) {
                result.extracted_keywords.push(harmonize_kw);
                i += 1;
                continue;
            }
        }

        // Priority 8f: Kicker / Multikicker / Replicate cost lines — must run BEFORE Priority 9
        // (spell catch-all) so these keyword declarations on spell cards don't become Unimplemented.
        // We cannot use is_keyword_cost_line here because it would also catch "escape", "flashback",
        // etc. whose specific em-dash parsers run between Priority 9 and Priority 13.
        // Note: "mayhem" IS in is_keyword_cost_line and is handled at Priority 1b via MTGJSON
        // keywords when present; this guard catches it when keywords[] is empty.
        if alt((
            tag::<_, _, OracleError<'_>>("kicker"),
            tag("multikicker"),
            tag("replicate"),
            tag("mayhem"),
        ))
        .parse(lower.as_str())
        .is_ok()
        {
            if let Some(cost) = parse_kicker_additional_cost_line(&line, &lower) {
                merge_kicker_additional_cost(&mut result.additional_cost, cost);
            }
            if let Some(kw) = parse_keyword_from_oracle(&lower) {
                result.extracted_keywords.push(kw);
            }
            i += 1;
            continue;
        }

        // CR 702.34a: Flashback em-dash form — "Flashback—{cost}", "Flashback—Tap N
        // creatures...", or compound "Flashback—{mana}, Pay N life." The comma in
        // compound costs prevents `extract_keyword_line` (priority 1b) from
        // recognising the line as a keyword-only line, and Priority 9 would
        // otherwise route it to the spell-effect catch-all and produce
        // `Unimplemented`. Intercept it here, before the spell catch-all, and
        // delegate to `parse_keyword_from_oracle`'s em-dash dispatcher.
        if lower_starts_with(&lower, "flashback") && line.contains('\u{2014}') {
            // Strip trailing punctuation so the em-dash dispatcher sees a clean
            // cost string. Reminder text was already removed by `strip_reminder_text`
            // upstream, but the trailing period from "Pay 3 life." remains.
            let lower_clean = lower.trim_end_matches('.').trim();
            if let Some(kw) = parse_keyword_from_oracle(lower_clean) {
                result.extracted_keywords.push(kw);
                i += 1;
                continue;
            }
        }

        // CR 702.27a: Buyback em-dash form — "Buyback—Sacrifice a land." (Constant
        // Mists) etc. MTGJSON omits the Buyback keyword when the cost is non-mana,
        // so `extract_keyword_line` bails and the line would otherwise fall through
        // to the spell-effect catch-all and produce `Unimplemented`. Intercept here
        // before the spell catch-all, mirroring the Flashback em-dash intercept above.
        // structural: not dispatch — em-dash char presence gates the cost sub-parser,
        // which uses nom combinators in `parse_buyback_cost` / `parse_oracle_cost`.
        if lower_starts_with(&lower, "buyback") && line.contains('\u{2014}') {
            let lower_clean = lower.trim_end_matches('.').trim();
            if let Some(kw) = parse_keyword_from_oracle(lower_clean) {
                result.extracted_keywords.push(kw);
                i += 1;
                continue;
            }
        }

        // CR 702.120a: Escalate is a keyword additional-cost declaration on
        // modal spells. Intercept before the instant/sorcery effect catch-all
        // so "Escalate—Tap an untapped creature you control." is extracted as
        // keyword data instead of an Unimplemented spell ability.
        if tag::<_, _, OracleError<'_>>("escalate")
            .parse(lower.as_str())
            .is_ok()
        {
            let lower_clean = lower.trim_end_matches('.').trim();
            if let Some(kw) = parse_keyword_from_oracle(lower_clean) {
                result.extracted_keywords.push(kw);
                i += 1;
                continue;
            }
        }

        // Priority 9: Imperative verb for instants/sorceries
        if is_spell {
            // B7: Strip ability-word prefix and attach condition for spell effects.
            let mut spell_body_lines = Vec::new();
            let mut spell_description_lines = Vec::new();
            let Some(prepared_line) = prepare_spell_resolution_line(&line) else {
                i += 1;
                continue;
            };
            let aw_condition = prepared_line.ability_word_condition.clone();
            let mut spell_min_x_value = min_x_value.max(prepared_line.min_x_value);
            spell_body_lines.push(prepared_line.effect_text.clone());
            spell_description_lines.push(prepared_line.line);

            let mut next_i = i + 1;
            while next_i < lines.len() {
                if level_consumed.contains(&next_i)
                    || preparsed_consumed.contains(&next_i)
                    || spacecraft_consumed.contains(&next_i)
                    || parse_oracle_block(&lines, next_i).is_some()
                {
                    break;
                }

                let Some(next_prepared) = prepare_spell_resolution_line(lines[next_i]) else {
                    let next_line = strip_reminder_text(lines[next_i].trim());
                    let next_min_x_value = x_annotation_min_value(&next_line);
                    let next_stripped = strip_x_cant_be_zero_suffix(&next_line);
                    if next_min_x_value > 0 && next_stripped.is_empty() {
                        spell_min_x_value = spell_min_x_value.max(next_min_x_value);
                        next_i += 1;
                    }
                    break;
                };

                if next_prepared.has_ability_word_prefix
                    || starts_with_until_duration(&next_prepared.effect_text)
                    || ends_with_quoted_activated_ability(&prepared_line.effect_text)
                    || is_self_exile_cleanup_line(&next_prepared.effect_text, card_name)
                    || is_standalone_spell_keyword_action_line(&prepared_line.effect_text)
                    || !is_spell_resolution_instruction_line(
                        &next_prepared,
                        card_name,
                        mtgjson_keyword_names,
                        &result,
                        &mut ctx,
                    )
                {
                    break;
                }

                spell_body_lines.push(next_prepared.effect_text);
                spell_min_x_value = spell_min_x_value.max(next_prepared.min_x_value);
                spell_description_lines.push(next_prepared.line);
                next_i += 1;
            }

            let effect_line = spell_body_lines.join(" ");
            let description = spell_description_lines.join("\n");
            // CR 608.2c: Pre-strip "instead if [condition]" or trailing "instead"
            // from the effect text before chain parsing. This allows
            // strip_mana_value_conditional inside the chain parser to handle
            // mid-position MV conditions (e.g., "if it has mana value 4 or less")
            // that precede "instead if [ability word condition]".
            let (effect_line_clean, instead_condition, is_instead) =
                strip_instead_clause(&effect_line, &mut ctx);
            let parse_line = if is_instead {
                effect_line_clean.as_str()
            } else {
                effect_line.as_str()
            };
            ctx.subject = None;
            ctx.actor = None;
            // CR 701.38 (Council's-dilemma vote) + CR 101.4 (APNAP for
            // Battlebond friend-or-foe — no dedicated CR section). Both
            // shapes produce a single Vote effect with per-choice sub-effects. The
            // dispatcher in `parse_vote_block` recognises the entire opener +
            // per-class clauses and returns a synthesised AbilityDefinition;
            // when it matches we use that directly rather than chunk-splitting
            // the text through `parse_effect_chain_with_context`, which would
            // mis-parse `"For each player, choose friend or foe."` as an
            // Unimplemented chunk and leave the per-class clauses to chain as
            // ordinary sequential effects.
            // CR 700.3: Pile-separation primitive (Make an Example and the
            // Liliana −6 / Fact-or-Fiction family). The dispatcher consumes
            // the entire three-sentence block as a single effect — chain
            // parsing would mis-parse "Each opponent separates ..." as
            // Unimplemented{separate} followed by a stray Sacrifice
            // sub-ability with a `repeat_for` rider.
            let mut def = if let Some(pile_def) =
                crate::parser::oracle_separate_piles::parse_separate_into_piles(
                    parse_line,
                    AbilityKind::Spell,
                ) {
                pile_def
            } else if let Some(vote_def) =
                crate::parser::oracle_vote::parse_vote_block(parse_line, AbilityKind::Spell)
            {
                vote_def
            } else {
                parse_effect_chain_with_context(parse_line, AbilityKind::Spell, &mut ctx)
            };
            def.min_x_value = spell_min_x_value;
            def.description = Some(description);
            // CR 608.2c: Compose ability word condition with chain-extracted condition.
            // When both exist (e.g., Revolt + MV ≤ 4), compose through
            // `merge_ability_condition` which dedupes structurally-equal conditions
            // (e.g., "Delirium —" ability word + literal "if there are four or more
            // card types..." phrase both emit the same `QuantityCheck`) and flattens
            // nested `And` trees.
            // Ability-word condition (if any) is the "existing" baseline —
            // the chain-extracted condition is merged onto it, preserving the
            // historical `[ability_word, chain]` ordering when both are distinct.
            let chain = def.condition.take();
            def.condition = match (
                ability_word_to_ability_condition(&aw_condition, &mut ctx),
                chain,
            ) {
                (Some(aw), Some(chain)) => Some(merge_ability_condition(Some(aw), chain)),
                (Some(aw), None) => Some(aw),
                (None, chain) => chain,
            };
            if let Some(instead_condition) = instead_condition {
                def.condition = Some(merge_ability_condition(
                    def.condition.take(),
                    instead_condition,
                ));
            }
            i = next_i;
            // CR 706: If the parsed chain ends with "roll a dN", consume
            // subsequent d20 table lines and attach them as die result branches.
            if has_roll_die_pattern(&lower) {
                i = attach_die_result_branches_to_chain(&mut def, &lines, i);
            }
            // CR 608.2c: Cross-line "instead" replacement — when a conditional line
            // replaces the entire preceding ability, compose them so the engine resolves
            // the binary choice correctly. The "instead" sub has the condition; the base
            // ability becomes the fallback when the condition is not met.
            if is_instead || is_instead_replacement_line(&effect_line) {
                if let Some(condition) = def.condition.take() {
                    if let Some(mut base) = result.abilities.pop() {
                        // Save the base ability's continuation chain in else_ability
                        // so the engine can run it when the condition is NOT met.
                        def.condition = Some(AbilityCondition::ConditionInstead {
                            inner: Box::new(condition),
                        });
                        def.else_ability = base.sub_ability.take();
                        base.sub_ability = Some(Box::new(def));
                        result.abilities.push(base);
                        continue;
                    }
                    // No previous ability to compose with — restore condition and push standalone.
                    def.condition = Some(condition);
                }
            }
            result.abilities.push(def);
            continue;
        }

        // Priority 12: Roman numeral chapters (saga) — skip
        if is_saga_chapter(&lower) {
            i += 1;
            continue;
        }

        // "The flashback cost is equal to its mana cost" → extract Flashback keyword
        if is_flashback_equal_mana_cost(&lower) {
            if parsed_result_recently_granted_flashback(&result) {
                i += 1;
                continue;
            }
            result.extracted_keywords.push(Keyword::Flashback(
                crate::types::keywords::FlashbackCost::Mana(
                    crate::types::mana::ManaCost::SelfManaCost,
                ),
            ));
            i += 1;
            continue;
        }

        // CR 702.49d: Commander ninjutsu is not in MTGJSON keywords — extract explicitly.
        if lower_starts_with(&lower, "commander ninjutsu ") {
            if let Some(kw) = parse_keyword_from_oracle(&lower) {
                result.extracted_keywords.push(kw);
                i += 1;
                continue;
            }
        }

        // CR 702.138: Escape — parse cost and exile count from Oracle text.
        // Must run before is_keyword_cost_line so the em-dash format is intercepted.
        if lower_starts_with(&lower, "escape") && line.contains('\u{2014}') {
            if let Some(escape_kw) = parse_escape_keyword(&line) {
                result.extracted_keywords.push(escape_kw);
                i += 1;
                continue;
            }
        }

        // CR 702.24: Cumulative upkeep — parse cost from Oracle text.
        // Must run before is_keyword_cost_line so the line is not silently skipped.
        // Format: "Cumulative upkeep—[cost]" or "Cumulative upkeep {mana}" (space-separated).
        if lower_starts_with(&lower, "cumulative upkeep") {
            if let Some(kw) = parse_cumulative_upkeep_keyword(&line) {
                result.extracted_keywords.push(kw);
                i += 1;
                continue;
            }
        }

        // Priority 13: Keyword cost lines — extract keyword if parseable, then skip.
        // MTGJSON provides keyword names (e.g. "Morph") but not parameterized forms.
        // The Oracle text has the full form (e.g. "Morph {2}{B}{G}{U}") which we extract here.
        if is_keyword_cost_line(&lower) {
            if let Some(kw) = parse_keyword_from_oracle(&lower) {
                result.extracted_keywords.push(kw);
            }
            i += 1;
            continue;
        }

        // Priority 13b: Kicker/Multikicker — skip (handled by keywords)
        if alt((tag::<_, _, OracleError<'_>>("kicker"), tag("multikicker")))
            .parse(lower.as_str())
            .is_ok()
        {
            i += 1;
            continue;
        }

        // Priority 13c: Vehicle tier lines "N+ | keyword(s)" — skip (conditional stat grant)
        if is_vehicle_tier_line(&lower) {
            i += 1;
            continue;
        }

        // Priority 13d: "Activate only..." constraint — skip
        if lower_starts_with(&lower, "activate ") {
            i += 1;
            continue;
        }

        // Priority 13e: "X can't be 0." — casting constraint annotation, not an ability.
        // These appear as standalone lines on X-cost spells. Earlier empty-line
        // handling stamps the previous ability's `min_x_value`; this guard is a
        // defensive fallback for already-normalized forms.
        if lower.trim_end_matches('.') == "x can't be 0" {
            if let Some(previous) = result.abilities.last_mut() {
                previous.min_x_value = previous.min_x_value.max(1);
            }
            i += 1;
            continue;
        }

        // Priority 14: Ability word — strip prefix and re-classify effect.
        // B7: Known ability words (Threshold, Metalcraft, Delirium, Spell mastery, Revolt)
        // are mapped to typed conditions and attached to the resulting definition.
        if let Some((aw_name, effect_text)) = strip_ability_word_with_name(&line) {
            let aw_condition = ability_word_to_condition(&aw_name);
            let effect_lower = effect_text.to_lowercase();

            // Try as trigger
            if has_trigger_prefix(&effect_lower) {
                // CR 707.9a: Thread the running trigger count as the base index.
                let mut triggers = parse_trigger_lines_at_index(
                    &effect_text,
                    card_name,
                    Some(result.triggers.len()),
                    &mut ctx,
                );
                i += 1;
                // CR 706: Consume subsequent d20 table lines for triggered die rolls.
                if has_roll_die_pattern(&effect_lower) {
                    if let Some(last) = triggers.last_mut() {
                        if let Some(ref mut execute) = last.execute {
                            i = attach_die_result_branches_to_chain(execute, &lines, i);
                        }
                    }
                }
                result.triggers.extend(triggers);
                continue;
            }
            // Try as keyword — the ability-word prefix ("Void Shields —") was
            // stripped, so the remainder may be a keyword line that Priority 1b
            // missed because it ran on the unprefixed original line.
            if let Some(kw) = parse_keyword_from_oracle(&effect_lower) {
                if !matches!(kw, Keyword::Unknown(_)) {
                    result.extracted_keywords.push(kw);
                    i += 1;
                    continue;
                }
            }
            // Try as static
            if is_static_pattern(&effect_lower) {
                let effect_static = normalize_self_refs_for_static(&effect_text, card_name);
                let mut defs =
                    parse_static_line_with_graveyard_keyword_continuation(&effect_static);
                if !defs.is_empty() {
                    if let Some(cond) = aw_condition.clone() {
                        for def in &mut defs {
                            if def.condition.is_none() {
                                def.condition = Some(cond.clone());
                            }
                        }
                    }
                    result.statics.extend(defs);
                    i += 1;
                    continue;
                }
            }
            // Try as effect
            ctx.subject = None;
            ctx.actor = None;
            let def = parse_effect_chain_with_context(&effect_text, AbilityKind::Spell, &mut ctx);
            if !has_unimplemented(&def) {
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Leftover permanent text can still be a valid static even when classifier
        // heuristics miss it. Try the actual static parser before falling through
        // to generic dispatch/unimplemented categorization.
        let static_line = normalize_self_refs_for_static(&line, card_name);
        let defs = parse_static_line_with_graveyard_keyword_continuation(&static_line);
        if !defs.is_empty() {
            result.statics.extend(defs);
            i += 1;
            continue;
        }

        // Priority 14a: Nom dispatch — try effect, trigger, static, and replacement
        // sub-parsers. If any succeeds, use the result directly.
        let nom_effect = dispatch_line_nom(&line, card_name, ctx.host_self_reference.clone());
        if !matches!(nom_effect, Effect::Unimplemented { .. }) {
            result
                .abilities
                .push(AbilityDefinition::new(AbilityKind::Spell, nom_effect));
            i += 1;
            continue;
        }

        // Priority 15: Final fallback — wrap as Unimplemented with diagnostic trace.
        result
            .abilities
            .push(make_unimplemented_with_effect(&line, nom_effect));
        i += 1;
    }

    reconcile_self_chosen_type_statics(&mut result, types);

    // Architectural rule: the parser must never silently discard Oracle
    // text. Run the swallow audit against the parsed result so any unrep-
    // resented clause surfaces as a parse_warning instead of disappearing
    // (Phase 1: observability only — see swallow_check.rs for detector
    // catalog and Phase 2 demotion plan).
    let mut swallow_diags = Vec::new();
    super::swallow_check::check_swallowed_clauses(oracle_text, &result, &mut swallow_diags);
    for d in swallow_diags {
        ctx.push_diagnostic(d);
    }

    parsed_abilities_to_doc_ir(result, oracle_text, card_name, &mut ctx)
}

fn activation_zone_from_self_cost(cost: &AbilityCost) -> Option<Zone> {
    match cost {
        AbilityCost::Discard { self_ref: true, .. } => Some(Zone::Hand),
        AbilityCost::Exile {
            filter: Some(TargetFilter::SelfRef),
            zone: Some(zone),
            ..
        } => Some(*zone),
        AbilityCost::Composite { costs } => costs.iter().find_map(activation_zone_from_self_cost),
        _ => None,
    }
}

/// Effect-side companion to `activation_zone_from_self_cost`.
///
/// CR 113.6m + CR 602.1: an activated ability whose *effect* moves the object
/// it's printed on out of a particular non-battlefield zone (e.g. "Put this
/// card from your hand onto the battlefield") functions only from that zone.
/// The cost-based derivation cannot see this because the zone lives in the
/// effect, not the cost. This walks the parsed effect chain for a self-
/// `ChangeZone` whose `origin` is a non-battlefield zone and `destination` is
/// the battlefield, returning that origin as the activation zone.
fn activation_zone_from_self_effect(def: &AbilityDefinition) -> Option<Zone> {
    if let Effect::ChangeZone {
        origin: Some(origin),
        destination: Zone::Battlefield,
        target: TargetFilter::SelfRef,
        ..
    } = *def.effect
    {
        if origin != Zone::Battlefield {
            return Some(origin);
        }
    }
    def.sub_ability
        .as_deref()
        .and_then(activation_zone_from_self_effect)
}

/// CR 608.2k: Source zone of a non-self `AbilityCost::Exile` component
/// ("Exile a nonland card from your hand"), if present. Effect-side companion
/// to `activation_zone_from_self_cost`: returns `None` for a self-ref exile
/// (Scavenge), which is auto-paid and never back-referenced as a cost-paid
/// object. Recurses into `Composite`.
fn non_self_exile_cost_zone(cost: &AbilityCost) -> Option<Zone> {
    match cost {
        AbilityCost::Exile {
            filter: Some(TargetFilter::SelfRef),
            ..
        } => None,
        AbilityCost::Exile {
            zone: Some(zone @ (Zone::Hand | Zone::Graveyard)),
            ..
        } => Some(*zone),
        AbilityCost::Composite { costs } => costs.iter().find_map(non_self_exile_cost_zone),
        _ => None,
    }
}

fn parse_activated_ability_definition(
    cost_text: &str,
    effect_text: &str,
    description: &str,
    card_name: &str,
    ctx: &mut ParseContext,
) -> (AbilityDefinition, String) {
    let (effect_text, constraints) = strip_activated_constraints(effect_text);
    let normalized_cost_text = normalize_self_refs_for_static(cost_text, card_name);
    let cost = parse_oracle_cost(&normalized_cost_text);

    // CR 608.2k: expose this ability's exile-cost source zone so the effect
    // parser can disambiguate "the exiled card" as a cost-paid-object
    // reference. Restored after the effect parse — no leak to sibling abilities.
    let prev_exile_zone = ctx.current_ability_exile_cost_zone.take();
    ctx.current_ability_exile_cost_zone = non_self_exile_cost_zone(&cost);

    // Retry with `~` normalization if the first pass left an Unimplemented node
    // or emitted a target-fallback warning.
    let mut def = parse_activated_with_self_ref_fallback(&effect_text, card_name, ctx);

    ctx.current_ability_exile_cost_zone = prev_exile_zone;
    normalize_activated_mana_instead_delta(&mut def);
    if def.activation_zone.is_none() {
        def.activation_zone = activation_zone_from_self_cost(&cost);
    }
    // CR 113.6m: fall back to the effect-side derivation — an ability whose
    // effect moves the source out of a non-battlefield zone functions only
    // from that zone. Cost-based derivation keeps priority.
    if def.activation_zone.is_none() {
        def.activation_zone = activation_zone_from_self_effect(&def);
    }
    def.cost = Some(cost);
    def.description = Some(description.to_string());
    if constraints.sorcery_speed() {
        def.sorcery_speed = true;
    }
    if !constraints.restrictions.is_empty() {
        def.activation_restrictions = constraints.restrictions;
    }
    extract_cost_reduction_from_chain(&mut def);
    (def, effect_text)
}

/// Convert a `ParsedAbilities` into an `OracleDocIr` using `PreLowered*` variants.
///
/// Preserves source ordering: abilities, triggers, statics, replacements are pushed
/// in their parsed order. Scalar fields (modal, additional_cost, solve_condition,
/// strive_cost) are pushed as their corresponding `OracleItemIr` variants.
fn parsed_abilities_to_doc_ir(
    result: ParsedAbilities,
    oracle_text: &str,
    card_name: &str,
    ctx: &mut ParseContext,
) -> OracleDocIr {
    let mut items: Vec<OracleItemIr> = Vec::new();
    for def in result.abilities {
        items.push(OracleItemIr::PreLoweredSpell(def));
    }
    for def in result.triggers {
        items.push(OracleItemIr::PreLoweredTrigger(def));
    }
    for def in result.statics {
        items.push(OracleItemIr::PreLoweredStatic(def));
    }
    for def in result.replacements {
        items.push(OracleItemIr::PreLoweredReplacement(def));
    }
    for kw in result.extracted_keywords {
        items.push(OracleItemIr::Keyword(kw));
    }
    if let Some(modal) = result.modal {
        items.push(OracleItemIr::Modal(modal));
    }
    if let Some(cost) = result.additional_cost {
        items.push(OracleItemIr::AdditionalCost(cost));
    }
    for restriction in result.casting_restrictions {
        items.push(OracleItemIr::CastingRestriction(restriction));
    }
    for option in result.casting_options {
        items.push(OracleItemIr::CastingOption(option));
    }
    if let Some(condition) = result.solve_condition {
        items.push(OracleItemIr::SolveCondition(condition));
    }
    if let Some(cost) = result.strive_cost {
        items.push(OracleItemIr::StriveCost(cost));
    }
    OracleDocIr {
        items,
        source_text: oracle_text.to_string(),
        card_name: card_name.to_string(),
        diagnostics: std::mem::take(&mut ctx.diagnostics),
    }
}

/// Parse Oracle text into structured ability definitions.
///
/// This is the public API entry point — a thin wrapper around [`parse_oracle_ir`]
/// (IR production) and [`lower_oracle_ir`] (IR lowering). `parse_oracle_ir`
/// creates a fresh `ParseContext` internally so diagnostics start empty;
/// they flow through `OracleDocIr.diagnostics` → `ParsedAbilities.parse_warnings`.
#[tracing::instrument(
    level = "debug",
    skip(oracle_text, mtgjson_keyword_names, types, subtypes)
)]
pub fn parse_oracle_text(
    oracle_text: &str,
    card_name: &str,
    mtgjson_keyword_names: &[String],
    types: &[String],
    subtypes: &[String],
) -> ParsedAbilities {
    let ir = parse_oracle_ir(
        oracle_text,
        card_name,
        mtgjson_keyword_names,
        types,
        subtypes,
    );
    lower_oracle_ir(&ir)
}

/// Try to parse "Equip {cost}" or "Equip — {cost}" lines.
/// Caller must verify the line starts with "equip" (case-insensitive) before calling.
///
/// CR 702.6a: Equip is the keyword. Distinct from "equipment" (a subtype noun)
/// and "equipped" (the static-grant subject) — both of which begin with the
/// same five letters. The caller's `lower_starts_with("equip")` check matches
/// all three; this function defends with a word-boundary guard so
/// "Equipment you control have equip {0}" (Puresteel Paladin granted-equip
/// pattern) does not slice off the first 5 bytes of "Equipment" and parse the
/// remainder ("ment you control...") as a malformed activated ability cost.
fn try_parse_equip(line: &str) -> Option<AbilityDefinition> {
    let (activation_line, cost_reduction) = split_trailing_self_cost_reduction(line);
    // Caller already verified lower.starts_with("equip") — strip 5-char prefix.
    // "equip" is always ASCII so byte length == char length.
    let rest = activation_line.get("equip".len()..)?;
    // Word-boundary guard: the keyword "equip" must terminate before a
    // non-keyword character. Permitted continuations: whitespace, em-dash,
    // hyphen, `{` (mana cost), or end-of-string. Anything else (e.g. 'm' from
    // "equipment", 'p' from "equipped" — though that's filtered earlier, 'a'
    // from a hypothetical "equipa") is a different word and must not match.
    if let Some(next) = rest.chars().next() {
        if !matches!(next, ' ' | '\t' | '\u{2014}' | '-' | '{' | '.') {
            return None;
        }
    }
    let rest = rest.trim();
    // Strip leading "—" or "- "
    let cost_text = rest
        .strip_prefix('—')
        .or_else(|| rest.strip_prefix('-'))
        .unwrap_or(rest)
        .trim();

    if cost_text.is_empty() {
        return None;
    }

    let (cost_text, constraints) = strip_activated_constraints(cost_text);
    let target = parse_equip_target_filter(&cost_text)?;
    let cost = parse_equip_cost(&cost_text);
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Attach {
            attachment: crate::types::ability::TargetFilter::SelfRef,
            target,
        },
    )
    .cost(cost)
    .description(line.to_string())
    .sorcery_speed();
    if !constraints.restrictions.is_empty() {
        for restriction in constraints.restrictions {
            if !ability.activation_restrictions.contains(&restriction) {
                ability.activation_restrictions.push(restriction);
            }
        }
    }
    ability.cost_reduction = cost_reduction;
    Some(ability)
}

fn parse_equip_target_filter(cost_text: &str) -> Option<TargetFilter> {
    let lower = cost_text.to_ascii_lowercase();
    let Ok((_, descriptor)) =
        nom::sequence::terminated(take_until::<_, _, OracleError<'_>>("{"), tag("{"))
            .parse(lower.as_str())
    else {
        return Some(default_equip_target_filter());
    };
    let descriptor = descriptor.trim();
    if descriptor.is_empty() {
        return Some(default_equip_target_filter());
    }

    if tag::<_, _, OracleError<'_>>("pay")
        .parse(descriptor)
        .is_ok()
    {
        return Some(default_equip_target_filter());
    }

    if alt((
        tag::<_, _, OracleError<'_>>("abilities"),
        tag::<_, _, OracleError<'_>>("costs"),
    ))
    .parse(descriptor)
    .is_ok()
    {
        return None;
    }

    if all_consuming(tag::<_, _, OracleError<'_>>("commander"))
        .parse(descriptor)
        .is_ok()
    {
        return Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(crate::types::ability::ControllerRef::You)
                .properties(vec![crate::types::ability::FilterProp::IsCommander]),
        ));
    }

    let (filter, rest) = super::oracle_target::parse_type_phrase(descriptor);
    if !rest.trim().is_empty() {
        return None;
    }

    equip_target_filter_with_controller(filter)
}

fn equip_target_filter_with_controller(filter: TargetFilter) -> Option<TargetFilter> {
    match filter {
        TargetFilter::Typed(mut typed) => {
            typed.controller = Some(crate::types::ability::ControllerRef::You);
            if !equip_target_has_explicit_attachable_type(&typed) {
                typed
                    .type_filters
                    .insert(0, crate::types::ability::TypeFilter::Creature);
            }
            Some(TargetFilter::Typed(typed))
        }
        TargetFilter::Or { filters } => Some(TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(equip_target_filter_with_controller)
                .collect::<Option<Vec<_>>>()?,
        }),
        _ => None,
    }
}

fn equip_target_has_explicit_attachable_type(typed: &TypedFilter) -> bool {
    typed.type_filters.iter().any(|filter| {
        matches!(
            filter,
            crate::types::ability::TypeFilter::Creature
                | crate::types::ability::TypeFilter::Planeswalker
        )
    })
}

fn default_equip_target_filter() -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::creature().controller(crate::types::ability::ControllerRef::You),
    )
}

fn parse_equip_cost(cost_text: &str) -> AbilityCost {
    let cost = parse_oracle_cost(cost_text);
    if !matches!(cost, AbilityCost::Unimplemented { .. }) {
        return cost;
    }

    parse_first_mana_cost_in_text(cost_text)
        .map(|cost| AbilityCost::Mana { cost })
        .unwrap_or(cost)
}

fn parse_first_mana_cost_in_text(text: &str) -> Option<ManaCost> {
    let upper = text.to_ascii_uppercase();
    let (_, cost) = nom::sequence::preceded(
        take_until::<_, _, OracleError<'_>>("{"),
        super::oracle_nom::primitives::parse_mana_cost,
    )
    .parse(upper.as_str())
    .ok()?;
    Some(cost)
}

fn split_trailing_self_cost_reduction(
    line: &str,
) -> (&str, Option<crate::types::ability::CostReduction>) {
    let lower = line.to_lowercase();
    let Some(((), reduction_text)) = nom_on_lower(line, &lower, |input| {
        value((), (take_until(". this ability costs "), tag(". "))).parse(input)
    }) else {
        return (line, None);
    };
    let Some(reduction) = try_parse_cost_reduction(reduction_text) else {
        return (line, None);
    };
    let activation_len = line.len() - ". ".len() - reduction_text.len();
    (line[..activation_len].trim(), Some(reduction))
}

/// Try to parse a planeswalker loyalty line: "+N:", "−N:", "0:", "[+N]:", "[−N]:", "[0]:"
fn try_parse_loyalty_line(line: &str, ctx: &mut ParseContext) -> Option<AbilityDefinition> {
    let trimmed = line.trim();

    // Try bracket format first: [+2]: ..., [−1]: ..., [0]: ...
    if let Some(after_open) = trimmed.strip_prefix('[') {
        if let Some((inner, rest)) = after_open.split_once(']') {
            if let Some(effect_text) = rest.trim().strip_prefix(':') {
                if let Some(amount) = parse_loyalty_number(inner) {
                    let effect_text = effect_text.trim();
                    ctx.subject = None;
                    ctx.actor = None;
                    let mut def =
                        parse_effect_chain_with_context(effect_text, AbilityKind::Activated, ctx);
                    def.cost = Some(AbilityCost::Loyalty { amount });
                    def.description = Some(trimmed.to_string());
                    apply_loyalty_restrictions(&mut def);
                    return Some(def);
                }
            }
        }
    }

    // Try bare format: +2: ..., −1: ..., 0: ...
    if let Some((prefix, effect_text)) = trimmed.split_once(':') {
        if let Some(amount) = parse_loyalty_number(prefix) {
            // Verify it looks like a loyalty prefix (starts with +, −, –, -, or is "0")
            let first_char = prefix.trim().chars().next()?;
            if first_char == '+'
                || first_char == '−'
                || first_char == '–'
                || first_char == '-'
                || prefix.trim() == "0"
            {
                let effect_text = effect_text.trim();
                ctx.subject = None;
                ctx.actor = None;
                let mut def =
                    parse_effect_chain_with_context(effect_text, AbilityKind::Activated, ctx);
                def.cost = Some(AbilityCost::Loyalty { amount });
                def.description = Some(trimmed.to_string());
                apply_loyalty_restrictions(&mut def);
                return Some(def);
            }
        }
    }

    None
}

/// CR 606.3: A player may activate a loyalty ability only during a main phase
/// of their turn with an empty stack, and only if no player has previously
/// activated a loyalty ability of that permanent that turn. The planeswalker
/// activation path (`game::planeswalker::can_activate_loyalty_ability`) is the
/// authoritative gate for the "once per permanent per turn" rule — it reads
/// `obj.loyalty_activations_this_turn` against a cap raised by
/// `state.extra_loyalty_activations_this_turn` (The Chain Veil class). We do
/// NOT add `ActivationRestriction::OnlyOnceEachTurn` here: that restriction is
/// per-ability-index, while CR 606.3 is per-permanent (across ALL loyalty
/// ability indices). Conflating the two would (a) incorrectly allow a +2 and
/// a -1 on the same planeswalker in one turn and (b) block The Chain Veil's
/// "as though none of its loyalty abilities have been activated this turn"
/// cap-raise from ever taking effect.
fn apply_loyalty_restrictions(def: &mut AbilityDefinition) {
    // CR 606.3: "...only during a main phase of their turn when the stack is empty..."
    def.sorcery_speed = true;
    if !def
        .activation_restrictions
        .contains(&ActivationRestriction::AsSorcery)
    {
        def.activation_restrictions
            .push(ActivationRestriction::AsSorcery);
    }
}

/// Parse a loyalty number string like "+2", "−3", "0", "-1".
fn parse_loyalty_number(s: &str) -> Option<i32> {
    let s = s.trim();
    // Normalize Unicode minus signs
    let normalized = s.replace(['−', '–'], "-");
    // "+N" → positive
    if let Some(rest) = normalized.strip_prefix('+') {
        return rest.parse::<i32>().ok();
    }
    // "-N" or bare number
    normalized.parse::<i32>().ok()
}

/// CR 601.2f: Walk the sub_ability chain to find a terminal `Unimplemented` that is
/// a cost reduction pattern. If found, remove it from the chain and return the parsed
/// `CostReduction`. The cost reduction may be several levels deep (e.g., Boseiju has
/// SearchLibrary → ChangeZone → ChangeZone → Unimplemented(cost reduction)).
fn extract_cost_reduction_from_chain(def: &mut AbilityDefinition) {
    if let Some(reduction) = strip_cost_reduction_node(&mut def.sub_ability) {
        def.cost_reduction = Some(reduction);
    }
}

/// Recursively walk the sub_ability chain. If a node is an `Unimplemented` cost
/// reduction, remove it and return the parsed `CostReduction`.
fn strip_cost_reduction_node(
    slot: &mut Option<Box<AbilityDefinition>>,
) -> Option<crate::types::ability::CostReduction> {
    let sub = slot.as_mut()?;
    if let Effect::Unimplemented {
        description: Some(ref desc),
        ..
    } = *sub.effect
    {
        if let Some(reduction) = super::oracle_cost::try_parse_cost_reduction(&desc.to_lowercase())
        {
            // Remove this node, promote its child (usually None).
            *slot = sub.sub_ability.take();
            return Some(reduction);
        }
    }
    // Recurse into the chain.
    strip_cost_reduction_node(&mut sub.sub_ability)
}

/// Find the position of ":" that indicates an activated ability cost/effect split.
/// The left side must look like a cost (contains "{", or starts with cost-like words,
/// or is a loyalty marker).
pub(super) fn find_activated_colon(line: &str) -> Option<usize> {
    let colon_pos = find_top_level_colon(line)?;
    let prefix = &line[..colon_pos];

    // Contains mana symbols
    if prefix.contains('{') {
        return Some(colon_pos);
    }

    // Starts with cost-like words (all ASCII — case-insensitive prefix check)
    let trimmed = prefix.trim();
    let cost_starters = [
        "sacrifice",
        "discard",
        "pay",
        "remove",
        "exile",
        "return",
        "tap",
        "untap",
        "put",
    ];
    // Only lowercase when needed (skipped entirely if '{' was found above)
    let lower_prefix = trimmed.to_lowercase();
    if cost_starters.iter().any(|s| lower_prefix.starts_with(s)) {
        return Some(colon_pos);
    }

    None
}

fn find_top_level_colon(line: &str) -> Option<usize> {
    let mut paren_depth = 0u32;
    let mut in_quotes = false;

    for (idx, ch) in line.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => paren_depth += 1,
            ')' if !in_quotes => paren_depth = paren_depth.saturating_sub(1),
            ':' if !in_quotes && paren_depth == 0 => return Some(idx),
            _ => {}
        }
    }

    None
}

pub(super) fn strip_activated_constraints(text: &str) -> (String, ActivatedConstraintAst) {
    let mut remaining = text.trim().trim_end_matches('.').trim().to_string();
    let mut constraints = ActivatedConstraintAst::default();

    'parse_constraints: loop {
        let lower = remaining.to_lowercase();
        let tp = TextPair::new(&remaining, &lower);

        if let Some((before, after)) = tp.rsplit_around(" and only if ") {
            if !before.original.trim().is_empty() {
                let mut condition_text = after.original.trim().to_string();
                strip_once_per_turn_suffix(&mut condition_text, &mut constraints.restrictions);
                remaining = before
                    .original
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        condition: parse_restriction_condition(&condition_text),
                    });
                continue;
            }
        }

        // CR 602.2: "Any player may activate this ability." — strip as a recognized
        // annotation. This appears as a trailing sentence on activated abilities.
        if let Some(prefix) = lower.strip_suffix("any player may activate this ability") {
            let end = remaining.len() - "any player may activate this ability".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints.any_player_may_activate = true;
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        for (suffix, parsed) in [
            (
                "activate only as a sorcery and only once each turn",
                vec![
                    ActivationRestriction::AsSorcery,
                    ActivationRestriction::OnlyOnceEachTurn,
                ],
            ),
            (
                "activate only as a sorcery and only once",
                vec![
                    ActivationRestriction::AsSorcery,
                    ActivationRestriction::OnlyOnce,
                ],
            ),
            (
                "activate only during your turn and only once each turn",
                vec![
                    ActivationRestriction::DuringYourTurn,
                    ActivationRestriction::OnlyOnceEachTurn,
                ],
            ),
            (
                "activate only during your upkeep and only once each turn",
                vec![
                    ActivationRestriction::DuringYourUpkeep,
                    ActivationRestriction::OnlyOnceEachTurn,
                ],
            ),
        ] {
            if lower.ends_with(suffix) {
                let end = remaining.len() - suffix.len();
                remaining = remaining[..end]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                constraints.restrictions.extend(parsed);
                if remaining.is_empty() {
                    break 'parse_constraints;
                }
                continue 'parse_constraints;
            }
        }

        if let Some(prefix) = lower.strip_suffix("activate only as a sorcery") {
            let end = remaining.len() - "activate only as a sorcery".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::AsSorcery);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate only as an instant") {
            let end = remaining.len() - "activate only as an instant".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::AsInstant);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate only during your turn") {
            let end = remaining.len() - "activate only during your turn".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::DuringYourTurn);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate only during your upkeep") {
            let end = remaining.len() - "activate only during your upkeep".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::DuringYourUpkeep);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate only during combat") {
            let end = remaining.len() - "activate only during combat".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::DuringCombat);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) =
            lower.strip_suffix("activate only during your turn, before attackers are declared")
        {
            let end = remaining.len()
                - "activate only during your turn, before attackers are declared".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::DuringYourTurn);
            constraints
                .restrictions
                .push(ActivationRestriction::BeforeAttackersDeclared);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) =
            lower.strip_suffix("activate only during combat before combat damage has been dealt")
        {
            let end = remaining.len()
                - "activate only during combat before combat damage has been dealt".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::DuringCombat);
            constraints
                .restrictions
                .push(ActivationRestriction::BeforeCombatDamage);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate only once each turn") {
            let end = remaining.len() - "activate only once each turn".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::OnlyOnceEachTurn);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate only once") {
            let end = remaining.len() - "activate only once".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::OnlyOnce);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate no more than twice each turn") {
            let end = remaining.len() - "activate no more than twice each turn".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::MaxTimesEachTurn { count: 2 });
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate no more than three times each turn") {
            let end = remaining.len() - "activate no more than three times each turn".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::MaxTimesEachTurn { count: 3 });
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(idx) = tp.rfind("activate only if ") {
            if idx == 0 {
                let mut condition_text = remaining["activate only if ".len()..].trim().to_string();
                strip_once_per_turn_suffix(&mut condition_text, &mut constraints.restrictions);
                remaining.clear();
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        condition: parse_restriction_condition(&condition_text),
                    });
                break;
            }
            if lower[..idx].ends_with(". ") {
                let mut condition_text = remaining[idx + "activate only if ".len()..]
                    .trim()
                    .to_string();
                strip_once_per_turn_suffix(&mut condition_text, &mut constraints.restrictions);
                remaining = remaining[..idx]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        condition: parse_restriction_condition(&condition_text),
                    });
                continue;
            }
        }

        if let Some(idx) = tp.rfind("activate only from ") {
            if idx == 0 || lower[..idx].ends_with(". ") {
                let restriction_text = remaining[idx + "activate only from ".len()..]
                    .trim()
                    .to_string();
                remaining = remaining[..idx]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                let full_text = format!("from {restriction_text}");
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        condition: parse_restriction_condition(&full_text),
                    });
                continue;
            }
        }

        if let Some(idx) = tp.rfind("activate only ") {
            if idx == 0 || lower[..idx].ends_with(". ") {
                let restriction_text = remaining[idx + "activate only ".len()..].trim().to_string();
                remaining = remaining[..idx]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        condition: parse_restriction_condition(&restriction_text),
                    });
                continue;
            }
        }

        if let Some(idx) = tp.rfind("activate no more than ") {
            if idx == 0 || lower[..idx].ends_with(". ") {
                let restriction_text = remaining[idx + "activate no more than ".len()..]
                    .trim()
                    .to_string();
                remaining = remaining[..idx]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                let full_text = format!("no more than {restriction_text}");
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        condition: parse_restriction_condition(&full_text),
                    });
                continue;
            }
        }

        break;
    }

    (remaining, constraints)
}

/// CR 602.5b: Recognize a standalone `"Activate only once each turn"` cadence
/// sentence — the trailing restriction on cards like Luxurious Locomotive's
/// "Crew 1. Activate only once each turn." Pure / side-effect-free.
///
/// Only the standalone imperative sentence is recognized here. The conjoined
/// `"activate only if [X] and only once each turn"` tail is a different
/// grammatical shape with its own slicing requirement, handled by
/// `strip_once_per_turn_suffix`; the strictly-once-ever `" and only once"`
/// form is likewise that function's concern (it maps to
/// `ActivationRestriction::OnlyOnce`, which `ActivationCadence` does not model).
fn recognize_once_each_turn_cadence(text: &str) -> Option<ActivationCadence> {
    let lower = text.trim().trim_end_matches('.').to_lowercase();
    let matched = all_consuming(tag::<_, _, OracleError<'_>>("activate only once each turn"))
        .parse(lower.as_str())
        .is_ok();
    matched.then_some(ActivationCadence::OncePerTurn)
}

/// CR 702.122 + CR 602.5b: Parse a Crew keyword line, capturing an optional
/// trailing "Activate only once each turn." cadence sentence. MTGJSON supplies
/// `Crew:N` without the cadence, so this re-extracts the full keyword from Oracle
/// text when the line carries the standalone restriction sentence; the merge in
/// `synthesis.rs` then replaces the cadence-less MTGJSON keyword. Returns `None`
/// when there is no cadence sentence, leaving the MTGJSON keyword untouched.
/// `lower` is the reminder-stripped, lowercased line.
fn parse_crew_keyword(lower: &str) -> Option<Keyword> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("crew ").parse(lower).ok()?;
    let (power, after_power) = parse_number(rest)?;
    // After the power, the only modeled tail is the cadence sentence: "Crew N.
    // Activate only once each turn." A bare "Crew N" (no tail) yields None so the
    // MTGJSON keyword is kept as-is.
    let tail = after_power.trim_start_matches(|c: char| c == '.' || c.is_whitespace());
    if recognize_once_each_turn_cadence(tail) == Some(ActivationCadence::OncePerTurn) {
        Some(Keyword::Crew {
            power,
            once_per_turn: ActivationCadence::OncePerTurn,
        })
    } else {
        None
    }
}

/// Strip "and only once each turn" / "and only once" compound suffixes from a condition_text
/// extracted from "activate only if [condition_text]", pushing the corresponding
/// `OnlyOnceEachTurn`/`OnlyOnce` restriction.
///
/// Uses the `text.len() - suffix.len()` offset idiom (CR 602.5b): all suffixes are ASCII,
/// so byte-length slicing is safe.
fn strip_once_per_turn_suffix(
    condition_text: &mut String,
    restrictions: &mut Vec<ActivationRestriction>,
) {
    if strip_condition_suffix(
        condition_text,
        " and only as a sorcery",
        ActivationRestriction::AsSorcery,
        restrictions,
    ) {
        strip_once_per_turn_suffix(condition_text, restrictions);
        return;
    }

    let lower = condition_text.to_lowercase();
    if lower.ends_with(" and only once each turn") {
        let stripped_len = condition_text.len() - " and only once each turn".len();
        *condition_text = condition_text[..stripped_len]
            .trim_end_matches(|c: char| c == ',' || c.is_whitespace())
            .to_string();
        restrictions.push(ActivationRestriction::OnlyOnceEachTurn);
    } else if lower.ends_with(" and only once") {
        let stripped_len = condition_text.len() - " and only once".len();
        *condition_text = condition_text[..stripped_len]
            .trim_end_matches(|c: char| c == ',' || c.is_whitespace())
            .to_string();
        restrictions.push(ActivationRestriction::OnlyOnce);
    }
}

fn strip_condition_suffix(
    condition_text: &mut String,
    suffix: &'static str,
    restriction: ActivationRestriction,
    restrictions: &mut Vec<ActivationRestriction>,
) -> bool {
    let lower = condition_text.to_lowercase();
    let suffix_len = match take_until::<_, _, OracleError<'_>>(suffix).parse(lower.as_str()) {
        Ok((rest, _))
            if all_consuming(tag::<_, _, OracleError<'_>>(suffix))
                .parse(rest)
                .is_ok() =>
        {
            suffix.len()
        }
        Err(_) => return false,
        _ => return false,
    };
    let stripped_len = condition_text.len() - suffix_len;
    *condition_text = condition_text[..stripped_len]
        .trim_end_matches(|c: char| c == ',' || c.is_whitespace()) // allow-noncombinator: structural punctuation cleanup after suffix parse
        .to_string();
    restrictions.push(restriction);
    true
}

/// Strip trailing "X can't be 0." / "This ability can't be copied and X can't
/// be 0." constraint annotations from Oracle text. These are activation/casting
/// restrictions that annotate X-cost abilities but are not themselves effects.
fn strip_x_cant_be_zero_suffix(line: &str) -> String {
    let lower = line.to_lowercase();
    let trimmed = lower.trim_end_matches('.');
    // Standalone cases: entire line is only an activation/casting annotation.
    if matches!(
        trimmed,
        "x can't be 0" | "this ability can't be copied and x can't be 0"
    ) {
        return String::new();
    }
    // Suffix case: "... X can't be 0." at end of line
    for suffix in [
        ". this ability can't be copied and x can't be 0",
        " this ability can't be copied and x can't be 0",
        ". x can't be 0",
        " x can't be 0",
    ] {
        if let Some(pos) = trimmed.rfind(suffix) {
            let mut result = line[..pos].to_string();
            // Preserve trailing period if we stripped at a sentence boundary
            if suffix.starts_with('.') {
                result.push('.');
            }
            return result.trim_end().to_string();
        }
    }
    line.to_string()
}

fn x_annotation_marks_ability_uncopyable(line: &str) -> bool {
    let lower = line.to_lowercase();
    scan_contains(&lower, "this ability can't be copied and x can't be 0")
}

fn x_annotation_min_value(line: &str) -> u32 {
    let lower = line.to_lowercase();
    if scan_contains(&lower, "x can't be 0") {
        1
    } else {
        0
    }
}

/// Primary nom-based dispatcher for Oracle text lines.
///
/// Create an Unimplemented fallback ability.
pub(super) fn make_unimplemented(line: &str) -> AbilityDefinition {
    tracing::debug!(oracle_text = line, "unimplemented ability line");
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Unimplemented {
            name: "unknown".to_string(),
            description: Some(line.to_string()),
        },
    )
    .description(line.to_string())
}

/// Check if an AbilityDefinition (or its sub_ability chain) contains Unimplemented effects.
pub(super) fn has_unimplemented(def: &AbilityDefinition) -> bool {
    if matches!(*def.effect, Effect::Unimplemented { .. }) {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        return has_unimplemented(sub);
    }
    false
}

/// Parse an activated-ability effect chain with self-reference fallback.
///
/// Tries the raw text first so patterns that depend on the literal card name
/// (e.g. possessive forms like "Marwyn's power") keep working, then retries
/// with `~`-normalized text if the first pass left the result unimplemented
/// *or* emitted a `target-fallback` warning. The latter is the Metalhead
/// class: the effect parsed to a concrete variant but `parse_target` silently
/// fell back to `TargetFilter::Any` because the bare card-name wasn't
/// recognized as a self-reference. Warnings from the discarded pass are
/// dropped so they don't pollute coverage output.
pub(super) fn parse_activated_with_self_ref_fallback(
    effect_text: &str,
    card_name: &str,
    ctx: &mut ParseContext,
) -> AbilityDefinition {
    // Pre-diagnostics stay in ctx naturally — only manage trial-parse diagnostics.
    let pre_snapshot = ctx.diagnostics.len();

    ctx.subject = None;
    ctx.actor = None;
    let def = parse_effect_chain_with_context(effect_text, AbilityKind::Activated, ctx);
    let first_has_target_fallback = ctx.diagnostics[pre_snapshot..]
        .iter()
        .any(|d| matches!(d, OracleDiagnostic::TargetFallback { .. }));
    let first_clean = !has_unimplemented(&def) && !first_has_target_fallback;

    if first_clean {
        // First parse is clean — keep its diagnostics.
        return def;
    }

    let normalized = normalize_self_refs_for_static(effect_text, card_name);
    if normalized == effect_text {
        // No normalization change — keep first-pass diagnostics.
        return def;
    }

    // Save first-pass diagnostics for potential restoration.
    let first_diagnostics: Vec<OracleDiagnostic> = ctx.diagnostics[pre_snapshot..].to_vec();
    ctx.diagnostics.truncate(pre_snapshot);

    ctx.subject = None;
    ctx.actor = None;
    let alt = parse_effect_chain_with_context(&normalized, AbilityKind::Activated, ctx);
    let alt_has_target_fallback = ctx.diagnostics[pre_snapshot..]
        .iter()
        .any(|d| matches!(d, OracleDiagnostic::TargetFallback { .. }));
    let alt_clean = !has_unimplemented(&alt) && !alt_has_target_fallback;

    if alt_clean {
        // Normalized pass is strictly better — keep only its diagnostics (already in ctx).
        alt
    } else {
        // Neither pass was clean; prefer the original result and preserve
        // both passes' diagnostics so the coverage dashboard reflects reality.
        let alt_diagnostics: Vec<OracleDiagnostic> = ctx.diagnostics[pre_snapshot..].to_vec();
        ctx.diagnostics.truncate(pre_snapshot);
        ctx.diagnostics.extend(first_diagnostics);
        ctx.diagnostics.extend(alt_diagnostics);
        def
    }
}

fn normalize_activated_mana_instead_delta(def: &mut AbilityDefinition) {
    let Effect::Mana {
        produced:
            ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: base_count },
            },
        ..
    } = def.effect.as_ref()
    else {
        return;
    };
    let Some(sub) = def.sub_ability.as_mut() else {
        return;
    };
    let Some(AbilityCondition::ConditionInstead { inner }) = sub.condition.take() else {
        return;
    };
    let Effect::Mana {
        produced:
            ManaProduction::Colorless {
                count:
                    QuantityExpr::Fixed {
                        value: replacement_count,
                    },
            },
        ..
    } = sub.effect.as_mut()
    else {
        sub.condition = Some(AbilityCondition::ConditionInstead { inner });
        return;
    };
    let delta = replacement_count.saturating_sub(*base_count);
    if delta == 0 {
        sub.condition = Some(AbilityCondition::ConditionInstead { inner });
        return;
    }
    *replacement_count = delta;
    sub.condition = Some(*inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_effect::parse_effect_chain;
    use crate::types::ability::{
        AbilityCondition, AggregateFunction, Comparator, ContinuousModification, ControllerRef,
        Duration, Effect, FilterProp, ManaProduction, ManaSpendRestriction,
        ModalSelectionConstraint, MultiTargetSpec, ObjectScope, ParsedCondition, PlayerFilter,
        PlayerScope, PreventionAmount, PtStat, PtValue, PtValueScope, QuantityExpr, QuantityRef,
        ReplacementCondition, RoundingMode, SharedQuality, SharedQualityRelation, ShieldKind,
        StaticCondition, TargetFilter, TriggerCondition, TypeFilter, TypedFilter,
    };
    use crate::types::keywords::{FlashbackCost, KeywordKind, WardCost};
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::{CostModifyMode, ProhibitionScope, StaticMode};
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    fn parse(
        text: &str,
        name: &str,
        kw: &[Keyword],
        types: &[&str],
        subtypes: &[&str],
    ) -> ParsedAbilities {
        let keyword_names: Vec<String> = kw.iter().map(keyword_display_name).collect();
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
        parse_oracle_text(text, name, &keyword_names, &types, &subtypes)
    }

    /// Parse with raw MTGJSON keyword names (for testing keyword extraction).
    fn parse_with_keyword_names(
        text: &str,
        name: &str,
        keyword_names: &[&str],
        types: &[&str],
        subtypes: &[&str],
    ) -> ParsedAbilities {
        let keyword_names: Vec<String> = keyword_names.iter().map(|s| s.to_string()).collect();
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
        parse_oracle_text(text, name, &keyword_names, &types, &subtypes)
    }

    #[test]
    fn lightning_bolt_spell_effect() {
        let r = parse(
            "Lightning Bolt deals 3 damage to any target.",
            "Lightning Bolt",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Spell);
    }

    /// Issue #1696 — Myrkul, Lord of Bones end-to-end: the death trigger exiles
    /// the dying creature and creates an enchantment token copy of it. Verifies
    /// the full parse pipeline produces (a) an exile effect (which publishes the
    /// tracked set the copy reads) and (b) a `CopyTokenOf` carrying the
    /// `SetCardTypes { [Enchantment] }` exception (CR 205.1a + CR 707.9d) — the
    /// card-type override that was previously dropped, so the token came out as
    /// a creature copy instead of the intended enchantment.
    #[test]
    fn myrkul_full_ability_exiles_and_creates_enchantment_copy() {
        let r = parse(
            "Whenever another nontoken creature you control dies, you may exile it. \
             If you do, create a token that's a copy of that card, except it's an \
             enchantment and loses all other card types.",
            "Myrkul, Lord of Bones",
            &[],
            &["Creature"],
            &["God"],
        );

        // Recursively collect every effect reachable from a triggered ability,
        // descending through delayed-trigger wrappers and sub/else branches.
        fn collect<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a Effect>) {
            out.push(&def.effect);
            if let Effect::CreateDelayedTrigger { effect, .. } = def.effect.as_ref() {
                collect(effect, out);
            }
            if let Some(sub) = def.sub_ability.as_deref() {
                collect(sub, out);
            }
            if let Some(els) = def.else_ability.as_deref() {
                collect(els, out);
            }
        }

        // A pure triggered-ability card lands in `triggers`, not `abilities`;
        // the trigger's effect tree hangs off `execute`.
        let mut effects = Vec::new();
        for ability in r.abilities.iter() {
            collect(ability, &mut effects);
        }
        for trigger in r.triggers.iter() {
            if let Some(exec) = trigger.execute.as_deref() {
                collect(exec, &mut effects);
            }
        }

        let expected_override = ContinuousModification::SetCardTypes {
            core_types: vec![crate::types::card_type::CoreType::Enchantment],
        };
        let has_enchantment_copy = effects.iter().any(|e| match e {
            Effect::CopyTokenOf {
                additional_modifications,
                ..
            } => additional_modifications.contains(&expected_override),
            _ => false,
        });
        assert!(
            has_enchantment_copy,
            "expected a CopyTokenOf carrying SetCardTypes([Enchantment]); effects = {effects:#?}"
        );

        let has_exile = effects.iter().any(|e| {
            matches!(
                e,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    ..
                }
            )
        });
        assert!(
            has_exile,
            "expected an Exile (ChangeZone to Exile) effect; effects = {effects:#?}"
        );
    }

    #[test]
    fn ghostfire_has_self_color_cda_and_spell_damage() {
        let r = parse(
            "Ghostfire is colorless.\nGhostfire deals 3 damage to any target.",
            "Ghostfire",
            &[],
            &["Instant"],
            &[],
        );

        assert_eq!(r.statics.len(), 1, "expected one self color CDA static");
        let static_def = &r.statics[0];
        assert!(static_def.characteristic_defining);
        assert_eq!(static_def.affected, Some(TargetFilter::SelfRef));
        assert_eq!(
            static_def.modifications,
            vec![ContinuousModification::SetColor { colors: vec![] }]
        );
        assert_eq!(
            static_def.active_zones,
            vec![
                Zone::Library,
                Zone::Hand,
                Zone::Battlefield,
                Zone::Graveyard,
                Zone::Stack,
                Zone::Exile,
                Zone::Command,
            ]
        );

        assert_eq!(r.abilities.len(), 1, "expected one spell ability");
        assert_eq!(r.abilities[0].kind, AbilityKind::Spell);
        assert!(matches!(*r.abilities[0].effect, Effect::DealDamage { .. }));
    }

    #[test]
    fn mindlock_orb_routes_to_static_search_prohibition() {
        let r = parse(
            "Players can't search libraries.",
            "Mindlock Orb",
            &[],
            &["Artifact"],
            &[],
        );
        assert!(
            r.abilities.is_empty(),
            "Mindlock Orb should not emit spell abilities"
        );
        assert_eq!(r.statics.len(), 1, "expected one static search prohibition");
        assert_eq!(
            r.statics[0].mode,
            StaticMode::CantSearchLibrary {
                cause: ProhibitionScope::AllPlayers,
            }
        );
    }

    /// CR 115.1 + CR 701.9b: "random target X" — the parser stamps
    /// `target_selection_mode = Random` on the produced `AbilityDefinition`.
    /// The runtime then short-circuits `WaitingFor::TargetSelection` and picks
    /// from `state.rng`. End-to-end check: text → parse → mode field.
    ///
    /// Uses an "a random target" prefix (article + random + target). The
    /// article-stripping arm in `parse_target_with_ctx` recognises both
    /// "a target" and "a random target" so the underlying filter parses
    /// identically to the controller-choice case while `ctx` records the mode.
    #[test]
    fn random_target_creature_marks_ability_random_mode() {
        use crate::types::ability::TargetSelectionMode;
        let r = parse(
            "~ deals 3 damage to a random target creature.",
            "Test Card",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        assert!(matches!(
            r.abilities[0].target_selection_mode,
            TargetSelectionMode::Random
        ));
    }

    /// CR 115.1 + CR 701.9b: "random target X" without the leading article —
    /// matches Power Struggle's "exchanges control of random target artifact".
    /// The bare-"random " arm sets the selection mode on `ctx` directly.
    #[test]
    fn random_target_without_article_marks_random_mode() {
        use crate::types::ability::TargetSelectionMode;
        let r = parse(
            "~ deals 3 damage to random target creature.",
            "Test Card",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        assert!(matches!(
            r.abilities[0].target_selection_mode,
            TargetSelectionMode::Random
        ));
    }

    /// CR 115.1: Ordinary "target X" stays at `Chosen` (default), so existing
    /// cards keep their controller-driven target prompt. Negative test for the
    /// random-mode plumbing — this exists so a future regression that flips
    /// the default cannot pass silently.
    #[test]
    fn ordinary_target_creature_keeps_chosen_mode() {
        use crate::types::ability::TargetSelectionMode;
        let r = parse(
            "~ deals 3 damage to target creature.",
            "Test Card",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        assert!(matches!(
            r.abilities[0].target_selection_mode,
            TargetSelectionMode::Chosen
        ));
    }

    #[test]
    fn leadership_vacuum_returns_target_players_commanders_to_command_zone() {
        let r = parse(
            "Target player returns each commander they control from the battlefield to the command zone.\nDraw a card.",
            "Leadership Vacuum",
            &[],
            &["Instant"],
            &[],
        );
        assert!(
            r.parse_warnings.is_empty(),
            "unexpected parse warnings: {:?}",
            r.parse_warnings
        );
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::TargetOnly {
                target: TargetFilter::Player
            }
        ));
        let sub = r.abilities[0]
            .sub_ability
            .as_ref()
            .expect("expected target-player sub-ability");
        match &*sub.effect {
            Effect::ChangeZoneAll {
                origin,
                destination,
                target:
                    TargetFilter::Typed(TypedFilter {
                        controller: Some(ControllerRef::You),
                        properties,
                        ..
                    }),
                ..
            } => {
                assert_eq!(*origin, None);
                assert_eq!(*destination, Zone::Command);
                assert!(properties.contains(&FilterProp::IsCommander));
            }
            other => panic!("expected command-zone ChangeZoneAll, got {other:?}"),
        }
    }

    #[test]
    fn thought_partition_choose_one_of_those_cards_has_no_target_fallback() {
        let r = parse(
            "Target opponent reveals all nonland cards in their hand. You may choose one of those cards. If you do, it perpetually becomes white and its mana cost perpetually becomes {5}.",
            "Thought Partition",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(
            r.parse_warnings
                .iter()
                .all(|warning| !matches!(warning, OracleDiagnostic::TargetFallback { .. })),
            "unexpected target fallback warnings: {:?}",
            r.parse_warnings
        );
    }

    #[test]
    fn nonmodal_spell_contiguous_resolution_lines_chain_once() {
        let r = parse("Scry 1.\nDraw a card.", "Test Opt", &[], &["Instant"], &[]);

        assert_eq!(r.abilities.len(), 1);
        assert!(r.modal.is_none());
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
        let draw = r.abilities[0]
            .sub_ability
            .as_ref()
            .expect("draw should be chained after scry");
        assert!(matches!(
            *draw.effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
    }

    #[test]
    fn modal_spell_block_keeps_mode_branches_separate() {
        let r = parse(
            "Choose one —\n• Scry 1.\n• Draw a card.",
            "Test Charm",
            &[],
            &["Instant"],
            &[],
        );

        let modal = r.modal.expect("modal metadata should remain on spell face");
        assert_eq!(modal.mode_count, 2);
        assert_eq!(r.abilities.len(), 2);
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
        assert!(matches!(
            *r.abilities[1].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
    }

    #[test]
    fn non_spell_permanent_resolution_like_lines_do_not_merge() {
        let r = parse(
            "Target player draws a card.\nTarget player gains 3 life.",
            "Test Permanent",
            &[],
            &["Artifact"],
            &[],
        );

        assert_eq!(r.abilities.len(), 2);
        assert!(r.abilities[0].sub_ability.is_none());
        assert!(matches!(*r.abilities[0].effect, Effect::Draw { .. }));
        assert!(matches!(*r.abilities[1].effect, Effect::GainLife { .. }));
    }

    #[test]
    fn multani_cda_parses_total_cards_in_all_players_hands() {
        let r = parse(
            "Multani's power and toughness are each equal to the total number of cards in all players' hands.",
            "Multani, Maro-Sorcerer",
            &[],
            &["Creature"],
            &[],
        );

        assert!(
            r.abilities.is_empty(),
            "unexpected abilities: {:?}",
            r.abilities
        );
        assert_eq!(r.statics.len(), 1);
        let qty = QuantityExpr::Ref {
            qty: QuantityRef::HandSize {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Sum,
                    exclude: None,
                },
            },
        };
        assert_eq!(
            r.statics[0].modifications,
            vec![
                ContinuousModification::SetDynamicPower { value: qty.clone() },
                ContinuousModification::SetDynamicToughness { value: qty },
            ]
        );
    }

    #[test]
    fn kicker_and_or_line_sets_two_kicker_costs() {
        let r = parse(
            "Kicker {B} and/or {R}\nWhen ~ enters, if it was kicked twice, draw a card.",
            "Test Kicker",
            &[],
            &["Creature"],
            &[],
        );

        match r.additional_cost.expect("additional cost") {
            AdditionalCost::Kicker { costs, repeatable } => {
                assert!(!repeatable);
                assert_eq!(costs.len(), 2);
                assert!(matches!(
                    &costs[0],
                    AbilityCost::Mana {
                        cost: ManaCost::Cost { shards, generic: 0 }
                    } if shards == &vec![ManaCostShard::Black]
                ));
                assert!(matches!(
                    &costs[1],
                    AbilityCost::Mana {
                        cost: ManaCost::Cost { shards, generic: 0 }
                    } if shards == &vec![ManaCostShard::Red]
                ));
            }
            other => panic!("expected two-cost Kicker, got {other:?}"),
        }
    }

    #[test]
    fn keyword_extracted_kicker_and_or_line_sets_two_kicker_costs() {
        let r = parse_with_keyword_names(
            "Kicker {G} and/or {1}{U}\n\
             When you cast this spell, if it was kicked with its {G} kicker, draw a card.\n\
             When you cast this spell, if it was kicked with its {1}{U} kicker, scry 1.",
            "Test Kicker",
            &["Kicker"],
            &["Creature"],
            &["Eldrazi"],
        );

        match r.additional_cost.expect("additional cost") {
            AdditionalCost::Kicker { costs, repeatable } => {
                assert!(!repeatable);
                assert_eq!(costs.len(), 2);
                assert!(matches!(
                    &costs[0],
                    AbilityCost::Mana {
                        cost: ManaCost::Cost { shards, generic: 0 }
                    } if shards == &vec![ManaCostShard::Green]
                ));
                assert!(matches!(
                    &costs[1],
                    AbilityCost::Mana {
                        cost: ManaCost::Cost { shards, generic: 1 }
                    } if shards == &vec![ManaCostShard::Blue]
                ));
            }
            other => panic!("expected two-cost Kicker, got {other:?}"),
        }
    }

    #[test]
    fn multikicker_line_sets_repeatable_kicker_cost() {
        let r = parse(
            "Multikicker {1}{G}\nWhen ~ enters, draw a card.",
            "Test Multikicker",
            &[],
            &["Creature"],
            &[],
        );

        match r.additional_cost.expect("additional cost") {
            AdditionalCost::Kicker { costs, repeatable } => {
                assert!(repeatable);
                assert_eq!(costs.len(), 1);
                assert!(matches!(
                    &costs[0],
                    AbilityCost::Mana {
                        cost: ManaCost::Cost { shards, generic: 1 }
                    } if shards == &vec![ManaCostShard::Green]
                ));
            }
            other => panic!("expected repeatable Kicker, got {other:?}"),
        }
    }

    #[test]
    fn non_mana_kicker_line_uses_oracle_cost_parser() {
        let r = parse(
            "Kicker—Sacrifice a land.\nWhen ~ enters, draw a card.",
            "Test Nonmana Kicker",
            &[],
            &["Creature"],
            &[],
        );

        match r.additional_cost.expect("additional cost") {
            AdditionalCost::Kicker { costs, repeatable } => {
                assert!(!repeatable);
                assert_eq!(costs.len(), 1);
                assert!(matches!(&costs[0], AbilityCost::Sacrifice { count: 1, .. }));
            }
            other => panic!("expected non-mana Kicker, got {other:?}"),
        }
    }

    #[test]
    fn harrow_parses_required_sacrifice_land_additional_cost() {
        let r = parse(
            "As an additional cost to cast this spell, sacrifice a land.\nSearch your library for up to two basic land cards, put them onto the battlefield, then shuffle.",
            "Harrow",
            &[],
            &["Instant"],
            &[],
        );

        match r.additional_cost.expect("additional cost") {
            AdditionalCost::Required(AbilityCost::Sacrifice { target, count }) => {
                assert_eq!(count, 1);
                assert_eq!(
                    target,
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Land))
                );
            }
            other => panic!("expected required sacrifice-land cost, got {other:?}"),
        }
        assert_eq!(r.abilities.len(), 1);
        assert!(r.abilities[0].cost.is_none());
    }

    /// Issue #1965 — Eldritch Evolution: required creature sacrifice + library
    /// search whose mana-value cap tracks the sacrificed creature (+2).
    #[test]
    fn eldritch_evolution_parses_sacrifice_cost_and_dynamic_search_filter() {
        let r = parse(
            "As an additional cost to cast this spell, sacrifice a creature.\n\
             Search your library for a creature card with mana value X or less, where X is 2 plus the sacrificed creature's mana value. \
             Put that card onto the battlefield, then shuffle. Exile Eldritch Evolution.",
            "Eldritch Evolution",
            &[],
            &["Sorcery"],
            &[],
        );
        match r.additional_cost.expect("additional cost") {
            AdditionalCost::Required(AbilityCost::Sacrifice { target, count }) => {
                assert_eq!(count, 1);
                assert_eq!(target, TargetFilter::Typed(TypedFilter::creature()));
            }
            other => panic!("expected required sacrifice-creature cost, got {other:?}"),
        }
        assert_eq!(r.abilities.len(), 1);
        let Effect::SearchLibrary { filter, .. } = r.abilities[0].effect.as_ref() else {
            panic!(
                "expected SearchLibrary spell effect, got {:?}",
                r.abilities[0].effect
            );
        };
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected typed search filter, got {filter:?}");
        };
        let cmc = typed
            .properties
            .iter()
            .find_map(|p| match p {
                FilterProp::Cmc { comparator, value } => Some((comparator, value)),
                _ => None,
            })
            .expect("search filter must carry Cmc bound");
        assert_eq!(*cmc.0, Comparator::LE);
        assert_eq!(
            *cmc.1,
            QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::CostPaidObject,
                    },
                }),
                offset: 2,
            }
        );
    }

    /// Issue #1997 — Embiggen: +1/+1 per typeline component on the targeted creature.
    #[test]
    fn embiggen_parses_non_brushwagg_pump_scaled_by_typeline_components() {
        use crate::types::ability::{ObjectScope, PtValue, TypeFilter};
        let r = parse(
            "Until end of turn, target non-Brushwagg creature gets +1/+1 for each supertype, card type, and subtype it has.",
            "Embiggen",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        match r.abilities[0].effect.as_ref() {
            Effect::Pump {
                power,
                toughness,
                target,
            } => {
                let PtValue::Quantity(expr) = power else {
                    panic!("expected dynamic power, got {power:?}");
                };
                assert_eq!(toughness, &PtValue::Quantity(expr.clone()));
                let TargetFilter::Typed(typed) = target else {
                    panic!("expected typed creature target, got {target:?}");
                };
                assert!(
                    typed.type_filters.contains(&TypeFilter::Creature),
                    "must target creatures"
                );
                assert!(
                    typed.type_filters.iter().any(|t| {
                        matches!(
                            t,
                            TypeFilter::Non(inner)
                                if matches!(inner.as_ref(), TypeFilter::Subtype(s) if s == "Brushwagg")
                        )
                    }),
                    "must exclude Brushwagg, got {:?}",
                    typed.type_filters
                );
                let crate::types::ability::QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::ObjectTypelineComponentCount { scope },
                } = expr
                else {
                    panic!("expected typeline component count, got {expr:?}");
                };
                assert_eq!(*scope, ObjectScope::Recipient);
            }
            other => panic!("expected Pump, got {other:?}"),
        }
    }

    #[test]
    fn toxic_deluge_full_oracle_parses_x_life_cost_and_x_pump() {
        let r = parse(
            "As an additional cost to cast this spell, pay X life.\nAll creatures get -X/-X until end of turn.",
            "Toxic Deluge",
            &[],
            &["Sorcery"],
            &[],
        );

        assert_eq!(
            r.additional_cost,
            Some(AdditionalCost::Required(AbilityCost::PayLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }))
        );
        assert_eq!(r.abilities.len(), 1);
        match r.abilities[0].effect.as_ref() {
            Effect::PumpAll {
                power,
                toughness,
                target,
            } => {
                assert_eq!(power, &PtValue::Variable("-X".to_string()));
                assert_eq!(toughness, &PtValue::Variable("-X".to_string()));
                assert_eq!(target, &TargetFilter::Typed(TypedFilter::creature()));
            }
            other => panic!("expected all-creature -X/-X pump, got {other:?}"),
        }
    }

    #[test]
    fn immoral_bargain_full_oracle_parses_exact_x_targets_and_required_x_sacrifice() {
        let r = parse(
            "As an additional cost to cast this spell, sacrifice X creatures.\nDestroy X target nonland permanents.",
            "Immoral Bargain",
            &[],
            &["Sorcery"],
            &[],
        );

        assert_eq!(
            r.additional_cost,
            Some(AdditionalCost::Required(AbilityCost::Sacrifice {
                target: TargetFilter::Typed(TypedFilter::creature()),
                count: u32::MAX,
            }))
        );
        assert_eq!(r.abilities.len(), 1);
        let x = QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        };
        assert_eq!(r.abilities[0].multi_target, Some(MultiTargetSpec::exact(x)));
    }

    #[test]
    fn llanowar_elves_mana_ability() {
        let r = parse(
            "{T}: Add {G}.",
            "Llanowar Elves",
            &[],
            &["Creature"],
            &["Elf", "Druid"],
        );
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
    }

    /// Issue #1990 — Spellskite must parse to forced-self `ChangeTargets` so the
    /// AI `SpellskitePriorityPolicy` effect-shape gate fires at runtime.
    #[test]
    fn spellskite_activated_change_targets_forced_to_self() {
        use crate::types::ability::TargetFilter;
        use crate::types::game_state::RetargetScope;

        let r = parse(
            "{U/P}: Change a target of target spell or ability to ~.",
            "Spellskite",
            &[],
            &["Artifact", "Creature"],
            &["Phyrexian", "Horror"],
        );
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
        assert!(matches!(
            r.abilities[0].effect.as_ref(),
            Effect::ChangeTargets {
                scope: RetargetScope::Single,
                forced_to: Some(TargetFilter::SelfRef),
                ..
            }
        ));
    }

    #[test]
    fn priest_of_titania_mana_ability_supported() {
        let r = parse(
            "{T}: Add {G} for each Elf on the battlefield.",
            "Priest of Titania",
            &[],
            &["Creature"],
            &["Elf", "Druid"],
        );
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
        assert!(matches!(*r.abilities[0].effect, Effect::Mana { .. }));
    }

    #[test]
    fn distinct_card_type_choose_wires_remainder_on_bottom() {
        use crate::types::ability::{ChooseFromZoneConstraint, LibraryPosition};
        let r = parse(
            "Flying, vigilance, deathtouch, lifelink\nWhen Atraxa enters, reveal the top ten cards of your library. For each card type, you may put a card of that type from among the revealed cards into your hand. Put the rest on the bottom of your library in a random order.",
            "Atraxa, Grand Unifier",
            &[
                Keyword::Flying,
                Keyword::Vigilance,
                Keyword::Deathtouch,
                Keyword::Lifelink,
            ],
            &["Creature"],
            &["Phyrexian", "Angel"],
        );
        assert_eq!(r.triggers.len(), 1);
        let trigger = &r.triggers[0];
        let def = trigger
            .execute
            .as_ref()
            .expect("trigger should have execute");
        assert!(
            !has_unimplemented(def),
            "ETB should not contain Unimplemented effects: {def:?}",
        );

        // Walk the effect chain: RevealTop → ChooseFromZone → ChangeZone(Library→Hand) → PutAtLibraryPosition(Bottom)
        let choose_def = def
            .sub_ability
            .as_ref()
            .expect("RevealTop should chain to ChooseFromZone");
        assert!(
            matches!(
                &*choose_def.effect,
                Effect::ChooseFromZone {
                    up_to: true,
                    constraint: Some(ChooseFromZoneConstraint::DistinctCardTypes { .. }),
                    ..
                }
            ),
            "Expected ChooseFromZone with DistinctCardTypes constraint, got {:?}",
            choose_def.effect,
        );

        let change_zone_def = choose_def
            .sub_ability
            .as_ref()
            .expect("ChooseFromZone should chain to ChangeZone(Library→Hand)");
        assert!(
            matches!(
                &*change_zone_def.effect,
                Effect::ChangeZone {
                    origin: Some(Zone::Library),
                    destination: Zone::Hand,
                    ..
                }
            ),
            "Expected ChangeZone(Library→Hand), got {:?}",
            change_zone_def.effect,
        );

        let bottom_def = change_zone_def
            .sub_ability
            .as_ref()
            .expect("ChangeZone should chain to PutAtLibraryPosition(Bottom) for unchosen cards");
        assert!(
            matches!(
                &*bottom_def.effect,
                Effect::PutAtLibraryPosition {
                    position: LibraryPosition::Bottom,
                    ..
                }
            ),
            "Expected PutAtLibraryPosition(Bottom), got {:?}",
            bottom_def.effect,
        );
    }

    #[test]
    fn blocked_wurms_beyond_first_pump_have_dynamic_quantity_no_warning() {
        for (name, pt, expected_power_factor) in
            [("Johtull Wurm", "-2/-1", -2), ("Jungle Wurm", "-1/-1", -1)]
        {
            let r = parse(
                &format!(
                    "Whenever this creature becomes blocked, it gets {pt} until end of turn for each creature blocking it beyond the first."
                ),
                name,
                &[],
                &["Creature"],
                &["Wurm"],
            );

            assert_eq!(r.triggers.len(), 1);
            assert_eq!(r.triggers[0].mode, TriggerMode::BecomesBlocked);
            assert!(
                r.parse_warnings.iter().all(|warning| warning
                    .to_string()
                    .split_whitespace()
                    .next()
                    != Some("Swallow:DynamicQty")),
                "unexpected DynamicQty warning for {name}: {:?}",
                r.parse_warnings
            );
            let execute = r.triggers[0]
                .execute
                .as_ref()
                .expect("trigger should have execute");
            match execute.effect.as_ref() {
                Effect::Pump { power, .. } => match power {
                    PtValue::Quantity(QuantityExpr::Multiply { factor, inner }) => {
                        assert_eq!(*factor, expected_power_factor);
                        assert!(matches!(
                            inner.as_ref(),
                            QuantityExpr::ClampMin {
                                inner,
                                minimum: 0,
                            } if matches!(inner.as_ref(), QuantityExpr::Offset { offset: -1, .. })
                        ));
                    }
                    other => panic!("expected dynamic power multiplier, got {other:?}"),
                },
                other => panic!("expected Pump, got {other:?}"),
            }
        }
    }

    /// CR 706.2 + CR 706.3b: "where X is the result" binds X to the preceding
    /// die roll. Hammer Helper's inline +X/+0 pump must parse as a dynamic
    /// power modification referencing `EventContextAmount`, not be swallowed.
    #[test]
    fn hammer_helper_die_result_pump_parses_dynamic_power_no_warning() {
        let r = parse(
            "Gain control of target creature until end of turn. Untap that creature and roll a six-sided die. Until end of turn, it gains haste and gets +X/+0, where X is the result.",
            "Hammer Helper",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(
            r.parse_warnings
                .iter()
                .all(|warning| warning.to_string().split_whitespace().next()
                    != Some("Swallow:DynamicQty")),
            "unexpected DynamicQty warning: {:?}",
            r.parse_warnings
        );
        assert_eq!(r.abilities.len(), 1);
        // GainControl → Untap → RollDie → GenericEffect
        let generic = r.abilities[0]
            .sub_ability
            .as_ref()
            .and_then(|a| a.sub_ability.as_ref())
            .and_then(|a| a.sub_ability.as_ref())
            .expect("GenericEffect should be the 4th link of the chain");
        let Effect::GenericEffect {
            static_abilities, ..
        } = generic.effect.as_ref()
        else {
            panic!("expected GenericEffect, got {:?}", generic.effect);
        };
        let mods = &static_abilities[0].modifications;
        assert!(
            mods.contains(&ContinuousModification::AddDynamicPower {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
            }),
            "expected AddDynamicPower(EventContextAmount), got {mods:?}"
        );
    }

    #[test]
    fn bhaal_myrkul_half_starting_life_static_has_typed_condition_no_dynamic_qty_warning() {
        for (name, subject) in [
            ("Bane, Lord of Darkness", "Bane"),
            ("Bhaal, Lord of Murder", "Bhaal"),
            ("Myrkul, Lord of Bones", "Myrkul"),
        ] {
            let r = parse(
                &format!(
                    "As long as your life total is less than or equal to half your starting life total, {subject} has indestructible."
                ),
                name,
                &[],
                &["Creature"],
                &[],
            );

            assert_eq!(r.statics.len(), 1, "{name}: {r:#?}");
            assert!(
                r.parse_warnings.iter().all(|warning| warning
                    .to_string()
                    .split_whitespace()
                    .next()
                    != Some("Swallow:DynamicQty")),
                "unexpected DynamicQty warning for {name}: {:?}",
                r.parse_warnings
            );
            assert!(
                r.statics[0]
                    .modifications
                    .contains(&ContinuousModification::AddKeyword {
                        keyword: Keyword::Indestructible,
                    }),
                "expected indestructible grant for {name}: {:?}",
                r.statics[0].modifications
            );
            match r.statics[0]
                .condition
                .as_ref()
                .expect("expected static condition")
            {
                StaticCondition::QuantityComparison {
                    lhs:
                        QuantityExpr::Ref {
                            qty:
                                QuantityRef::LifeTotal {
                                    player: PlayerScope::Controller,
                                },
                        },
                    comparator: Comparator::LE,
                    rhs:
                        QuantityExpr::DivideRounded {
                            inner,
                            divisor: 2,
                            rounding: RoundingMode::Down,
                        },
                } => {
                    assert!(matches!(
                        inner.as_ref(),
                        QuantityExpr::Ref {
                            qty: QuantityRef::StartingLifeTotal
                        }
                    ));
                }
                other => panic!("expected typed half-starting-life comparison, got {other:?}"),
            }
        }
    }

    #[test]
    fn murder_spell_destroy() {
        let r = parse("Destroy target creature.", "Murder", &[], &["Instant"], &[]);
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Spell);
    }

    #[test]
    fn cut_down_destroy_target_uses_total_power_toughness_filter() {
        let r = parse(
            "Destroy target creature with total power and toughness 5 or less.",
            "Cut Down",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let Effect::Destroy { target, .. } = &*r.abilities[0].effect else {
            panic!("expected Destroy effect");
        };
        let TargetFilter::Typed(tf) = target else {
            panic!("expected typed target, got {target:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf.properties.contains(&FilterProp::PtComparison {
            stat: PtStat::TotalPowerToughness,
            scope: PtValueScope::Current,
            comparator: Comparator::LE,
            value: QuantityExpr::Fixed { value: 5 },
        }));
    }

    #[test]
    fn counterspell_spell_counter() {
        let r = parse(
            "Counter target spell.",
            "Counterspell",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
    }

    #[test]
    fn parser_reaches_static_line_for_blocks_each_combat_if_able() {
        let r = parse(
            "This creature blocks each combat if able.",
            "Watchdog",
            &[],
            &["Creature"],
            &["Dog"],
        );
        assert_eq!(r.abilities.len(), 0);
        assert_eq!(r.statics.len(), 1);
        assert_eq!(
            r.statics[0].mode,
            crate::types::statics::StaticMode::MustBlock
        );
    }

    #[test]
    fn parser_reaches_static_line_for_attacks_or_blocks_each_combat_if_able() {
        let r = parse(
            "This creature attacks or blocks each combat if able.",
            "Iron Golem",
            &[],
            &["Creature"],
            &["Golem"],
        );
        assert_eq!(r.abilities.len(), 0, "{r:#?}");
        assert_eq!(r.statics.len(), 2, "{r:#?}");
        assert!(r
            .statics
            .iter()
            .any(|def| def.mode == crate::types::statics::StaticMode::MustAttack));
        assert!(r
            .statics
            .iter()
            .any(|def| def.mode == crate::types::statics::StaticMode::MustBlock));
        assert!(r
            .statics
            .iter()
            .all(|def| def.affected == Some(TargetFilter::SelfRef)));
    }

    #[test]
    fn parser_reaches_static_line_for_other_goblins_attack_each_combat_if_able() {
        let r = parse(
            "Other Goblin creatures you control attack each combat if able.",
            "Goblin Assault",
            &[],
            &["Enchantment"],
            &[],
        );
        assert_eq!(r.abilities.len(), 0, "{r:#?}");
        assert_eq!(r.statics.len(), 1, "{r:#?}");
        assert_eq!(
            r.statics[0].mode,
            crate::types::statics::StaticMode::MustAttack
        );
    }

    #[test]
    fn bonesplitter_static_plus_equip() {
        let r = parse(
            "Equipped creature gets +2/+0.\nEquip {1}",
            "Bonesplitter",
            &[],
            &["Artifact"],
            &["Equipment"],
        );
        assert_eq!(r.statics.len(), 1);
        assert_eq!(r.abilities.len(), 1); // equip ability
    }

    #[test]
    fn rancor_enchant_static_trigger() {
        let r = parse(
            "Enchant creature\nEnchanted creature gets +2/+0 and has trample.\nWhen Rancor is put into a graveyard from the battlefield, return Rancor to its owner's hand.",
            "Rancor",
            &[],
            &["Enchantment"],
            &["Aura"],
        );
        // Enchant line skipped (priority 2)
        assert_eq!(r.statics.len(), 1);
        assert_eq!(r.triggers.len(), 1);
    }

    /// CR 303.4 + CR 601.2i + CR 201.5: Taught by Surrak — a {4}{G} Aura with
    /// a self-cast "draw a card" trigger and an `+2/+2 / haste` static grant on
    /// the enchanted creature. The "Commander enchantment" line is a playtest
    /// mechanic (Unknown Event set, 2023+) and remains intentionally
    /// `Effect::Unimplemented` — implementing zone-following Aura attachment is
    /// non-trivial new infrastructure that is out of scope for this card. The
    /// remaining two abilities (cast trigger + aura static) MUST parse via the
    /// existing class-level patterns.
    #[test]
    fn taught_by_surrak_class_patterns_parse() {
        let oracle = "Commander enchantment (This aura enchants a commander creature, and remains attached to the creature as it moves between any face-up zones. You can cast it on a Commander in your command zone.)\nWhen you cast Taught by Surrak, draw a card.\nEnchanted creature gets +2/+2 and gains haste.";
        let r = parse(oracle, "Taught by Surrak", &[], &["Enchantment"], &["Aura"]);

        // CR 601.2i + CR 603.2: the cast trigger parses with TargetFilter::SelfRef
        // on the source spell and Stack as the active zone (CR 117.2a + CR 113.6).
        assert_eq!(r.triggers.len(), 1, "expected exactly one trigger");
        let trigger = &r.triggers[0];
        assert_eq!(trigger.mode, TriggerMode::SpellCast);
        assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));
        assert!(trigger.trigger_zones.contains(&Zone::Stack));

        // CR 121.1 + CR 603.2: the trigger's effect body is `Effect::Draw` for
        // the controller (TargetFilter::Controller), count = 1.
        let execute = trigger
            .execute
            .as_ref()
            .expect("trigger should have execute body");
        assert!(
            !has_unimplemented(execute),
            "trigger effect should be fully implemented, got {:?}",
            execute.effect
        );
        let Effect::Draw { count, target, .. } = &*execute.effect else {
            panic!(
                "expected Effect::Draw in trigger body, got {:?}",
                execute.effect
            );
        };
        assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
        assert!(
            matches!(target, TargetFilter::Controller),
            "expected TargetFilter::Controller for 'draw a card' \
             (the trigger's controller draws); got {target:?}",
        );

        // CR 303.4 + CR 613.1f + CR 613.4c: the aura's static grant — Haste
        // (layer 6, ability-adding) and +2/+2 (layer 7c, P/T modification)
        // applied to the enchanted creature (TypedFilter::creature() with the
        // EnchantedBy property).
        assert_eq!(r.statics.len(), 1, "expected exactly one static");
        let static_def = &r.statics[0];
        assert_eq!(static_def.mode, StaticMode::Continuous);
        assert_eq!(
            static_def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(static_def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }));
        assert!(static_def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 2 }));
        assert!(static_def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }));

        // CR n/a (playtest, Unknown Event): the "Commander enchantment" line is
        // not implemented — it lands as Effect::Unimplemented carrying the
        // original phrase. Verify the diagnostic is preserved (no silent drop)
        // and that no spurious trigger/static was synthesized from it.
        let unimplemented_count = r
            .abilities
            .iter()
            .filter(|ab| matches!(&*ab.effect, Effect::Unimplemented { .. }))
            .count();
        assert_eq!(
            unimplemented_count,
            1,
            "expected exactly one Unimplemented ability (the Commander \
             enchantment playtest keyword line); got {} unimplemented \
             out of {} total abilities",
            unimplemented_count,
            r.abilities.len()
        );
    }

    #[test]
    fn commander_permission_line_is_deck_construction_text() {
        let r = parse(
            "Teferi, Temporal Archmage can be your commander.",
            "Teferi, Temporal Archmage",
            &[],
            &["Planeswalker"],
            &["Teferi"],
        );

        assert!(r.abilities.is_empty());
        assert!(r.triggers.is_empty());
        assert!(r.statics.is_empty());
        assert!(r.replacements.is_empty());
    }

    // CR 100.2a / CR 903.5b: deck-construction copy-limit sentences parse into a
    // typed `DeckCopyLimit`. The combinator both extracts the value (for deck
    // validation) and recognizes the line so it does not fall through to
    // `Effect::Unimplemented`. Tested over all five real phrase shapes, with the
    // trailing period present (the real Oracle text always carries it).
    #[test]
    fn parse_deck_copy_limit_all_phrase_shapes() {
        // Variant 1: "any number" → Unlimited (Relentless Rats).
        assert_eq!(
            all_consuming(parse_deck_copy_limit)
                .parse("a deck can have any number of cards named relentless rats.")
                .unwrap()
                .1,
            DeckCopyLimit::Unlimited
        );
        // Variant 2: "up to seven" → UpTo(7) (Seven Dwarves).
        assert_eq!(
            all_consuming(parse_deck_copy_limit)
                .parse("a deck can have up to seven cards named seven dwarves.")
                .unwrap()
                .1,
            DeckCopyLimit::UpTo(7)
        );
        // Variant 3: "up to nine" → UpTo(9) (Nazgûl — Unicode subject).
        assert_eq!(
            all_consuming(parse_deck_copy_limit)
                .parse("a deck can have up to nine cards named nazgûl.")
                .unwrap()
                .1,
            DeckCopyLimit::UpTo(9)
        );
        // Variant 4: "only one card named" with DCI em-dash prefix → UpTo(1)
        // (Once More with Feeling). Exercises the singular "card named" matcher.
        assert_eq!(
            all_consuming(parse_deck_copy_limit)
                .parse(
                    "dci ruling \u{2014} a deck can have only one card named once more with feeling."
                )
                .unwrap()
                .1,
            DeckCopyLimit::UpTo(1)
        );
        // Shared singular/plural matcher proof: "up to one card named".
        assert_eq!(
            all_consuming(parse_deck_copy_limit)
                .parse("a deck can have up to one card named x.")
                .unwrap()
                .1,
            DeckCopyLimit::UpTo(1)
        );
        // Variant 5: Megalegendary reminder body → UpTo(1) (Vazal). No subject.
        assert_eq!(
            all_consuming(parse_deck_copy_limit)
                .parse("your deck can have only one copy of this card.")
                .unwrap()
                .1,
            DeckCopyLimit::UpTo(1)
        );
    }

    #[test]
    fn vazal_copy_limit_extracted_from_reminder_body() {
        // Vazal's limit lives only inside the Megalegendary reminder body, so the
        // line scanner must descend into parenthesized text.
        assert_eq!(
            compute_deck_copy_limit_from_text(
                "Megalegendary (Your deck can have only one copy of this card.)"
            ),
            Some(DeckCopyLimit::UpTo(1))
        );
    }

    #[test]
    fn deck_construction_copy_limit_sentence_positive_cases() {
        // All five variants are recognized (consumed silently, not Unimplemented).
        assert!(is_deck_construction_copy_limit_sentence(
            "A deck can have any number of cards named Tempest Hawk."
        ));
        // Engine's normalized self-reference "~".
        assert!(is_deck_construction_copy_limit_sentence(
            "A deck can have any number of cards named ~."
        ));
        // Trailing period is optional.
        assert!(is_deck_construction_copy_limit_sentence(
            "A deck can have any number of cards named Tempest Hawk"
        ));
        // "up to N" is now ACCEPTED (was rejected before typed-limit support).
        assert!(is_deck_construction_copy_limit_sentence(
            "A deck can have up to seven cards named Seven Dwarves."
        ));
        assert!(is_deck_construction_copy_limit_sentence(
            "A deck can have up to nine cards named Nazgûl."
        ));
        // DCI singleton and the bare Megalegendary keyword line.
        assert!(is_deck_construction_copy_limit_sentence(
            "DCI ruling \u{2014} A deck can have only one card named Once More with Feeling."
        ));
        assert!(is_deck_construction_copy_limit_sentence("Megalegendary"));
    }

    #[test]
    fn deck_construction_copy_limit_sentence_negative_cases() {
        // Wrong determiner — "Your deck ... cards named" is not a supported shape.
        assert!(!is_deck_construction_copy_limit_sentence(
            "Your deck can have any number of cards named X."
        ));
        // "can contain" is a different (unsupported) phrasing — out of scope.
        assert!(!is_deck_construction_copy_limit_sentence(
            "A deck can contain any number of cards named X."
        ));
        // Unrelated static lines must not match.
        assert!(!is_deck_construction_copy_limit_sentence(
            "Creatures you control get +1/+1."
        ));
        // Empty subject after the "named " prefix.
        assert!(!is_deck_construction_copy_limit_sentence(
            "A deck can have any number of cards named ."
        ));
        assert!(!is_deck_construction_copy_limit_sentence(
            "A deck can have any number of cards named"
        ));
    }

    #[test]
    fn tempest_hawk_oracle_text_produces_no_unimplemented_static() {
        // Full Oracle text fixture for Tempest Hawk — the bug surface from
        // GitHub issue #1074. Before the fix, the "A deck can have any number
        // of cards named Tempest Hawk." line fell through to
        // Effect::Unimplemented { name: "static_structure", .. }. After the
        // fix, it must be silently consumed.
        let r = parse(
            "Flying\n\
             Whenever this creature deals combat damage to a player, you may search your library for a card named Tempest Hawk, reveal it, put it into your hand, then shuffle.\n\
             A deck can have any number of cards named Tempest Hawk.",
            "Tempest Hawk",
            &[Keyword::Flying],
            &["Creature"],
            &["Bird"],
        );

        // No ability should be Unimplemented with name "static_structure".
        let static_unimplemented: Vec<&AbilityDefinition> = r
            .abilities
            .iter()
            .filter(|a| {
                matches!(
                    &*a.effect,
                    Effect::Unimplemented { name, .. } if name == "static_structure"
                )
            })
            .collect();
        assert!(
            static_unimplemented.is_empty(),
            "deck-construction line must be silently consumed, but produced \
             {} static_structure Unimplemented entries: {:#?}",
            static_unimplemented.len(),
            static_unimplemented
        );
    }

    #[test]
    fn vazal_megalegendary_line_consumed_and_limit_extracted() {
        // CR 100.2a / CR 903.5b: Vazal's "Megalegendary (Your deck can have only
        // one copy of this card.)" line must not surface as Unimplemented, and
        // its UpTo(1) limit must be extractable from the full Oracle text (the
        // limit lives only in the reminder body).
        let vazal_text = "Megalegendary (Your deck can have only one copy of this card.)\n\
             Vigilance, trample\n\
             Vazal, the Compleat has the activated abilities of all other permanents on the battlefield.";
        let r = parse(
            vazal_text,
            "Vazal, the Compleat",
            &[Keyword::Vigilance, Keyword::Trample],
            &["Creature"],
            &["Phyrexian", "Praetor"],
        );
        let megalegendary_unimplemented = r.abilities.iter().any(|a| {
            matches!(
                &*a.effect,
                Effect::Unimplemented { name, .. } if name.eq_ignore_ascii_case("megalegendary")
            )
        });
        assert!(
            !megalegendary_unimplemented,
            "Megalegendary line must be consumed silently, not Unimplemented"
        );
        assert_eq!(
            compute_deck_copy_limit_from_text(vazal_text),
            Some(DeckCopyLimit::UpTo(1))
        );
    }

    #[test]
    fn oracle_text_allows_commander_uses_commander_permission_parser() {
        assert!(oracle_text_allows_commander(
            "Teferi, Temporal Archmage can be your commander.",
            "Teferi, Temporal Archmage",
        ));
        assert!(oracle_text_allows_commander(
            "Spell commander (This card can be your commander. In Limited, it can partner like other monocolored legends.)",
            "Clear, the Mind",
        ));
        assert!(!oracle_text_allows_commander(
            "Teferi, Temporal Archmage can't be your commander.",
            "Teferi, Temporal Archmage",
        ));
    }

    #[test]
    fn non_spell_target_sentence_routes_to_effect_parser() {
        let r = parse(
            "Target player draws a card.",
            "Test Permanent",
            &[],
            &["Artifact"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let Effect::Draw { count, target, .. } = &*r.abilities[0].effect else {
            panic!("expected Effect::Draw, got {:?}", r.abilities[0].effect);
        };
        assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
        // CR 601.2c: "Target player draws ..." selects a player target during
        // spell announcement — the parsed Draw must carry a Player filter, not
        // Controller (which would always draw for the caster).
        assert!(
            matches!(target, TargetFilter::Player),
            "expected TargetFilter::Player for 'Target player draws a card.', got {target:?}",
        );
    }

    #[test]
    fn ashlings_command_modal_target_player_draws_carries_player_filter() {
        // CR 601.2c + CR 700.2: Each "target player" mode-clause of a modal
        // spell is an independent target chosen during spell announcement.
        // Mode 2 ("Target player draws two cards") MUST surface a Player
        // target on the parsed Draw effect so `collect_target_slots` emits
        // an independent slot per Draw mode (otherwise the caster always draws).
        let r = parse(
            "Choose two —\n\
             • Create a token that's a copy of target Elemental you control.\n\
             • Target player draws two cards.\n\
             • Ashling's Command deals 2 damage to each creature target player controls.\n\
             • Target player creates two Treasure tokens.",
            "Ashling's Command",
            &[],
            &["Instant"],
            &[],
        );
        // Modal spell exposes one ability with chained sub_ability per mode.
        // Find the Draw clause anywhere in the chain and assert its target.
        fn find_draw(
            ab: &crate::types::ability::AbilityDefinition,
        ) -> Option<&crate::types::ability::TargetFilter> {
            if let Effect::Draw { target, .. } = &*ab.effect {
                return Some(target);
            }
            ab.sub_ability.as_deref().and_then(find_draw)
        }
        let mut draw_target = None;
        for ab in r.abilities.iter() {
            if let Some(t) = find_draw(ab) {
                draw_target = Some(t);
                break;
            }
        }
        let target = draw_target.expect("expected a Draw effect somewhere in the modal chain");
        assert!(
            matches!(target, TargetFilter::Player),
            "Mode 2 Draw must carry TargetFilter::Player so each modal mode \
             surfaces an independent target slot, got {target:?}",
        );
    }

    #[test]
    fn ashlings_command_modal_target_player_creates_tokens_carries_player_filter() {
        // CR 111.2 + CR 601.2c: Each "Target player creates ..." mode-clause
        // of a modal spell is an independent target chosen during spell
        // announcement. Mode 4 of Ashling's Command MUST surface a Player
        // filter on the parsed Token effect's `owner` field so
        // `collect_target_slots` emits an independent slot per token mode
        // (otherwise the caster always creates the tokens).
        let r = parse(
            "Choose two —\n\
             • Create a token that's a copy of target Elemental you control.\n\
             • Target player draws two cards.\n\
             • Ashling's Command deals 2 damage to each creature target player controls.\n\
             • Target player creates two Treasure tokens.",
            "Ashling's Command",
            &[],
            &["Instant"],
            &[],
        );
        fn find_token(
            ab: &crate::types::ability::AbilityDefinition,
        ) -> Option<&crate::types::ability::TargetFilter> {
            if let Effect::Token { owner, .. } = &*ab.effect {
                return Some(owner);
            }
            ab.sub_ability.as_deref().and_then(find_token)
        }
        // Find a Token effect whose owner is `Player` (mode 4). Mode 1 also
        // creates a token but its owner is `Controller`, so we keep searching.
        let mut owner_target = None;
        for ab in r.abilities.iter() {
            // Walk the entire chain, collecting any Player-owner Token we see.
            let mut cur: Option<&crate::types::ability::AbilityDefinition> = Some(ab);
            while let Some(node) = cur {
                if let Some(t) = find_token(node) {
                    if matches!(t, TargetFilter::Player) {
                        owner_target = Some(t);
                        break;
                    }
                }
                cur = node.sub_ability.as_deref();
            }
            if owner_target.is_some() {
                break;
            }
        }
        let target = owner_target
            .expect("expected a Token effect with TargetFilter::Player owner in the modal chain");
        assert!(
            matches!(target, TargetFilter::Player),
            "Mode 4 Token must carry owner=TargetFilter::Player so each modal \
             mode surfaces an independent target slot, got {target:?}",
        );
    }

    #[test]
    fn modal_target_player_creates_spawn_tokens_with_quoted_mana_ability() {
        let r = parse(
            "Choose two —\n\
             • Target player creates X 0/1 colorless Eldrazi Spawn creature tokens with \"Sacrifice this token: Add {C}.\"\n\
             • Target player scries X, then draws a card.",
            "Kozilek's Command",
            &[],
            &["Kindred", "Instant"],
            &["Eldrazi"],
        );

        let first_mode = r.abilities.first().expect("first mode");
        match &*first_mode.effect {
            Effect::Token {
                name,
                power,
                toughness,
                types,
                colors,
                count,
                owner,
                static_abilities,
                ..
            } => {
                assert_eq!(name, "Eldrazi Spawn");
                assert_eq!(power, &PtValue::Fixed(0));
                assert_eq!(toughness, &PtValue::Fixed(1));
                assert_eq!(
                    types,
                    &vec![
                        "Creature".to_string(),
                        "Eldrazi".to_string(),
                        "Spawn".to_string()
                    ]
                );
                assert!(colors.is_empty());
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Variable { name }
                    } if name == "X"
                ));
                assert_eq!(owner, &TargetFilter::Player);
                assert!(static_abilities.iter().any(|static_definition| {
                    static_definition.modifications.iter().any(|modification| {
                        matches!(
                            modification,
                            ContinuousModification::GrantAbility { definition }
                                if matches!(*definition.effect, Effect::Mana { .. })
                                    && matches!(
                                        definition.cost,
                                        Some(AbilityCost::Sacrifice {
                                            target: TargetFilter::SelfRef,
                                            count: 1
                                        })
                                    )
                        )
                    })
                }));
            }
            other => panic!("expected first mode Token, got {other:?}"),
        }
    }

    /// CR 700.2 + CR 700.2c + CR 601.2b: Kozilek's Command is a four-mode
    /// "Choose two —" instant whose X threads through every mode. This pins the
    /// full parsed shape so a regression in any single mode (or in the modal
    /// metadata) is caught at the parser layer before the runtime tests in
    /// `crates/engine/src/game/casting.rs` exercise the cast pipeline.
    #[test]
    fn kozileks_command_full_four_mode_parse() {
        let r = parse(
            "Choose two —\n\
             • Target player creates X 0/1 colorless Eldrazi Spawn creature tokens with \"Sacrifice this token: Add {C}.\"\n\
             • Target player scries X, then draws a card.\n\
             • Exile target creature with mana value X or less.\n\
             • Exile up to X target cards from graveyards.",
            "Kozilek's Command",
            &[],
            &["Kindred", "Instant"],
            &["Eldrazi"],
        );

        // CR 700.2 + CR 700.2d: four selectable modes, exactly two chosen.
        assert_eq!(
            r.abilities.len(),
            4,
            "Kozilek's Command must parse four modal modes, got {}",
            r.abilities.len()
        );
        let modal = r
            .modal
            .expect("Kozilek's Command must carry modal metadata");
        assert_eq!(modal.min_choices, 2);
        assert_eq!(modal.max_choices, 2);
        assert_eq!(modal.mode_count, 4);
        assert_eq!(
            modal.mode_descriptions.len(),
            4,
            "every mode must surface a description string for the targeting UI"
        );

        // Mode 0 — "Target player creates X 0/1 colorless Eldrazi Spawn tokens
        // with quoted Sacrifice: Add {C} ability." Owner is the targeted player
        // (CR 601.2c), count is the announced X (CR 107.3), and the granted
        // activated ability sacrifices the token (CR 701.21 — Sacrifice keyword
        // action) to add {C}.
        match &*r.abilities[0].effect {
            Effect::Token {
                name,
                power,
                toughness,
                colors,
                count,
                owner,
                static_abilities,
                ..
            } => {
                assert_eq!(name, "Eldrazi Spawn");
                assert_eq!(power, &PtValue::Fixed(0));
                assert_eq!(toughness, &PtValue::Fixed(1));
                assert!(colors.is_empty(), "Eldrazi Spawn is colorless");
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Variable { name }
                        } if name == "X"
                    ),
                    "token count must be the announced X, got {count:?}"
                );
                assert_eq!(
                    owner,
                    &TargetFilter::Player,
                    "mode 0 must surface an independent player target for the token owner"
                );
                assert!(
                    static_abilities.iter().any(|static_definition| {
                        static_definition.modifications.iter().any(|modification| {
                            matches!(
                                modification,
                                ContinuousModification::GrantAbility { definition }
                                    if matches!(*definition.effect, Effect::Mana { .. })
                                        && matches!(
                                            definition.cost,
                                            Some(AbilityCost::Sacrifice {
                                                target: TargetFilter::SelfRef,
                                                count: 1
                                            })
                                        )
                            )
                        })
                    }),
                    "Eldrazi Spawn must grant 'Sacrifice this token: Add {{C}}'"
                );
            }
            other => panic!("expected mode 0 Token, got {other:?}"),
        }

        // Mode 1 — "Target player scries X, then draws a card." The scry count
        // is the announced X (CR 701.22a), routed to the chosen player
        // (CR 601.2c), and a Draw follows in the sub-ability chain.
        match &*r.abilities[1].effect {
            Effect::Scry { count, target } => {
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Variable { name }
                        } if name == "X"
                    ),
                    "scry count must be the announced X, got {count:?}"
                );
                assert_eq!(
                    target,
                    &TargetFilter::Player,
                    "scry must route to the chosen target player"
                );
            }
            other => panic!("expected mode 1 Scry, got {other:?}"),
        }
        fn find_draw(
            ab: &crate::types::ability::AbilityDefinition,
        ) -> Option<&crate::types::ability::Effect> {
            if matches!(&*ab.effect, Effect::Draw { .. }) {
                return Some(&ab.effect);
            }
            ab.sub_ability.as_deref().and_then(find_draw)
        }
        assert!(
            find_draw(&r.abilities[1]).is_some(),
            "mode 1 must chain a Draw after the scry ('then draws a card')"
        );

        // Mode 2 — "Exile target creature with mana value X or less." This is
        // the X-dependent target legality that gates the deferred-target flow
        // (CR 202.3 mana value + CR 601.2b X-before-targets). Exile keyword
        // action is CR 701.13; destination is the exile zone (CR 406).
        match &*r.abilities[2].effect {
            Effect::ChangeZone {
                origin,
                destination,
                target,
                ..
            } => {
                assert_eq!(*destination, Zone::Exile, "mode 2 exiles the creature");
                assert!(
                    origin.is_none() || *origin == Some(Zone::Battlefield),
                    "mode 2 exiles a battlefield creature, got origin {origin:?}"
                );
                let TargetFilter::Typed(typed) = target else {
                    panic!("mode 2 target must be a typed creature filter, got {target:?}");
                };
                assert!(
                    typed.type_filters.contains(&TypeFilter::Creature),
                    "mode 2 must target a creature, got {:?}",
                    typed.type_filters
                );
                let cmc = typed
                    .properties
                    .iter()
                    .find_map(|prop| match prop {
                        FilterProp::Cmc { comparator, value } => Some((comparator, value)),
                        _ => None,
                    })
                    .expect("mode 2 must carry a Cmc filter prop");
                assert_eq!(
                    *cmc.0,
                    Comparator::LE,
                    "'mana value X or less' must parse as a <= bound"
                );
                assert!(
                    matches!(
                        cmc.1,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Variable { name }
                        } if name == "X"
                    ),
                    "mode 2 Cmc bound must be the announced X, got {:?}",
                    cmc.1
                );
            }
            other => panic!("expected mode 2 ChangeZone→Exile, got {other:?}"),
        }

        // Mode 3 — "Exile up to X target cards from graveyards." A variable
        // ("up to X") multi-target (CR 601.2c) whose maximum is the announced X,
        // exiling cards from the graveyard zone (CR 701.13 + CR 406). The
        // graveyard origin lives on the target filter; the up-to bound lives on
        // the ability's `multi_target` spec.
        let mode3 = &r.abilities[3];
        match &*mode3.effect {
            Effect::ChangeZone {
                destination,
                target,
                ..
            } => {
                assert_eq!(*destination, Zone::Exile, "mode 3 exiles the cards");
                assert_eq!(
                    target.extract_in_zone(),
                    Some(Zone::Graveyard),
                    "mode 3 must target cards in a graveyard, got {target:?}"
                );
                // Optionality ("up to X" => 0..=X) is asserted below via the
                // MultiTargetSpec floor of zero, the source of truth for
                // multi-target modes; the ChangeZone `up_to` bool is not.
            }
            other => panic!("expected mode 3 ChangeZone→Exile, got {other:?}"),
        }
        let spec = mode3
            .multi_target
            .as_ref()
            .expect("mode 3 'up to X target cards' must carry a MultiTargetSpec");
        assert_eq!(
            spec.min,
            QuantityExpr::Fixed { value: 0 },
            "'up to X' has a floor of zero targets"
        );
        assert_eq!(
            spec.max,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string()
                }
            }),
            "'up to X' maximum must be the announced X, got {:?}",
            spec.max
        );
    }

    #[test]
    fn target_player_scrys_carries_player_filter() {
        // CR 701.22a + CR 601.2c: "Target player scrys N" surfaces an
        // independent player target on the parsed Scry effect — the resolver
        // routes the scry to the chosen player, not the spell's controller.
        let r = parse(
            "Target player scries 2.",
            "Test Permanent",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let Effect::Scry { count, target } = &*r.abilities[0].effect else {
            panic!("expected Effect::Scry, got {:?}", r.abilities[0].effect);
        };
        assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
        assert!(
            matches!(target, TargetFilter::Player),
            "expected TargetFilter::Player for 'Target player scries 2.', got {target:?}",
        );
    }

    #[test]
    fn target_player_surveils_carries_player_filter() {
        // CR 701.25a + CR 601.2c: "Target player surveils N" surfaces an
        // independent player target on the parsed Surveil effect — the
        // resolver routes the surveil to the chosen player, not the spell's
        // controller. (Mirrors the Draw + Scry tests above.)
        let r = parse(
            "Target player surveils 2.",
            "Test Permanent",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let Effect::Surveil { count, target } = &*r.abilities[0].effect else {
            panic!("expected Effect::Surveil, got {:?}", r.abilities[0].effect);
        };
        assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
        assert!(
            matches!(target, TargetFilter::Player),
            "expected TargetFilter::Player for 'Target player surveils 2.', got {target:?}",
        );
    }

    #[test]
    fn target_player_mills_carries_player_filter() {
        // CR 701.13a + CR 601.2c: "Target player mills N" surfaces an
        // independent player target on the parsed Mill effect — the resolver
        // routes the mill to the chosen player, not the spell's controller.
        // Mirror coverage for the Scry/Surveil tests above so the conjugated
        // verb path ("mills" via y/s normalization) is pinned for regression.
        let r = parse(
            "Target player mills 3.",
            "Test Permanent",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let Effect::Mill { count, target, .. } = &*r.abilities[0].effect else {
            panic!("expected Effect::Mill, got {:?}", r.abilities[0].effect);
        };
        assert_eq!(*count, QuantityExpr::Fixed { value: 3 });
        assert!(
            matches!(target, TargetFilter::Player),
            "expected TargetFilter::Player for 'Target player mills 3.', got {target:?}",
        );
    }

    #[test]
    fn non_spell_conditional_sentence_routes_to_effect_parser() {
        let r = parse(
            "If you sacrificed a Food this turn, draw a card.",
            "Test Permanent",
            &[],
            &["Enchantment"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
    }

    #[test]
    fn devourer_of_destiny_opening_hand_reveal_creates_first_upkeep_dig() {
        let r = parse(
            "You may reveal this card from your opening hand. If you do, at the beginning of your first upkeep, look at the top four cards of your library. You may put one of those cards back on top of your library. Exile the rest.\nWhen you cast this spell, exile target permanent that's one or more colors.",
            "Devourer of Destiny",
            &[],
            &["Creature"],
            &["Eldrazi"],
        );

        assert_eq!(r.abilities.len(), 1);
        let begin_game = &r.abilities[0];
        assert_eq!(begin_game.kind, AbilityKind::BeginGame);
        assert!(begin_game.optional);
        assert!(matches!(
            &*begin_game.effect,
            Effect::Reveal {
                target: TargetFilter::SelfRef
            }
        ));

        let delayed = begin_game
            .sub_ability
            .as_deref()
            .expect("reveal should create a delayed first-upkeep trigger");
        let Effect::CreateDelayedTrigger {
            condition, effect, ..
        } = &*delayed.effect
        else {
            panic!("expected CreateDelayedTrigger, got {:?}", delayed.effect);
        };
        assert_eq!(
            condition,
            &DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::Upkeep,
                player: PlayerId(0),
            }
        );

        let Effect::Dig {
            count,
            destination,
            keep_count,
            up_to,
            filter,
            rest_destination,
            reveal,
            ..
        } = &*effect.effect
        else {
            panic!("expected Dig payload, got {:?}", effect.effect);
        };
        assert_eq!(*count, QuantityExpr::Fixed { value: 4 });
        assert_eq!(*destination, Some(Zone::Library));
        assert_eq!(*keep_count, Some(1));
        assert!(*up_to);
        assert!(matches!(filter, TargetFilter::Any));
        assert_eq!(*rest_destination, Some(Zone::Exile));
        assert!(!reveal);
    }

    /// CR 103.6a + CR 122.1 + CR 701.13a: Gemstone Caverns' begin-game line must
    /// capture BOTH the "with a luck counter on it" entry counter AND the
    /// "If you do, exile a card from your hand" dependent sub-ability gated by
    /// `IfYouDo` — neither may be silently dropped.
    #[test]
    fn gemstone_caverns_begin_game_captures_counter_and_exile_sub_ability() {
        let r = parse(
            "If this card is in your opening hand and you're not the starting player, you may begin the game with Gemstone Caverns on the battlefield with a luck counter on it. If you do, exile a card from your hand.",
            "Gemstone Caverns",
            &[],
            &["Land"],
            &[],
        );

        assert_eq!(r.abilities.len(), 1);
        let begin_game = &r.abilities[0];
        assert_eq!(begin_game.kind, AbilityKind::BeginGame);
        assert!(begin_game.optional);

        let Effect::ChangeZone {
            destination,
            origin,
            target,
            enter_with_counters,
            ..
        } = &*begin_game.effect
        else {
            panic!("expected ChangeZone, got {:?}", begin_game.effect);
        };
        assert_eq!(*destination, Zone::Battlefield);
        assert_eq!(*origin, Some(Zone::Hand));
        assert!(matches!(target, TargetFilter::SelfRef));
        assert_eq!(
            enter_with_counters,
            &vec![(
                crate::types::counter::CounterType::Generic("luck".to_string()),
                QuantityExpr::Fixed { value: 1 },
            )],
        );

        let sub = begin_game
            .sub_ability
            .as_deref()
            .expect("'If you do, exile a card from your hand' must create a sub-ability");
        assert_eq!(sub.condition, Some(AbilityCondition::effect_performed()));
        assert!(
            !has_unimplemented(sub),
            "exile-from-hand sub-ability must not be Unimplemented: {:?}",
            sub.effect
        );
        assert!(matches!(
            &*sub.effect,
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Exile,
                ..
            }
        ));
    }

    /// A Leyline-style begin-game line carries no counter clause and no
    /// "If you do" follow-up — the optional clauses must be truly optional so
    /// the branch is not over-fitted to Gemstone Caverns.
    #[test]
    fn leyline_begin_game_has_no_counters_or_sub_ability() {
        let r = parse(
            "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou have hexproof.",
            "Leyline of Sanctity",
            &[],
            &["Enchantment"],
            &[],
        );

        let begin_game = r
            .abilities
            .iter()
            .find(|a| a.kind == AbilityKind::BeginGame)
            .expect("Leyline begin-game ability must parse");
        assert!(begin_game.optional);
        assert!(begin_game.sub_ability.is_none());
        let Effect::ChangeZone {
            enter_with_counters,
            ..
        } = &*begin_game.effect
        else {
            panic!("expected ChangeZone, got {:?}", begin_game.effect);
        };
        assert!(enter_with_counters.is_empty());
    }

    /// CR 103.5b: Serum Powder's mulligan-time ability must classify as
    /// `AbilityKind::Mulligan` with a non-Unimplemented effect. Runtime
    /// dispatch lives in `mulligan.rs::handle_serum_powder`; the stack guard
    /// in `effects/mod.rs` ensures this ability never resolves through
    /// normal stack resolution.
    #[test]
    fn serum_powder_mulligan_ability_classifies_as_mulligan_kind() {
        let r = parse(
            "{T}: Add {C}.\nAny time you could mulligan and this card is in your hand, you may exile all the cards from your hand, then draw that many cards.",
            "Serum Powder",
            &[],
            &["Artifact"],
            &[],
        );

        assert_eq!(r.abilities.len(), 2);
        // structural: not dispatch — iterator search over parsed ability list in test
        let mulligan = r
            .abilities
            .iter()
            .find(|a| a.kind == AbilityKind::Mulligan)
            .expect("mulligan-time ability should be classified as AbilityKind::Mulligan");
        assert!(mulligan.optional);
        assert!(
            !matches!(&*mulligan.effect, Effect::Unimplemented { .. }),
            "mulligan ability must not be Unimplemented, got {:?}",
            mulligan.effect
        );
    }

    #[test]
    fn player_shroud_routes_to_static_parser() {
        let r = parse("You have shroud.", "Ivory Mask", &[], &["Enchantment"], &[]);
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert_eq!(r.statics[0].mode, crate::types::statics::StaticMode::Shroud);
    }

    #[test]
    fn top_of_library_peek_routes_to_static_parser() {
        let r = parse(
            "You may look at the top card of your library any time.",
            "Bolas's Citadel",
            &[],
            &["Artifact"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert_eq!(
            r.statics[0].mode,
            crate::types::statics::StaticMode::MayLookAtTopOfLibrary
        );
    }

    #[test]
    fn lose_all_abilities_routes_to_static_parser() {
        let r = parse(
            "Cards in graveyards lose all abilities.",
            "Yixlid Jailer",
            &[],
            &["Creature"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert!(r.statics[0]
            .modifications
            .contains(&crate::types::ability::ContinuousModification::RemoveAllAbilities));
    }

    #[test]
    fn colored_creature_lord_routes_to_static_parser() {
        let r = parse(
            "Black creatures get +1/+1.",
            "Bad Moon",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert!(r.statics[0]
            .modifications
            .contains(&crate::types::ability::ContinuousModification::AddPower { value: 1 }));
    }

    #[test]
    fn filtered_creatures_you_control_route_to_static_parser() {
        let r = parse(
            "Creatures you control with mana value 3 or less get +1/+0.",
            "Hero of the Dunes",
            &[],
            &["Creature"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert!(matches!(
            r.statics[0].affected,
            Some(crate::types::ability::TargetFilter::Typed(
                crate::types::ability::TypedFilter {
                    controller: Some(crate::types::ability::ControllerRef::You),
                    ..
                }
            ))
        ));
    }

    #[test]
    fn favorable_winds_routes_to_static_parser() {
        let r = parse(
            "Creatures you control with flying get +1/+1.",
            "Favorable Winds",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert!(matches!(
            r.statics[0].affected,
            Some(crate::types::ability::TargetFilter::Typed(
                crate::types::ability::TypedFilter {
                    controller: Some(crate::types::ability::ControllerRef::You),
                    ref properties,
                    ..
                }
            )) if properties == &vec![crate::types::ability::FilterProp::WithKeyword {
                value: Keyword::Flying,
            }]
        ));
    }

    #[test]
    fn must_attack_routes_to_static_parser() {
        let r = parse(
            "This creature attacks each combat if able.",
            "Primordial Ooze",
            &[],
            &["Creature"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert_eq!(
            r.statics[0].mode,
            crate::types::statics::StaticMode::MustAttack
        );
    }

    #[test]
    fn thassas_oracle_win_condition_gated_by_devotion_vs_library() {
        // GH #582 — CR 104.2b + CR 107.3i + CR 608.2c + CR 700.5: Thassa's
        // Oracle's chained WinTheGame sub_ability must be gated by a typed
        // `AbilityCondition::QuantityCheck` comparing devotion-to-blue
        // against the controller's library size. The X binding from sentence
        // 1 ("where X is your devotion to blue") must forward-fill across
        // the sentence boundary into sentence 3's "If X is greater than or
        // equal to ...", and the X-substitution post-pass must recurse into
        // the chained sub_ability's `condition` slot.
        let r = parse(
            "When this creature enters, look at the top X cards of your library, where X is your devotion to blue. Put up to one of them on top of your library and the rest on the bottom of your library in a random order. If X is greater than or equal to the number of cards in your library, you win the game.",
            "Thassa's Oracle",
            &[],
            &["Creature"],
            &["Merfolk", "Wizard"],
        );
        assert_eq!(r.triggers.len(), 1, "expected single ETB trigger");
        let exec = r.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute body");
        // Walk to the innermost SequentialSibling chain — the WinTheGame node.
        let mut node = exec;
        while let Some(sub) = node.sub_ability.as_ref() {
            if matches!(
                *sub.effect,
                crate::types::ability::Effect::WinTheGame { .. }
            ) {
                node = sub;
                break;
            }
            node = sub;
        }
        assert!(
            matches!(
                *node.effect,
                crate::types::ability::Effect::WinTheGame { .. }
            ),
            "expected to find WinTheGame in the SequentialSibling chain, got {:?}",
            node.effect
        );
        let cond = node
            .condition
            .as_ref()
            .expect("WinTheGame must be gated by a condition, not unconditional");
        match cond {
            crate::types::ability::AbilityCondition::QuantityCheck {
                lhs,
                comparator,
                rhs,
            } => {
                assert_eq!(*comparator, crate::types::ability::Comparator::GE);
                // LHS must be Devotion (NOT Variable("X")) — proves Step 1b
                // forward-fill AND Step 3 condition recursion both fired.
                match lhs {
                    crate::types::ability::QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::Devotion { .. },
                    } => {}
                    other => panic!(
                        "lhs must be Devotion (forward-fill + condition X-subst applied); got {other:?}"
                    ),
                }
                // RHS: cards in your library.
                match rhs {
                    crate::types::ability::QuantityExpr::Ref {
                        qty:
                            crate::types::ability::QuantityRef::ZoneCardCount {
                                zone: crate::types::ability::ZoneRef::Library,
                                scope: crate::types::ability::CountScope::Controller,
                                ..
                            },
                    } => {}
                    other => {
                        panic!("rhs must be ZoneCardCount{{Library, Controller}}; got {other:?}")
                    }
                }
            }
            other => panic!("expected AbilityCondition::QuantityCheck, got {other:?}"),
        }
        // CR L4: no Condition_If SwallowedClause remains for this trigger body.
        assert!(
            r.parse_warnings.iter().all(|w| !matches!(
                w,
                OracleDiagnostic::SwallowedClause { detector, .. } if detector == "Condition_If"
            )),
            "unexpected Condition_If SwallowedClause: {:?}",
            r.parse_warnings
        );
    }

    #[test]
    fn incubate_parses_as_effect() {
        let r = parse(
            "When this creature enters, incubate 3.",
            "Converter Beast",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1);
        let trigger_def = r.triggers[0].execute.as_ref().unwrap();
        assert!(
            matches!(&*trigger_def.effect, crate::types::ability::Effect::Incubate { count }
                if matches!(count, crate::types::ability::QuantityExpr::Fixed { value: 3 })),
            "Expected Incubate {{ count: Fixed(3) }}, got {:?}",
            trigger_def.effect
        );
    }

    #[test]
    fn attack_this_turn_if_able_parses_as_effect() {
        let r = parse(
            "Target creature attacks this turn if able.\nDraw a card.",
            "Boiling Blood",
            &[],
            &["Instant"],
            &[],
        );
        assert!(!r.abilities.is_empty());
        assert!(
            matches!(
                &*r.abilities[0].effect,
                crate::types::ability::Effect::GenericEffect {
                    static_abilities,
                    ..
                } if !static_abilities.is_empty()
                    && static_abilities[0].mode == crate::types::statics::StaticMode::MustAttack
            ),
            "Expected GenericEffect with MustAttack, got {:?}",
            r.abilities[0].effect
        );
    }

    #[test]
    fn no_maximum_hand_size_routes_to_static_parser() {
        let r = parse(
            "You have no maximum hand size.",
            "Spellbook",
            &[],
            &["Artifact"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert_eq!(
            r.statics[0].mode,
            crate::types::statics::StaticMode::NoMaximumHandSize
        );
    }

    #[test]
    fn block_restriction_routes_to_static_parser() {
        let r = parse(
            "This creature can block only creatures with flying.",
            "Cloud Pirates",
            &[],
            &["Creature"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert_eq!(
            r.statics[0].mode,
            crate::types::statics::StaticMode::BlockRestriction
        );
    }

    #[test]
    fn granted_activated_static_routes_before_colon_parse() {
        let r = parse(
            "Enchanted land has \"{T}: Add two mana of any one color.\"",
            "Gift of Paradise",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        let grant = r.statics[0].modifications.iter().find(|m| {
            matches!(
                m,
                crate::types::ability::ContinuousModification::GrantAbility { .. }
            )
        });
        assert!(
            grant.is_some(),
            "should contain a GrantAbility modification"
        );
        if let crate::types::ability::ContinuousModification::GrantAbility { definition } =
            grant.unwrap()
        {
            assert_eq!(
                definition.kind,
                crate::types::ability::AbilityKind::Activated
            );
        }
    }

    #[test]
    fn spell_targets_attacking_or_blocking_creature_as_disjunction() {
        let r = parse(
            "Joust Through deals 3 damage to target attacking or blocking creature. You gain 1 life.",
            "Joust Through",
            &[],
            &["Instant"],
            &[],
        );

        assert_eq!(r.abilities.len(), 1);
        let Effect::DealDamage { target, .. } = &*r.abilities[0].effect else {
            panic!("expected DealDamage, got {:?}", r.abilities[0].effect);
        };
        let TargetFilter::Or { filters } = target else {
            panic!("expected Or target, got {target:?}");
        };
        assert_eq!(filters.len(), 2);
        for (filter, property) in [
            (&filters[0], FilterProp::Attacking),
            (&filters[1], FilterProp::Blocking),
        ] {
            let TargetFilter::Typed(typed) = filter else {
                panic!("expected Typed branch, got {filter:?}");
            };
            assert!(typed.type_filters.contains(&TypeFilter::Creature));
            assert!(typed.properties.contains(&property));
        }
        assert!(matches!(
            r.abilities[0]
                .sub_ability
                .as_deref()
                .map(|def| &*def.effect),
            Some(Effect::GainLife { .. })
        ));
    }

    #[test]
    fn quoted_granted_ability_is_not_misclassified_as_activated() {
        let r = parse(
            "White creatures you control have \"{T}: You gain 1 life.\"",
            "Resplendent Mentor",
            &[],
            &["Creature"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
    }

    #[test]
    fn spell_grants_quoted_ability_to_outlaw_creatures() {
        let r = parse(
            "Until end of turn, outlaw creatures you control get +1/+0 and gain \"{T}: This creature deals damage equal to its power to target creature.\"",
            "Dead Before Sunrise",
            &[],
            &["Instant"],
            &[],
        );

        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].duration, Some(Duration::UntilEndOfTurn));
        let Effect::GenericEffect {
            static_abilities, ..
        } = &*r.abilities[0].effect
        else {
            panic!("expected GenericEffect, got {:?}", r.abilities[0].effect);
        };
        assert_eq!(static_abilities.len(), 1);
        let static_def = &static_abilities[0];
        let Some(TargetFilter::Typed(affected)) = &static_def.affected else {
            panic!(
                "expected typed affected filter, got {:?}",
                static_def.affected
            );
        };
        assert_eq!(affected.controller, Some(ControllerRef::You));
        assert!(affected.type_filters.contains(&TypeFilter::Creature));
        assert!(affected.type_filters.iter().any(|type_filter| {
            matches!(type_filter, TypeFilter::AnyOf(filters) if filters.len() == 5)
        }));
        assert!(static_def.modifications.iter().any(|modification| {
            matches!(modification, ContinuousModification::AddPower { value: 1 })
        }));
        assert!(static_def.modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::GrantAbility { definition }
                    if matches!(&*definition.effect, Effect::DealDamage { .. })
            )
        }));
    }

    #[test]
    fn quoted_spell_grant_does_not_absorb_next_line_delayed_trigger() {
        let r = parse(
            "Until end of turn, target creature gains haste and \"{0}: Untap this creature. Activate only once.\"\nDraw a card at the beginning of the next turn's upkeep.",
            "Touch of Vitae",
            &[],
            &["Instant"],
            &[],
        );

        assert!(
            r.parse_warnings
                .iter()
                .all(|warning| !matches!(warning, OracleDiagnostic::CascadeLoss { .. })),
            "unexpected cascade-loss warning: {:?}",
            r.parse_warnings
        );
        assert_eq!(r.abilities.len(), 2);

        let first = &r.abilities[0];
        assert_eq!(
            first.duration,
            Some(crate::types::ability::Duration::UntilEndOfTurn)
        );
        let Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } = &*first.effect
        else {
            panic!("expected immediate GenericEffect, got {:?}", first.effect);
        };
        assert!(matches!(target, Some(TargetFilter::Typed(_))));
        assert!(static_abilities.iter().any(|static_def| {
            static_def.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::GrantAbility { definition }
                        if matches!(&*definition.effect, Effect::Untap { .. })
                )
            })
        }));

        assert!(matches!(
            *r.abilities[1].effect,
            Effect::CreateDelayedTrigger { .. }
        ));
    }

    #[test]
    fn activated_as_sorcery_constraint_sets_sorcery_speed() {
        let r = parse(
            "{2}{W}, Sacrifice this artifact: Target creature you control gets +2/+2 and gains flying until end of turn. Draw a card. Activate only as a sorcery.",
            "Basilica Skullbomb",
            &[],
            &["Artifact"],
            &[],
        );

        assert_eq!(r.abilities.len(), 1);
        assert!(r.abilities[0].sorcery_speed);
        assert!(r.abilities[0]
            .activation_restrictions
            .contains(&crate::types::ability::ActivationRestriction::AsSorcery));
        let draw = r.abilities[0]
            .sub_ability
            .as_ref()
            .expect("expected draw follow-up");
        assert!(matches!(
            *draw.effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
        let no_activate_tail = draw
            .sub_ability
            .as_ref()
            .is_none_or(|tail| !matches!(*tail.effect, Effect::Unimplemented { ref name, .. } if name == "activate"));
        assert!(no_activate_tail);
    }

    #[test]
    fn owen_grady_shared_noun_counter_choice_activated() {
        use crate::types::counter::CounterType;

        // CR 122.1b shared-noun counter choice on an activate-as-a-sorcery
        // ability: "{T}: Put your choice of a menace, trample, reach, or haste
        // counter on target Dinosaur. Activate only as a sorcery."
        let r = parse(
            "{T}: Put your choice of a menace, trample, reach, or haste counter on target Dinosaur. Activate only as a sorcery.",
            "Owen Grady, Raptor Trainer",
            &[],
            &["Creature"],
            &["Human"],
        );

        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];

        // Tap cost + sorcery-speed activation restriction.
        assert_eq!(ability.cost, Some(crate::types::ability::AbilityCost::Tap));
        assert!(ability.sorcery_speed);
        assert!(ability
            .activation_restrictions
            .contains(&crate::types::ability::ActivationRestriction::AsSorcery));

        // The shared "on target Dinosaur" target is lifted to the TargetOnly head.
        assert!(
            matches!(&*ability.effect, Effect::TargetOnly { .. }),
            "expected TargetOnly head, got {:?}",
            ability.effect
        );
        let head_target = ability
            .effect
            .target_filter()
            .expect("TargetOnly head must surface its shared target");
        assert!(
            // allow-noncombinator: test assertion on Debug output, not parsing dispatch
            format!("{head_target:?}").contains("Dinosaur"),
            "expected shared target to be a Dinosaur filter, got {head_target:?}"
        );

        // Body is the ChooseOneOf with 4 keyword PutCounter branches on ParentTarget.
        let choice = ability
            .sub_ability
            .as_deref()
            .expect("counter choice must be chained as a sub-ability");
        let Effect::ChooseOneOf { chooser, branches } = &*choice.effect else {
            panic!("expected ChooseOneOf sub-ability, got {:?}", choice.effect);
        };
        assert_eq!(*chooser, PlayerFilter::Controller);
        assert_eq!(branches.len(), 4);

        let expected = [
            KeywordKind::Menace,
            KeywordKind::Trample,
            KeywordKind::Reach,
            KeywordKind::Haste,
        ];
        for (i, kind) in expected.iter().enumerate() {
            match &*branches[i].effect {
                Effect::PutCounter {
                    counter_type,
                    count,
                    target,
                } => {
                    assert_eq!(
                        *counter_type,
                        CounterType::Keyword(*kind),
                        "branch {i} should be {kind:?}"
                    );
                    assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
                    assert_eq!(*target, TargetFilter::ParentTarget);
                }
                other => panic!("expected branch {i} PutCounter, got {other:?}"),
            }
        }

        // No Unimplemented anywhere in the chain.
        assert!(
            !matches!(&*ability.effect, Effect::Unimplemented { .. }),
            "head must not be Unimplemented"
        );
        for branch in branches {
            assert!(
                !matches!(&*branch.effect, Effect::Unimplemented { .. }),
                "branch must not be Unimplemented"
            );
        }
    }

    #[test]
    fn spell_cast_restrictions_parse_into_top_level_metadata() {
        let r = parse(
            "Cast this spell only during combat on an opponent's turn.\nReturn X target creature cards from your graveyard to the battlefield. Sacrifice those creatures at the beginning of the next end step.",
            "Wake the Dead",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(
            r.casting_restrictions,
            vec![
                CastingRestriction::DuringCombat,
                CastingRestriction::DuringOpponentsTurn,
            ]
        );
        assert!(!matches!(
            *r.abilities[0].effect,
            Effect::Unimplemented { ref name, .. } if name == "cast"
        ));
    }

    #[test]
    fn spell_casting_option_parses_trap_alternative_cost() {
        let r = parse(
            "If an opponent searched their library this turn, you may pay {0} rather than pay this spell's mana cost.\nTarget opponent mills thirteen cards.",
            "Archive Trap",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.casting_options.len(), 1);
        assert_eq!(
            r.casting_options[0],
            SpellCastingOption::alternative_cost(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 0,
                    shards: vec![],
                },
            })
            .condition(crate::types::ability::ParsedCondition::OpponentSearchedLibraryThisTurn)
        );
        assert_eq!(r.abilities.len(), 1);
        assert!(!matches!(
            *r.abilities[0].effect,
            Effect::Unimplemented { ref name, .. } if name == "pay"
        ));
    }

    #[test]
    fn spell_casting_option_parses_composite_alternative_cost() {
        let r = parse(
            "You may pay 1 life and exile a blue card from your hand rather than pay this spell's mana cost.\nCounter target spell.",
            "Force of Will",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.casting_options.len(), 1);
        assert!(matches!(
            r.casting_options[0].cost,
            Some(AbilityCost::Composite { .. })
        ));
    }

    #[test]
    fn spell_casting_option_parses_flash_permission_with_extra_cost() {
        let r = parse(
            "You may cast this spell as though it had flash if you pay {2} more to cast it.\nDestroy all creatures. They can't be regenerated.",
            "Rout",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.casting_options.len(), 1);
        assert_eq!(
            r.casting_options[0],
            SpellCastingOption::as_though_had_flash().cost(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 2,
                    shards: vec![],
                },
            })
        );
        assert_eq!(r.abilities.len(), 1);
    }

    #[test]
    fn permanent_casting_option_parses_flash_permission_with_extra_cost() {
        let r = parse(
            "You may cast this spell as though it had flash if you pay {2} more to cast it.\nWhen this creature enters, draw a card.",
            "Example Ambusher",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(r.casting_options.len(), 1);
        assert_eq!(
            r.casting_options[0],
            SpellCastingOption::as_though_had_flash().cost(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 2,
                    shards: vec![],
                },
            })
        );
        assert_eq!(r.triggers.len(), 1);
    }

    #[test]
    fn old_aura_flash_drawback_parses_cleanup_sacrifice_trigger() {
        let r = parse(
            "You may cast this spell as though it had flash. If you cast it any time a sorcery couldn't have been cast, the controller of the permanent it becomes sacrifices it at the beginning of the next cleanup step.\nEnchant creature\nEnchanted creature gets +1/+0.",
            "Lightning Reflexes",
            &[],
            &["Enchantment"],
            &["Aura"],
        );

        assert_eq!(
            r.casting_options,
            vec![SpellCastingOption::as_though_had_flash()]
        );
        assert_eq!(r.triggers.len(), 1);
        assert!(matches!(
            r.triggers[0].condition,
            Some(TriggerCondition::CastTimingPermission {
                permission: CastTimingPermission::AsThoughHadFlash,
            })
        ));
        let delayed = r.triggers[0]
            .execute
            .as_ref()
            .expect("cleanup trigger executes delayed trigger");
        assert!(matches!(
            *delayed.effect,
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase {
                    phase: Phase::Cleanup
                },
                ..
            }
        ));
    }

    #[test]
    fn spell_casting_option_parses_free_cast_condition() {
        let r = parse(
            "If this spell is the first spell you've cast this game, you may cast it without paying its mana cost.\nLook at the top five cards of your library.",
            "Once Upon a Time",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(
            r.casting_options,
            vec![SpellCastingOption::free_cast()
                .condition(crate::types::ability::ParsedCondition::FirstSpellThisGame)]
        );
    }

    #[test]
    fn spell_resolution_free_cast_from_hand_is_effect_not_static() {
        let r = parse(
            "Return up to three target artifacts and/or creatures to their owners' hands.\nYou may cast a spell with mana value 4 or less from your hand without paying its mana cost.",
            "Baral's Expertise",
            &[],
            &["Sorcery"],
            &[],
        );

        assert_eq!(r.statics.len(), 0);
        assert_eq!(r.abilities.len(), 1);
        let cast = r.abilities[0].sub_ability.as_ref().unwrap_or_else(|| {
            panic!(
                "free cast instruction should be chained after bounce, got {:?}",
                r.abilities[0]
            )
        });
        assert!(cast.optional);
        match &*cast.effect {
            Effect::CastFromZone {
                target: TargetFilter::Typed(filter),
                without_paying_mana_cost: true,
                mode: crate::types::ability::CardPlayMode::Cast,
                ..
            } => {
                assert_eq!(filter.type_filters, vec![TypeFilter::Card]);
                assert_eq!(
                    filter.controller,
                    Some(crate::types::ability::ControllerRef::You)
                );
                assert!(filter
                    .properties
                    .iter()
                    .any(|prop| matches!(prop, FilterProp::InZone { zone: Zone::Hand })));
                assert!(filter.properties.iter().any(|prop| matches!(
                    prop,
                    FilterProp::Cmc {
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 4 },
                    }
                )));
            }
            effect => panic!("expected optional CastFromZone, got {effect:?}"),
        }
        assert!(r.parse_warnings.is_empty());
    }

    #[test]
    fn permanent_free_cast_from_hand_remains_static_permission() {
        let r = parse(
            "You may cast spells from your hand without paying their mana costs.",
            "Omniscience",
            &[],
            &["Enchantment"],
            &[],
        );

        assert_eq!(r.abilities.len(), 0);
        assert_eq!(r.statics.len(), 1);
        assert!(matches!(
            r.statics[0].mode,
            StaticMode::CastFromHandFree { .. }
        ));
    }

    #[test]
    fn spell_casting_option_ignores_followup_if_you_do_sentence() {
        let r = parse(
            "Return up to two target creature cards from your graveyard to your hand.\nYou may cast this spell for {2}{B/G}{B/G}. If you do, ignore the bracketed text.",
            "Graveyard Dig",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(
            r.casting_options,
            vec![SpellCastingOption::alternative_cost(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 2,
                    shards: vec![
                        crate::types::mana::ManaCostShard::BlackGreen,
                        crate::types::mana::ManaCostShard::BlackGreen,
                    ],
                },
            })]
        );
    }

    #[test]
    fn goblin_chainwhirler_etb_trigger() {
        let r = parse(
            "First strike\nWhen Goblin Chainwhirler enters the battlefield, it deals 1 damage to each opponent and each creature and planeswalker they control.",
            "Goblin Chainwhirler",
            &[Keyword::FirstStrike],
            &["Creature"],
            &["Goblin", "Warrior"],
        );
        assert_eq!(r.triggers.len(), 1);
        assert_eq!(r.abilities.len(), 0); // keyword line skipped
    }

    #[test]
    fn baneslayer_angel_keywords_only() {
        let r = parse(
            "Flying, first strike, lifelink, protection from Demons and from Dragons",
            "Baneslayer Angel",
            &[Keyword::Flying, Keyword::FirstStrike, Keyword::Lifelink],
            &["Creature"],
            &["Angel"],
        );
        // Keywords line should be mostly skipped; protection clause may produce unimplemented
        // The key assertion: no activated abilities, no triggers
        assert_eq!(r.abilities.len(), 0);
        assert_eq!(r.triggers.len(), 0);
    }

    #[test]
    fn questing_beast_mixed() {
        let r = parse(
            "Vigilance, deathtouch, haste\nQuesting Beast can't be blocked by creatures with power 2 or less.\nCombat damage that would be dealt by creatures you control can't be prevented.\nWhenever Questing Beast deals combat damage to a planeswalker, it deals that much damage to target planeswalker that player controls.",
            "Questing Beast",
            &[Keyword::Vigilance, Keyword::Deathtouch, Keyword::Haste],
            &["Creature"],
            &["Beast"],
        );
        // "can't be prevented" now parses as an ability (Effect::AddRestriction) rather than replacement
        assert_eq!(r.abilities.len(), 1);
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::AddRestriction { .. }
        ));
        // Should have static and trigger
        assert!(!r.statics.is_empty());
        assert!(!r.triggers.is_empty());
    }

    #[test]
    fn jace_loyalty_abilities() {
        let r = parse(
            "+2: Look at the top card of target player's library. You may put that card on the bottom of that player's library.\n0: Draw three cards, then put two cards from your hand on top of your library in any order.\n\u{2212}1: Return target creature to its owner's hand.\n\u{2212}12: Exile all cards from target player's library, then that player shuffles their hand into their library.",
            "Jace, the Mind Sculptor",
            &[],
            &["Planeswalker"],
            &["Jace"],
        );
        assert_eq!(r.abilities.len(), 4);
        // All should be activated with loyalty costs
        for ab in r.abilities.iter() {
            assert_eq!(ab.kind, AbilityKind::Activated);
        }
    }

    /// Issue #878: loyalty lines must stay separate activated abilities; the +1
    /// must not require targets (otherwise the UI auto-dispatches the sole legal
    /// -3 when the player clicks Teferi).
    ///
    /// PR #1441 re-seam: the flash-timing grant must be PLAYER-scoped
    /// (`target: Controller` + `UntilNextTurnOf { Controller }`), not
    /// object-scoped (`target: SelfRef`). The object seam was pruned the instant
    /// Teferi left play, violating CR 611.2a/c. The inner static must still grant
    /// `CastWithKeyword { Flash }` against a Sorcery-typed `affected` filter.
    #[test]
    fn teferi_time_raveler_loyalty_abilities_parse() {
        let r = parse(
            "Each opponent can cast spells only any time they could cast a sorcery.\n\
             [+1]: Until your next turn, you may cast sorcery spells as though they had flash.\n\
             [\u{2212}3]: Return up to one target artifact, creature, or enchantment to its owner's hand. Draw a card.",
            "Teferi, Time Raveler",
            &[],
            &["Planeswalker"],
            &["Teferi"],
        );
        assert_eq!(r.abilities.len(), 2, "abilities: {:?}", r.abilities);
        assert!(matches!(
            r.abilities[0].cost,
            Some(AbilityCost::Loyalty { amount: 1 })
        ));
        assert!(matches!(
            r.abilities[1].cost,
            Some(AbilityCost::Loyalty { amount: -3 })
        ));

        let Effect::GenericEffect {
            static_abilities,
            duration,
            target,
        } = &*r.abilities[0].effect
        else {
            panic!(
                "+1 must grant flash timing via GenericEffect, got {:?}",
                r.abilities[0].effect
            );
        };

        // CR 611.2c: player-scoped grant — resolves to SpecificPlayer at effect.rs.
        assert_eq!(
            *target,
            Some(TargetFilter::Controller),
            "+1 grant must be player-scoped (Controller), not object-scoped (SelfRef)"
        );
        // CR 611.2a: lifetime governed by duration, expiring at the controller's next turn.
        assert_eq!(
            *duration,
            Some(crate::types::ability::Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            }),
            "+1 grant must expire 'until your next turn'"
        );

        // The inner static grants Flash to Sorcery spells the controller casts.
        let inner = match &static_abilities[0].modifications[0] {
            ContinuousModification::GrantStaticAbility { definition } => definition,
            other => panic!("expected GrantStaticAbility, got {other:?}"),
        };
        assert!(
            matches!(
                &inner.mode,
                StaticMode::CastWithKeyword {
                    keyword: Keyword::Flash
                }
            ),
            "inner static must be CastWithKeyword(Flash), got {:?}",
            inner.mode
        );
        let Some(TargetFilter::Typed(tf)) = &inner.affected else {
            panic!(
                "inner static affected must be a Typed sorcery filter, got {:?}",
                inner.affected
            );
        };
        assert!(
            tf.type_filters.contains(&TypeFilter::Sorcery),
            "inner affected filter must constrain to Sorcery, got {:?}",
            tf.type_filters
        );
        assert_eq!(
            tf.controller,
            Some(ControllerRef::You),
            "inner affected filter must scope to spells you cast"
        );
    }

    #[test]
    fn forest_reminder_text_only() {
        let r = parse("({T}: Add {G}.)", "Forest", &[], &["Land"], &["Forest"]);
        // Reminder text should be stripped/skipped
        assert_eq!(r.abilities.len(), 0);
    }

    #[test]
    fn mox_pearl_mana_ability() {
        let r = parse("{T}: Add {W}.", "Mox Pearl", &[], &["Artifact"], &[]);
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
    }

    #[test]
    fn parses_return_forest_cost_untap_activated_ability() {
        let r = parse(
            "Return a Forest you control to its owner's hand: Untap target creature. Activate only once each turn.",
            "Quirion Ranger",
            &[],
            &["Creature"],
            &["Elf", "Ranger"],
        );

        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Activated);
        assert!(matches!(*ability.effect, Effect::Untap { .. }));
        assert!(ability
            .activation_restrictions
            .iter()
            .any(|restriction| matches!(restriction, ActivationRestriction::OnlyOnceEachTurn)));
        match ability.cost.as_ref() {
            Some(AbilityCost::ReturnToHand {
                count,
                filter: Some(TargetFilter::Typed(filter)),
                from_zone: None,
            }) => {
                assert_eq!(*count, 1);
                assert_eq!(filter.get_subtype(), Some("Forest"));
            }
            other => panic!("expected Forest ReturnToHand cost, got {other:?}"),
        }
    }

    #[test]
    fn ability_word_prefixed_activated_ability_preserves_restrictions() {
        let r = parse(
            "Threshold — Put three cards from your graveyard on the bottom of your library: This creature gets +3/+3 until end of turn. Activate only once each turn and only if there are seven or more cards in your graveyard.",
            "Test Scrounger",
            &[],
            &["Creature"],
            &[],
        );

        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Activated);
        assert!(matches!(
            ability.cost.as_ref(),
            Some(AbilityCost::EffectCost { effect })
                if matches!(effect.as_ref(), Effect::PutAtLibraryPosition { .. })
        ));
        assert!(matches!(
            ability.effect.as_ref(),
            Effect::Pump {
                target: TargetFilter::SelfRef,
                ..
            }
        ));
        assert!(ability.condition.is_none());
        assert!(ability
            .activation_restrictions
            .iter()
            .any(|restriction| matches!(restriction, ActivationRestriction::OnlyOnceEachTurn)));
        assert!(ability.activation_restrictions.iter().any(|restriction| {
            matches!(
                restriction,
                ActivationRestriction::RequiresCondition {
                    condition: Some(
                        crate::types::ability::ParsedCondition::ZoneCardCountAtLeast {
                            zone: Zone::Graveyard,
                            count: 7
                        }
                    )
                }
            )
        }));
    }

    #[test]
    fn parses_activate_only_land_condition_into_activation_restriction() {
        let r = parse(
            "{T}: Add {U}.\n{T}: Add {B}. Activate only if you control an Island or a Swamp.",
            "Gloomlake Verge",
            &[],
            &["Land"],
            &[],
        );
        assert_eq!(r.abilities.len(), 2);
        let second = &r.abilities[1];
        assert!(matches!(
            second.activation_restrictions.as_slice(),
            [ActivationRestriction::RequiresCondition {
                condition: Some(
                    crate::types::ability::ParsedCondition::YouControlLandSubtypeAny { .. }
                )
            }]
        ));
    }

    #[test]
    fn parses_urza_tower_conditional_mana_as_delta() {
        let r = parse(
            "{T}: Add {C}. If you control an Urza's Mine and an Urza's Power-Plant, add {C}{C}{C} instead.",
            "Urza's Tower",
            &[],
            &["Land"],
            &["Urza's", "Tower"],
        );
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        match ability.effect.as_ref() {
            Effect::Mana {
                produced: ManaProduction::Colorless { count },
                ..
            } => assert_eq!(*count, QuantityExpr::Fixed { value: 1 }),
            other => panic!("expected base colorless mana, got {other:?}"),
        }
        let sub = ability
            .sub_ability
            .as_ref()
            .expect("expected conditional delta");
        match sub.effect.as_ref() {
            Effect::Mana {
                produced: ManaProduction::Colorless { count },
                ..
            } => assert_eq!(*count, QuantityExpr::Fixed { value: 2 }),
            other => panic!("expected colorless mana delta, got {other:?}"),
        }
        match sub.condition.as_ref().expect("expected condition") {
            AbilityCondition::And { conditions } => assert_eq!(conditions.len(), 2),
            other => panic!("expected conjunction condition, got {other:?}"),
        }
    }

    /// CR 205.3i + CR 614.1a + CR 605.1a: All three Urza lands share a single
    /// parsed shape — an activated mana ability (`{T}: Add {C}.` per CR 605.1a)
    /// plus a conditional `Add {C}{C}{C} instead` sub-ability whose "instead"
    /// makes it a replacement effect (CR 614.1a) gated on the player
    /// controlling the OTHER two Urza land subtypes (from the CR 205.3i land
    /// type list: Mine, Power-Plant, Tower). The
    /// critical assertion is the cross-naming of the `And` branches: a
    /// regression that emits `[Mine, Mine]` instead of `[Mine, Power-Plant]`
    /// would let Urza's Tower count itself as one of the required lands and
    /// silently change the rules. Each row in the table below pins the exact
    /// pair of subtypes the parsed condition must reference.
    #[test]
    fn urzas_lands_share_delta_shape() {
        // (card name, oracle text, expected subtypes on the And conditions in
        // the order the parser emits them)
        let cases: [(&str, &str, [&str; 2], &[&str]); 3] = [
            (
                "Urza's Tower",
                "{T}: Add {C}. If you control an Urza's Mine and an Urza's Power-Plant, add {C}{C}{C} instead.",
                ["Mine", "Power-Plant"],
                &["Urza's", "Tower"],
            ),
            (
                "Urza's Power Plant",
                "{T}: Add {C}. If you control an Urza's Mine and an Urza's Tower, add {C}{C}{C} instead.",
                ["Mine", "Tower"],
                &["Urza's", "Power-Plant"],
            ),
            (
                "Urza's Mine",
                "{T}: Add {C}. If you control an Urza's Power-Plant and an Urza's Tower, add {C}{C}{C} instead.",
                ["Power-Plant", "Tower"],
                &["Urza's", "Mine"],
            ),
        ];

        for (name, text, expected_subs, subtypes) in cases {
            let r = parse(text, name, &[], &["Land"], subtypes);
            assert_eq!(r.abilities.len(), 1, "{name}: expected one ability");
            let ability = &r.abilities[0];

            match ability.effect.as_ref() {
                Effect::Mana {
                    produced: ManaProduction::Colorless { count },
                    ..
                } => assert_eq!(
                    *count,
                    QuantityExpr::Fixed { value: 1 },
                    "{name}: base mana must be exactly one colorless"
                ),
                other => panic!("{name}: expected base colorless mana, got {other:?}"),
            }

            let sub = ability
                .sub_ability
                .as_ref()
                .unwrap_or_else(|| panic!("{name}: expected conditional delta sub-ability"));

            match sub.effect.as_ref() {
                Effect::Mana {
                    produced: ManaProduction::Colorless { count },
                    ..
                } => assert_eq!(
                    *count,
                    QuantityExpr::Fixed { value: 2 },
                    "{name}: delta must be +2 colorless (total 3 minus base 1)"
                ),
                other => panic!("{name}: expected colorless mana delta, got {other:?}"),
            }

            let conditions = match sub
                .condition
                .as_ref()
                .unwrap_or_else(|| panic!("{name}: expected sub-ability condition"))
            {
                AbilityCondition::And { conditions } => conditions,
                other => panic!("{name}: expected And condition, got {other:?}"),
            };
            assert_eq!(
                conditions.len(),
                2,
                "{name}: And must have exactly two ControllerControlsMatching branches"
            );

            let extracted: Vec<&str> = conditions
                .iter()
                .map(|c| match c {
                    AbilityCondition::ControllerControlsMatching {
                        filter: TargetFilter::Typed(typed),
                    } => typed
                        .get_subtype()
                        .unwrap_or_else(|| panic!("{name}: filter must carry a subtype")),
                    other => panic!(
                        "{name}: expected ControllerControlsMatching with Typed filter, got {other:?}"
                    ),
                })
                .collect();

            assert_eq!(
                extracted,
                expected_subs.to_vec(),
                "{name}: And branches must reference the OTHER two Urza land subtypes — \
                 a regression here lets the land count itself as one of the required pieces"
            );
        }
    }

    #[test]
    fn parses_ugin_labyrinth_exiled_card_mana_as_delta() {
        let r = parse(
            "Imprint — When this land enters, you may exile a colorless card with mana value 7 or greater from your hand.\n{T}: Add {C}. If a card is exiled with Ugin's Labyrinth, add {C}{C} instead.",
            "Ugin's Labyrinth",
            &[],
            &["Land"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        match ability.effect.as_ref() {
            Effect::Mana {
                produced: ManaProduction::Colorless { count },
                ..
            } => assert_eq!(*count, QuantityExpr::Fixed { value: 1 }),
            other => panic!("expected base colorless mana, got {other:?}"),
        }
        let sub = ability
            .sub_ability
            .as_ref()
            .expect("expected conditional delta");
        match sub.effect.as_ref() {
            Effect::Mana {
                produced: ManaProduction::Colorless { count },
                ..
            } => assert_eq!(*count, QuantityExpr::Fixed { value: 1 }),
            other => panic!("expected colorless mana delta, got {other:?}"),
        }
        match sub.condition.as_ref().expect("expected condition") {
            AbilityCondition::QuantityCheck {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::CardsExiledBySource,
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("expected exiled-with-source condition, got {other:?}"),
        }
    }

    #[test]
    fn parses_compound_activate_only_constraints() {
        let r = parse(
            "{T}: Add {R}. Activate only as a sorcery and only once each turn.",
            "Careful Forge",
            &[],
            &["Artifact"],
            &[],
        );
        assert_eq!(
            r.abilities[0].activation_restrictions,
            vec![
                ActivationRestriction::AsSorcery,
                ActivationRestriction::OnlyOnceEachTurn,
            ]
        );
    }

    #[test]
    fn crew_with_activate_only_once_each_turn_carries_cadence() {
        // CR 702.122 + CR 602.5b: Luxurious Locomotive — "Crew 1. Activate only
        // once each turn." The trailing cadence sentence upgrades the keyword's
        // `once_per_turn` field from the cadence-less MTGJSON `Crew`.
        let r = parse_with_keyword_names(
            "Crew 1. Activate only once each turn. (Tap any number of creatures you control with total power 1 or more: This Vehicle becomes an artifact creature until end of turn.)",
            "Luxurious Locomotive",
            &["Crew"],
            &["Artifact"],
            &["Vehicle"],
        );
        assert!(
            r.extracted_keywords.contains(&Keyword::Crew {
                power: 1,
                once_per_turn: ActivationCadence::OncePerTurn,
            }),
            "expected Crew {{ power: 1, once_per_turn: OncePerTurn }}, got {:?}",
            r.extracted_keywords
        );
    }

    #[test]
    fn plain_crew_line_extracts_unlimited_cadence() {
        // A bare "Crew N" line (no cadence sentence) parses as the default
        // `Unlimited` cadence — no once-per-turn restriction is invented.
        let r = parse_with_keyword_names(
            "Crew 3 (Tap any number of creatures you control with total power 3 or more: This Vehicle becomes an artifact creature until end of turn.)",
            "Smuggler's Copter",
            &["Crew"],
            &["Artifact"],
            &["Vehicle"],
        );
        assert!(
            r.extracted_keywords.contains(&Keyword::Crew {
                power: 3,
                once_per_turn: ActivationCadence::Unlimited,
            }),
            "a plain Crew line keeps the default Unlimited cadence; got {:?}",
            r.extracted_keywords
        );
    }

    #[test]
    fn kirol_standalone_activate_only_once_each_turn_unchanged() {
        // Regression witness: Kirol, Attentive First-Year — a NORMAL activated
        // ability with a standalone "Activate only once each turn." sentence.
        // Factoring `recognize_once_each_turn_cadence` must not disturb this
        // path; the ability still carries `OnlyOnceEachTurn`.
        let r = parse(
            "Tap two untapped creatures you control: Copy target triggered ability you control. You may choose new targets for the copy. Activate only once each turn.",
            "Kirol, Attentive First-Year",
            &[],
            &["Creature"],
            &["Elf", "Druid"],
        );
        assert_eq!(r.abilities.len(), 1);
        assert!(
            r.abilities[0]
                .activation_restrictions
                .contains(&ActivationRestriction::OnlyOnceEachTurn),
            "Kirol's activated ability must still carry OnlyOnceEachTurn; got {:?}",
            r.abilities[0].activation_restrictions
        );
    }

    #[test]
    fn parses_activate_only_if_condition_and_only_once_each_turn() {
        // CR 602.5b: "Activate only if [condition] and only once each turn" must produce
        // both a RequiresCondition restriction (with the condition) and OnlyOnceEachTurn.
        // Tests the general pattern, not a single card.
        use crate::types::ability::{ParsedCondition, PlayerFilter};
        let r = parse(
            "{1}{R}: Put a +1/+1 counter on this creature. Activate only if an opponent lost life this turn and only once each turn.",
            "Test Card",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let restrictions = &r.abilities[0].activation_restrictions;
        assert!(
            restrictions.contains(&ActivationRestriction::OnlyOnceEachTurn),
            "expected OnlyOnceEachTurn restriction"
        );
        assert!(
            restrictions.iter().any(|r| matches!(
                r,
                ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::PlayerCountAtLeast {
                        filter: PlayerFilter::OpponentLostLife,
                        minimum: 1,
                    })
                }
            )),
            "expected RequiresCondition with OpponentLostLife"
        );
    }

    #[test]
    fn parses_activate_only_if_condition_and_only_as_sorcery() {
        let r = parse(
            "{2}{G}{G}: Return this card from your graveyard to the battlefield. Activate only if there are four or more card types among cards in your graveyard and only as a sorcery.",
            "Delirium Test",
            &[],
            &["Creature"],
            &[],
        );

        assert_eq!(r.abilities.len(), 1);
        let restrictions = &r.abilities[0].activation_restrictions;
        assert!(restrictions.contains(&ActivationRestriction::AsSorcery));
        assert!(restrictions.iter().any(|restriction| matches!(
            restriction,
            ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::ZoneCardTypeCountAtLeast {
                    zone: Zone::Graveyard,
                    count: 4
                })
            }
        )));
        assert!(r.parse_warnings.is_empty());
    }

    #[test]
    fn parses_activate_only_timing_and_only_if_condition() {
        let r = parse(
            "{1}{B}: Return this card from your graveyard to your hand. Activate only during your turn and only if an opponent lost life this turn.",
            "Gutterbones",
            &[],
            &["Creature"],
            &[],
        );
        let restrictions = &r.abilities[0].activation_restrictions;
        assert!(restrictions.contains(&ActivationRestriction::DuringYourTurn));
        assert!(restrictions.iter().any(|restriction| matches!(
            restriction,
            ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::PlayerCountAtLeast {
                    filter: PlayerFilter::OpponentLostLife,
                    minimum: 1,
                })
            }
        )));
        assert!(r.parse_warnings.iter().all(|warning| warning
            .to_string()
            .split_whitespace()
            .next()
            != Some("Swallow:Condition_If")));
    }

    #[test]
    fn parses_activate_only_filtered_spell_count_condition() {
        use crate::types::ability::{
            Comparator, CountScope, ParsedCondition, QuantityExpr, QuantityRef,
        };

        let r = parse(
            "{R}: Exile this creature, then return it to the battlefield transformed under its owner's control. \
             Activate only as a sorcery and only if you've cast three or more instant and/or sorcery spells this turn.",
            "Urabrask",
            &[],
            &["Creature"],
            &[],
        );

        let restrictions = &r.abilities[0].activation_restrictions;
        assert!(restrictions.contains(&ActivationRestriction::AsSorcery));
        assert!(restrictions.iter().any(|restriction| matches!(
            restriction,
            ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::SpellsCastThisTurn {
                            scope: CountScope::Controller,
                            filter: Some(TargetFilter::Or { .. }),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                })
            }
        )));
        assert!(r.parse_warnings.is_empty());
    }

    #[test]
    fn parses_activate_only_filtered_morbid_condition() {
        use crate::types::ability::{Comparator, ParsedCondition, QuantityExpr, QuantityRef};

        let r = parse(
            "{1}{B}: Return this card from your graveyard to the battlefield. \
             Activate only if a non-Skeleton creature died under your control this turn.",
            "Cult Conscript",
            &[],
            &["Creature"],
            &["Skeleton", "Warrior"],
        );

        assert!(r.abilities[0]
            .activation_restrictions
            .iter()
            .any(|restriction| matches!(
                restriction,
                ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::ZoneChangeCountThisTurn { .. },
                        },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 1 },
                    })
                }
            )));
        assert!(r.parse_warnings.is_empty());
    }

    #[test]
    fn parses_activate_only_as_sorcery_and_only_if_hand_size_condition() {
        let r = parse(
            "{2}{B}: Return this card from your graveyard to the battlefield. Activate only as a sorcery and only if you have one or fewer cards in hand.",
            "Dread Wanderer",
            &[],
            &["Creature"],
            &[],
        );
        let restrictions = &r.abilities[0].activation_restrictions;
        assert!(restrictions.contains(&ActivationRestriction::AsSorcery));
        assert!(restrictions.iter().any(|restriction| matches!(
            restriction,
            ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::HandSizeOneOf { counts })
            } if counts == &vec![0, 1]
        )));
        assert!(r.parse_warnings.iter().all(|warning| warning
            .to_string()
            .split_whitespace()
            .next()
            != Some("Swallow:Condition_If")));
    }

    #[test]
    fn extracts_protection_keyword_from_oracle_text() {
        use crate::types::keywords::ProtectionTarget;
        // Soldier of the Pantheon: MTGJSON lists "Protection" as keyword name,
        // Oracle text has the full "Protection from multicolored"
        let r = parse_with_keyword_names(
            "Protection from multicolored",
            "Soldier of the Pantheon",
            &["protection"], // MTGJSON keyword name (lowercased)
            &["Creature"],
            &["Human", "Soldier"],
        );
        assert_eq!(r.extracted_keywords.len(), 1);
        assert!(matches!(
            &r.extracted_keywords[0],
            Keyword::Protection(ProtectionTarget::Multicolored)
        ));
    }

    #[test]
    fn extracts_keyword_after_ability_word_prefix() {
        use crate::types::ability::{Comparator, FilterProp, QuantityExpr, TargetFilter};
        use crate::types::keywords::ProtectionTarget;

        let r = parse_with_keyword_names(
            "Void Shields — Protection from mana value 3 or less",
            "Reaver Titan",
            &["protection"],
            &["Artifact", "Creature"],
            &["Vehicle"],
        );
        assert_eq!(r.extracted_keywords.len(), 1);
        let Keyword::Protection(ProtectionTarget::Filter(TargetFilter::Typed(tf))) =
            &r.extracted_keywords[0]
        else {
            panic!(
                "expected filter-based protection, got {:?}",
                r.extracted_keywords
            );
        };
        assert!(matches!(
            tf.properties.as_slice(),
            [FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 3 },
            }]
        ));
    }

    #[test]
    fn skips_keywords_already_in_mtgjson() {
        // "Flying" is in MTGJSON — exact name match, should not be re-extracted
        let r = parse_with_keyword_names(
            "Flying",
            "Serra Angel",
            &["flying", "vigilance"],
            &["Creature"],
            &["Angel"],
        );
        assert!(r.extracted_keywords.is_empty());
    }

    #[test]
    fn extracts_new_keywords_from_mixed_line() {
        use crate::types::keywords::ProtectionTarget;
        // "flying" exact-matches MTGJSON (skipped), "protection from red" prefix-matches (extracted)
        let r = parse_with_keyword_names(
            "Flying, protection from red",
            "Test Card",
            &["flying", "protection"],
            &["Creature"],
            &[],
        );
        assert_eq!(r.extracted_keywords.len(), 1);
        assert!(matches!(
            &r.extracted_keywords[0],
            Keyword::Protection(ProtectionTarget::Color(crate::types::mana::ManaColor::Red))
        ));
    }

    #[test]
    fn end_to_end_toxic_keyword_no_unimplemented() {
        // End-to-end: "Toxic 2" with MTGJSON keyword name "toxic" should be
        // fully handled — no Unimplemented effects in output
        let r = parse_with_keyword_names(
            "Toxic 2",
            "Glistener Elf",
            &["toxic"],
            &["Creature"],
            &["Phyrexian", "Elf", "Warrior"],
        );
        let has_unimplemented = r.abilities.iter().any(|a| {
            matches!(
                *a.effect,
                crate::types::ability::Effect::Unimplemented { .. }
            )
        });
        assert!(
            !has_unimplemented,
            "Toxic keyword line should not produce Unimplemented effects"
        );
    }

    // CR 205.3g: Spacecraft is an artifact subtype that can appear in subtype filters.
    #[test]
    fn end_to_end_beyond_the_quiet_no_spacecraft_gap() {
        let r = parse(
            "Exile all creatures and Spacecraft.",
            "Beyond the Quiet",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        assert!(
            !has_unimplemented(&r.abilities[0]),
            "Beyond the Quiet should not produce Unimplemented effects: {:?}",
            r.abilities[0]
        );
        match &*r.abilities[0].effect {
            Effect::ChangeZoneAll {
                destination,
                target,
                ..
            } => {
                assert_eq!(*destination, Zone::Exile);
                match target {
                    TargetFilter::Or { filters } => {
                        assert_eq!(filters.len(), 2);
                        assert_eq!(filters[0], TargetFilter::Typed(TypedFilter::creature()));
                        assert_eq!(
                            filters[1],
                            TargetFilter::Typed(
                                TypedFilter::default().subtype("Spacecraft".to_string())
                            )
                        );
                    }
                    other => panic!("expected Creature/Spacecraft Or filter, got {other:?}"),
                }
            }
            other => panic!("expected ChangeZoneAll, got {other:?}"),
        }
    }

    #[test]
    fn end_to_end_suspend_sorcery_no_unimplemented() {
        // CR 702.62a: "Suspend N—{cost}" on a sorcery must not produce Unimplemented.
        // Ancestral Vision: "Suspend 4—{U}\nTarget player draws three cards."
        let r = parse_with_keyword_names(
            "Suspend 4\u{2014}{U}\nTarget player draws three cards.",
            "Ancestral Vision",
            &["suspend"],
            &["Sorcery"],
            &[],
        );
        let has_unimplemented = r.abilities.iter().any(|a| {
            matches!(
                *a.effect,
                crate::types::ability::Effect::Unimplemented { .. }
            )
        });
        assert!(
            !has_unimplemented,
            "Suspend keyword line on sorcery should not produce Unimplemented"
        );
        // Should have extracted the parameterized Suspend keyword
        let suspend_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Suspend { .. }));
        assert!(suspend_kw.is_some(), "Should extract Suspend keyword");
        if let Some(Keyword::Suspend { count, .. }) = suspend_kw {
            assert_eq!(*count, 4);
        }
    }

    #[test]
    fn end_to_end_typecycling_no_unimplemented() {
        // "Plainscycling {2}" with MTGJSON keyword name should not produce Unimplemented
        let r = parse_with_keyword_names(
            "Plainscycling {2}",
            "Twisted Abomination",
            &["plainscycling"],
            &["Creature"],
            &["Zombie", "Mutant"],
        );
        let has_unimplemented = r.abilities.iter().any(|a| {
            matches!(
                *a.effect,
                crate::types::ability::Effect::Unimplemented { .. }
            )
        });
        assert!(
            !has_unimplemented,
            "Typecycling keyword line should not produce Unimplemented effects"
        );
    }

    #[test]
    fn no_extraction_without_mtgjson_keywords() {
        // Without MTGJSON keywords, keyword-only lines are not detected
        // (prevents false positives like "Equip {1}" being eaten)
        let r = parse_with_keyword_names(
            "Equip {1}",
            "Bonesplitter",
            &[],
            &["Artifact"],
            &["Equipment"],
        );
        assert!(r.extracted_keywords.is_empty());
        // Line should fall through to equip ability parsing
        assert_eq!(r.abilities.len(), 1);
    }

    // ── Modal parsing tests ──────────────────────────────────────────────

    #[test]
    fn choose_one_modal_metadata() {
        let r = parse(
            "Choose one —\n• Deal 3 damage to any target.\n• Draw a card.\n• Gain 3 life.",
            "Test Charm",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 3);
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 3);
        assert_eq!(modal.mode_descriptions.len(), 3);
    }

    #[test]
    fn choose_two_modal_metadata() {
        let r = parse(
            "Choose two —\n• Counter target spell.\n• Return target permanent to its owner's hand.\n• Tap all creatures your opponents control.\n• Draw a card.",
            "Cryptic Command",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 4);
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 2);
        assert_eq!(modal.max_choices, 2);
        assert_eq!(modal.mode_count, 4);
    }

    #[test]
    fn choose_one_or_both_modal_metadata() {
        let r = parse(
            "Choose one or both —\n• Destroy target artifact.\n• Destroy target enchantment.",
            "Wear // Tear",
            &[],
            &["Instant"],
            &[],
        );
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 2);
        assert_eq!(modal.mode_count, 2);
    }

    #[test]
    fn choose_one_conditional_choose_both_modal_metadata() {
        let r = parse(
            "Choose one. If you control a commander as you cast this spell, you may choose both instead.\n• Draw a card.\n• Gain 3 life.",
            "Will Test",
            &[],
            &["Instant"],
            &[],
        );
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 2);
        assert_eq!(
            modal.constraints,
            vec![ModalSelectionConstraint::ConditionalMaxChoices {
                condition: crate::types::ability::ModalSelectionCondition::Static {
                    condition: StaticCondition::ControlsCommander {
                        ownership: crate::types::ability::CommanderOwnership::Any,
                    },
                },
                max_choices: 2,
                otherwise_max_choices: 1,
            }]
        );
        assert!(r.parse_warnings.is_empty());
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
        assert!(matches!(
            *r.abilities[1].effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
    }

    fn assert_shared_creature_type_max(expr: &QuantityExpr) {
        let QuantityExpr::Ref {
            qty:
                QuantityRef::ObjectCountBySharedQuality {
                    filter:
                        TargetFilter::Typed(TypedFilter {
                            type_filters,
                            controller,
                            properties,
                        }),
                    quality,
                    aggregate,
                },
        } = expr
        else {
            panic!("expected ObjectCountBySharedQuality quantity, got {expr:?}");
        };
        assert_eq!(type_filters.as_slice(), &[TypeFilter::Creature]);
        assert_eq!(controller, &Some(ControllerRef::You));
        assert!(properties.is_empty());
        assert_eq!(quality, &SharedQuality::CreatureType);
        assert_eq!(aggregate, &AggregateFunction::Max);
    }

    #[test]
    fn skemfar_shadowsage_gain_mode_parses_shared_creature_type_count() {
        let r = parse(
            "You gain X life, where X is the greatest number of creatures you control that have a creature type in common.",
            "Skemfar Shadowsage",
            &[],
            &["Creature"],
            &["Elf", "Cleric"],
        );
        let Effect::GainLife { amount, .. } = &*r.abilities[0].effect else {
            panic!("expected GainLife, got {:?}", r.abilities[0].effect);
        };
        assert_shared_creature_type_max(amount);
    }

    #[test]
    fn basalt_ravager_damage_parses_shared_creature_type_count() {
        let r = parse(
            "Basalt Ravager deals X damage to any target, where X is the greatest number of creatures you control that have a creature type in common.",
            "Basalt Ravager",
            &[],
            &["Creature"],
            &["Giant", "Wizard"],
        );
        let Effect::DealDamage { amount, .. } = &*r.abilities[0].effect else {
            panic!("expected DealDamage, got {:?}", r.abilities[0].effect);
        };
        assert_shared_creature_type_max(amount);
    }

    #[test]
    fn white_lotus_tile_mana_parses_shared_creature_type_count() {
        let r = parse(
            "{T}: Add X mana of any one color, where X is the greatest number of creatures you control that have a creature type in common.",
            "White Lotus Tile",
            &[],
            &["Artifact"],
            &[],
        );
        let Effect::Mana {
            produced: ManaProduction::AnyOneColor { count, .. },
            ..
        } = &*r.abilities[0].effect
        else {
            panic!(
                "expected AnyOneColor mana ability, got {:?}",
                r.abilities[0].effect
            );
        };
        assert_shared_creature_type_max(count);
    }

    #[test]
    fn conditional_modal_max_reuses_static_condition_parser() {
        let r = parse(
            "Choose one. If you control a Wizard as you cast this spell, you may choose two instead.\n• Target player draws two cards.\n• Destroy target artifact.\n• ~ deals 5 damage to target creature.",
            "Flame Test",
            &[],
            &["Instant"],
            &[],
        );
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 3);
        assert_eq!(modal.constraints.len(), 1);
        assert!(matches!(
            modal.constraints[0],
            ModalSelectionConstraint::ConditionalMaxChoices {
                condition: crate::types::ability::ModalSelectionCondition::Static {
                    condition: StaticCondition::IsPresent { .. },
                },
                max_choices: 2,
                otherwise_max_choices: 1,
            }
        ));
        assert!(r.parse_warnings.is_empty());
    }

    #[test]
    fn conditional_modal_max_supports_compound_presence_conditions() {
        let r = parse(
            "Choose one. If you control an artifact and an enchantment as you cast this spell, you may choose both instead.\n• Exile target creature or planeswalker.\n• Return target creature or planeswalker card from your graveyard to your hand.",
            "Soul Test",
            &[],
            &["Sorcery"],
            &[],
        );
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 2);
        assert_eq!(modal.constraints.len(), 1);
        assert!(matches!(
            modal.constraints[0],
            ModalSelectionConstraint::ConditionalMaxChoices {
                condition: crate::types::ability::ModalSelectionCondition::Static {
                    condition: StaticCondition::And { .. },
                },
                max_choices: 2,
                otherwise_max_choices: 1,
            }
        ));
        assert!(r.parse_warnings.is_empty());
    }

    #[test]
    fn conditional_modal_max_supports_kicker_condition() {
        let r = parse(
            "Kicker {2}{G}\nChoose one. If this spell was kicked, choose any number instead.\n• Draw a card.\n• Gain 3 life.\n• Scry 1.",
            "Inscription Test",
            &[],
            &["Sorcery"],
            &[],
        );
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 3);
        assert!(matches!(
            modal.constraints[0],
            ModalSelectionConstraint::ConditionalMaxChoices {
                condition: crate::types::ability::ModalSelectionCondition::AdditionalCostPaid {
                    source: crate::types::ability::AdditionalCostPaymentSource::Kicker,
                    variant: None,
                    kicker_cost: None,
                    min_count: 1,
                },
                max_choices: 3,
                otherwise_max_choices: 1,
            }
        ));
        assert!(r.parse_warnings.is_empty());
    }

    #[test]
    fn conditional_modal_max_supports_additional_cost_paid_condition() {
        let r = parse(
            "Choose one. If this spell's additional cost was paid, choose both instead.\n• Destroy target artifact.\n• Destroy target creature with mana value 3 or greater.",
            "Blight Test",
            &[],
            &["Sorcery"],
            &[],
        );
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 2);
        assert!(matches!(
            modal.constraints[0],
            ModalSelectionConstraint::ConditionalMaxChoices {
                condition: crate::types::ability::ModalSelectionCondition::AdditionalCostPaid {
                    source: crate::types::ability::AdditionalCostPaymentSource::Any,
                    variant: None,
                    kicker_cost: None,
                    min_count: 1,
                },
                max_choices: 2,
                otherwise_max_choices: 1,
            }
        ));
        assert!(r.parse_warnings.is_empty());
    }

    #[test]
    fn conditional_modal_max_supports_life_threshold_conditions() {
        let exact = parse(
            "Choose one. If you have exactly 13 life, you may choose both instead.\n• Draw a card.\n• Gain 3 life.",
            "Life Test",
            &[],
            &["Instant"],
            &[],
        );
        let modal = exact.modal.expect("should have modal metadata");
        assert!(matches!(
            modal.constraints[0],
            ModalSelectionConstraint::ConditionalMaxChoices {
                condition: crate::types::ability::ModalSelectionCondition::Static {
                    condition: StaticCondition::QuantityComparison {
                        comparator: Comparator::EQ,
                        ..
                    },
                },
                max_choices: 2,
                otherwise_max_choices: 1,
            }
        ));
        assert!(exact.parse_warnings.is_empty());

        let opponent_gap = parse(
            "Choose one. If an opponent has at least 5 more life than you, choose any number instead.\n• Draw a card.\n• Gain 3 life.\n• Scry 1.",
            "Catch Up Test",
            &[],
            &["Sorcery"],
            &[],
        );
        let modal = opponent_gap.modal.expect("should have modal metadata");
        assert!(matches!(
            modal.constraints[0],
            ModalSelectionConstraint::ConditionalMaxChoices {
                condition: crate::types::ability::ModalSelectionCondition::Static {
                    condition: StaticCondition::QuantityComparison {
                        comparator: Comparator::GE,
                        ..
                    },
                },
                max_choices: 3,
                otherwise_max_choices: 1,
            }
        ));
        assert!(opponent_gap.parse_warnings.is_empty());
    }

    #[test]
    fn triggered_conditional_modal_max_supports_dash_delimiter() {
        let r = parse(
            "When this creature enters, choose one. If an opponent has at least 5 more life than you, choose any number instead—\n• Draw a card.\n• Gain 3 life.\n• Scry 1.",
            "Catch Up Test",
            &[],
            &["Creature"],
            &[],
        );
        let trigger = r.triggers.first().expect("should have trigger");
        let execute = trigger
            .execute
            .as_deref()
            .expect("should have modal execute");
        let modal = execute.modal.as_ref().expect("should have modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 3);
        assert!(matches!(
            modal.constraints[0],
            ModalSelectionConstraint::ConditionalMaxChoices {
                condition: crate::types::ability::ModalSelectionCondition::Static {
                    condition: StaticCondition::QuantityComparison {
                        comparator: Comparator::GE,
                        ..
                    },
                },
                max_choices: 3,
                otherwise_max_choices: 1,
            }
        ));
        assert!(r.parse_warnings.is_empty());
    }

    #[test]
    fn spell_temporal_whenever_line_builds_delayed_trigger() {
        let r = parse(
            "Whenever you cast a creature spell this turn, draw a card.",
            "Glimpse Test",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(r.triggers.is_empty());
        assert_eq!(r.abilities.len(), 1);
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::CreateDelayedTrigger { .. }
        ));
        let Effect::CreateDelayedTrigger { condition, .. } = &*r.abilities[0].effect else {
            panic!("expected delayed trigger, got {:?}", r.abilities[0].effect);
        };
        let crate::types::ability::DelayedTriggerCondition::WheneverEvent { trigger } = condition
        else {
            panic!("expected WheneverEvent, got {condition:?}");
        };
        assert_eq!(trigger.mode, TriggerMode::SpellCast);
        assert_eq!(trigger.valid_target, Some(TargetFilter::Controller));
        assert!(trigger.valid_card.is_some());
        assert!(r.parse_warnings.is_empty());
    }

    #[test]
    fn spell_temporal_enters_line_builds_delayed_trigger() {
        let r = parse(
            "Whenever a creature enters this turn, you may draw a card.",
            "Beck Test",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(r.triggers.is_empty());
        assert_eq!(r.abilities.len(), 1);
        let Effect::CreateDelayedTrigger {
            condition, effect, ..
        } = &*r.abilities[0].effect
        else {
            panic!("expected delayed trigger, got {:?}", r.abilities[0].effect);
        };
        let crate::types::ability::DelayedTriggerCondition::WheneverEvent { trigger } = condition
        else {
            panic!("expected WheneverEvent, got {condition:?}");
        };
        assert_eq!(trigger.mode, TriggerMode::ChangesZone);
        assert_eq!(trigger.destination, Some(Zone::Battlefield));
        assert!(trigger.valid_card.is_some());
        assert!(effect.optional);
        assert!(r.parse_warnings.is_empty());
    }

    #[test]
    fn ability_word_modal_block_strips_prefix_before_modal_parse() {
        let r = parse(
            "Delirium — Choose one. If there are four or more card types among cards in your graveyard, choose both instead.\n• Draw a card.\n• Gain 3 life.",
            "Test Delirium",
            &[],
            &["Instant"],
            &[],
        );
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 2);
        assert_eq!(modal.constraints.len(), 1);
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
        assert!(matches!(
            *r.abilities[1].effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
    }

    #[test]
    fn labeled_modal_bullets_use_effect_bodies() {
        let r = parse(
            "Choose one —\n• Alpha — Draw a card.\n• Beta — Gain 3 life.",
            "Test Charm",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 2);
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
        assert!(matches!(
            *r.abilities[1].effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));

        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(
            modal.mode_descriptions,
            vec![
                "Alpha — Draw a card.".to_string(),
                "Beta — Gain 3 life.".to_string()
            ]
        );
    }

    #[test]
    fn triggered_modal_block_routes_modes_through_effect_parser() {
        let r = parse(
            "When you set this scheme in motion, choose one —\n• Search your library for a creature card, reveal it, put it into your hand, then shuffle.\n• You may put a creature card from your hand onto the battlefield.",
            "Introductions Are In Order",
            &[],
            &["Scheme"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.triggers.len(), 1);

        let trigger = &r.triggers[0];
        assert_eq!(trigger.mode, TriggerMode::SetInMotion);

        let execute = trigger
            .execute
            .as_ref()
            .expect("trigger should have execute");
        assert!(matches!(
            *execute.effect,
            Effect::GenericEffect {
                ref static_abilities,
                duration: None,
                target: None,
            } if static_abilities.is_empty()
        ));
        let modal = execute.modal.as_ref().expect("execute should be modal");
        assert_eq!(modal.mode_count, 2);
        assert_eq!(execute.mode_abilities.len(), 2);

        assert!(matches!(
            *execute.mode_abilities[0].effect,
            Effect::SearchLibrary { .. }
        ));
        let search_sub = execute.mode_abilities[0]
            .sub_ability
            .as_ref()
            .expect("search mode should have change-zone followup");
        assert!(matches!(
            *search_sub.effect,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                ..
            }
        ));

        assert!(matches!(
            *execute.mode_abilities[1].effect,
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                ..
            }
        ));
    }

    #[test]
    fn triggered_modal_labeled_modes_strip_labels_before_effect_parse() {
        let r = parse(
            "At the beginning of your upkeep, choose one that hasn't been chosen —\n• Buffet — Create three Food tokens.\n• See a Show — Create two 2/2 white Performer creature tokens.\n• Play Games — Search your library for a card, put that card into your hand, discard a card at random, then shuffle.\n• Go to Sleep — You lose 15 life. Sacrifice Night Out in Vegas.",
            "Night Out in Vegas",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.triggers.len(), 1);

        let execute = r.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute");
        let modal = execute.modal.as_ref().expect("execute should be modal");
        assert_eq!(modal.mode_count, 4);
        assert_eq!(
            modal.constraints,
            vec![ModalSelectionConstraint::NoRepeatThisGame]
        );
        assert_eq!(execute.mode_abilities.len(), 4);

        assert!(matches!(
            *execute.mode_abilities[2].effect,
            Effect::SearchLibrary { .. }
        ));
        let search_sub = execute.mode_abilities[2]
            .sub_ability
            .as_ref()
            .expect("play games mode should have change-zone followup");
        assert!(matches!(
            *search_sub.effect,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                ..
            }
        ));

        assert!(matches!(
            *execute.mode_abilities[3].effect,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 15 },
                ..
            }
        ));
    }

    // CR 702.xxx: Prepare (Strixhaven) — Biblioplex Tomekeeper's ETB is a
    // modal trigger whose branches invoke the `becomes prepared` / `becomes
    // unprepared` imperatives. The modal-branch builder must route each
    // branch body through the same effect-chain parser that recognizes these
    // imperatives at the top level. Assign when WotC publishes SOS CR update.
    #[test]
    fn biblioplex_modal_etb_routes_becomes_prepared_branches() {
        let r = parse(
            "When this creature enters, choose up to one —\n• Target creature becomes prepared. (Only creatures with prepare spells can become prepared.)\n• Target creature becomes unprepared.",
            "Biblioplex Tomekeeper",
            &[],
            &["Creature"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.triggers.len(), 1);

        let execute = r.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute");
        let modal = execute.modal.as_ref().expect("execute should be modal");
        assert_eq!(modal.mode_count, 2);
        assert_eq!(execute.mode_abilities.len(), 2);

        // First branch: Target creature becomes prepared.
        assert!(matches!(
            *execute.mode_abilities[0].effect,
            Effect::BecomePrepared { .. }
        ));
        // Second branch: Target creature becomes unprepared.
        assert!(matches!(
            *execute.mode_abilities[1].effect,
            Effect::BecomeUnprepared { .. }
        ));
    }

    #[test]
    fn triggered_modal_header_supports_you_may_choose_and_constraints() {
        let r = parse(
            "At the beginning of combat on your turn, you may choose two. Each mode must target a different player.\n• Target player creates a 2/1 white and black Inkling creature token with flying.\n• Target player draws a card and loses 1 life.\n• Target player puts a +1/+1 counter on each creature they control.",
            "Shadrix Silverquill",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1);
        let execute = r.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute");
        let modal = execute.modal.as_ref().expect("execute should be modal");
        assert_eq!(modal.min_choices, 2);
        assert_eq!(modal.max_choices, 2);
        assert_eq!(modal.mode_count, 3);
        assert_eq!(
            modal.constraints,
            vec![ModalSelectionConstraint::DifferentTargetPlayers]
        );
    }

    #[test]
    fn triggered_modal_commander_condition_caps_choose_both() {
        let r = parse(
            "At the beginning of combat on your turn, choose one. If you control a commander, you may choose both instead.\n• Create a 1/1 white Soldier creature token.\n• Put a +1/+1 counter on each Soldier you control.",
            "SOLDIER Military Program",
            &[],
            &["Enchantment"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1);
        let execute = r.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute");
        let modal = execute.modal.as_ref().expect("execute should be modal");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 2);
        assert_eq!(
            modal.constraints,
            vec![ModalSelectionConstraint::ConditionalMaxChoices {
                condition: crate::types::ability::ModalSelectionCondition::Static {
                    condition: StaticCondition::ControlsCommander {
                        ownership: crate::types::ability::CommanderOwnership::Any,
                    },
                },
                max_choices: 2,
                otherwise_max_choices: 1,
            }]
        );
        assert!(r.parse_warnings.is_empty());
    }

    #[test]
    fn monument_to_endurance_parses_no_repeat_this_turn() {
        let r = parse(
            "At the beginning of your end step, choose one that hasn't been chosen this turn —\n• Put a +1/+1 counter on Monument to Endurance.\n• You gain 4 life.\n• Create a 0/0 green Hydra creature token with \"This creature gets +1/+1 for each counter on it.\"",
            "Monument to Endurance",
            &[],
            &["Enchantment", "Creature"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1);
        let execute = r.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute");
        let modal = execute.modal.as_ref().expect("execute should be modal");
        assert_eq!(modal.mode_count, 3);
        assert_eq!(
            modal.constraints,
            vec![ModalSelectionConstraint::NoRepeatThisTurn]
        );
        assert_eq!(execute.mode_abilities.len(), 3);
    }

    #[test]
    fn non_modal_spell_has_no_modal_metadata() {
        let r = parse(
            "Deal 3 damage to any target.",
            "Lightning Bolt",
            &[],
            &["Instant"],
            &[],
        );
        assert!(r.modal.is_none());
    }

    #[test]
    fn modal_activated_ability_bow_of_nylea() {
        let r = parse(
            "Attacking creatures you control have deathtouch.\n{1}{G}, {T}: Choose one —\n• Put a +1/+1 counter on target creature.\n• Bow of Nylea deals 2 damage to target creature with flying.\n• You gain 3 life.\n• Put up to four target cards from your graveyard on the bottom of your library in any order.",
            "Bow of Nylea",
            &[],
            &["Enchantment", "Artifact"],
            &[],
        );
        // First ability is the static deathtouch line, parsed as a regular ability
        // Second ability is the modal activated ability
        let modal_def = r.abilities.iter().find(|a| a.modal.is_some());
        assert!(modal_def.is_some(), "should have a modal activated ability");
        let modal_def = modal_def.unwrap();
        let modal = modal_def.modal.as_ref().unwrap();
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 4);
        assert_eq!(modal_def.mode_abilities.len(), 4);
        assert!(modal_def.cost.is_some(), "should have a cost");
    }

    #[test]
    fn modal_activated_ability_cankerbloom() {
        let r = parse(
            "{1}, Sacrifice Cankerbloom: Choose one —\n• Destroy target artifact.\n• Destroy target enchantment.",
            "Cankerbloom",
            &[],
            &["Creature"],
            &[],
        );
        let modal_def = r.abilities.iter().find(|a| a.modal.is_some());
        assert!(modal_def.is_some(), "should have a modal activated ability");
        let modal = modal_def.unwrap().modal.as_ref().unwrap();
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 2);
        // Spell-level modal should NOT be set (this is an activated ability modal)
        assert!(r.modal.is_none(), "spell-level modal should be None");
    }

    #[test]
    fn modal_activated_ability_preserves_activation_restrictions() {
        let r = parse(
            "{G}: Choose one. Activate only once each turn.\n\
             • Until end of turn, this creature becomes a Rhino with base power and toughness 4/4 and gains trample.\n\
             • Until end of turn, this creature becomes a Bird with base power and toughness 2/2 and gains flying.",
            "Test Shifter",
            &[],
            &["Creature"],
            &[],
        );
        let modal_def = r
            .abilities
            .iter()
            .find(|ability| ability.modal.is_some())
            .expect("should have a modal activated ability");
        assert!(
            modal_def
                .activation_restrictions
                .contains(&ActivationRestriction::OnlyOnceEachTurn),
            "modal activated ability should preserve once-per-turn restriction"
        );
    }

    #[test]
    fn modal_activated_ability_uses_normalized_mode_bodies() {
        let r = parse(
            "{1}, {T}: Choose one —\n• Alpha — Draw a card.\n• Beta — Gain 3 life.",
            "Test Relic",
            &[],
            &["Artifact"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let modal_def = &r.abilities[0];
        let modal = modal_def
            .modal
            .as_ref()
            .expect("should have modal metadata");
        assert_eq!(modal.mode_count, 2);
        assert_eq!(modal_def.mode_abilities.len(), 2);
        assert!(matches!(
            *modal_def.mode_abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
        assert!(matches!(
            *modal_def.mode_abilities[1].effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
        assert!(modal_def.cost.is_some(), "should preserve activated cost");
    }

    // ── Spree (CR 702.172) ──────────────────────────────────────────────

    #[test]
    fn spree_phantom_interference_parses_modal_with_mode_costs() {
        let text = "Spree (Choose one or more additional costs.)\n\
                     + {3} — Create a 2/2 white Spirit creature token with flying.\n\
                     + {1} — Counter target spell unless its controller pays {2}.";
        let result = parse(
            text,
            "Phantom Interference",
            &[Keyword::Spree],
            &["Instant"],
            &[],
        );
        let modal = result.modal.expect("should have modal");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 2);
        assert_eq!(modal.mode_count, 2);
        assert_eq!(modal.mode_costs.len(), 2);
        // Mode 0: {3}
        assert_eq!(
            modal.mode_costs[0],
            ManaCost::Cost {
                shards: vec![],
                generic: 3
            }
        );
        // Mode 1: {1}
        assert_eq!(
            modal.mode_costs[1],
            ManaCost::Cost {
                shards: vec![],
                generic: 1
            }
        );
        // Mode descriptions are effect-text only (post-separator)
        assert!(modal.mode_descriptions[0].contains("Create a 2/2"));
        assert!(modal.mode_descriptions[1].contains("Counter target spell"));
        // Two mode abilities parsed (not Unimplemented)
        assert_eq!(result.abilities.len(), 2);
        assert!(!matches!(
            *result.abilities[0].effect,
            Effect::Unimplemented { .. }
        ));
    }

    #[test]
    fn spree_colored_mode_costs_parsed_correctly() {
        // Final Showdown has colored mode costs
        let text = "Spree (Choose one or more additional costs.)\n\
                     + {1} — All creatures lose all abilities until end of turn.\n\
                     + {1} — Choose a creature you control. It gains indestructible until end of turn.\n\
                     + {3}{W}{W} — Destroy all creatures.";
        let result = parse(text, "Final Showdown", &[Keyword::Spree], &["Instant"], &[]);
        let modal = result.modal.expect("should have modal");
        assert_eq!(modal.mode_count, 3);
        assert_eq!(modal.max_choices, 3);
        assert_eq!(modal.mode_costs.len(), 3);
        // Third mode: {3}{W}{W}
        if let ManaCost::Cost { shards, generic } = &modal.mode_costs[2] {
            assert_eq!(*generic, 3);
            assert_eq!(shards.len(), 2); // WW
        } else {
            panic!("Expected ManaCost::Cost for mode 2");
        }
    }

    #[test]
    fn tiered_restoration_magic_parses_modal_with_mode_costs() {
        let text = "Tiered (Choose one additional cost.)\n\
                    • Cure — {0} — Target permanent gains hexproof and indestructible until end of turn.\n\
                    • Cura — {1} — Target permanent gains hexproof and indestructible until end of turn. You gain 3 life.\n\
                    • Curaga — {3}{W} — Permanents you control gain hexproof and indestructible until end of turn. You gain 6 life.";
        let result = parse(text, "Restoration Magic", &[], &["Instant"], &[]);
        let modal = result.modal.expect("Tiered should parse as modal");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 3);
        assert_eq!(modal.mode_costs.len(), 3);
        assert_eq!(modal.mode_costs[0], ManaCost::zero());
        assert_eq!(modal.mode_costs[1], ManaCost::generic(1));
        assert_eq!(
            modal.mode_costs[2],
            ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 3
            }
        );
        assert!(result
            .abilities
            .iter()
            .all(|ability| { !matches!(*ability.effect, Effect::Unimplemented { .. }) }));
    }

    #[test]
    fn parse_saga_the_eldest_reborn() {
        let oracle = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI — Each opponent discards a card.\nII — Put target creature card from a graveyard onto the battlefield under your control.\nIII — Return target nonland permanent card from your graveyard to the battlefield under your control.";
        let result = parse_oracle_text(
            oracle,
            "The Eldest Reborn",
            &[],
            &["Enchantment".to_string()],
            &["Saga".to_string()],
        );

        // 3 chapter triggers
        assert_eq!(
            result.triggers.len(),
            3,
            "Expected 3 chapter triggers, got: {:?}",
            result.triggers.len()
        );
        for (i, trigger) in result.triggers.iter().enumerate() {
            assert_eq!(trigger.mode, TriggerMode::CounterAdded);
            let filter = trigger
                .counter_filter
                .as_ref()
                .expect("should have counter_filter");
            assert_eq!(
                filter.counter_type,
                crate::types::counter::CounterType::Lore
            );
            assert_eq!(filter.threshold, Some((i + 1) as u32));
            assert_eq!(trigger.trigger_zones, vec![Zone::Battlefield]);
        }

        // 1 ETB replacement for lore counter
        assert!(
            !result.replacements.is_empty(),
            "Expected at least 1 replacement (ETB lore counter)"
        );
        let etb = &result.replacements[0];
        assert_eq!(etb.event, ReplacementEvent::Moved);
        assert_eq!(etb.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn discard_self_to_battlefield_instead_is_replacement_not_spell_ability() {
        let result = parse(
            "If a spell or ability an opponent controls causes you to discard this card, put it onto the battlefield instead of putting it into your graveyard.",
            "Loxodon Smiter",
            &[],
            &["Creature"],
            &["Elephant", "Soldier"],
        );

        assert_eq!(result.replacements.len(), 1);
        assert!(result.abilities.is_empty());
        assert!(result
            .parse_warnings
            .iter()
            .all(|warning| warning.category_name() != "swallowed-clause"));
    }

    #[test]
    fn damage_to_self_counter_instead_is_replacement_not_spell_ability() {
        let result = parse(
            "If damage would be dealt to this creature, put that many +1/+1 counters on it instead.",
            "Phytohydra",
            &[],
            &["Creature"],
            &["Plant", "Hydra"],
        );

        assert_eq!(result.replacements.len(), 1);
        assert!(result.abilities.is_empty());
        assert!(result
            .parse_warnings
            .iter()
            .all(|warning| warning.category_name() != "swallowed-clause"));
    }

    #[test]
    fn parse_saga_multi_chapter_line() {
        let oracle = "(Reminder text.)\nI, II — Draw a card.\nIII — Discard a card.";
        let result = parse_oracle_text(
            oracle,
            "Test Saga",
            &[],
            &["Enchantment".to_string()],
            &["Saga".to_string()],
        );

        // I and II share the same effect, III is separate = 3 triggers total
        assert_eq!(result.triggers.len(), 3);
        assert_eq!(
            result.triggers[0]
                .counter_filter
                .as_ref()
                .unwrap()
                .threshold,
            Some(1)
        );
        assert_eq!(
            result.triggers[1]
                .counter_filter
                .as_ref()
                .unwrap()
                .threshold,
            Some(2)
        );
        assert_eq!(
            result.triggers[2]
                .counter_filter
                .as_ref()
                .unwrap()
                .threshold,
            Some(3)
        );
    }

    #[test]
    fn ghirapur_grand_prix_put_counter_uses_speed_quantity() {
        let oracle = "When you planeswalk here, all players start their engines! (If you have no speed, it starts at 1. It increases once on each of your turns when an opponent loses life. Max speed is 4.)\nAt the beginning of your end step, put X +1/+1 counters on target creature you control, where X is your speed.\nWhen you planeswalk away from Ghirapur Grand Prix, each player with the highest speed among players creates three Treasure tokens.";
        let result = parse_oracle_text(
            oracle,
            "Ghirapur Grand Prix",
            &[],
            &[],
            &["Avishkar".to_string()],
        );

        let end_step_trigger = result
            .triggers
            .iter()
            .find(|trigger| {
                trigger
                    .description
                    .as_deref()
                    .is_some_and(|d| d.contains("put X +1/+1 counters"))
            })
            .expect("expected end-step trigger");
        let execute = end_step_trigger.execute.as_ref().expect("expected execute");
        assert!(matches!(
            *execute.effect,
            Effect::PutCounter {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Speed { .. },
                },
                ..
            }
        ));
    }

    #[test]
    fn parse_saga_subtypes_detection() {
        // Non-saga should NOT produce chapter triggers
        let oracle = "I — Draw a card.";
        let result =
            parse_oracle_text(oracle, "Not A Saga", &[], &["Enchantment".to_string()], &[]);
        assert!(
            result.triggers.is_empty(),
            "Non-saga subtypes should not produce chapter triggers"
        );
    }

    // ── Feature #1: Reflexive triggers ("when you do") ──────────────

    #[test]
    fn reflexive_trigger_when_you_do_sentence_split() {
        // "you may pay {1}. When you do, draw a card" — sentence-split produces
        // a chunk starting with "When you do, ..." that strip_if_you_do_conditional handles.
        let r = parse(
            "Whenever ~ attacks, you may pay {1}. When you do, draw a card.",
            "Test Card",
            &[],
            &["Creature"],
            &[],
        );
        assert!(!r.triggers.is_empty(), "should parse the trigger");
        let abilities = r.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute");
        // First ability is PayCost (optional), second is Draw with WhenYouDo condition.
        // CR 603.12: "when you do" is a reflexive trigger, distinct from "if you do".
        assert!(
            matches!(*abilities.effect, Effect::PayCost { .. }),
            "first effect should be PayCost, got {:?}",
            abilities.effect,
        );
        let sub = abilities
            .sub_ability
            .as_ref()
            .expect("should have sub_ability");
        assert_eq!(
            sub.condition,
            Some(crate::types::ability::AbilityCondition::WhenYouDo),
            "sub-ability should have WhenYouDo condition"
        );
        assert!(
            matches!(*sub.effect, Effect::Draw { .. }),
            "sub effect should be Draw, got {:?}",
            sub.effect,
        );
    }

    #[test]
    fn reflexive_trigger_when_you_do_comma_split() {
        // "when you do, attach ~ to it" — comma-separated, starts_prefix_clause
        // must prevent splitting at the comma boundary.
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain(
            "When you do, attach Ancestral Katana to it",
            crate::types::ability::AbilityKind::Spell,
        );
        assert_eq!(
            def.condition,
            Some(crate::types::ability::AbilityCondition::WhenYouDo),
            "should detect WhenYouDo condition"
        );
        assert!(
            matches!(*def.effect, Effect::Attach { .. }),
            "effect should be Attach, got {:?}",
            def.effect,
        );
    }

    // ── Feature #2: "Cast without paying" effects ───────────────────

    #[test]
    fn cast_without_paying_mana_cost() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("cast it without paying its mana cost");
        assert!(
            matches!(
                effect,
                Effect::CastFromZone {
                    target: TargetFilter::ParentTarget,
                    without_paying_mana_cost: true,
                    ..
                }
            ),
            "expected CastFromZone with ParentTarget + without_paying, got {:?}",
            effect,
        );
    }

    #[test]
    fn cast_that_card() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("cast that card");
        assert!(
            matches!(
                effect,
                Effect::CastFromZone {
                    target: TargetFilter::ParentTarget,
                    without_paying_mana_cost: false,
                    ..
                }
            ),
            "expected CastFromZone with ParentTarget + paying, got {:?}",
            effect,
        );
    }

    #[test]
    fn cast_clause_splits_correctly() {
        // "exile the top card of your library, then cast it without paying its mana cost"
        // "cast it..." should be a separate clause, not merged with "exile..."
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain(
            "exile the top card of your library, then cast it without paying its mana cost",
            crate::types::ability::AbilityKind::Spell,
        );
        // First effect is ExileTop (dedicated top-of-library exile), sub is CastFromZone
        assert!(
            matches!(*def.effect, Effect::ExileTop { .. }),
            "first effect should be ExileTop, got {:?}",
            def.effect,
        );
        let sub = def
            .sub_ability
            .as_ref()
            .expect("should have sub_ability for cast");
        assert!(
            matches!(
                *sub.effect,
                Effect::CastFromZone {
                    without_paying_mana_cost: true,
                    ..
                }
            ),
            "sub effect should be CastFromZone with without_paying, got {:?}",
            sub.effect,
        );
    }

    // ── Feature #3: "For each" iteration ────────────────────────────

    #[test]
    fn for_each_prefix_creates_token() {
        // "for each opponent, create a 2/2 black Zombie creature token"
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{QuantityExpr, QuantityRef};
        let def = parse_effect_chain(
            "for each opponent, create a 2/2 black Zombie creature token",
            crate::types::ability::AbilityKind::Spell,
        );
        // CR 111.1 + CR 616.1: a bare single-clause "for each X, create a token"
        // folds the iteration into the token's `count` (one batched CreateToken
        // event), so it must NOT carry a repeat loop. See
        // `try_fold_token_repeat_into_count`.
        assert!(
            def.repeat_for.is_none(),
            "bare for-each token must fold into count, not loop: {:?}",
            def.repeat_for
        );
        let Effect::Token { count, .. } = &*def.effect else {
            panic!("inner effect should be Token, got {:?}", def.effect);
        };
        assert!(
            matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::PlayerCount { .. }
                }
            ),
            "count should carry the per-opponent quantity, got {count:?}"
        );
    }

    #[test]
    fn for_each_prefix_exiles() {
        // "for each opponent, exile up to one target nonland permanent"
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain(
            "for each opponent, exile up to one target nonland permanent",
            crate::types::ability::AbilityKind::Spell,
        );
        assert!(def.repeat_for.is_some(), "repeat_for should be set");
        assert!(
            matches!(*def.effect, Effect::ChangeZone { .. }),
            "inner effect should be ChangeZone (exile), got {:?}",
            def.effect,
        );
    }

    #[test]
    fn for_each_trailing_still_works() {
        // Existing "for each" trailing pattern should still work
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("draw a card for each creature you control");
        assert!(
            matches!(
                effect,
                Effect::Draw {
                    count: QuantityExpr::Ref { .. },
                    ..
                }
            ),
            "trailing 'for each' should produce dynamic Draw, got {:?}",
            effect,
        );
    }

    // ── Coverage batch: keyword granting ──────────────────────────────

    #[test]
    fn gain_haste_keyword_granting() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("gain haste");
        assert!(
            matches!(effect, Effect::GenericEffect { .. }),
            "expected GenericEffect for 'gain haste', got {:?}",
            effect,
        );
    }

    #[test]
    fn gain_flying_until_end_of_turn() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("gain flying until end of turn");
        assert!(
            matches!(effect, Effect::GenericEffect { .. }),
            "expected GenericEffect for 'gain flying until end of turn', got {:?}",
            effect,
        );
    }

    #[test]
    fn gain_trample_and_haste() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("gain trample and haste");
        assert!(
            matches!(effect, Effect::GenericEffect { .. }),
            "expected GenericEffect for 'gain trample and haste', got {:?}",
            effect,
        );
    }

    // ── Coverage batch: investigate ───────────────────────────────────

    #[test]
    fn investigate_parses() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("investigate");
        assert!(
            matches!(effect, Effect::Investigate),
            "expected Investigate, got {:?}",
            effect,
        );
    }

    #[test]
    fn investigate_twice_uses_repeat_for() {
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain("investigate twice", AbilityKind::Spell);
        assert!(
            matches!(*def.effect, Effect::Investigate),
            "first effect should be Investigate, got {:?}",
            def.effect,
        );
        // CR 609.3: "twice" → repeat_for = Fixed(2), resolver handles repetition.
        assert_eq!(def.repeat_for, Some(QuantityExpr::Fixed { value: 2 }));
        assert!(def.sub_ability.is_none());
    }

    #[test]
    fn repeat_this_process_you_may_sets_controller_choice() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::RepeatContinuation;
        // CR 107.1c: Ad Nauseam — "You may repeat this process any number of
        // times." sets the root ability's `repeat_until` to a controller
        // decision, instead of being silently dropped.
        let def = parse_effect_chain(
            "Reveal the top card of your library and put that card into your hand. \
             You lose life equal to its mana value. \
             You may repeat this process any number of times.",
            AbilityKind::Spell,
        );
        assert_eq!(
            def.repeat_until,
            Some(RepeatContinuation::ControllerChoice),
            "expected repeat_until = ControllerChoice, got {:?}",
            def.repeat_until,
        );
    }

    #[test]
    fn repeat_this_process_if_you_do_stays_recognized_without_predicate() {
        use crate::parser::oracle_effect::parse_effect_chain;
        // CR 608.2c: Primal Surge — "If you do, repeat this process." is the
        // game-state-predicate form, a deferred unit. The directive is still
        // recognized (no Unimplemented gap) but sets no `repeat_until`.
        let def = parse_effect_chain(
            "Exile the top card of your library. If it's a permanent card, you \
             may put it onto the battlefield. If you do, repeat this process.",
            AbilityKind::Spell,
        );
        assert_eq!(
            def.repeat_until, None,
            "the 'if you do' form is deferred — no predicate set, got {:?}",
            def.repeat_until,
        );
    }

    #[test]
    fn proliferate_twice_uses_repeat_for() {
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain("proliferate twice", AbilityKind::Spell);
        assert!(
            matches!(*def.effect, Effect::Proliferate),
            "first effect should be Proliferate, got {:?}",
            def.effect,
        );
        assert_eq!(def.repeat_for, Some(QuantityExpr::Fixed { value: 2 }));
        assert!(def.sub_ability.is_none());
    }

    #[test]
    fn investigate_three_times_uses_repeat_for() {
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain("investigate three times", AbilityKind::Spell);
        assert!(matches!(*def.effect, Effect::Investigate));
        // CR 609.3: "three times" → repeat_for = Fixed(3), not cloned sub_ability chain.
        assert_eq!(
            def.repeat_for,
            Some(QuantityExpr::Fixed { value: 3 }),
            "expected repeat_for=Fixed(3), got {:?}",
            def.repeat_for
        );
        assert!(
            def.sub_ability.is_none(),
            "should not clone sub_abilities — resolver handles repetition"
        );
    }

    #[test]
    fn repeat_suffix_preserves_sub_ability_chain() {
        // Verifies that "twice" suffix doesn't drop sub_abilities from compound effects.
        // "scry 2 twice" → Scry with repeat_for=Fixed(2), no cloned chain.
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain("scry 2 twice", AbilityKind::Spell);
        assert!(
            matches!(*def.effect, Effect::Scry { .. }),
            "expected Scry, got {:?}",
            def.effect,
        );
        assert_eq!(def.repeat_for, Some(QuantityExpr::Fixed { value: 2 }));
    }

    #[test]
    fn repeat_suffix_on_draw_card() {
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain("draw a card twice", AbilityKind::Spell);
        // "draw a card" should parse as Draw, with repeat_for = 2
        assert!(matches!(
            &*def.effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
        assert_eq!(def.repeat_for, Some(QuantityExpr::Fixed { value: 2 }));
    }

    // ── Phthisis: destroy + lose life equal to power plus toughness ──────

    /// CR 119.3 + CR 208.1: Phthisis — "Destroy target creature. Its controller
    /// loses life equal to its power plus its toughness." The second clause is a
    /// chained LoseLife whose amount is Sum([Power(Anaphoric), Toughness(Anaphoric)]).
    /// The destroy effect sets `effect_context_object` to the destroyed creature's
    /// LKI, supplying the Anaphoric referent at runtime.
    #[test]
    fn phthisis_destroy_then_lose_life_power_plus_toughness() {
        let oracle = "Destroy target creature. Its controller loses life equal to its power plus its toughness.";
        let def = parse_effect_chain(oracle, AbilityKind::Spell);
        // The root effect is Destroy.
        assert!(
            matches!(&*def.effect, Effect::Destroy { .. }),
            "root effect should be Destroy, got {:?}",
            def.effect,
        );
        // The chained sub-ability must be LoseLife.
        let sub = def
            .sub_ability
            .as_deref()
            .expect("Phthisis must have a chained sub_ability for the life loss");
        assert!(
            matches!(&*sub.effect, Effect::LoseLife { .. }),
            "sub_ability effect should be LoseLife, got {:?}",
            sub.effect,
        );
        // The life-loss amount must be Sum([Power(Anaphoric), Toughness(Anaphoric)]).
        let Effect::LoseLife { amount, .. } = &*sub.effect else {
            panic!("expected LoseLife");
        };
        match amount {
            QuantityExpr::Sum { exprs } => {
                assert_eq!(exprs.len(), 2, "Sum must have exactly two operands");
                assert!(
                    matches!(
                        exprs[0],
                        QuantityExpr::Ref {
                            qty: QuantityRef::Power {
                                scope: ObjectScope::Anaphoric
                            }
                        }
                    ),
                    "first operand must be Power(Anaphoric), got {:?}",
                    exprs[0]
                );
                assert!(
                    matches!(
                        exprs[1],
                        QuantityExpr::Ref {
                            qty: QuantityRef::Toughness {
                                scope: ObjectScope::Anaphoric
                            }
                        }
                    ),
                    "second operand must be Toughness(Anaphoric), got {:?}",
                    exprs[1]
                );
            }
            other => panic!("amount must be Sum, got {other:?}"),
        }
        // No Unimplemented anywhere in the chain.
        assert!(
            !matches!(&*sub.effect, Effect::Unimplemented { .. }),
            "LoseLife sub-effect must not be Unimplemented"
        );
    }

    // ── Coverage batch: gold tokens ──────────────────────────────────

    #[test]
    fn create_gold_token() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("create a Gold token");
        assert!(
            matches!(effect, Effect::Token { ref name, .. } if name == "Gold"),
            "expected Gold Token, got {:?}",
            effect,
        );
    }

    // ── Coverage batch: become the monarch ────────────────────────────

    #[test]
    fn become_the_monarch_imperative() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("become the monarch");
        assert!(
            matches!(effect, Effect::BecomeMonarch),
            "expected BecomeMonarch, got {:?}",
            effect,
        );
    }

    #[test]
    fn you_become_the_monarch_subject() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("you become the monarch");
        assert!(
            matches!(effect, Effect::BecomeMonarch),
            "expected BecomeMonarch, got {:?}",
            effect,
        );
    }

    // ── Coverage batch: prevent damage ────────────────────────────────

    #[test]
    fn prevent_next_3_damage() {
        use crate::parser::oracle_effect::parse_effect;
        use crate::types::ability::PreventionAmount;
        let effect =
            parse_effect("prevent the next 3 damage that would be dealt to any target this turn");
        match effect {
            Effect::PreventDamage {
                amount: PreventionAmount::Next(3),
                ..
            } => {}
            _ => panic!("expected PreventDamage with Next(3), got {:?}", effect),
        }
    }

    #[test]
    fn prevent_all_combat_damage() {
        use crate::parser::oracle_effect::parse_effect;
        use crate::types::ability::{PreventionAmount, PreventionScope};
        let effect = parse_effect("prevent all combat damage that would be dealt this turn");
        match effect {
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                scope: PreventionScope::CombatDamage,
                ..
            } => {}
            _ => panic!(
                "expected PreventDamage All + CombatDamage, got {:?}",
                effect
            ),
        }
    }

    #[test]
    fn prevent_dynamic_amount_where_x_is_counters() {
        use crate::types::ability::{ObjectScope, PreventionAmount, QuantityExpr, QuantityRef};
        use crate::types::counter::CounterType;
        // Cover of Winter class: "prevent X … where X is the number of age
        // counters on this enchantment". The chunk machinery strips the
        // trailing "where x is …" binding and `apply_where_x_effect_expression`
        // re-applies it onto `Effect::PreventDamage::amount_dynamic`. Driven
        // through the full `parse` path because the chunk-level where-X
        // mechanism does not run inside the single-clause `parse_effect`.
        let parsed = parse(
            "If a creature would deal combat damage to you and/or one or more creatures \
             you control, prevent X of that damage, where X is the number of age counters \
             on this enchantment.",
            "Cover of Winter",
            &[],
            &["Snow", "Enchantment"],
            &[],
        );
        let prevent = parsed
            .abilities
            .iter()
            .find(|a| matches!(&*a.effect, Effect::PreventDamage { .. }))
            .expect("expected a PreventDamage ability");
        match &*prevent.effect {
            Effect::PreventDamage {
                amount: PreventionAmount::Next(1),
                amount_dynamic:
                    Some(QuantityExpr::Ref {
                        qty:
                            QuantityRef::CountersOn {
                                scope: ObjectScope::Source,
                                counter_type: Some(ct),
                            },
                    }),
                ..
            } => assert_eq!(*ct, CounterType::Age),
            other => panic!("expected PreventDamage with dynamic age counters, got {other:?}"),
        }
        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|w| w.to_string().split_whitespace().next() != Some("Swallow:DynamicQty")),
            "DynamicQty swallow warning should clear, got {:?}",
            parsed.parse_warnings
        );
    }

    #[test]
    fn prevent_all_damage_has_no_dynamic_amount() {
        use crate::parser::oracle_effect::parse_effect;
        use crate::types::ability::PreventionAmount;
        let effect = parse_effect("prevent all damage that would be dealt this turn");
        match effect {
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                ..
            } => {}
            other => panic!("expected PreventDamage All + no dynamic, got {other:?}"),
        }
    }

    #[test]
    fn prevent_next_3_has_no_dynamic_amount() {
        use crate::parser::oracle_effect::parse_effect;
        use crate::types::ability::PreventionAmount;
        let effect =
            parse_effect("prevent the next 3 damage that would be dealt to any target this turn");
        match effect {
            Effect::PreventDamage {
                amount: PreventionAmount::Next(3),
                amount_dynamic: None,
                ..
            } => {}
            other => panic!("expected PreventDamage Next(3) + no dynamic, got {other:?}"),
        }
    }

    #[test]
    fn spell_prevention_keeps_preceding_dynamic_gain_life() {
        use crate::types::ability::{PreventionAmount, QuantityExpr, QuantityRef};

        let parsed = parse(
            "You gain 1 life for each creature on the battlefield. Prevent all combat damage that would be dealt this turn.",
            "Blunt the Assault",
            &[],
            &["Instant"],
            &[],
        );

        assert!(
            parsed.replacements.is_empty(),
            "spell prevention should parse as resolving effect, got {:?}",
            parsed.replacements
        );
        assert_eq!(parsed.abilities.len(), 1);
        let ability = &parsed.abilities[0];
        match &*ability.effect {
            Effect::GainLife {
                amount:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. },
                    },
                ..
            } => {}
            other => panic!("expected dynamic GainLife, got {other:?}"),
        }
        let prevention = ability
            .sub_ability
            .as_ref()
            .expect("expected prevention follow-up");
        assert!(matches!(
            &*prevention.effect,
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                ..
            }
        ));
        assert!(
            parsed.parse_warnings.iter().all(|warning| warning
                .to_string()
                .split_whitespace()
                .next()
                != Some("Swallow:DynamicQty")),
            "unexpected dynamic quantity warning: {:?}",
            parsed.parse_warnings
        );
    }

    // ── Coverage batch: play from exile ────────────────────────────────

    #[test]
    fn play_that_card() {
        use crate::parser::oracle_effect::parse_effect;
        use crate::types::ability::CardPlayMode;
        let effect = parse_effect("play that card");
        match effect {
            Effect::CastFromZone {
                mode: CardPlayMode::Play,
                target: TargetFilter::ParentTarget,
                ..
            } => {}
            _ => panic!("expected CastFromZone with Play mode, got {:?}", effect),
        }
    }

    #[test]
    fn cast_uses_cast_mode() {
        use crate::parser::oracle_effect::parse_effect;
        use crate::types::ability::CardPlayMode;
        let effect = parse_effect("cast that card");
        match effect {
            Effect::CastFromZone {
                mode: CardPlayMode::Cast,
                ..
            } => {}
            _ => panic!("expected CastFromZone with Cast mode, got {:?}", effect),
        }
    }

    // ── Coverage batch: shuffle and put on top ─────────────────────────

    #[test]
    fn put_that_card_on_top_abbreviated() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("put that card on top");
        assert!(
            matches!(effect, Effect::PutAtLibraryPosition { .. }),
            "expected PutAtLibraryPosition for abbreviated form, got {:?}",
            effect,
        );
    }

    #[test]
    fn put_them_on_top_abbreviated() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("put them on top");
        assert!(
            matches!(effect, Effect::PutAtLibraryPosition { .. }),
            "expected PutAtLibraryPosition for 'put them on top', got {:?}",
            effect,
        );
    }

    #[test]
    fn put_on_top_of_library_long_form() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("put it on top of your library");
        assert!(
            matches!(effect, Effect::PutAtLibraryPosition { .. }),
            "expected PutAtLibraryPosition for long form, got {:?}",
            effect,
        );
    }

    #[test]
    fn enlightened_tutor_chain() {
        // CR 701.24b: "search, reveal, then shuffle and put that card on top"
        // Should produce: SearchLibrary → Shuffle → PutAtLibraryPosition (no ChangeZone→Hand)
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        let chain = parse_effect_chain(
            "Search your library for an artifact or enchantment card, reveal it, then shuffle and put that card on top",
            AbilityKind::Spell,
        );
        // First effect: SearchLibrary with reveal
        assert!(
            matches!(*chain.effect, Effect::SearchLibrary { reveal: true, .. }),
            "expected SearchLibrary with reveal, got {:?}",
            chain.effect,
        );
        // Sub_ability: Shuffle
        let sub1 = chain
            .sub_ability
            .as_ref()
            .expect("should have sub_ability (Shuffle)");
        assert!(
            matches!(*sub1.effect, Effect::Shuffle { .. }),
            "expected Shuffle as second effect, got {:?}",
            sub1.effect,
        );
        // Sub_ability of Shuffle: PutOnTop
        let sub2 = sub1
            .sub_ability
            .as_ref()
            .expect("should have sub_ability (PutAtLibraryPosition)");
        assert!(
            matches!(*sub2.effect, Effect::PutAtLibraryPosition { .. }),
            "expected PutAtLibraryPosition as third effect, got {:?}",
            sub2.effect,
        );
        // No further sub_abilities
        assert!(
            sub2.sub_ability.is_none(),
            "PutAtLibraryPosition should be the last effect in chain",
        );
    }

    #[test]
    fn choice_partition_after_search_routes_chosen_and_rest() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{AbilityKind, Chooser};

        let chain = parse_effect_chain(
            "Search your library for up to four cards with different names and reveal them. Target opponent chooses two of those cards. Put the chosen cards into your graveyard and the rest into your hand. Then shuffle.",
            AbilityKind::Spell,
        );
        let choose = chain
            .sub_ability
            .as_ref()
            .and_then(|search_move| search_move.sub_ability.as_ref())
            .expect("search move should chain to ChooseFromZone");
        assert!(matches!(
            &*choose.effect,
            Effect::ChooseFromZone {
                count: 2,
                chooser: Chooser::Opponent,
                ..
            }
        ));
        let chosen_move = choose
            .sub_ability
            .as_ref()
            .expect("choice should route chosen cards first");
        assert!(matches!(
            &*chosen_move.effect,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Graveyard,
                ..
            }
        ));
        let rest_move = chosen_move
            .sub_ability
            .as_ref()
            .expect("chosen move should route the unchosen remainder");
        assert!(matches!(
            &*rest_move.effect,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Hand,
                ..
            }
        ));
    }

    #[test]
    fn emergent_growth_routes_to_spell_not_static() {
        // Emergent Growth: compound pump + must-be-blocked should route to spell
        // effect parsing, not static parsing.
        let parsed = parse(
            "Target creature gets +5/+5 until end of turn and must be blocked this turn if able.",
            "Emergent Growth",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(
            !parsed.abilities.is_empty(),
            "Emergent Growth should produce a spell ability, got abilities={:?}, statics={:?}",
            parsed.abilities,
            parsed.statics,
        );
        assert!(
            parsed.statics.is_empty(),
            "Emergent Growth should NOT produce static abilities, got {:?}",
            parsed.statics,
        );
    }

    // -----------------------------------------------------------------------
    // Channel (CR 207.2c — ability word)
    // -----------------------------------------------------------------------

    #[test]
    fn channel_parses_as_activated_from_hand() {
        // Eiganjo, Seat of the Empire — Channel line
        let r = parse(
            "Channel — {2}{W}, Discard this card: It deals 4 damage to target attacking or blocking creature.",
            "Eiganjo, Seat of the Empire",
            &[],
            &["Land"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Activated);
        // CR 207.2c: Channel is an ability word — the underlying ability activates from hand
        assert_eq!(ability.activation_zone, Some(Zone::Hand));
        // Cost should contain mana + self-ref discard, not Unimplemented
        match ability.cost.as_ref().unwrap() {
            AbilityCost::Composite { costs } => {
                assert!(
                    costs.iter().any(|c| matches!(c, AbilityCost::Mana { .. })),
                    "Channel cost should include mana, got {:?}",
                    costs
                );
                assert!(
                    costs
                        .iter()
                        .any(|c| matches!(c, AbilityCost::Discard { self_ref: true, .. })),
                    "Channel cost should include self-ref discard, got {:?}",
                    costs
                );
                assert!(
                    !costs
                        .iter()
                        .any(|c| matches!(c, AbilityCost::Unimplemented { .. })),
                    "Channel cost should NOT contain Unimplemented, got {:?}",
                    costs
                );
            }
            other => panic!("Expected Composite cost, got {:?}", other),
        }
        // Effect should not be Unimplemented
        assert!(
            !matches!(*ability.effect, Effect::Unimplemented { .. }),
            "Channel effect should not be Unimplemented, got {:?}",
            ability.effect,
        );
    }

    #[test]
    fn gogo_copy_ability_targets_controlled_stack_ability_and_strips_annotations() {
        let r = parse(
            "{X}{X}, {T}: Copy target activated or triggered ability you control X times. You may choose new targets for the copies. This ability can't be copied and X can't be 0. (Mana abilities can't be targeted.)",
            "Gogo, Master of Mimicry",
            &[],
            &["Creature"],
            &["Wizard"],
        );
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Activated);
        assert!(ability.cant_be_copied);
        assert_eq!(ability.min_x_value, 1);
        assert!(matches!(
            ability.repeat_for,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Variable { ref name }
            }) if name == "X"
        ));
        let Effect::CopySpell { target, .. } = &*ability.effect else {
            panic!("expected CopySpell, got {:?}", ability.effect);
        };
        assert!(matches!(
            target,
            TargetFilter::StackAbility {
                controller: Some(ControllerRef::You)
            }
        ));
        assert!(
            ability.sub_ability.is_none(),
            "retarget annotation should not become a sub-ability: {:?}",
            ability.sub_ability
        );
    }

    #[test]
    fn spell_x_cant_be_zero_annotation_sets_min_x_value() {
        let r = parse(
            "Draw X cards.\nX can't be 0.",
            "Test X Draw",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Spell);
        assert_eq!(ability.min_x_value, 1);
        assert!(matches!(
            *ability.effect,
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Variable { ref name }
                },
                ..
            } if name == "X"
        ));
    }

    #[test]
    fn channel_with_em_dash_variant() {
        // Test both em-dash (—) and double-hyphen (--) parsing
        let r = parse(
            "Channel -- {1}{G}, Discard this card: Search your library for a basic land card, reveal it, put it into your hand, then shuffle.",
            "Test Channel Card",
            &[],
            &["Creature"],
            &["Spirit"],
        );
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
        assert_eq!(r.abilities[0].activation_zone, Some(Zone::Hand));
    }

    // -----------------------------------------------------------------------
    // CR 113.6m — activation zone derived from a self-ChangeZone *effect*
    // -----------------------------------------------------------------------

    #[test]
    fn put_self_from_hand_onto_battlefield_activates_from_hand() {
        // Talon Gates of Madara — the {4}: Put this card from your hand onto
        // the battlefield ability. The "from your hand" lives in the effect,
        // not the cost, so activation_zone must be derived effect-side.
        let r = parse(
            "{4}: Put this card from your hand onto the battlefield.",
            "Talon Gates of Madara",
            &[],
            &["Land"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Activated);
        // CR 113.6m: effect moves the source out of hand → functions from hand.
        assert_eq!(ability.activation_zone, Some(Zone::Hand));
    }

    #[test]
    fn put_self_from_graveyard_onto_battlefield_activates_from_graveyard() {
        // Building-block test: the derivation generalizes across origin zones,
        // not just Talon Gates' Hand. CR 113.6m example: Reassembling Skeleton.
        let r = parse(
            "{2}: Put this card from your graveyard onto the battlefield.",
            "Test Recursion Land",
            &[],
            &["Land"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Activated);
        assert_eq!(ability.activation_zone, Some(Zone::Graveyard));
    }

    #[test]
    fn battlefield_self_changezone_leaves_activation_zone_unset() {
        // Negative control: a normal battlefield-activated ability whose effect
        // does NOT move the source out of a non-battlefield zone must keep
        // activation_zone == None (→ defaults to Battlefield at runtime).
        let r = parse(
            "{1}{U}: Return Test Bounce Creature to its owner's hand.",
            "Test Bounce Creature",
            &[],
            &["Creature"],
            &["Bird"],
        );
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Activated);
        assert_eq!(
            ability.activation_zone, None,
            "a self-bounce (battlefield → hand) must not derive an activation zone"
        );
    }

    // -----------------------------------------------------------------------
    // Boast (CR 702.142 — keyword ability)
    // -----------------------------------------------------------------------

    #[test]
    fn boast_mana_cost_parses_as_activated_with_restrictions() {
        // CR 702.142a: Boast with mana cost — e.g. Axgard Braggart
        let r = parse(
            "Boast \u{2014} {1}{W}: Untap Axgard Braggart. Put a +1/+1 counter on it. (Activate only if this creature attacked this turn and only once each turn.)",
            "Axgard Braggart",
            &[],
            &["Creature"],
            &["Dwarf", "Warrior"],
        );
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Activated);
        assert!(
            ability.activation_zone.is_none(),
            "Boast activates from battlefield (default), not hand"
        );
        assert!(
            matches!(
                ability.cost,
                Some(AbilityCost::Composite { .. }) | Some(AbilityCost::Mana { .. })
            ),
            "Boast should have mana cost, got {:?}",
            ability.cost
        );
        assert!(
            ability
                .activation_restrictions
                .contains(&ActivationRestriction::OnlyOnceEachTurn),
            "Boast must have OnlyOnceEachTurn restriction"
        );
        assert!(
            ability.activation_restrictions.iter().any(|r| matches!(
                r,
                ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::SourceAttackedThisTurn)
                }
            )),
            "Boast must have SourceAttackedThisTurn restriction"
        );
    }

    #[test]
    fn boast_text_only_cost_parses_as_activated() {
        // CR 702.142a: Boast with sacrifice cost — Broadside Bombardiers
        let r = parse(
            "Boast \u{2014} Sacrifice another creature or artifact: This creature deals damage equal to 2 plus the sacrificed permanent's mana value to any target. (Activate only if this creature attacked this turn and only once each turn.)",
            "Broadside Bombardiers",
            &[],
            &["Creature"],
            &["Goblin", "Pirate"],
        );
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Activated);
        assert!(
            matches!(ability.cost, Some(AbilityCost::Sacrifice { .. })),
            "Boast cost should be Sacrifice, got {:?}",
            ability.cost
        );
        assert!(
            ability
                .activation_restrictions
                .contains(&ActivationRestriction::OnlyOnceEachTurn),
            "Boast must have OnlyOnceEachTurn restriction"
        );
        assert!(
            ability.activation_restrictions.iter().any(|r| matches!(
                r,
                ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::SourceAttackedThisTurn)
                }
            )),
            "Boast must have SourceAttackedThisTurn restriction"
        );
    }

    #[test]
    fn boast_double_hyphen_variant() {
        // CR 702.142: Test double-hyphen variant
        let r = parse(
            "Boast -- {B}: Target opponent loses 1 life and you gain 1 life. (Activate only if this creature attacked this turn and only once each turn.)",
            "Duskwielder",
            &[],
            &["Creature"],
            &["Elf", "Berserker"],
        );
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
        assert!(r.abilities[0]
            .activation_restrictions
            .contains(&ActivationRestriction::OnlyOnceEachTurn),);
    }

    #[test]
    fn exhaust_mana_cost_parses_as_activated_with_once_per_game_restriction() {
        let r = parse(
            "Exhaust \u{2014} {3}: Draw a card.",
            "Adrenaline Jockey",
            &[],
            &["Creature"],
            &["Human", "Pilot"],
        );
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Activated);
        assert_eq!(ability.ability_tag, Some(AbilityTag::Exhaust));
        assert!(matches!(
            ability.cost,
            Some(AbilityCost::Mana {
                cost: ManaCost::Cost { generic: 3, .. }
            })
        ));
        assert!(
            ability
                .activation_restrictions
                .contains(&ActivationRestriction::OnlyOnce),
            "Exhaust must have OnlyOnce restriction"
        );
    }

    #[test]
    fn self_exile_from_hand_mana_ability_activates_from_hand() {
        let r = parse(
            "Exile this creature from your hand: Add {G}.",
            "Elvish Spirit Guide",
            &[],
            &["Creature"],
            &["Elf", "Spirit"],
        );
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Activated);
        assert_eq!(ability.activation_zone, Some(Zone::Hand));
        assert!(matches!(*ability.effect, Effect::Mana { .. }));
        assert!(matches!(
            ability.cost,
            Some(AbilityCost::Exile {
                filter: Some(TargetFilter::SelfRef),
                zone: Some(Zone::Hand),
                count: 1,
            })
        ));
    }

    // ── Escape keyword parsing ──────────────────────────────────────────────

    #[test]
    fn parse_escape_sentinels_eyes() {
        // CR 702.138: Standard escape format — {W}, exile two
        let r = parse(
            "Enchant creature\nEnchanted creature gets +1/+1 and has vigilance.\nEscape\u{2014}{W}, Exile two other cards from your graveyard.",
            "Sentinel's Eyes",
            &[Keyword::Enchant(TargetFilter::Typed(crate::types::ability::TypedFilter::creature()))],
            &["Enchantment"],
            &["Aura"],
        );
        let escape_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Escape { .. }));
        assert!(escape_kw.is_some(), "Escape keyword should be extracted");
        match escape_kw.unwrap() {
            Keyword::Escape { cost, exile_count } => {
                assert_eq!(*exile_count, 2);
                assert!(matches!(cost, ManaCost::Cost { generic: 0, shards } if shards.len() == 1));
            }
            _ => unreachable!(),
        }
        // No Unimplemented abilities for the escape line
        assert!(
            !r.abilities
                .iter()
                .any(|a| matches!(*a.effect, Effect::Unimplemented { .. })),
            "Escape line should not produce Unimplemented"
        );
    }

    #[test]
    fn parse_escape_high_cost() {
        // CR 702.138: Higher cost — {3}{B}{B}, exile five
        let r = parse(
            "Escape\u{2014}{3}{B}{B}, Exile five other cards from your graveyard.",
            "Test Card",
            &[],
            &["Creature"],
            &[],
        );
        let escape_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Escape { .. }));
        assert!(escape_kw.is_some());
        match escape_kw.unwrap() {
            Keyword::Escape { cost, exile_count } => {
                assert_eq!(*exile_count, 5);
                assert!(matches!(cost, ManaCost::Cost { generic: 3, shards } if shards.len() == 2));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_escape_eight_exile() {
        // CR 702.138: Edge case — exile eight
        let r = parse(
            "Escape\u{2014}{R}{R}, Exile eight other cards from your graveyard.",
            "Test Card",
            &[],
            &["Creature"],
            &[],
        );
        match r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Escape { .. }))
            .unwrap()
        {
            Keyword::Escape { exile_count, .. } => assert_eq!(*exile_count, 8),
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_harmonize_channeled_dragonfire() {
        // Harmonize — keyword with mana cost parsed from Oracle text.
        // MTGJSON uses space-separated format, NOT em-dash.
        let r = parse(
            "Channeled Dragonfire deals 2 damage to any target.\nHarmonize {5}{R}{R} (You may cast this card from your graveyard for its harmonize cost. You may tap a creature you control to reduce that cost by {X}, where X is its power. Then exile this spell.)",
            "Channeled Dragonfire",
            &[],
            &["Instant"],
            &[],
        );
        let harmonize_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Harmonize(_)));
        assert!(harmonize_kw.is_some(), "Harmonize keyword not extracted");
        match harmonize_kw.unwrap() {
            Keyword::Harmonize(cost) => {
                // {5}{R}{R} = 5 generic + 2 red = total 7
                assert_eq!(cost.mana_value(), 7);
            }
            _ => unreachable!(),
        }
    }

    /// CR 110.2a + CR 202.3 + CR 603.12: Ancient Brass Dragon's reflexive "put
    /// any number of target creature cards with total mana value X or less from
    /// graveyards onto the battlefield under your control, where X is the
    /// result" must parse into a `ChangeZone` whose target is a graveyard
    /// creature filter, with an unlimited multi-target spec and a
    /// `TotalManaValue` constraint bound to the die result (issue #1602,
    /// Deliverable 2).
    #[test]
    fn ancient_brass_dragon_reflexive_graveyard_reanimation() {
        use crate::types::ability::{
            AbilityDefinition, Effect, MultiTargetSpec, QuantityExpr, QuantityRef, TargetFilter,
        };
        use crate::types::game_state::TargetSelectionConstraint;
        use crate::types::zones::Zone;

        // Find the AbilityDefinition node whose effect is the reanimation
        // `ChangeZone`, walking the RollDie result branches and sub/else chains.
        fn find_change_zone_def(def: &AbilityDefinition) -> Option<&AbilityDefinition> {
            if matches!(def.effect.as_ref(), Effect::ChangeZone { .. }) {
                return Some(def);
            }
            if let Effect::RollDie { results, .. } = def.effect.as_ref() {
                for branch in results {
                    if let Some(found) = find_change_zone_def(&branch.effect) {
                        return Some(found);
                    }
                }
            }
            if let Some(found) = def.sub_ability.as_deref().and_then(find_change_zone_def) {
                return Some(found);
            }
            def.else_ability.as_deref().and_then(find_change_zone_def)
        }

        let r = parse(
            "Flying\nWhenever this creature deals combat damage to a player, roll a \
             d20. When you do, put any number of target creature cards with total \
             mana value X or less from graveyards onto the battlefield under your \
             control, where X is the result.",
            "Ancient Brass Dragon",
            &[],
            &["Creature"],
            &["Elder", "Dragon"],
        );

        let trigger = r
            .triggers
            .iter()
            .find(|t| t.execute.is_some())
            .expect("Ancient Brass Dragon should produce a combat-damage trigger");
        let execute = trigger.execute.as_deref().unwrap();
        let cz_def =
            find_change_zone_def(execute).expect("reflexive ChangeZone reanimation must parse");

        let Effect::ChangeZone {
            destination,
            target,
            enters_under,
            up_to,
            ..
        } = cz_def.effect.as_ref()
        else {
            panic!("expected ChangeZone, got {:?}", cz_def.effect);
        };

        // CR 110.2a: onto the battlefield under your control.
        assert_eq!(*destination, Zone::Battlefield);
        assert_eq!(
            *enters_under,
            Some(crate::types::ability::ControllerRef::You)
        );
        // The MV phrase strip must not have eaten the zone suffix: the filter
        // still resolves the graveyard origin.
        assert_eq!(
            target.extract_in_zone(),
            Some(Zone::Graveyard),
            "target must carry InZone(Graveyard) after the MV-phrase strip; got {target:?}"
        );
        assert!(
            matches!(target, TargetFilter::Typed(_)),
            "target should be a Typed creature filter, got {target:?}"
        );
        // "any number of target" → unlimited multi-target.
        assert_eq!(cz_def.multi_target, Some(MultiTargetSpec::unlimited(0)));
        // "up to / any number of" makes the selection optional.
        assert!(*up_to);
        // CR 202.3: TotalManaValue cap bound to the die result.
        assert_eq!(
            cz_def.target_constraints,
            vec![TargetSelectionConstraint::TotalManaValue {
                comparator: crate::types::ability::Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
            }],
            "target_constraints must carry the where-X-bound MV cap"
        );
    }

    /// CR 706.2 + CR 706.4 + CR 603.12: Ancient Bronze Dragon's reflexive
    /// "put X +1/+1 counters on each of up to two target creatures, where X is
    /// the result" must bind X to the die roll via `EventContextAmount`, NOT to
    /// a `Variable("the result")` that resolves to 0 (issue #1602, Deliverable 1).
    #[test]
    fn ancient_bronze_dragon_reflexive_counts_die_result() {
        use crate::types::ability::{AbilityDefinition, Effect, QuantityExpr, QuantityRef};

        // Walk an ability-definition chain (effect + sub_ability + else_ability)
        // collecting every `PutCounter.count` it contains.
        fn collect_put_counter_counts<'a>(
            def: &'a AbilityDefinition,
            out: &mut Vec<&'a QuantityExpr>,
        ) {
            if let Effect::PutCounter { count, .. } = def.effect.as_ref() {
                out.push(count);
            }
            if let Effect::RollDie { results, .. } = def.effect.as_ref() {
                for branch in results {
                    collect_put_counter_counts(&branch.effect, out);
                }
            }
            if let Some(sub) = def.sub_ability.as_deref() {
                collect_put_counter_counts(sub, out);
            }
            if let Some(else_def) = def.else_ability.as_deref() {
                collect_put_counter_counts(else_def, out);
            }
        }

        let r = parse(
            "Flying\nWhenever this creature deals combat damage to a player, roll a \
             d20. When you do, put X +1/+1 counters on each of up to two target \
             creatures, where X is the result.",
            "Ancient Bronze Dragon",
            &[],
            &["Creature"],
            &["Dragon"],
        );

        let trigger = r
            .triggers
            .iter()
            .find(|t| t.execute.is_some())
            .expect("Ancient Bronze Dragon should produce a combat-damage trigger");
        let execute = trigger.execute.as_deref().unwrap();
        let mut counts = Vec::new();
        collect_put_counter_counts(execute, &mut counts);

        assert!(
            !counts.is_empty(),
            "expected a PutCounter in the reflexive sub-ability chain"
        );
        for count in counts {
            assert_eq!(
                count,
                &QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                "PutCounter.count must bind X to the die result via \
                 EventContextAmount, not Variable(\"the result\") (which would \
                 resolve to 0)"
            );
        }
    }

    #[test]
    fn parse_harmonize_wild_ride() {
        // Harmonize with lower cost
        let r = parse(
            "Target creature gets +3/+0 and gains haste until end of turn.\nHarmonize {4}{R} (You may cast this card from your graveyard for its harmonize cost. You may tap a creature you control to reduce that cost by {X}, where X is its power. Then exile this spell.)",
            "Wild Ride",
            &[],
            &["Instant"],
            &[],
        );
        let harmonize_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Harmonize(_)));
        assert!(harmonize_kw.is_some(), "Harmonize keyword not extracted");
        match harmonize_kw.unwrap() {
            Keyword::Harmonize(cost) => {
                assert_eq!(cost.mana_value(), 5);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_harmonize_no_reminder_text() {
        // Some cards have no reminder text (e.g., Ureni's Counsel)
        let r = parse(
            "Draw three cards.\nHarmonize {8}{R}{R}",
            "Ureni's Counsel",
            &[],
            &["Sorcery"],
            &[],
        );
        let harmonize_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Harmonize(_)));
        assert!(harmonize_kw.is_some(), "Harmonize keyword not extracted");
        match harmonize_kw.unwrap() {
            Keyword::Harmonize(cost) => {
                assert_eq!(cost.mana_value(), 10);
            }
            _ => unreachable!(),
        }
    }

    // ── Cumulative upkeep (CR 702.24) ──

    #[test]
    fn parse_cumulative_upkeep_mana_cost() {
        // CR 702.24a: Mana-only cumulative upkeep — space-separated format.
        let r = parse(
            "Cumulative upkeep {1} (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)",
            "Mystic Remora",
            &[],
            &["Enchantment"],
            &[],
        );
        let cu_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::CumulativeUpkeep(_)));
        assert!(cu_kw.is_some(), "CumulativeUpkeep keyword not extracted");
        match cu_kw.unwrap() {
            Keyword::CumulativeUpkeep(AbilityCost::Mana {
                cost: ManaCost::Cost { generic, shards },
            }) => {
                assert_eq!(*generic, 1);
                assert!(shards.is_empty());
            }
            other => panic!("expected Mana({{1}}), got {other:?}"),
        }
    }

    #[test]
    fn parse_cumulative_upkeep_life_payment() {
        // CR 702.24a: Non-mana cost with em-dash separator.
        let r = parse(
            "Cumulative upkeep\u{2014}Pay 2 life. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)",
            "Inner Sanctum",
            &[],
            &["Enchantment"],
            &[],
        );
        let cu_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::CumulativeUpkeep(_)));
        assert!(cu_kw.is_some(), "CumulativeUpkeep keyword not extracted");
        match cu_kw.unwrap() {
            Keyword::CumulativeUpkeep(AbilityCost::PayLife { amount }) => {
                assert_eq!(*amount, QuantityExpr::Fixed { value: 2 });
            }
            other => panic!("expected PayLife(2), got {other:?}"),
        }
    }

    #[test]
    fn parse_cumulative_upkeep_sacrifice() {
        // CR 702.24a: Sacrifice cost.
        let r = parse(
            "Cumulative upkeep\u{2014}Sacrifice a land. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)",
            "Polar Kraken",
            &[],
            &["Creature"],
            &[],
        );
        let cu_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::CumulativeUpkeep(_)));
        assert!(cu_kw.is_some(), "CumulativeUpkeep keyword not extracted");
        match cu_kw.unwrap() {
            Keyword::CumulativeUpkeep(AbilityCost::Sacrifice { target, count }) => {
                assert_eq!(*count, 1);
                // Target should be a typed filter (Land subtype filter).
                assert!(
                    matches!(target, TargetFilter::Typed(_)),
                    "expected Typed Land filter, got {target:?}"
                );
            }
            other => panic!("expected Sacrifice(Land, 1), got {other:?}"),
        }
    }

    #[test]
    fn parse_cumulative_upkeep_or_mana() {
        // CR 702.24a: "{G} or {W}" — disjunctive (alternative) mana cost.
        let r = parse(
            "Cumulative upkeep {G} or {W} (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)",
            "Elephant Grass",
            &[],
            &["Enchantment"],
            &[],
        );
        let cu_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::CumulativeUpkeep(_)));
        assert!(cu_kw.is_some(), "CumulativeUpkeep keyword not extracted");
        match cu_kw.unwrap() {
            Keyword::CumulativeUpkeep(AbilityCost::OneOf { costs }) => {
                assert_eq!(costs.len(), 2);
                for c in costs {
                    assert!(
                        matches!(c, AbilityCost::Mana { .. }),
                        "expected each branch to be Mana, got {c:?}"
                    );
                }
            }
            other => panic!("expected OneOf with 2 Mana costs, got {other:?}"),
        }
    }

    #[test]
    fn parse_two_cumulative_upkeep_instances_both_extracted() {
        // CR 702.24b: A permanent can have multiple cumulative upkeep
        // abilities. Each must surface as its own Keyword entry, AND each
        // must carry its own typed cost so the synthesis pipeline produces
        // independent triggers (not two copies of one cost).
        let r = parse(
            "Cumulative upkeep {1}\nCumulative upkeep\u{2014}Pay 1 life.",
            "Test Two-Instance Permanent",
            &[],
            &["Enchantment"],
            &[],
        );
        let cu_kws: Vec<_> = r
            .extracted_keywords
            .iter()
            .filter(|k| matches!(k, Keyword::CumulativeUpkeep(_)))
            .collect();
        assert_eq!(
            cu_kws.len(),
            2,
            "expected two CumulativeUpkeep keywords, got {cu_kws:?}"
        );

        // Order-independent check: one must be Mana{generic:1}, the other
        // PayLife{Fixed:1}. A regression to zero-cost sentinels would fail
        // both predicates.
        let has_mana_one = cu_kws.iter().any(|k| {
            matches!(
                k,
                Keyword::CumulativeUpkeep(AbilityCost::Mana {
                    cost: ManaCost::Cost { generic: 1, shards },
                }) if shards.is_empty()
            )
        });
        let has_pay_life_one = cu_kws.iter().any(|k| {
            matches!(
                k,
                Keyword::CumulativeUpkeep(AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                })
            )
        });
        assert!(
            has_mana_one,
            "expected one CumulativeUpkeep(Mana({{1}})), got {cu_kws:?}"
        );
        assert!(
            has_pay_life_one,
            "expected one CumulativeUpkeep(PayLife(1)), got {cu_kws:?}"
        );
    }

    #[test]
    fn earthbend_chain_defaults_target() {
        use crate::parser::oracle_effect::parse_effect_chain;

        // Single chunk: "Earthbend 3" — passes through imperative pipeline
        let simple = parse_effect_chain("Earthbend 3", crate::types::ability::AbilityKind::Spell);
        match &*simple.effect {
            Effect::Animate { target, .. } => {
                assert_eq!(
                    simple.duration,
                    Some(crate::types::ability::Duration::Permanent)
                );
                assert!(
                    matches!(target, TargetFilter::Typed(tf) if tf.type_filters.contains(&crate::types::ability::TypeFilter::Land)),
                    "simple earthbend should target land, got {target:?}"
                );
            }
            other => panic!("Expected Animate for simple earthbend, got {other:?}"),
        }

        // Full stripped text from Cracked Earth Technique
        let full = parse_effect_chain(
            "Earthbend 3, then earthbend 3. You gain 3 life.",
            crate::types::ability::AbilityKind::Spell,
        );
        eprintln!("Full chain first effect: {:?}", full.effect);
        match &*full.effect {
            Effect::Animate { target, .. } => {
                assert_eq!(
                    full.duration,
                    Some(crate::types::ability::Duration::Permanent)
                );
                assert!(
                    matches!(target, TargetFilter::Typed(tf) if tf.type_filters.contains(&crate::types::ability::TypeFilter::Land)),
                    "chain earthbend should target land, got {target:?}"
                );
            }
            other => panic!("Expected Animate for chain earthbend, got {other:?}"),
        }
    }

    /// CR 122.1: Toph's "earthbend X, where X is the number of experience
    /// counters you have" must thread the dynamic count through to PutCounter,
    /// not collapse to Fixed { value: 0 }. Walks the parsed chain:
    /// Animate → PutCounter (count = PlayerCounter Experience Controller) →
    /// CreateDelayedTrigger.
    #[test]
    fn earthbend_x_where_x_is_experience_counters() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{CountScope, QuantityExpr, QuantityRef};
        use crate::types::player::PlayerCounterKind;

        let def = parse_effect_chain(
            "Earthbend X, where X is the number of experience counters you have.",
            crate::types::ability::AbilityKind::Spell,
        );
        assert!(
            matches!(&*def.effect, Effect::Animate { .. }),
            "outer effect should be Animate, got {:?}",
            def.effect
        );

        let put_counters = def
            .sub_ability
            .as_deref()
            .expect("Animate should have PutCounter sub_ability");
        match &*put_counters.effect {
            Effect::PutCounter {
                counter_type,
                count,
                ..
            } => {
                assert_eq!(
                    counter_type,
                    &crate::types::counter::CounterType::Plus1Plus1
                );
                assert_eq!(
                    *count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::PlayerCounter {
                            kind: PlayerCounterKind::Experience,
                            scope: CountScope::Controller,
                        },
                    },
                    "Toph's PutCounter count should be a typed PlayerCounter ref, not Fixed 0"
                );
            }
            other => panic!("Expected PutCounter, got {other:?}"),
        }

        let delayed = put_counters
            .sub_ability
            .as_deref()
            .expect("PutCounter should chain into the delayed return trigger");
        assert!(
            matches!(&*delayed.effect, Effect::CreateDelayedTrigger { .. }),
            "expected CreateDelayedTrigger, got {:?}",
            delayed.effect,
        );
    }

    #[test]
    fn search_put_onto_battlefield_tapped() {
        use crate::parser::oracle_effect::parse_effect_chain;

        // Rampant Growth pattern: "Search...put that card onto the battlefield tapped, then shuffle."
        let def = parse_effect_chain(
            "Search your library for a basic land card, put that card onto the battlefield tapped, then shuffle.",
            crate::types::ability::AbilityKind::Spell,
        );
        assert!(matches!(&*def.effect, Effect::SearchLibrary { .. }));
        let change_zone = def
            .sub_ability
            .as_ref()
            .expect("should have ChangeZone sub_ability");
        match &*change_zone.effect {
            Effect::ChangeZone {
                origin,
                destination,
                enter_tapped,
                ..
            } => {
                assert_eq!(*origin, Some(crate::types::zones::Zone::Library));
                assert_eq!(*destination, crate::types::zones::Zone::Battlefield);
                assert!(enter_tapped, "searched land should enter tapped");
            }
            other => panic!("Expected ChangeZone, got {other:?}"),
        }
        // "then shuffle" must produce a Shuffle effect in the sub_ability chain
        let shuffle = change_zone
            .sub_ability
            .as_ref()
            .expect("should have Shuffle sub_ability");
        assert!(
            matches!(&*shuffle.effect, Effect::Shuffle { .. }),
            "Expected Shuffle after ChangeZone, got {:?}",
            shuffle.effect,
        );

        // Earthbender pattern: search follows a period + "Then"
        let def2 = parse_effect_chain(
            "Earthbend 2. Then search your library for a basic land card, put it onto the battlefield tapped, then shuffle.",
            crate::types::ability::AbilityKind::Spell,
        );
        // First effect is Animate (earthbend); the earthbend clause builds a deeper chain
        // (PutCounter → CreateDelayedTrigger → RegisterBending) before the "Then" search.
        // Walk the chain to find SearchLibrary.
        let mut cursor = def2.sub_ability.as_deref();
        while let Some(node) = cursor {
            if matches!(&*node.effect, Effect::SearchLibrary { .. }) {
                break;
            }
            cursor = node.sub_ability.as_deref();
        }
        let search = cursor.expect("should find SearchLibrary in earthbend chain");
        assert!(matches!(&*search.effect, Effect::SearchLibrary { .. }));
        let cz = search
            .sub_ability
            .as_ref()
            .expect("should chain to ChangeZone");
        match &*cz.effect {
            Effect::ChangeZone {
                destination,
                enter_tapped,
                ..
            } => {
                assert_eq!(*destination, crate::types::zones::Zone::Battlefield);
                assert!(
                    enter_tapped,
                    "searched land after 'Then' should enter tapped"
                );
            }
            other => panic!("Expected ChangeZone after Then-search, got {other:?}"),
        }
        let shuffle2 = cz
            .sub_ability
            .as_ref()
            .expect("should have Shuffle after earthbender ChangeZone");
        assert!(
            matches!(&*shuffle2.effect, Effect::Shuffle { .. }),
            "Expected Shuffle after earthbender ChangeZone, got {:?}",
            shuffle2.effect,
        );

        // Negative case: search to hand (no "battlefield tapped")
        let tutor = parse_effect_chain(
            "Search your library for a card, put that card into your hand, then shuffle.",
            crate::types::ability::AbilityKind::Spell,
        );
        let cz_hand = tutor.sub_ability.as_ref().expect("should have ChangeZone");
        match &*cz_hand.effect {
            Effect::ChangeZone {
                destination,
                enter_tapped,
                ..
            } => {
                assert_eq!(*destination, crate::types::zones::Zone::Hand);
                assert!(!enter_tapped, "search-to-hand should not be tapped");
            }
            other => panic!("Expected ChangeZone to Hand, got {other:?}"),
        }
        let shuffle3 = cz_hand
            .sub_ability
            .as_ref()
            .expect("should have Shuffle after search-to-hand");
        assert!(
            matches!(&*shuffle3.effect, Effect::Shuffle { .. }),
            "Expected Shuffle after search-to-hand ChangeZone, got {:?}",
            shuffle3.effect,
        );
    }

    #[test]
    fn strip_counter_conditional_prefix_quest() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{
            AbilityCondition, AbilityKind, Comparator, QuantityExpr, QuantityRef,
        };

        let def = parse_effect_chain(
            "if it has four or more quest counters on it, put a +1/+1 counter on target creature you control",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &def.condition,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(counter_type) } },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 4 },
                }) if *counter_type == crate::types::counter::CounterType::Generic("quest".to_string())
            ),
            "Expected QuantityCheck(quest >= 4), got {:?}",
            def.condition,
        );
        assert!(
            matches!(&*def.effect, Effect::PutCounter { counter_type, count: QuantityExpr::Fixed { value: 1 }, .. } if *counter_type == crate::types::counter::CounterType::Plus1Plus1),
            "Expected PutCounter P1P1, got {:?}",
            def.effect,
        );
    }

    #[test]
    fn strip_counter_conditional_suffix_hunger() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{
            AbilityCondition, AbilityKind, Comparator, QuantityExpr, QuantityRef,
        };

        let def = parse_effect_chain(
            "destroy this enchantment if it has five or more hunger counters on it",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &def.condition,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(counter_type) } },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 5 },
                }) if *counter_type == crate::types::counter::CounterType::Generic("hunger".to_string())
            ),
            "Expected QuantityCheck(hunger >= 5), got {:?}",
            def.condition,
        );
        assert!(
            matches!(&*def.effect, Effect::Destroy { .. }),
            "Expected Destroy effect, got {:?}",
            def.effect,
        );
    }

    #[test]
    fn strip_counter_conditional_p1p1_normalization() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{
            AbilityCondition, AbilityKind, Comparator, QuantityExpr, QuantityRef,
        };

        let def = parse_effect_chain(
            "if it has three or more +1/+1 counters on it, sacrifice this Aura",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &def.condition,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(counter_type) } },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                }) if *counter_type == crate::types::counter::CounterType::Plus1Plus1
            ),
            "Expected QuantityCheck(P1P1 >= 3), got {:?}",
            def.condition,
        );
    }

    #[test]
    fn strip_counter_conditional_one_or_more_oil() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{
            AbilityCondition, AbilityKind, Comparator, QuantityExpr, QuantityRef,
        };

        let def = parse_effect_chain(
            "if it has one or more oil counters on it, put an oil counter on it",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &def.condition,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(counter_type) } },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                }) if *counter_type == crate::types::counter::CounterType::Generic("oil".to_string())
            ),
            "Expected QuantityCheck(oil >= 1), got {:?}",
            def.condition,
        );
    }

    #[test]
    fn strip_counter_conditional_no_ice_counters() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{
            AbilityCondition, AbilityKind, Comparator, QuantityExpr, QuantityRef,
        };

        let def = parse_effect_chain(
            "if it has no ice counters on it, transform it",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &def.condition,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(counter_type) } },
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Fixed { value: 0 },
                }) if *counter_type == crate::types::counter::CounterType::Generic("ice".to_string())
            ),
            "Expected QuantityCheck(ice == 0), got {:?}",
            def.condition,
        );
    }

    #[test]
    fn earthbender_ascension_landfall_chain() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{
            AbilityCondition, AbilityKind, Comparator, QuantityExpr, QuantityRef, TargetFilter,
        };

        let def = parse_effect_chain(
            "put a quest counter on this enchantment. When you do, if it has four or more quest counters on it, put a +1/+1 counter on target creature you control. It gains trample until end of turn.",
            AbilityKind::Spell,
        );

        // Node 1: PutCounter(quest, 1, SelfRef), no condition
        assert!(def.condition.is_none(), "Node 1 should have no condition");
        assert!(
            matches!(&*def.effect, Effect::PutCounter { counter_type, count: QuantityExpr::Fixed { value: 1 }, target: TargetFilter::SelfRef } if *counter_type == crate::types::counter::CounterType::Generic("quest".to_string())),
            "Node 1 should be PutCounter(quest, SelfRef), got {:?}",
            def.effect,
        );

        // Node 2: PutCounter(P1P1, 1, Typed(creature+You)), condition = QuantityCheck(quest >= 4)
        let node2 = def
            .sub_ability
            .as_ref()
            .expect("should have node 2 (P1P1 counter)");
        assert!(
            matches!(
                &node2.condition,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(counter_type) } },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 4 },
                }) if *counter_type == crate::types::counter::CounterType::Generic("quest".to_string())
            ),
            "Node 2 condition should be QuantityCheck(quest >= 4), got {:?}",
            node2.condition,
        );
        match &*node2.effect {
            Effect::PutCounter {
                counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(tf),
            } => {
                assert_eq!(
                    counter_type,
                    &crate::types::counter::CounterType::Plus1Plus1
                );
                assert!(
                    tf.controller == Some(crate::types::ability::ControllerRef::You),
                    "P1P1 target should be creature you control, got {:?}",
                    tf,
                );
            }
            other => panic!("Node 2 should be PutCounter(P1P1, Typed), got {other:?}"),
        }

        // Node 3: GenericEffect(trample, ParentTarget), duration = UntilEndOfTurn
        let node3 = node2
            .sub_ability
            .as_ref()
            .expect("should have node 3 (trample grant)");
        match &*node3.effect {
            Effect::GenericEffect {
                target, duration, ..
            } => {
                assert!(
                    matches!(target, Some(TargetFilter::ParentTarget)),
                    "Node 3 target should be ParentTarget, got {target:?}",
                );
                assert!(
                    matches!(
                        duration,
                        Some(crate::types::ability::Duration::UntilEndOfTurn)
                    ),
                    "Node 3 duration should be UntilEndOfTurn, got {duration:?}",
                );
            }
            other => panic!("Node 3 should be GenericEffect(trample), got {other:?}"),
        }
    }

    #[test]
    fn semicolon_keyword_splitting_defender_reach() {
        let r = parse_with_keyword_names(
            "Defender; reach",
            "Wall of Nets",
            &["defender", "reach"],
            &["Creature"],
            &["Wall"],
        );
        assert!(
            r.extracted_keywords.is_empty(),
            "MTGJSON-covered keywords should not be re-extracted"
        );
        // The key assertion: both keywords are recognized (no unimplemented abilities)
        assert!(
            r.abilities.is_empty(),
            "No abilities should be produced from a keyword-only line"
        );
    }

    #[test]
    fn semicolon_keyword_splitting_first_strike_banding() {
        let r = parse_with_keyword_names(
            "First strike; banding",
            "Test Card",
            &["first strike", "banding"],
            &["Creature"],
            &[],
        );
        assert!(
            r.abilities.is_empty(),
            "No abilities from keyword-only semicolon line"
        );
    }

    #[test]
    fn semicolon_keyword_splitting_vigilance_menace() {
        let r = parse_with_keyword_names(
            "Vigilance; menace",
            "Test Card",
            &["vigilance", "menace"],
            &["Creature"],
            &[],
        );
        assert!(
            r.abilities.is_empty(),
            "No abilities from keyword-only semicolon line"
        );
    }

    #[test]
    fn semicolon_does_not_split_activated_ability() {
        // A line with a colon should NOT be split on semicolons
        let r = parse_with_keyword_names(
            "{T}: Draw a card; you lose 1 life.",
            "Test Card",
            &[],
            &["Creature"],
            &[],
        );
        // Should be parsed as a single activated ability
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
    }

    #[test]
    fn semicolon_no_split_single_keyword() {
        // A single keyword without semicolons should continue to work
        let r =
            parse_with_keyword_names("Flying", "Test Bird", &["flying"], &["Creature"], &["Bird"]);
        assert!(
            r.abilities.is_empty(),
            "No abilities from single keyword line"
        );
    }

    // -- Strive parsing tests --------------------------------------------------

    #[test]
    fn strive_mana_symbol_parse() {
        use crate::parser::oracle_util::parse_mana_symbols;
        let result = parse_mana_symbols("{2}{U}");
        assert!(result.is_some());
        let (cost, rest) = result.unwrap();
        assert_eq!(cost.mana_value(), 3);
        assert_eq!(rest, "");
    }

    #[test]
    fn strive_ability_word_strip() {
        use crate::parser::oracle_modal::strip_ability_word;
        let input = "Strive \u{2014} This spell costs {2}{U} more to cast for each target beyond the first.";
        let stripped = strip_ability_word(input);
        assert!(
            stripped.is_some(),
            "strip_ability_word should match Strive line"
        );
        let text = stripped.unwrap();
        assert!(
            text.starts_with("This spell costs"),
            "expected 'This spell costs...' got: {}",
            text
        );
    }

    #[test]
    fn strive_cost_parsed_from_oracle_text() {
        // CR 207.2c + CR 601.2f: Strive per-target surcharge.
        let text = "Strive \u{2014} This spell costs {2}{U} more to cast for each target beyond the first.";
        let r = parse(text, "Test Card", &[], &["Instant"], &[]);
        assert!(r.strive_cost.is_some());
        assert_eq!(r.strive_cost.unwrap().mana_value(), 3);
    }

    #[test]
    fn strive_cost_parsed_different_cost() {
        let r = parse(
            "Strive — This spell costs {1}{B} more to cast for each target beyond the first.\nDestroy target creature.",
            "Cruel Feeding",
            &[],
            &["Instant"],
            &[],
        );
        assert!(r.strive_cost.is_some(), "strive_cost should be parsed");
        let cost = r.strive_cost.unwrap();
        assert_eq!(cost.mana_value(), 2);
    }

    #[test]
    fn no_strive_cost_on_normal_spell() {
        let r = parse(
            "Target creature gets +3/+3 until end of turn.",
            "Giant Growth",
            &[],
            &["Instant"],
            &[],
        );
        assert!(r.strive_cost.is_none());
    }

    #[test]
    fn strive_line_consumed_not_reparsed() {
        let r = parse(
            "Strive \u{2014} This spell costs {1}{R} more to cast for each target beyond the first.\nDraw a card.",
            "Test Strive Card",
            &[],
            &["Instant"],
            &[],
        );
        assert!(r.strive_cost.is_some());
        assert!(
            r.abilities.len() <= 2,
            "strive_cost was set; abilities={}",
            r.abilities.len()
        );
        let has_strive_ability = r.abilities.iter().any(|a| {
            a.description
                .as_ref()
                .is_some_and(|d| d.to_lowercase().contains("strive"))
        });
        assert!(
            !has_strive_ability,
            "strive line should be consumed, not produce an ability"
        );
    }

    /// CR 207.2c (Strive) + CR 115.1d ("any number of") + CR 707.2 (CopyTokenOf) +
    /// CR 702.10 (Haste) + CR 603.7 (delayed trigger): Twinflame's full parse —
    /// multi-target {min:0,max:None}, per-target CopyTokenOf{ParentTarget,
    /// extra_keywords:[Haste]}, delayed exile of "those tokens" with
    /// uses_tracked_set=true.
    #[test]
    fn twinflame_full_parse() {
        use crate::types::ability::{Effect, MultiTargetSpec, TargetFilter};
        use crate::types::keywords::Keyword;

        let r = parse(
            "Strive \u{2014} This spell costs {2}{R} more to cast for each target beyond the first.\nChoose any number of target creatures you control. For each of them, create a token that's a copy of that creature, except it has haste. Exile those tokens at the beginning of the next end step.",
            "Twinflame",
            &[],
            &["Sorcery"],
            &[],
        );

        // Strive cost extracted.
        let strive = r.strive_cost.as_ref().expect("strive_cost set");
        assert_eq!(strive.mana_value(), 3);

        // One spell ability with multi_target.
        assert_eq!(r.abilities.len(), 1, "expected single spell ability");
        let ab = &r.abilities[0];
        assert_eq!(
            ab.multi_target,
            Some(MultiTargetSpec::unlimited(0)),
            "expected any-number multi_target"
        );

        // Walk the chain: TargetOnly(creature) → CopyTokenOf → CreateDelayedTrigger.
        let copy = ab.sub_ability.as_ref().expect("CopyTokenOf sub-ability");
        match &*copy.effect {
            Effect::CopyTokenOf {
                target,
                extra_keywords,
                ..
            } => {
                assert!(matches!(target, TargetFilter::ParentTarget));
                assert_eq!(extra_keywords, &vec![Keyword::Haste]);
            }
            other => panic!("expected CopyTokenOf, got {other:?}"),
        }

        let delayed = copy
            .sub_ability
            .as_ref()
            .expect("CreateDelayedTrigger sub-ability");
        match &*delayed.effect {
            Effect::CreateDelayedTrigger {
                uses_tracked_set, ..
            } => assert!(
                *uses_tracked_set,
                "'those tokens' must mark uses_tracked_set=true"
            ),
            other => panic!("expected CreateDelayedTrigger, got {other:?}"),
        }
    }

    // ── Mana spend restriction extensions ─────────────────────────────

    #[test]
    fn mana_spend_restriction_activate_only() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::ManaSpendRestriction;
        let result = parse_mana_spend_restriction("spend this mana only to activate abilities");
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::ActivateOnly)
        );
    }

    #[test]
    fn mana_spend_restriction_noncreature_spells() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::ManaSpendRestriction;
        let result =
            parse_mana_spend_restriction("spend this mana only to cast noncreature spells");
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellType("Noncreature".to_string()))
        );
    }

    #[test]
    fn mana_spend_restriction_spell_only() {
        let result = crate::parser::oracle_effect::mana::parse_mana_spend_restriction(
            "spend this mana only to cast spells",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellOnly)
        );
    }

    #[test]
    fn mana_spend_restriction_x_cost_only() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::ManaSpendRestriction;
        let result = parse_mana_spend_restriction("spend this mana only on costs that include {x}");
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::XCostOnly)
        );
    }

    #[test]
    fn mana_spend_restriction_instant_or_sorcery() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::ManaSpendRestriction;
        let result =
            parse_mana_spend_restriction("spend this mana only to cast instant or sorcery spells");
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellType(
                "Instant or Sorcery".to_string()
            ))
        );
    }

    #[test]
    fn mana_spend_restriction_colorless_eldrazi_spell_or_activation() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::ManaSpendRestriction;
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast colorless eldrazi spells or activate abilities of colorless eldrazi",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellTypeOrAbilityActivation(
                "Colorless Eldrazi".to_string()
            ))
        );
    }

    #[test]
    fn mana_spend_restriction_singular_source_ability_activation() {
        let result = crate::parser::oracle_effect::mana::parse_mana_spend_restriction(
            "spend this mana only to cast an artifact spell or activate an ability of an artifact source",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellTypeOrAbilityActivation(
                "Artifact".to_string()
            ))
        );
    }

    #[test]
    fn mana_spend_restriction_or_to_activate_source_ability() {
        let result = crate::parser::oracle_effect::mana::parse_mana_spend_restriction(
            "spend this mana only to cast an assassin spell or to activate an ability of an assassin source",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellTypeOrAbilityActivation(
                "Assassin".to_string()
            ))
        );
    }

    #[test]
    fn mana_spend_restriction_bare_activation_or_is_unsupported() {
        let result = crate::parser::oracle_effect::mana::parse_mana_spend_restriction(
            "spend this mana only to cast an artifact spell or activate an ability",
        );
        assert_eq!(result, None);
    }

    #[test]
    fn mana_spend_restriction_ally_spell_or_source_activation() {
        let result = crate::parser::oracle_effect::mana::parse_mana_spend_restriction(
            "spend this mana only to cast an ally spell or activate an ability of an ally source",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellTypeOrAbilityActivation(
                "Ally".to_string()
            ))
        );
    }

    #[test]
    fn mana_spend_restriction_flashback_spells() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        let result =
            parse_mana_spend_restriction("spend this mana only to cast spells with flashback");
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellWithKeywordKind(
                KeywordKind::Flashback,
            ))
        );
    }

    #[test]
    fn mana_spend_restriction_flashback_spells_from_graveyard() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast spells with flashback from a graveyard",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellWithKeywordKindFromZone {
                kind: KeywordKind::Flashback,
                zone: Zone::Graveyard,
            })
        );
    }

    #[test]
    fn mana_spend_restriction_mana_value_ge() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::{Comparator, ManaSpendRestriction};
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast spells with mana value 5 or greater",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellWithManaValue {
                comparator: Comparator::GE,
                value: 5,
            })
        );
    }

    #[test]
    fn mana_spend_restriction_mana_value_le() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::{Comparator, ManaSpendRestriction};
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast spells with mana value 3 or less",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellWithManaValue {
                comparator: Comparator::LE,
                value: 3,
            })
        );
    }

    #[test]
    fn mana_spend_restriction_mana_value_singular_spell_ge() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::{Comparator, ManaSpendRestriction};
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast a spell with mana value 4 or greater",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellWithManaValue {
                comparator: Comparator::GE,
                value: 4,
            })
        );
    }

    #[test]
    fn mana_spend_restriction_mana_value_rejects_trailing_text() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast spells with mana value 5 or greater nonsense",
        );
        assert_eq!(result, None);
    }

    #[test]
    fn mana_spend_restriction_color_count_exactly() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::{Comparator, ManaSpendRestriction};
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast spells with exactly three colors",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellWithColorCount {
                comparator: Comparator::EQ,
                count: 3,
            })
        );
    }

    #[test]
    fn mana_spend_restriction_color_count_exactly_one_color() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::{Comparator, ManaSpendRestriction};
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast a spell with exactly one color",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellWithColorCount {
                comparator: Comparator::EQ,
                count: 1,
            })
        );
    }

    #[test]
    fn mana_spend_restriction_color_count_or_more() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::{Comparator, ManaSpendRestriction};
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast spells with two or more colors",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellWithColorCount {
                comparator: Comparator::GE,
                count: 2,
            })
        );
    }

    #[test]
    fn mana_spend_restriction_color_count_or_fewer() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::{Comparator, ManaSpendRestriction};
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast spells with two or fewer colors",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellWithColorCount {
                comparator: Comparator::LE,
                count: 2,
            })
        );
    }

    #[test]
    fn mana_spend_restriction_from_graveyard() {
        assert_eq!(
            crate::parser::oracle_effect::mana::parse_mana_spend_restriction(
                "spend this mana only to cast a spell from your graveyard"
            )
            .map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellFromZone(Zone::Graveyard))
        );
        assert_eq!(
            crate::parser::oracle_effect::mana::parse_mana_spend_restriction(
                "spend this mana only to cast spells from exile"
            )
            .map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellFromZone(Zone::Exile))
        );
    }

    #[test]
    fn mana_spend_restriction_on_costs_that_contain_x() {
        // "contain" is an alias for the existing "include" X-cost wording.
        assert_eq!(
            crate::parser::oracle_effect::mana::parse_mana_spend_restriction(
                "spend this mana only on costs that contain {x}"
            )
            .map(|(r, _)| r),
            Some(ManaSpendRestriction::XCostOnly)
        );
    }

    #[test]
    fn mana_spend_restriction_chosen_type_cant_be_countered() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::mana::ManaSpellGrant;
        // Cavern of Souls pattern
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast a creature spell of the chosen type, and that spell can't be countered",
        );
        let (restriction, grants) = result.expect("should parse");
        assert_eq!(restriction, ManaSpendRestriction::ChosenCreatureType);
        assert_eq!(grants, vec![ManaSpellGrant::CantBeCountered]);
    }

    #[test]
    fn mana_spend_restriction_legendary_cant_be_countered() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::mana::ManaSpellGrant;
        // Delighted Halfling pattern
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast a legendary spell, and that spell can't be countered",
        );
        let (restriction, grants) = result.expect("should parse");
        assert_eq!(
            restriction,
            ManaSpendRestriction::SpellType("Legendary".to_string())
        );
        assert_eq!(grants, vec![ManaSpellGrant::CantBeCountered]);
    }

    #[test]
    fn top_level_static_flashback_grant_stays_on_graveyard_cards() {
        let result = parse(
            "Each instant and sorcery card in your graveyard has flashback.\nThe flashback cost is equal to that card's mana cost.",
            "Lier, Disciple of the Drowned",
            &[],
            &["Creature"],
            &["Human", "Wizard"],
        );
        assert!(result.extracted_keywords.is_empty());
        assert_eq!(result.statics.len(), 1);
        let static_def = &result.statics[0];
        match static_def.affected.as_ref() {
            Some(TargetFilter::Or { filters }) => {
                assert_eq!(filters.len(), 2);
                for filter in filters {
                    let TargetFilter::Typed(tf) = filter else {
                        panic!("expected typed branch, got {:?}", filter);
                    };
                    assert_eq!(
                        tf.controller,
                        Some(crate::types::ability::ControllerRef::You)
                    );
                    assert!(
                        tf.properties.contains(&FilterProp::InZone {
                            zone: Zone::Graveyard
                        }),
                        "missing graveyard filter: {:?}",
                        tf.properties
                    );
                    assert!(
                        tf.type_filters.contains(&TypeFilter::Instant)
                            || tf.type_filters.contains(&TypeFilter::Sorcery)
                    );
                }
            }
            other => panic!("expected typed affected filter, got {:?}", other),
        }
        assert!(
            static_def
                .modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
                }),
            "missing flashback grant: {:?}",
            static_def.modifications
        );
    }

    #[test]
    fn same_line_static_flashback_grant_stays_on_graveyard_cards() {
        let result = parse(
            "Spells can't be countered.\nEach instant and sorcery card in your graveyard has flashback. The flashback cost is equal to that card's mana cost.",
            "Lier, Disciple of the Drowned",
            &[],
            &["Creature"],
            &["Human", "Wizard"],
        );
        assert!(result.extracted_keywords.is_empty());
        assert_eq!(result.statics.len(), 2);
        assert!(result.statics.iter().any(|static_def| {
            static_def
                .modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
                })
        }));
    }

    #[test]
    fn top_level_static_escape_grant_stays_on_graveyard_cards() {
        let result = parse(
            "Each nonland card in your graveyard has escape.\nThe escape cost is equal to the card's mana cost plus exile three other cards from your graveyard.",
            "Underworld Breach",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(result.extracted_keywords.is_empty());
        assert_eq!(result.statics.len(), 1);
        let static_def = &result.statics[0];
        let TargetFilter::Typed(tf) = static_def
            .affected
            .as_ref()
            .expect("expected affected filter")
        else {
            panic!("expected typed affected filter");
        };
        assert_eq!(
            tf.controller,
            Some(crate::types::ability::ControllerRef::You)
        );
        assert!(
            tf.properties.contains(&FilterProp::InZone {
                zone: Zone::Graveyard
            }),
            "missing graveyard filter: {:?}",
            tf.properties
        );
        assert!(
            static_def
                .modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Escape {
                        cost: ManaCost::SelfManaCost,
                        exile_count: 3,
                    },
                }),
            "missing escape grant: {:?}",
            static_def.modifications
        );
    }

    #[test]
    fn same_line_static_escape_grant_stays_on_graveyard_cards() {
        let result = parse(
            "Each nonland card in your graveyard has escape. The escape cost is equal to the card's mana cost plus exile three other cards from your graveyard.",
            "Underworld Breach",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(result.extracted_keywords.is_empty());
        assert_eq!(result.statics.len(), 1);
        assert!(result.statics.iter().any(|static_def| {
            static_def
                .modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Escape {
                        cost: ManaCost::SelfManaCost,
                        exile_count: 3,
                    },
                })
        }));
    }

    #[test]
    fn helper_parses_same_line_escape_grant_continuation() {
        let static_def = try_parse_graveyard_keyword_static_with_continuation(
            "Each nonland card in your graveyard has escape. The escape cost is equal to the card's mana cost plus exile three other cards from your graveyard.",
        )
        .expect("helper should parse same-line escape continuation");
        assert!(
            static_def
                .modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Escape {
                        cost: ManaCost::SelfManaCost,
                        exile_count: 3,
                    },
                }),
            "missing escape grant: {:?}",
            static_def.modifications
        );
    }

    #[test]
    fn escape_continuation_parser_accepts_self_mana_cost_clause() {
        let keyword = parse_graveyard_keyword_continuation(
            "The escape cost is equal to the card's mana cost plus exile three other cards from your graveyard.",
            GraveyardGrantedKeywordKind::Escape,
        )
        .expect("continuation should parse");
        assert_eq!(
            keyword,
            Keyword::Escape {
                cost: ManaCost::SelfManaCost,
                exile_count: 3,
            }
        );
    }

    #[test]
    fn escape_continuation_parser_rejects_trailing_text() {
        let keyword = parse_graveyard_keyword_continuation(
            "The escape cost is equal to the card's mana cost plus exile three other cards from your graveyard until end of turn.",
            GraveyardGrantedKeywordKind::Escape,
        );
        assert!(
            keyword.is_none(),
            "trailing text should reject continuation"
        );
    }

    #[test]
    fn viral_spawning_corrupted_line_parses_as_conditional_flashback_static() {
        let result = parse(
            "Create a 3/3 green Phyrexian Beast creature token with toxic 1. (Players dealt combat damage by it also get a poison counter.)\nCorrupted — As long as an opponent has three or more poison counters and this card is in your graveyard, it has flashback {2}{G}. (You may cast this card from your graveyard for its flashback cost. Then exile it.)",
            "Viral Spawning",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(result.statics.len(), 1);
        let static_def = &result.statics[0];
        assert_eq!(static_def.affected, Some(TargetFilter::SelfRef));
        assert!(
            static_def
                .modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                        generic: 2,
                        shards: vec![crate::types::mana::ManaCostShard::Green],
                    })),
                }),
            "missing flashback keyword: {:?}",
            static_def.modifications
        );
        assert!(
            matches!(static_def.condition, Some(StaticCondition::And { .. })),
            "expected conjunctive static condition, got {:?}",
            static_def.condition
        );
    }

    // ── Each player/opponent iteration ────────────────────────────────

    #[test]
    fn each_opponent_discards_produces_player_scope() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::PlayerFilter;
        let def = parse_effect_chain(
            "each opponent discards a card",
            crate::types::ability::AbilityKind::Spell,
        );
        assert_eq!(
            def.player_scope,
            Some(PlayerFilter::Opponent),
            "player_scope should be Opponent for 'each opponent discards'"
        );
        assert!(
            matches!(*def.effect, Effect::Discard { .. }),
            "inner effect should be Discard, got {:?}",
            def.effect,
        );
    }

    #[test]
    fn each_player_draws_produces_player_scope() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::PlayerFilter;
        let def = parse_effect_chain(
            "each player draws a card",
            crate::types::ability::AbilityKind::Spell,
        );
        assert_eq!(
            def.player_scope,
            Some(PlayerFilter::All),
            "player_scope should be All for 'each player draws'"
        );
        assert!(
            matches!(*def.effect, Effect::Draw { .. }),
            "inner effect should be Draw, got {:?}",
            def.effect,
        );
    }

    #[test]
    fn each_player_discards_their_hand_binds_count_to_scoped_player() {
        // #781 Wheel of Fortune: "Each player discards their hand, then draws
        // seven cards." The "their hand" count must bind to the iterated player
        // (ScopedPlayer), not the caster (Controller). Pre-fix it parsed to
        // HandSize{Controller}, so under player_scope iteration only the caster's
        // (already-emptied) hand size drove every player's discard count and
        // opponents kept their hands.
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{PlayerFilter, PlayerScope, QuantityExpr, QuantityRef};
        let def = parse_effect_chain(
            "Each player discards their hand, then draws seven cards.",
            crate::types::ability::AbilityKind::Spell,
        );
        assert_eq!(
            def.player_scope,
            Some(PlayerFilter::All),
            "player_scope should be All for 'each player'"
        );
        let count = match &*def.effect {
            Effect::Discard { count, .. } => count,
            other => panic!("expected Discard, got {other:?}"),
        };
        assert_eq!(
            *count,
            QuantityExpr::Ref {
                qty: QuantityRef::HandSize {
                    player: PlayerScope::ScopedPlayer,
                },
            },
            "discard count must bind 'their hand' to ScopedPlayer (#781)"
        );
    }

    #[test]
    fn each_opponent_loses_life_produces_player_scope() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::PlayerFilter;
        let def = parse_effect_chain(
            "each opponent loses 2 life",
            crate::types::ability::AbilityKind::Spell,
        );
        assert_eq!(
            def.player_scope,
            Some(PlayerFilter::Opponent),
            "player_scope should be Opponent for 'each opponent loses 2 life'"
        );
        assert!(
            matches!(*def.effect, Effect::LoseLife { .. }),
            "inner effect should be LoseLife, got {:?}",
            def.effect,
        );
    }

    #[test]
    fn each_opponent_with_no_cards_in_hand_preserves_condition() {
        let def = parse_effect_chain(
            "each opponent with no cards in hand loses 10 life",
            crate::types::ability::AbilityKind::Spell,
        );

        assert_eq!(def.player_scope, Some(PlayerFilter::Opponent));
        assert!(matches!(*def.effect, Effect::LoseLife { .. }));
        assert!(matches!(
            def.condition,
            Some(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::ScopedPlayer
                    }
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            })
        ));
    }

    #[test]
    fn each_opponent_mills_produces_player_scope() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::PlayerFilter;
        let def = parse_effect_chain(
            "each opponent mills three cards",
            crate::types::ability::AbilityKind::Spell,
        );
        assert_eq!(
            def.player_scope,
            Some(PlayerFilter::Opponent),
            "player_scope should be Opponent for 'each opponent mills'"
        );
        assert!(
            matches!(*def.effect, Effect::Mill { .. }),
            "inner effect should be Mill, got {:?}",
            def.effect,
        );
    }

    // --- Static parser greediness: spell lines with damage + restriction ---

    #[test]
    fn spell_damage_plus_cant_block_not_static() {
        // Mugging: "deals 2 damage to target creature. That creature can't block this turn."
        // Must produce a spell ability with DealDamage, NOT a static CantBlock.
        let r = parse(
            "Mugging deals 2 damage to target creature. That creature can't block this turn.",
            "Mugging",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(
            r.statics.is_empty(),
            "spell damage line should not produce static, got {:?}",
            r.statics
        );
        assert_eq!(r.abilities.len(), 1, "should produce one spell ability");
        assert!(
            matches!(*r.abilities[0].effect, Effect::DealDamage { .. }),
            "first effect should be DealDamage, got {:?}",
            r.abilities[0].effect
        );
        assert!(
            r.abilities[0].sub_ability.is_some(),
            "should chain to restriction sub_ability"
        );
    }

    #[test]
    fn spell_cost_reduction_for_creatures_that_attacked_stays_static() {
        let r = parse(
            "This spell costs {1} less to cast for each creature that attacked this turn.\nDraw three cards.",
            "Rowdy Research",
            &[],
            &["Instant"],
            &[],
        );

        assert_eq!(r.abilities.len(), 1);
        assert!(
            matches!(*r.abilities[0].effect, Effect::Draw { .. }),
            "real spell effect should be preserved, got {:?}",
            r.abilities[0].effect
        );
        assert_eq!(r.statics.len(), 1);
        let StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::Cost { generic: 1, .. },
            dynamic_count:
                Some(QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(filter),
                }),
            ..
        } = &r.statics[0].mode
        else {
            panic!(
                "expected self-spell ReduceCost over attacked creatures, got {:?}",
                r.statics[0].mode
            );
        };
        assert!(filter
            .type_filters
            .iter()
            .any(|filter| matches!(filter, TypeFilter::Creature)));
        assert!(filter
            .properties
            .iter()
            .any(|prop| matches!(prop, FilterProp::AttackedThisTurn)));
        assert!(matches!(r.statics[0].affected, Some(TargetFilter::SelfRef)));
        assert_eq!(
            r.statics[0].active_zones,
            vec![Zone::Hand, Zone::Stack, Zone::Command]
        );
        assert!(
            r.parse_warnings
                .iter()
                .all(|warning| warning.to_string().split_whitespace().next()
                    != Some("Swallow:DynamicQty")),
            "unexpected DynamicQty warning: {:?}",
            r.parse_warnings
        );
    }

    #[test]
    fn spell_cost_reduction_for_creatures_that_attacked_preserves_damage_effect() {
        let r = parse(
            "This spell costs {1} less to cast for each creature that attacked this turn.\nWitchstalker Frenzy deals 5 damage to target creature.",
            "Witchstalker Frenzy",
            &[],
            &["Instant"],
            &[],
        );

        assert_eq!(r.abilities.len(), 1);
        assert!(
            matches!(*r.abilities[0].effect, Effect::DealDamage { .. }),
            "real spell effect should be preserved, got {:?}",
            r.abilities[0].effect
        );
        assert_eq!(r.statics.len(), 1);
        assert!(
            matches!(
                r.statics[0].mode,
                StaticMode::ModifyCost {
                    mode: CostModifyMode::Reduce,
                    ..
                }
            ),
            "cost-reduction sentence should be a static, got {:?}",
            r.statics[0].mode
        );
        assert!(
            r.abilities
                .iter()
                .all(|ability| !matches!(*ability.effect, Effect::CastFromZone { .. })),
            "cost-reduction sentence must not become CastFromZone: {:?}",
            r.abilities
        );
    }

    #[test]
    fn negative_self_casting_restriction_stays_metadata() {
        let r = parse(
            "You can't cast Rock Jockey if you've played a land this turn.\nYou can't play lands if Rock Jockey was cast this turn.",
            "Rock Jockey",
            &[],
            &["Creature"],
            &["Goblin", "Knight"],
        );

        assert_eq!(
            r.casting_restrictions,
            vec![CastingRestriction::RequiresCondition {
                condition: Some(ParsedCondition::Not {
                    condition: Box::new(ParsedCondition::YouPlayedLandThisTurn),
                }),
            }]
        );
        assert!(
            r.abilities
                .iter()
                .all(|ability| !matches!(*ability.effect, Effect::CastFromZone { .. })),
            "negative casting restriction must not become CastFromZone: {:?}",
            r.abilities
        );
    }

    // CR 305.1 + CR 602.1 + CR 611.1 + CR 611.2c + CR 701.21a:
    // Pardic Miner — "Sacrifice this creature: Target player can't play lands
    // this turn." The activated ability resolves to a `GenericEffect` carrying
    // a `CantPlayLand` static with a `TargetFilter::Player` target slot and
    // `Duration::UntilEndOfTurn`. At resolution the runtime registers a
    // transient continuous effect bound to `SpecificPlayer { id }` (the chosen
    // target), and `player_has_static_other(state, target, "CantPlayLand")`
    // returns true through the new TCE scan in `check_static_other_by_name`.
    //
    // This is the class of "target player can't [verb] this turn" effects —
    // proves the parser routes the player-scoped restriction through
    // `parse_restriction_modes` and emits the canonical `GenericEffect` shape.
    #[test]
    fn activated_target_player_cant_play_lands_pardic_miner() {
        use crate::types::statics::StaticMode;
        let r = parse(
            "Sacrifice this creature: Target player can't play lands this turn.",
            "Pardic Miner",
            &[],
            &["Creature"],
            &["Dwarf"],
        );
        assert_eq!(
            r.abilities.len(),
            1,
            "Pardic Miner has exactly one activated ability"
        );
        let ab = &r.abilities[0];
        assert_eq!(ab.kind, AbilityKind::Activated);
        let Effect::GenericEffect {
            static_abilities,
            duration,
            target,
        } = &*ab.effect
        else {
            panic!("expected GenericEffect, got {:?}", ab.effect);
        };
        assert_eq!(
            *duration,
            Some(crate::types::ability::Duration::UntilEndOfTurn),
            "duration must be UntilEndOfTurn for 'this turn'"
        );
        assert_eq!(
            target.as_ref(),
            Some(&TargetFilter::Player),
            "target slot must be TargetFilter::Player for 'Target player'"
        );
        assert_eq!(static_abilities.len(), 1, "single CantPlayLand static");
        let def = &static_abilities[0];
        assert_eq!(
            def.mode,
            StaticMode::Other("CantPlayLand".to_string()),
            "mode must be CantPlayLand"
        );
        // CR 305.1 + CR 611.2c: AddStaticMode is required so the TCE carries
        // the mode into runtime queries (player_has_static_other). Without it
        // the transient effect has empty modifications and the prohibition
        // never reaches the play-land gate.
        assert!(
            def.modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddStaticMode { mode: StaticMode::Other(name) } if name == "CantPlayLand"
            )),
            "modifications must include AddStaticMode {{ Other(\"CantPlayLand\") }}, got {:?}",
            def.modifications
        );
    }

    #[test]
    fn spell_restriction_then_damage_skullcrack() {
        // Skullcrack: "Players can't gain life this turn. Damage can't be prevented this turn.
        //              Skullcrack deals 3 damage to target player or planeswalker."
        let r = parse(
            "Players can't gain life this turn. Damage can't be prevented this turn. Skullcrack deals 3 damage to target player or planeswalker.",
            "Skullcrack",
            &[],
            &["Instant"],
            &[],
        );
        assert!(
            r.statics.is_empty(),
            "spell damage line should not produce static, got {:?}",
            r.statics
        );
        assert_eq!(r.abilities.len(), 1);
        // Chain: GenericEffect(CantGainLife) → AddRestriction → DealDamage
        let ab = &r.abilities[0];
        assert!(
            matches!(*ab.effect, Effect::GenericEffect { .. }),
            "first clause should be GenericEffect(CantGainLife), got {:?}",
            ab.effect
        );
        let sub1 = ab
            .sub_ability
            .as_ref()
            .expect("should chain to AddRestriction");
        assert!(
            matches!(*sub1.effect, Effect::AddRestriction { .. }),
            "second clause should be AddRestriction, got {:?}",
            sub1.effect
        );
        let sub2 = sub1
            .sub_ability
            .as_ref()
            .expect("should chain to DealDamage");
        assert!(
            matches!(*sub2.effect, Effect::DealDamage { .. }),
            "third clause should be DealDamage, got {:?}",
            sub2.effect
        );
    }

    #[test]
    fn roiling_vortex_parses_trigger_lines_and_opponent_life_lock_activation() {
        use crate::types::statics::StaticMode;

        let r = parse(
            "At the beginning of each player's upkeep, this enchantment deals 1 damage to them.\nWhenever a player casts a spell, if no mana was spent to cast that spell, this enchantment deals 5 damage to that player.\n{R}: Your opponents can't gain life this turn.",
            "Roiling Vortex",
            &[],
            &["Enchantment"],
            &[],
        );

        assert_eq!(r.triggers.len(), 2, "expected both printed trigger lines");
        assert_eq!(r.abilities.len(), 1, "expected one activated ability");

        let ab = &r.abilities[0];
        assert_eq!(ab.kind, AbilityKind::Activated);
        let Effect::GenericEffect {
            static_abilities,
            duration,
            target,
        } = &*ab.effect
        else {
            panic!("expected GenericEffect, got {:?}", ab.effect);
        };

        assert_eq!(*target, None);
        assert_eq!(
            *duration,
            Some(crate::types::ability::Duration::UntilEndOfTurn)
        );
        assert!(static_abilities
            .iter()
            .any(|s| s.mode == StaticMode::CantGainLife));
        assert!(static_abilities.iter().any(|s| {
            matches!(
                s.affected,
                Some(TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::Opponent),
                    ..
                }))
            )
        }));
    }

    // CR 104.2b + CR 104.3b + CR 119.7 + CR 119.8 + CR 611.2b:
    // Everybody Lives! prints three sentences, the third of which is a
    // conjunction joining two player-subject restriction clauses
    // ("Players can't lose life this turn AND players can't lose the game
    // or win the game this turn."). All three statics — CantLoseLife,
    // CantLoseTheGame, CantWinTheGame — must land in the chain with
    // UntilEndOfTurn duration so the engine installs them as transient
    // continuous effects. Before this fix, the third sentence routed to
    // Effect::Unimplemented and the game-loss prevention did not fire,
    // allowing a player to win by causing an opponent to draw from an
    // empty library on the same turn Everybody Lives! resolved.
    #[test]
    fn everybody_lives_emits_cant_lose_life_lose_game_win_game_statics() {
        use crate::types::statics::StaticMode;
        let r = parse(
            "All creatures gain hexproof and indestructible until end of turn. \
             Players gain hexproof until end of turn. \
             Players can't lose life this turn and players can't lose the game \
             or win the game this turn.",
            "Everybody Lives!",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1, "single chained spell ability");

        // Walk the chain and collect every static mode emitted by every
        // GenericEffect node. The exact node assignment is an implementation
        // detail of the chain assembler; the contract is that the chain emits
        // CantLoseLife + CantLoseTheGame + CantWinTheGame (and no Unimplemented
        // chunk).
        let mut modes: Vec<StaticMode> = Vec::new();
        let mut node = Some(&r.abilities[0]);
        while let Some(def) = node {
            assert!(
                !matches!(*def.effect, Effect::Unimplemented { .. }),
                "no Unimplemented chunk should remain, got {:?}",
                def.effect
            );
            if let Effect::GenericEffect {
                ref static_abilities,
                ..
            } = *def.effect
            {
                for s in static_abilities {
                    modes.push(s.mode.clone());
                }
            }
            node = def.sub_ability.as_deref();
        }
        assert!(
            modes.contains(&StaticMode::CantLoseLife),
            "chain must emit CantLoseLife, got {modes:?}"
        );
        assert!(
            modes.contains(&StaticMode::CantLoseTheGame),
            "chain must emit CantLoseTheGame, got {modes:?}"
        );
        assert!(
            modes.contains(&StaticMode::CantWinTheGame),
            "chain must emit CantWinTheGame, got {modes:?}"
        );
    }

    #[test]
    fn avatars_wrath_parses_airbend_chain_cast_restriction_and_self_exile() {
        let r = parse(
            "Choose up to one target creature, then airbend all other creatures. (Exile them. While each one is exiled, its owner may cast it for {2} rather than its mana cost.)\nUntil your next turn, your opponents can't cast spells from anywhere other than their hands.\nExile Avatar's Wrath.",
            "Avatar's Wrath",
            &[],
            &["Sorcery"],
            &[],
        );

        assert_eq!(r.abilities.len(), 3);
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::TargetOnly {
                target: TargetFilter::Typed(_),
            }
        ));
        let airbend = r.abilities[0]
            .sub_ability
            .as_ref()
            .expect("airbend clause should chain from TargetOnly");
        assert!(matches!(
            *airbend.effect,
            Effect::ChangeZoneAll {
                destination: Zone::Exile,
                ..
            }
        ));
        let permission = airbend
            .sub_ability
            .as_ref()
            .expect("airbend clause should grant exile-cast permission");
        assert!(matches!(
            *permission.effect,
            Effect::GrantCastingPermission { .. }
        ));

        assert!(matches!(
            *r.abilities[1].effect,
            Effect::AddRestriction {
                restriction: crate::types::ability::GameRestriction::ProhibitActivity {
                    activity: crate::types::ability::ProhibitedActivity::CastOnlyFromZones { .. },
                    ..
                }
            }
        ));
        assert_eq!(
            r.abilities[1].duration,
            Some(crate::types::ability::Duration::UntilNextTurnOf {
                player: crate::types::ability::PlayerScope::Controller,
            })
        );

        assert!(matches!(
            *r.abilities[2].effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            }
        ));
    }

    #[test]
    fn spell_damage_plus_doesnt_untap() {
        // Chandra's Revolution: "deals 4 damage to target creature. Tap target land.
        //                        That land doesn't untap during its controller's next untap step."
        let r = parse(
            "Chandra's Revolution deals 4 damage to target creature. Tap target land. That land doesn't untap during its controller's next untap step.",
            "Chandra's Revolution",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(
            r.statics.is_empty(),
            "spell damage line should not produce static, got {:?}",
            r.statics
        );
        assert!(!r.abilities.is_empty(), "should produce spell abilities");
        assert!(
            matches!(*r.abilities[0].effect, Effect::DealDamage { .. }),
            "first effect should be DealDamage, got {:?}",
            r.abilities[0].effect
        );
    }

    #[test]
    fn spell_counter_tap_plus_doesnt_untap() {
        let r = parse(
            "Put a +1/+1 counter on up to one target creature you control. Tap up to one target creature you don't control, and that creature doesn't untap during its controller's next untap step.",
            "Winterthorn Blessing",
            &[],
            &["Instant"],
            &[],
        );
        assert!(
            r.statics.is_empty(),
            "spell next-untap restriction should not produce static, got {:?}",
            r.statics
        );

        let mut saw_counter = false;
        let mut saw_tap = false;
        let mut saw_cant_untap = false;
        for ability in &r.abilities {
            let mut cursor = Some(ability);
            while let Some(def) = cursor {
                match def.effect.as_ref() {
                    Effect::PutCounter { .. } => saw_counter = true,
                    Effect::Tap { .. } => saw_tap = true,
                    Effect::GenericEffect {
                        static_abilities,
                        duration,
                        ..
                    } => {
                        saw_cant_untap |= static_abilities.iter().any(|static_def| {
                            static_def.mode == crate::types::statics::StaticMode::CantUntap
                        }) && matches!(
                            duration,
                            Some(crate::types::ability::Duration::UntilNextStepOf {
                                step: crate::types::phase::Phase::Untap,
                                player: crate::types::ability::PlayerScope::Controller,
                            })
                        );
                    }
                    _ => {}
                }
                cursor = def.sub_ability.as_deref();
            }
        }

        assert!(saw_counter, "should parse the counter clause: {r:?}");
        assert!(saw_tap, "should parse the tap clause: {r:?}");
        assert!(
            saw_cant_untap,
            "should parse the next-untap restriction clause: {r:?}"
        );
    }

    #[test]
    fn creature_cant_block_still_produces_static() {
        // Regression guard: non-spell "can't block" must still produce static.
        let r = parse(
            "Defender\nThis creature can't attack.",
            "Guard Gomazoa",
            &[Keyword::Defender],
            &["Creature"],
            &[],
        );
        assert!(
            !r.statics.is_empty(),
            "creature restriction should still produce static"
        );
    }

    #[test]
    fn biomass_mutation_parses_as_generic_effect_with_dynamic_set_pt() {
        // CR 613.4b + CR 107.3m: "Creatures you control have base power and
        // toughness X/X until end of turn" is a one-shot layer-7b set effect.
        // The spell is an instant with {X} in cost, so X resolves to CostXPaid.
        use crate::types::ability::{ContinuousModification, Effect, QuantityExpr, QuantityRef};
        let r = parse(
            "Creatures you control have base power and toughness X/X until end of turn.",
            "Biomass Mutation",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1, "expected one spell ability");
        let eff = &*r.abilities[0].effect;
        let Effect::GenericEffect {
            static_abilities, ..
        } = eff
        else {
            panic!("expected GenericEffect, got {eff:?}");
        };
        assert_eq!(static_abilities.len(), 1);
        let mods = &static_abilities[0].modifications;
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
    }

    #[test]
    fn karn_sydri_artifact_animation_has_dynamic_mana_value_pt_no_warning() {
        for (name, text) in [
            (
                "Karn, Silver Golem",
                "{1}: Target noncreature artifact becomes an artifact creature with power and toughness each equal to its mana value until end of turn.",
            ),
            (
                "Sydri, Galvanic Genius",
                "{U}: Target noncreature artifact becomes an artifact creature with power and toughness each equal to its mana value until end of turn.",
            ),
        ] {
            let r = parse(text, name, &[], &["Artifact"], &[]);
            assert!(
                r.parse_warnings
                    .iter()
                    .all(|warning| warning.to_string().split_whitespace().next() != Some("Swallow:DynamicQty")),
                "unexpected DynamicQty warning for {name}: {:?}",
                r.parse_warnings
            );
            assert_eq!(r.abilities.len(), 1, "{name}: expected one activated ability");

            let Effect::GenericEffect {
                target: Some(TargetFilter::Typed(tf)),
                static_abilities,
                duration: Some(crate::types::ability::Duration::UntilEndOfTurn),
            } = r.abilities[0].effect.as_ref()
            else {
                panic!("{name}: expected UEOT GenericEffect, got {:?}", r.abilities[0].effect);
            };
            assert!(tf.type_filters.contains(&TypeFilter::Artifact));
            assert!(
                tf.type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Creature)))
            );
            assert_eq!(static_abilities.len(), 1);

            let mods = &static_abilities[0].modifications;
            let expected = QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Recipient,
                },
            };
            assert!(mods.contains(&ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Artifact,
            }));
            assert!(mods.contains(&ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Creature,
            }));
            assert!(mods.contains(&ContinuousModification::SetPowerDynamic {
                value: expected.clone(),
            }));
            assert!(mods.contains(&ContinuousModification::SetToughnessDynamic {
                value: expected,
            }));
        }
    }

    #[test]
    fn spell_pump_all_with_duration_not_static() {
        // CR 611.2a: Spell lines with subject + pump + duration are one-shot
        // continuous effects, not permanent static abilities.
        let r = parse(
            "Creatures you control get +2/+0 until end of turn.",
            "Test Spell",
            &[],
            &["Instant"],
            &[],
        );
        assert!(
            r.statics.is_empty(),
            "spell pump-all with duration should not produce static, got {:?}",
            r.statics,
        );
        assert_eq!(r.abilities.len(), 1, "should produce one spell ability");
        assert!(
            matches!(*r.abilities[0].effect, Effect::PumpAll { .. }),
            "effect should be PumpAll, got {:?}",
            r.abilities[0].effect,
        );
    }

    #[test]
    fn permanent_pump_all_without_duration_stays_static() {
        // CR 611.3a: Same pattern on a permanent is a static ability.
        let r = parse(
            "Creatures you control get +1/+1.",
            "Test Enchantment",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(
            !r.statics.is_empty(),
            "permanent pump-all should produce static ability",
        );
        assert!(
            r.abilities.is_empty(),
            "permanent pump-all should not produce spell ability, got {:?}",
            r.abilities,
        );
    }

    #[test]
    fn spell_restriction_with_duration_not_static() {
        // CR 611.2a: Spell lines with a restriction + duration are one-shot
        // continuous effects, not permanent statics. Tests a non-pump
        // `is_static_pattern` variant ("can't block") with a duration marker.
        let r = parse(
            "Creatures your opponents control can't block this turn.",
            "Test Spell",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(
            r.statics.is_empty(),
            "spell restriction with duration should not produce static, got {:?}",
            r.statics,
        );
        assert_eq!(r.abilities.len(), 1, "should produce one spell ability");
    }

    #[test]
    fn multi_line_spell_preserves_non_damage_static() {
        // Line 1 (no damage) should produce static; line 2 (damage) should produce ability.
        let r = parse(
            "Creatures you control have haste.\nBarrage of Boulders deals 1 damage to each creature you don't control.",
            "Barrage of Boulders",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(
            !r.statics.is_empty(),
            "non-damage line should still produce static"
        );
        assert!(
            !r.abilities.is_empty(),
            "damage line should produce spell ability"
        );
    }

    #[test]
    fn collected_company_dig_from_among() {
        let r = parse(
            "Look at the top six cards of your library. Put up to two creature cards with mana value 3 or less from among them onto the battlefield. Put the rest on the bottom of your library in any order.",
            "Collected Company",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1, "should produce one ability");
        match &*r.abilities[0].effect {
            Effect::Dig {
                count,
                destination,
                keep_count,
                up_to,
                filter,
                rest_destination,
                ..
            } => {
                assert_eq!(
                    *count,
                    QuantityExpr::Fixed { value: 6 },
                    "dig count should be 6"
                );
                assert_eq!(
                    *destination,
                    Some(Zone::Battlefield),
                    "kept cards go to battlefield"
                );
                assert_eq!(*keep_count, Some(2), "keep up to 2");
                assert!(*up_to, "should be up_to");
                assert!(
                    matches!(filter, TargetFilter::Typed(TypedFilter { ref type_filters, .. })
                        if type_filters.contains(&TypeFilter::Creature)),
                    "filter should require creatures, got {:?}",
                    filter,
                );
                assert_eq!(
                    *rest_destination,
                    Some(Zone::Library),
                    "rest go to bottom of library"
                );
            }
            other => {
                panic!(
                    "Expected Dig effect, got {:?}",
                    std::mem::discriminant(other)
                );
            }
        }
    }

    #[test]
    fn commune_with_nature_dig_from_among() {
        let r = parse(
            "Look at the top five cards of your library. You may reveal a creature card from among them and put it into your hand. Put the rest on the bottom of your library in any order.",
            "Commune with Nature",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        match &*r.abilities[0].effect {
            Effect::Dig {
                count,
                destination,
                keep_count,
                up_to,
                filter,
                rest_destination,
                ..
            } => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 5 });
                assert_eq!(*destination, Some(Zone::Hand));
                assert_eq!(*keep_count, Some(1));
                assert!(*up_to, "a creature card = up to 1");
                assert!(
                    matches!(filter, TargetFilter::Typed(TypedFilter { ref type_filters, .. })
                        if type_filters.contains(&TypeFilter::Creature)),
                    "filter should require creatures",
                );
                assert_eq!(*rest_destination, Some(Zone::Library));
            }
            other => {
                panic!(
                    "Expected Dig effect, got {:?}",
                    std::mem::discriminant(other)
                );
            }
        }
    }

    /// Visions (LEG / 4ED): "Look at the top five cards of target player's
    /// library. You may then have that player shuffle that library."
    ///
    /// End-to-end verification of the wrapper chain: the primary effect is a
    /// `Dig` (look-at) keyed on a player target, with a `may`-gated sub-ability
    /// emitting `Effect::Shuffle { target: ParentTarget }` that resolves at
    /// runtime against the parent's inherited `TargetRef::Player`. The
    /// `"shuffle that library"` anaphor is the new arm added in
    /// `parse_shuffle_ast`.
    #[test]
    fn visions_look_then_have_target_player_shuffle() {
        let result = parse(
            "Look at the top five cards of target player's library. You may then have that player shuffle that library.",
            "Visions",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(result.abilities.len(), 1, "Visions has one ability");
        let ability = &result.abilities[0];
        // Primary effect: Look at top 5 cards (Dig with reveal=false, no
        // keep_count — pure peek). The parent target is the player whose
        // library we are looking at.
        match &*ability.effect {
            Effect::Dig {
                count,
                keep_count,
                player,
                reveal,
                ..
            } => {
                assert_eq!(count, &QuantityExpr::Fixed { value: 5 }, "look at top 5");
                assert_eq!(
                    player,
                    &TargetFilter::Player,
                    "target player's library should surface a player target"
                );
                assert_eq!(
                    keep_count,
                    &Some(0),
                    "bare look-at instruction should be a pure peek"
                );
                assert!(!reveal, "look at (private), not reveal (public)");
            }
            other => panic!(
                "Expected Dig effect for sentence 1, got {:?}",
                std::mem::discriminant(other)
            ),
        }
        // Sub-ability: "you may then have that player shuffle that library"
        // → `may`-gated `Effect::Shuffle { target: ParentTarget }`.
        let sub = ability
            .sub_ability
            .as_ref()
            .expect("Visions should have a sub-ability for the shuffle clause");
        // CR 608.2d: A spell's resolution-time "you may" choice — the player
        // announces the optional shuffle while applying the effect.
        assert!(sub.optional, "sub-ability should be optional ('you may')");
        match &*sub.effect {
            Effect::Shuffle { target, .. } => {
                assert_eq!(
                    target,
                    &TargetFilter::ParentTarget,
                    "shuffle target should be the context-ref ParentTarget filter so it \
                     inherits the parent ability's targeted player at resolution",
                );
            }
            other => panic!(
                "Expected Effect::Shuffle in sub-ability, got {:?}",
                std::mem::discriminant(other)
            ),
        }
    }

    /// Satyr Wayfinder: "reveal the top four cards" → Dig with reveal=true,
    /// continuation patches keep_count, filter, rest_destination from "you may put a land card
    /// from among them into your hand. Put the rest into your graveyard."
    #[test]
    fn satyr_wayfinder_reveal_dig_from_among() {
        let result = parse_with_keyword_names(
            "When this creature enters, reveal the top four cards of your library. You may put a land card from among them into your hand. Put the rest into your graveyard.",
            "Satyr Wayfinder",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(result.triggers.len(), 1, "should have one ETB trigger");
        let execute = result.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute");
        match &*execute.effect {
            Effect::Dig {
                count,
                destination,
                keep_count,
                up_to,
                filter,
                rest_destination,
                reveal,
                ..
            } => {
                assert_eq!(
                    count,
                    &QuantityExpr::Fixed { value: 4 },
                    "dig count should be 4"
                );
                assert!(
                    reveal,
                    "should be reveal=true for 'reveal the top' (CR 701.20a)"
                );
                assert_eq!(destination, &Some(Zone::Hand), "kept cards go to hand");
                assert_eq!(keep_count, &Some(1), "keep up to 1 (a land card)");
                assert!(up_to, "'you may' = up to");
                assert!(
                    matches!(filter, TargetFilter::Typed(TypedFilter { ref type_filters, .. })
                        if type_filters.contains(&TypeFilter::Land)),
                    "filter should require lands, got {:?}",
                    filter,
                );
                assert_eq!(
                    rest_destination,
                    &Some(Zone::Graveyard),
                    "rest go to graveyard"
                );
            }
            other => {
                panic!(
                    "Expected Dig effect, got {:?}",
                    std::mem::discriminant(other)
                );
            }
        }
    }

    #[test]
    fn vrondiss_enrage_damage_received_watches_self_not_controller() {
        let result = parse(
            "Enrage — Whenever Vrondiss, Rage of Ancients is dealt damage, you may create a 5/4 red and green Dragon Spirit creature token with \"When this creature deals damage, sacrifice it.\"",
            "Vrondiss, Rage of Ancients",
            &[],
            &["Creature"],
            &["Dragon", "Barbarian"],
        );
        assert_eq!(result.triggers.len(), 1, "triggers={:?}", result.triggers);
        let trigger = &result.triggers[0];
        assert_eq!(trigger.mode, TriggerMode::DamageReceived);
        assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(trigger.valid_target, None);
    }

    #[test]
    fn heroic_trigger_not_misrouted_to_replacement() {
        // Favored Hoplite: "Heroic — Whenever you cast a spell that targets this creature,
        // put a +1/+1 counter on this creature and prevent all damage that would be dealt
        // to it this turn."
        // Should produce a trigger, NOT a replacement.
        let result = parse(
            "Heroic — Whenever you cast a spell that targets this creature, put a +1/+1 counter on this creature and prevent all damage that would be dealt to it this turn.",
            "Favored Hoplite",
            &[],
            &["Creature"],
            &["Human", "Soldier"],
        );
        assert_eq!(
            result.triggers.len(),
            1,
            "Should have 1 trigger, got {} triggers and {} replacements. triggers={:?} replacements={:?}",
            result.triggers.len(),
            result.replacements.len(),
            result.triggers,
            result.replacements,
        );
        assert_eq!(
            result.replacements.len(),
            0,
            "Should have 0 replacements, got {}: {:?}",
            result.replacements.len(),
            result.replacements,
        );
    }

    #[test]
    fn ability_word_trigger_not_static_or_replacement() {
        // "Constellation — Whenever an enchantment enters the battlefield under your control,
        // you gain 1 life." — ability-word-prefixed trigger should route to triggers.
        let result = parse(
            "Constellation — Whenever an enchantment you control enters, you gain 1 life.",
            "Test Card",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(
            result.triggers.len(),
            1,
            "Ability-word trigger should produce 1 trigger, got: triggers={:?}",
            result.triggers,
        );
    }

    #[test]
    fn ability_word_trigger_preserves_fixed_land_subtype_intervening_if() {
        let result = parse(
            "The Minstrel's Ballad — At the beginning of combat on your turn, if you control five or more Towns, create a 2/2 Elemental creature token that's all colors.",
            "The Wandering Minstrel",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(result.triggers.len(), 1, "triggers={:?}", result.triggers);
        let trigger = &result.triggers[0];
        assert_eq!(trigger.mode, TriggerMode::Phase);
        assert_eq!(trigger.phase, Some(Phase::BeginCombat));
        assert_eq!(
            trigger.constraint,
            Some(crate::types::ability::TriggerConstraint::OnlyDuringYourTurn)
        );
        match trigger.condition.as_ref() {
            Some(TriggerCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ObjectCount {
                                filter: TargetFilter::Typed(typed),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 5 },
            }) => {
                assert!(
                    typed
                        .type_filters
                        .contains(&TypeFilter::Subtype("Town".to_string())),
                    "expected Town subtype filter, got {:?}",
                    typed.type_filters
                );
                assert_eq!(typed.controller, Some(ControllerRef::You));
                assert!(typed.properties.contains(&FilterProp::InZone {
                    zone: Zone::Battlefield
                }));
            }
            other => panic!("expected Town ObjectCount trigger condition, got {other:?}"),
        }
    }

    #[test]
    fn b20_platinum_angel_both_statics() {
        // B20: Compound "can't win/lose" line must emit BOTH statics
        let result = parse(
            "You can't lose the game and your opponents can't win the game.",
            "Platinum Angel",
            &[],
            &["Creature"],
            &[],
        );
        assert!(
            result
                .statics
                .iter()
                .any(|s| s.mode == StaticMode::CantLoseTheGame),
            "should emit CantLoseTheGame, got: {:?}",
            result.statics,
        );
        assert!(
            result
                .statics
                .iter()
                .any(|s| s.mode == StaticMode::CantWinTheGame),
            "should emit CantWinTheGame, got: {:?}",
            result.statics,
        );
    }

    #[test]
    fn discard_unless_creature_card() {
        let r = parse(
            "Draw three cards. Then discard two cards unless you discard a creature card.",
            "Winternight Stories",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let sub = r.abilities[0]
            .sub_ability
            .as_ref()
            .expect("Should have sub_ability for discard");
        match &*sub.effect {
            Effect::Discard {
                count,
                unless_filter,
                ..
            } => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
                assert!(unless_filter.is_some(), "Expected unless_filter, got None");
            }
            other => panic!("Expected Discard, got {:?}", std::mem::discriminant(other)),
        }
    }

    #[test]
    fn analyze_the_pollen_parses_collect_evidence_search_override() {
        fn contains_reveal_top(ability: &AbilityDefinition) -> bool {
            matches!(&*ability.effect, Effect::RevealTop { .. })
                || ability
                    .sub_ability
                    .as_ref()
                    .is_some_and(|sub| contains_reveal_top(sub))
                || ability
                    .else_ability
                    .as_ref()
                    .is_some_and(|sub| contains_reveal_top(sub))
        }

        let result = parse_with_keyword_names(
            "As an additional cost to cast this spell, you may collect evidence 8. (Exile cards with total mana value 8 or greater from your graveyard.)\nSearch your library for a basic land card. If evidence was collected, instead search your library for a creature or land card. Reveal that card, put it into your hand, then shuffle.",
            "Analyze the Pollen",
            &["Collect evidence"],
            &["Sorcery"],
            &[],
        );

        assert_eq!(
            result.additional_cost,
            Some(AdditionalCost::Optional {
                cost: AbilityCost::CollectEvidence { amount: 8 },
                repeatable: false,
            })
        );
        assert_eq!(result.abilities.len(), 1);
        let ability = &result.abilities[0];
        match &*ability.effect {
            Effect::SearchLibrary {
                filter,
                count,
                reveal,
                ..
            } => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
                assert!(*reveal);
                match filter {
                    TargetFilter::Typed(tf) => {
                        assert!(tf.type_filters.contains(&TypeFilter::Land));
                        assert!(tf.properties.iter().any(|prop| matches!(
                            prop,
                            crate::types::ability::FilterProp::HasSupertype {
                                value: crate::types::card_type::Supertype::Basic
                            }
                        )));
                    }
                    other => panic!("Expected typed land filter, got {:?}", other),
                }
            }
            other => panic!("Expected SearchLibrary, got {:?}", other),
        }

        let override_search = ability
            .sub_ability
            .as_ref()
            .expect("expected override search");
        assert_eq!(
            override_search.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        );
        match &*override_search.effect {
            Effect::SearchLibrary {
                filter,
                count,
                reveal,
                ..
            } => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
                assert!(*reveal);
                match filter {
                    TargetFilter::Or { filters } => {
                        assert_eq!(filters.len(), 2);
                        assert!(filters.iter().any(|filter| matches!(
                            filter,
                            TargetFilter::Typed(tf)
                                if tf.type_filters.contains(&TypeFilter::Creature)
                        )));
                        assert!(filters.iter().any(|filter| matches!(
                            filter,
                            TargetFilter::Typed(tf)
                                if tf.type_filters.contains(&TypeFilter::Land)
                        )));
                    }
                    other => panic!("Expected creature-or-land filter, got {:?}", other),
                }
            }
            other => panic!("Expected override SearchLibrary, got {:?}", other),
        }

        let to_hand = override_search
            .else_ability
            .as_ref()
            .expect("expected shared continuation");
        assert!(matches!(
            *to_hand.effect,
            Effect::ChangeZone {
                destination: Zone::Hand,
                ..
            }
        ));
        let shuffle = to_hand.sub_ability.as_ref().expect("expected shuffle");
        assert!(matches!(*shuffle.effect, Effect::Shuffle { .. }));
        assert!(!contains_reveal_top(ability));
    }

    // ── Time Travel (CR 701.56) ──

    #[test]
    fn time_travel_standalone_spell() {
        let r = parse(
            "Time travel.\nDraw a card.",
            "Wibbly-Wobbly, Timey-Wimey",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 2);
        assert!(matches!(*r.abilities[0].effect, Effect::TimeTravel));
        assert!(matches!(*r.abilities[1].effect, Effect::Draw { .. }));
    }

    #[test]
    fn time_travel_in_trigger() {
        let r = parse(
            "Whenever this creature deals combat damage to a player, time travel.",
            "Time Beetle",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1);
        let exec = r.triggers[0].execute.as_ref().unwrap();
        assert!(matches!(*exec.effect, Effect::TimeTravel));
    }

    #[test]
    fn time_travel_activated_ability() {
        let r = parse(
            "{4}, {T}: Time travel. Activate only as a sorcery.",
            "Rotating Fireplace",
            &[],
            &["Artifact"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        assert!(matches!(*r.abilities[0].effect, Effect::TimeTravel));
        assert!(r.abilities[0].sorcery_speed);
    }

    // ── Exert (CR 701.43d) ──

    #[test]
    fn exert_with_when_you_do_pump() {
        let r = parse(
            "You may exert this creature as it attacks. When you do, it gets +1/+3 and gains lifelink until end of turn.",
            "Glory-Bound Initiate",
            &[],
            &["Creature"],
            &["Human", "Warrior"],
        );
        assert_eq!(r.triggers.len(), 1);
        assert_eq!(r.triggers[0].mode, TriggerMode::Exerted);
        let exec = r.triggers[0].execute.as_ref().unwrap();
        // The "gets +1/+3 and gains lifelink" is a continuous modification (GenericEffect),
        // not a direct Pump — parse_effect_chain handles this composite pattern.
        assert!(
            matches!(
                *exec.effect,
                Effect::GenericEffect { .. } | Effect::Pump { .. }
            ),
            "expected GenericEffect or Pump, got {:?}",
            exec.effect
        );
    }

    #[test]
    fn exert_standalone_line() {
        let r = parse(
            "You may exert this creature as it attacks.\nWhenever you exert a creature, you may discard a card. If you do, draw a card.",
            "Battlefield Scavenger",
            &[],
            &["Creature"],
            &[],
        );
        // Standalone exert line produces no output (trigger is separate)
        assert!(r.abilities.is_empty());
        assert_eq!(r.triggers.len(), 1);
        assert_eq!(r.triggers[0].mode, TriggerMode::Exerted);
        assert_eq!(r.triggers[0].valid_target, Some(TargetFilter::Controller));
        assert!(r.triggers[0].valid_card.is_some());
    }

    #[test]
    fn exert_with_card_name() {
        let r = parse(
            "You may exert Anep as it attacks. When you do, exile the top two cards of your library. Until the end of your next turn, you may play those cards.",
            "Anep, Vizier of Hazoret",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1);
        assert_eq!(r.triggers[0].mode, TriggerMode::Exerted);
    }

    #[test]
    fn exert_conditional() {
        let r = parse(
            "If this creature hasn't been exerted this turn, you may exert it as it attacks. When you do, untap all other creatures you control and after this phase, there is an additional combat phase.",
            "Combat Celebrant",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1);
        assert_eq!(r.triggers[0].mode, TriggerMode::Exerted);
    }

    // ── Leveler activated abilities (CR 711.2a + CR 711.2b) ──

    #[test]
    fn leveler_activated_abilities_get_level_counter_range() {
        let r = parse(
            "Level up {3}{R}\nLEVEL 1-2\n2/3\n{T}: This creature deals 1 damage to any target.\nLEVEL 3+\n2/4\n{T}: This creature deals 3 damage to any target.",
            "Brimstone Mage",
            &[Keyword::LevelUp(ManaCost::generic(0))],
            &["Creature"],
            &[],
        );
        // Two level-gated activated abilities
        let level_gated: Vec<_> = r
            .abilities
            .iter()
            .filter(|a| {
                a.activation_restrictions
                    .iter()
                    .any(|ar| matches!(ar, ActivationRestriction::LevelCounterRange { .. }))
            })
            .collect();
        assert_eq!(level_gated.len(), 2);

        // First level-gated ability: LEVEL 1-2
        assert_eq!(level_gated[0].kind, AbilityKind::Activated);
        assert!(level_gated[0].activation_restrictions.contains(
            &ActivationRestriction::LevelCounterRange {
                minimum: 1,
                maximum: Some(2),
            }
        ));

        // Second level-gated ability: LEVEL 3+
        assert_eq!(level_gated[1].kind, AbilityKind::Activated);
        assert!(level_gated[1].activation_restrictions.contains(
            &ActivationRestriction::LevelCounterRange {
                minimum: 3,
                maximum: None,
            }
        ));

        // No spurious triggers
        assert_eq!(r.triggers.len(), 0);
    }

    #[test]
    fn fatal_push_full_composition() {
        use crate::types::ability::AbilityCondition;

        // CR 608.2c: Two-line "instead" composition with ability word + MV conditions.
        // Base: Destroy target creature if MV ≤ 2
        // Revolt: Destroy that creature if MV ≤ 4 instead (when revolt active)
        let r = parse_oracle_text(
            "Destroy target creature if it has mana value 2 or less.\nRevolt \u{2014} Destroy that creature if it has mana value 4 or less instead if a permanent left the battlefield under your control this turn.",
            "Fatal Push",
            &[],
            &["Instant".to_string()],
            &[],
        );
        assert_eq!(
            r.abilities.len(),
            1,
            "should be ONE ability (instead composition)"
        );
        let ability = &r.abilities[0];

        // Base condition: TargetMatchesFilter with CmcLE(2)
        match &ability.condition {
            Some(AbilityCondition::TargetMatchesFilter { filter, .. }) => {
                if let TargetFilter::Typed(tf) = filter {
                    assert!(
                        tf.properties.iter().any(|p| matches!(
                            p,
                            FilterProp::Cmc {
                                comparator: Comparator::LE,
                                value: QuantityExpr::Fixed { value: 2 }
                            }
                        )),
                        "base should have CmcLE(2), got: {:?}",
                        tf.properties
                    );
                } else {
                    panic!("expected Typed filter on base condition");
                }
            }
            other => panic!("expected TargetMatchesFilter on base, got: {other:?}"),
        }

        // Sub-ability: ConditionInstead with And([Revolt, CmcLE(4)])
        let sub = ability
            .sub_ability
            .as_ref()
            .expect("should have sub_ability");
        match &sub.condition {
            Some(AbilityCondition::ConditionInstead { inner }) => match inner.as_ref() {
                AbilityCondition::And { conditions } => {
                    assert_eq!(conditions.len(), 2, "And should have 2 conditions");
                    // First: Revolt (QuantityCheck on zone-change count)
                    assert!(
                        matches!(&conditions[0], AbilityCondition::QuantityCheck { .. }),
                        "first condition should be QuantityCheck (revolt)"
                    );
                    // Second: CmcLE(4)
                    match &conditions[1] {
                        AbilityCondition::TargetMatchesFilter { filter, .. } => {
                            if let TargetFilter::Typed(tf) = filter {
                                assert!(
                                    tf.properties.iter().any(|p| matches!(
                                        p,
                                        FilterProp::Cmc {
                                            comparator: Comparator::LE,
                                            value: QuantityExpr::Fixed { value: 4 }
                                        }
                                    )),
                                    "revolt sub should have CmcLE(4), got: {:?}",
                                    tf.properties
                                );
                            } else {
                                panic!("expected Typed filter on revolt sub");
                            }
                        }
                        other => panic!("expected TargetMatchesFilter in And[1], got: {other:?}"),
                    }
                }
                other => panic!("expected And inside ConditionInstead, got: {other:?}"),
            },
            other => panic!("expected ConditionInstead on sub, got: {other:?}"),
        }
    }

    #[test]
    fn ferocious_ability_word_applies_power_condition_to_spell_effect() {
        use crate::types::ability::{AbilityCondition, PtStat, PtValueScope, QuantityRef};

        let r = parse_oracle_text(
            "You gain 5 life.\nFerocious \u{2014} You gain 10 life instead if you control a creature with power 4 or greater.",
            "Feed the Clan",
            &[],
            &["Instant".to_string()],
            &[],
        );
        assert!(r.parse_warnings.iter().all(|warning| {
            warning.to_string().split_whitespace().next() != Some("Swallow:Condition_If")
        }));
        let base = r
            .abilities
            .first()
            .expect("expected base gain-life ability");
        let ferocious = base
            .sub_ability
            .as_ref()
            .expect("expected conditional ferocious branch");
        assert!(matches!(
            *ferocious.effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 10 },
                ..
            }
        ));
        let Some(AbilityCondition::ConditionInstead { inner }) = ferocious.condition.as_ref()
        else {
            panic!(
                "expected ferocious ConditionInstead, got {:?}",
                ferocious.condition
            );
        };
        let AbilityCondition::QuantityCheck {
            lhs,
            comparator,
            rhs,
        } = inner.as_ref()
        else {
            panic!("expected ferocious QuantityCheck, got {inner:?}");
        };
        assert_eq!(*comparator, Comparator::GE);
        assert_eq!(*rhs, QuantityExpr::Fixed { value: 1 });
        let QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } = lhs
        else {
            panic!("expected ObjectCount lhs, got {lhs:?}");
        };
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected typed creature filter");
        };
        assert_eq!(filter.controller, Some(ControllerRef::You));
        assert!(filter.properties.contains(&FilterProp::PtComparison {
            stat: PtStat::Power,
            scope: PtValueScope::Current,
            comparator: Comparator::GE,
            value: QuantityExpr::Fixed { value: 4 },
        }));
    }

    #[test]
    fn instead_if_condition_composes_without_ability_word_mapping() {
        use crate::types::ability::{AbilityCondition, QuantityRef};

        let r = parse_oracle_text(
            "Brimstone Volley deals 3 damage to any target.\nMorbid \u{2014} Brimstone Volley deals 5 damage instead if a creature died this turn.",
            "Brimstone Volley",
            &[],
            &["Instant".to_string()],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let sub = r.abilities[0]
            .sub_ability
            .as_ref()
            .expect("instead branch should be attached to base ability");
        match &sub.condition {
            Some(AbilityCondition::ConditionInstead { inner }) => {
                assert!(matches!(
                    inner.as_ref(),
                    AbilityCondition::QuantityCheck {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::ZoneChangeCountThisTurn {
                                from: Some(Zone::Battlefield),
                                to: Some(Zone::Graveyard),
                                ..
                            },
                        },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 1 },
                    }
                ));
            }
            other => panic!("expected ConditionInstead quantity check, got {other:?}"),
        }
    }

    #[test]
    fn leading_conditional_instead_composes_self_replacement() {
        use crate::types::ability::{AbilityCondition, QuantityRef};

        // CR 614.15: "<ability word> — If <condition>, instead <effect>" — the
        // leading-conditional word order (condition FIRST, then "instead").
        // Arrow Storm: raid-gated self-replacement. The base 4-damage ability
        // becomes the fallback; the alternative 5-damage chain is gated by a
        // `ConditionInstead { AttackedThisTurn >= 1 }`.
        let r = parse_oracle_text(
            "Arrow Storm deals 4 damage to any target.\nRaid \u{2014} If you attacked this turn, instead Arrow Storm deals 5 damage to that permanent or player and the damage can't be prevented.",
            "Arrow Storm",
            &[],
            &["Sorcery".to_string()],
            &[],
        );
        // The leading-conditional "instead" line must NOT leave a swallowed-clause
        // warning — the condition and the alternative effect are both captured.
        assert!(
            r.parse_warnings.iter().all(|w| {
                let kind = w.to_string();
                // allow-noncombinator: test assertion on a diagnostic-warning kind string, not Oracle-text parsing dispatch
                !kind.contains("Condition_If") && !kind.contains("Replacement_Instead")
            }),
            "leading-conditional instead should not emit swallowed-clause warnings, got: {:?}",
            r.parse_warnings
        );
        assert_eq!(r.abilities.len(), 1, "should compose into ONE base ability");
        let base = &r.abilities[0];
        assert!(
            matches!(
                *base.effect,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 4 },
                    ..
                }
            ),
            "base should deal 4, got: {:?}",
            base.effect
        );
        let sub = base
            .sub_ability
            .as_ref()
            .expect("expected conditional self-replacement sub-ability");
        let Some(AbilityCondition::ConditionInstead { inner }) = sub.condition.as_ref() else {
            panic!("expected ConditionInstead on sub, got: {:?}", sub.condition);
        };
        assert!(
            matches!(
                inner.as_ref(),
                AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::AttackedThisTurn,
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                }
            ),
            "expected AttackedThisTurn >= 1 inside ConditionInstead, got: {inner:?}"
        );
    }

    #[test]
    fn leading_conditional_instead_threshold_graveyard_count() {
        use crate::types::ability::{AbilityCondition, QuantityRef};

        // CR 614.15: Lightning Surge — threshold-gated self-replacement using the
        // leading-conditional word order with a graveyard-count condition.
        let r = parse_oracle_text(
            "Lightning Surge deals 4 damage to any target.\nThreshold \u{2014} If there are seven or more cards in your graveyard, instead Lightning Surge deals 6 damage to that permanent or player and the damage can't be prevented.",
            "Lightning Surge",
            &[],
            &["Instant".to_string()],
            &[],
        );
        assert!(
            r.parse_warnings.iter().all(|w| {
                let kind = w.to_string();
                // allow-noncombinator: test assertion on a diagnostic-warning kind string, not Oracle-text parsing dispatch
                !kind.contains("Condition_If") && !kind.contains("Replacement_Instead")
            }),
            "threshold instead should not emit swallowed-clause warnings, got: {:?}",
            r.parse_warnings
        );
        assert_eq!(r.abilities.len(), 1);
        let sub = r.abilities[0]
            .sub_ability
            .as_ref()
            .expect("expected threshold self-replacement sub-ability");
        let Some(AbilityCondition::ConditionInstead { inner }) = sub.condition.as_ref() else {
            panic!("expected ConditionInstead on sub, got: {:?}", sub.condition);
        };
        assert!(
            matches!(
                inner.as_ref(),
                AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::GraveyardSize { .. },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 7 },
                }
            ),
            "expected GraveyardSize >= 7 inside ConditionInstead, got: {inner:?}"
        );
    }

    #[test]
    fn quantum_riddler_draw_line_parses_as_replacement_not_static() {
        let result = parse(
            "As long as you have one or fewer cards in hand, if you would draw one or more cards, you draw that many cards plus one instead.",
            "Quantum Riddler",
            &[],
            &["Creature"],
            &["Sphinx"],
        );

        assert_eq!(
            result.statics.len(),
            0,
            "line should not fall back to static parsing"
        );
        assert_eq!(
            result.replacements.len(),
            1,
            "line should parse as one replacement"
        );
        assert!(matches!(
            result.replacements[0].condition,
            Some(ReplacementCondition::OnlyIfQuantity { .. })
        ));
        assert_eq!(result.replacements[0].event, ReplacementEvent::Draw);
    }

    /// CR 205.3a: "[Subtype] [CoreType]" subject-predicate patterns like
    /// "Wizard creatures gain flying until end of turn" — the subtype+type compound
    /// must be fully consumed by parse_type_phrase so the subject-predicate parser
    /// can extract the filter.
    #[test]
    fn test_subtype_creatures_gain_keyword() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{ContinuousModification, Duration, TargetFilter, TypeFilter};
        use crate::types::keywords::Keyword;

        let def = parse_effect_chain(
            "wizard creatures gain flying until end of turn",
            crate::types::ability::AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::GenericEffect {
                static_abilities,
                duration,
                ..
            } => {
                assert_eq!(
                    *duration,
                    Some(Duration::UntilEndOfTurn),
                    "duration should be UntilEndOfTurn"
                );
                assert_eq!(static_abilities.len(), 1);
                let sa = &static_abilities[0];
                // Affected filter should include both Creature and Subtype("Wizard")
                if let Some(TargetFilter::Typed(tf)) = &sa.affected {
                    assert!(
                        tf.type_filters
                            .contains(&TypeFilter::Subtype("Wizard".to_string())),
                        "should contain Wizard subtype, got {:?}",
                        tf.type_filters
                    );
                    assert!(
                        tf.type_filters.contains(&TypeFilter::Creature),
                        "should contain Creature type, got {:?}",
                        tf.type_filters
                    );
                } else {
                    panic!("expected Typed filter, got {:?}", sa.affected);
                }
                assert!(sa.modifications.iter().any(|m| matches!(
                    m,
                    ContinuousModification::AddKeyword { keyword }
                        if *keyword == Keyword::Flying
                )));
            }
            other => panic!("expected GenericEffect, got {:?}", other),
        }
    }

    /// "Goblin creatures get +1/+1 until end of turn" — same [Subtype] [CoreType] pattern
    /// with a pump predicate instead of keyword grant.
    #[test]
    fn test_subtype_creatures_get_pump() {
        use crate::parser::oracle_effect::parse_effect_chain;

        let def = parse_effect_chain(
            "goblin creatures get +1/+1 until end of turn",
            crate::types::ability::AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::PumpAll { .. } => {}
            other => panic!("expected PumpAll, got {:?}", other),
        }
    }

    // CR 201.3 / CR 113.6: Petrified Hamlet — full four-line parse must
    // produce a ChangesZone trigger (choose a land card name, persist=true),
    // a continuous static granting `{T}: Add {C}.` to every land whose name
    // matches the chosen name, the CantBeActivated static on
    // `HasChosenName` sources, and the card's own `{T}: Add {C}.`
    // activated mana ability — zero Unimplemented ambiances.
    #[test]
    fn petrified_hamlet_full_parse() {
        use crate::types::ability::{ChoiceType, Effect};
        let text = "When this land enters, choose a land card name.\n\
                    Activated abilities of sources with the chosen name can't be activated unless they're mana abilities.\n\
                    Lands with the chosen name have \"{T}: Add {C}.\"\n\
                    {T}: Add {C}.";
        let r = parse(text, "Petrified Hamlet", &[], &["Land"], &[]);

        // No Unimplemented anywhere.
        for a in r.abilities.iter() {
            assert!(
                !matches!(*a.effect, Effect::Unimplemented { .. }),
                "ability Unimplemented: {:?}",
                a
            );
        }
        for t in &r.triggers {
            let exec = t.execute.as_ref().expect("trigger execute");
            assert!(
                !matches!(*exec.effect, Effect::Unimplemented { .. }),
                "trigger Unimplemented: {:?}",
                t
            );
        }

        // Trigger: choose-a-land-card-name with persist=true.
        assert_eq!(r.triggers.len(), 1);
        let trig = &r.triggers[0];
        assert_eq!(trig.mode, TriggerMode::ChangesZone);
        assert_eq!(trig.destination, Some(Zone::Battlefield));
        let trig_exec = trig.execute.as_ref().unwrap();
        assert!(
            matches!(
                *trig_exec.effect,
                Effect::Choose {
                    choice_type: ChoiceType::CardName,
                    persist: true,
                }
            ),
            "expected Choose{{CardName, persist:true}}, got {:?}",
            trig_exec.effect
        );

        // One activated mana ability ({T}: Add {C}).
        let mana_abils: Vec<_> = r
            .abilities
            .iter()
            .filter(|a| matches!(*a.effect, Effect::Mana { .. }))
            .collect();
        assert_eq!(mana_abils.len(), 1);

        // Two statics: CantBeActivated (HasChosenName) + continuous grant on
        // Lands-with-the-chosen-name.
        assert_eq!(r.statics.len(), 2);
        let has_cant_be_activated = r
            .statics
            .iter()
            .any(|s| matches!(&s.mode, StaticMode::CantBeActivated { .. }));
        assert!(has_cant_be_activated, "expected CantBeActivated static");

        let grant_static = r
            .statics
            .iter()
            .find(|s| matches!(&s.mode, StaticMode::Continuous))
            .expect("expected continuous grant static");
        match &grant_static.affected {
            Some(TargetFilter::And { filters }) => {
                assert_eq!(filters.len(), 2);
                assert_eq!(filters[1], TargetFilter::HasChosenName);
            }
            other => {
                panic!("expected And[Typed(Land), HasChosenName] for grant static, got {other:?}")
            }
        }
        assert_eq!(grant_static.modifications.len(), 1);
        assert!(matches!(
            &grant_static.modifications[0],
            ContinuousModification::GrantAbility { .. }
        ));
    }

    // CR 608.2 + CR 107.1a + CR 701.16a: Pox Plague — the "Each player loses
    // half their life, then discards half the cards in their hand, then
    // sacrifices half the permanents they control of their choice. Round down
    // each time." chain exercises all four fixes landed in the punisher-chain
    // commit:
    //   A. player_scope rewrite: `their life` / `their hand` → LifeTotal /
    //      HandSize so per-player iteration resolves against the scoped
    //      player, not the empty targets list or original controller.
    //   B. half-rounded inner: `half the cards in their hand` parses through
    //      the new `parse_cards_in_possessive_zone` combinator, producing a
    //      DivideRounded count rather than collapsing to 1.
    //   C. Sacrifice.count: a dynamic count lifted from
    //      `half the permanents they control` into the new count field, and
    //      the embedded ObjectCount filter lifted into `Sacrifice.target` so
    //      eligibility matches the same set the count was computed against.
    //   D. trailing rounding: `Round down each time` consumed by
    //      `strip_trailing_rounding_annotation` and back-applied through
    //      `rewrite_rounding_mode` — the chunk does not become an
    //      Unimplemented effect.
    #[test]
    fn pox_plague_full_parse() {
        use crate::types::ability::{QuantityExpr, QuantityRef, RoundingMode};

        let r = parse(
            "Each player loses half their life, then discards half the cards in their hand, then sacrifices half the permanents they control of their choice. Round down each time.",
            "Pox Plague",
            &[],
            &["Sorcery"],
            &[],
        );

        // A single top-level ability with player_scope: All.
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert!(
            matches!(
                ability.player_scope,
                Some(crate::types::ability::PlayerFilter::All)
            ),
            "expected player_scope All, got {:?}",
            ability.player_scope
        );

        // Fix A: LoseLife amount uses per-player-scoped LifeTotal.
        match &*ability.effect {
            Effect::LoseLife { amount, .. } => match amount {
                QuantityExpr::DivideRounded {
                    inner,
                    divisor,
                    rounding,
                } => {
                    assert_eq!(*divisor, 2);
                    assert_eq!(*rounding, RoundingMode::Down);
                    assert!(
                        matches!(
                            **inner,
                            QuantityExpr::Ref {
                                qty: QuantityRef::LifeTotal {
                                    player: crate::types::ability::PlayerScope::ScopedPlayer
                                }
                            }
                        ),
                        "expected LifeTotal, got {inner:?}"
                    );
                }
                other => panic!("expected DivideRounded LoseLife amount, got {other:?}"),
            },
            other => panic!("expected LoseLife top-level, got {other:?}"),
        }

        // Fix B + A: Discard count uses DivideRounded(HandSize) for the scoped player.
        let discard = ability.sub_ability.as_ref().expect("discard sub_ability");
        match &*discard.effect {
            Effect::Discard { count, .. } => match count {
                QuantityExpr::DivideRounded {
                    inner,
                    divisor,
                    rounding,
                } => {
                    assert_eq!(*divisor, 2);
                    assert_eq!(*rounding, RoundingMode::Down);
                    assert!(
                        matches!(
                            **inner,
                            QuantityExpr::Ref {
                                qty: QuantityRef::HandSize {
                                    player: crate::types::ability::PlayerScope::ScopedPlayer
                                }
                            }
                        ),
                        "expected HandSize, got {inner:?}"
                    );
                }
                other => panic!("expected DivideRounded Discard count, got {other:?}"),
            },
            other => panic!("expected Discard mid-chain, got {other:?}"),
        }

        // Fix C: Sacrifice carries DivideRounded(ObjectCount{Permanent,you-control})
        // as count, and the same Typed filter lifted into target.
        let sacrifice = discard.sub_ability.as_ref().expect("sacrifice sub_ability");
        match &*sacrifice.effect {
            Effect::Sacrifice { target, count, .. } => {
                assert!(!count.is_up_to(), "expected non-UpTo sacrifice count");
                match count {
                    QuantityExpr::DivideRounded {
                        inner,
                        divisor,
                        rounding,
                    } => {
                        assert_eq!(*divisor, 2);
                        assert_eq!(*rounding, RoundingMode::Down);
                        match &**inner {
                            QuantityExpr::Ref {
                                qty: QuantityRef::ObjectCount { filter },
                            } => match filter {
                                TargetFilter::Typed(tf) => {
                                    assert!(tf.type_filters.contains(&TypeFilter::Permanent));
                                }
                                other => panic!("expected Typed filter, got {other:?}"),
                            },
                            other => panic!("expected ObjectCount inner, got {other:?}"),
                        }
                    }
                    other => panic!("expected DivideRounded Sacrifice count, got {other:?}"),
                }
                match target {
                    TargetFilter::Typed(tf) => {
                        assert!(tf.type_filters.contains(&TypeFilter::Permanent));
                    }
                    other => panic!("expected Typed target lifted from count, got {other:?}"),
                }
            }
            other => panic!("expected Sacrifice tail, got {other:?}"),
        }

        // Fix D: "Round down each time" consumed — no Unimplemented anywhere.
        fn walk_no_unimpl(def: &crate::types::ability::AbilityDefinition) {
            assert!(
                !matches!(*def.effect, Effect::Unimplemented { .. }),
                "Unimplemented in Pox Plague chain: {:?}",
                def.effect
            );
            if let Some(sub) = def.sub_ability.as_ref() {
                walk_no_unimpl(sub);
            }
        }
        walk_no_unimpl(ability);
    }

    /// CR 702.94a + CR 400.3: End-to-end reproduction of Sliver Weftwinder's
    /// hand-grant line through the full `parse_oracle_text` pipeline.
    #[test]
    fn hand_grant_reaches_statics_through_full_pipeline() {
        let oracle = "Sliver cards in your hand have warp {3}.";
        let parsed = parse(oracle, "Sliver Weftwinder", &[], &["Creature"], &["Sliver"]);
        let hand_grant = parsed.statics.iter().find(|s| {
            s.mode == StaticMode::Continuous
                && s.affected
                    .as_ref()
                    .map(|a| a.extract_in_zone() == Some(Zone::Hand))
                    .unwrap_or(false)
        });
        assert!(
            hand_grant.is_some(),
            "hand-zone static should reach result.statics, got statics={:?}, abilities={:?}",
            parsed.statics,
            parsed.abilities,
        );
    }

    #[test]
    fn prevention_followup_if_this_way_does_not_emit_condition_warning() {
        let oracle = "Prevent the next X damage that would be dealt to target creature this turn, where X is your devotion to white. If damage is prevented this way, Acolyte's Reward deals that much damage to any target.";
        let parsed = parse(oracle, "Acolyte's Reward", &[], &["Instant"], &[]);

        assert!(
            parsed.parse_warnings.iter().all(|warning| warning
                .to_string()
                .split_whitespace()
                .next()
                != Some("Swallow:Condition_If")),
            "unexpected condition warning: {:?}",
            parsed.parse_warnings
        );

        let ability = parsed
            .abilities
            .first()
            .expect("expected prevention spell ability");
        assert!(matches!(*ability.effect, Effect::PreventDamage { .. }));
        assert!(
            ability.sub_ability.is_some(),
            "expected prevented-this-way follow-up sub-ability"
        );
    }

    #[test]
    fn may_cost_decline_if_you_dont_does_not_emit_condition_or_optional_warning() {
        let oracle = "({T}: Add {B} or {R}.)\nAs this land enters, you may pay 2 life. If you don't, it enters tapped.";
        let parsed = parse(
            oracle,
            "Blood Crypt",
            &[],
            &["Land"],
            &["Swamp", "Mountain"],
        );

        assert!(
            parsed.parse_warnings.iter().all(|warning| {
                let label = warning.to_string();
                let label = label.split_whitespace().next();
                label != Some("Swallow:Condition_If") && label != Some("Swallow:Optional_YouMay")
            }),
            "unexpected replacement choice warning: {:?}",
            parsed.parse_warnings
        );
        assert_eq!(parsed.replacements.len(), 1);
    }

    #[test]
    fn granted_trigger_you_may_draw_does_not_emit_optional_warning() {
        let oracle = "Enchant creature\nEnchanted creature gets +1/+1 and has \"Whenever this creature deals combat damage to a player, you may draw a card.\"";
        let parsed = parse(
            oracle,
            "Curious Obsession",
            &[],
            &["Enchantment"],
            &["Aura"],
        );

        assert!(
            parsed.parse_warnings.iter().all(|warning| warning
                .to_string()
                .split_whitespace()
                .next()
                != Some("Swallow:Optional_YouMay")),
            "unexpected optional warning: {:?}",
            parsed.parse_warnings
        );
        assert!(
            parsed.statics.iter().any(|static_def| {
                static_def.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        ContinuousModification::GrantTrigger { trigger }
                            if trigger.optional
                    )
                })
            }),
            "expected optional granted trigger, got statics={:?}",
            parsed.statics
        );
    }

    #[test]
    fn emblem_trigger_you_may_draw_does_not_emit_optional_warning() {
        let oracle =
            "[-6]: You get an emblem with \"Whenever a land you control enters, you may draw a card.\"";
        let parsed = parse(
            oracle,
            "Nissa, Vital Force",
            &[],
            &["Planeswalker"],
            &["Nissa"],
        );

        assert!(
            parsed.parse_warnings.iter().all(|warning| warning
                .to_string()
                .split_whitespace()
                .next()
                != Some("Swallow:Optional_YouMay")),
            "unexpected optional warning: {:?}",
            parsed.parse_warnings
        );
        assert!(
            parsed.abilities.iter().any(|ability| {
                matches!(
                    &*ability.effect,
                    Effect::CreateEmblem { triggers, .. }
                        if triggers.iter().any(|trigger| trigger.optional)
                )
            }),
            "expected emblem with optional trigger, got abilities={:?}",
            parsed.abilities
        );
    }

    #[test]
    fn must_block_if_able_static_does_not_emit_condition_warning() {
        let oracle = "Defender\nThis creature blocks each combat if able.";
        let parsed = parse(
            oracle,
            "Razorgrass Screen",
            &[Keyword::Defender],
            &["Artifact", "Creature"],
            &["Wall"],
        );

        assert!(
            parsed.parse_warnings.iter().all(|warning| warning
                .to_string()
                .split_whitespace()
                .next()
                != Some("Swallow:Condition_If")),
            "unexpected condition warning: {:?}",
            parsed.parse_warnings
        );
        assert!(parsed
            .statics
            .iter()
            .any(|static_def| static_def.mode == StaticMode::MustBlock));
    }

    #[test]
    fn temporary_comma_grant_must_attack_if_able_does_not_emit_condition_warning() {
        let oracle = "Damage can't be prevented this turn.\nCreatures you control have double strike, trample, and must attack if able until end of turn.";
        let parsed = parse(oracle, "Math is for Blockers", &[], &["Sorcery"], &[]);

        assert!(
            parsed.parse_warnings.iter().all(|warning| warning
                .to_string()
                .split_whitespace()
                .next()
                != Some("Swallow:Condition_If")),
            "unexpected condition warning: {:?}",
            parsed.parse_warnings
        );
        assert!(parsed.abilities.iter().any(|ability| {
            matches!(
                &*ability.effect,
                Effect::GenericEffect { static_abilities, .. }
                    if static_abilities
                        .iter()
                        .any(|static_def| static_def.mode == StaticMode::MustAttack)
            )
        }));
    }

    #[test]
    fn city_blessing_activation_restriction_does_not_emit_condition_warning() {
        let oracle = "Ascend (If you control ten or more permanents, you get the city's blessing for the rest of the game.)\n{T}: Add {C}.\n{5}, {T}: Draw a card. Activate only if you have the city's blessing.";
        let parsed = parse(oracle, "Arch of Orazca", &[], &["Land"], &[]);

        assert!(
            parsed.parse_warnings.iter().all(|warning| warning
                .to_string()
                .split_whitespace()
                .next()
                != Some("Swallow:Condition_If")),
            "unexpected condition warning: {:?}",
            parsed.parse_warnings
        );
        let draw_ability = parsed
            .abilities
            .iter()
            .find(|ability| matches!(*ability.effect, Effect::Draw { .. }))
            .expect("expected draw ability");
        assert!(draw_ability
            .activation_restrictions
            .iter()
            .any(|restriction| matches!(
                restriction,
                ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::HasCityBlessing)
                }
            )));
    }

    #[test]
    fn normalized_source_power_activation_restriction_does_not_emit_condition_warning() {
        let oracle = "{T}: This creature deals 4 damage to target creature. Activate only if this creature's power is 4 or greater.";
        let parsed = parse(
            oracle,
            "Bloodshot Trainee",
            &[],
            &["Creature"],
            &["Goblin", "Warrior"],
        );

        assert!(parsed.parse_warnings.is_empty());
        let damage_ability = parsed
            .abilities
            .iter()
            .find(|ability| matches!(*ability.effect, Effect::DealDamage { .. }))
            .expect("expected damage ability");
        assert!(damage_ability
            .activation_restrictions
            .iter()
            .any(|restriction| matches!(
                restriction,
                ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::SourcePowerAtLeast { minimum: 4 })
                }
            )));
    }

    #[test]
    fn instant_or_sorcery_cast_activation_restriction_does_not_emit_condition_warning() {
        let oracle = "{T}: You gain 2 life. Activate only if you've cast an instant or sorcery spell this turn.";
        let parsed = parse(oracle, "Potioner's Trove", &[], &["Artifact"], &[]);

        assert!(parsed.parse_warnings.is_empty());
        let gain_life_ability = parsed
            .abilities
            .iter()
            .find(|ability| matches!(*ability.effect, Effect::GainLife { .. }))
            .expect("expected gain-life ability");
        assert!(gain_life_ability
            .activation_restrictions
            .iter()
            .any(|restriction| matches!(
                restriction,
                ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::YouCastSpellThisTurn {
                        filter: Some(TargetFilter::Or { filters })
                    })
                } if filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(TypedFilter { type_filters, .. })
                        if type_filters == &vec![TypeFilter::Instant]
                )) && filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(TypedFilter { type_filters, .. })
                        if type_filters == &vec![TypeFilter::Sorcery]
                ))
            )));
    }

    #[test]
    fn crumbling_sanctuary_parses_as_replacement_without_swallowed_clause() {
        let parsed = parse(
            "If damage would be dealt to a player, that player exiles that many cards from the top of their library instead.",
            "Crumbling Sanctuary",
            &[],
            &["Artifact"],
            &[],
        );

        assert!(parsed.abilities.is_empty());
        assert_eq!(parsed.replacements.len(), 1);
        assert!(parsed.parse_warnings.iter().all(|warning| {
            warning.category_name() != "swallowed-clause"
                && warning.category_name() != "ignored-remainder"
        }));

        let replacement = &parsed.replacements[0];
        assert_eq!(replacement.event, ReplacementEvent::DamageDone);
        assert_eq!(
            replacement.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        );
        let execute = replacement.execute.as_ref().expect("execute present");
        assert!(matches!(
            *execute.effect,
            Effect::ExileTop {
                player: TargetFilter::PostReplacementDamageTarget,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount
                },
                face_down: false,
            }
        ));
    }

    #[test]
    fn dynamic_mana_per_color_does_not_emit_dynamic_qty_warning() {
        let oracle =
            "Vivid — {T}: For each color among permanents you control, add one mana of that color.";
        let parsed = parse(oracle, "Bloom Tender", &[], &["Creature"], &[]);

        assert!(
            parsed.parse_warnings.iter().all(|warning| warning
                .to_string()
                .split_whitespace()
                .next()
                != Some("Swallow:DynamicQty")),
            "unexpected dynamic quantity warning: {:?}",
            parsed.parse_warnings
        );

        let ability = parsed
            .abilities
            .first()
            .expect("expected parsed mana ability");
        assert!(matches!(
            &*ability.effect,
            Effect::Mana {
                produced: crate::types::ability::ManaProduction::DistinctColorsAmongPermanents { .. },
                ..
            }
        ));
    }

    #[test]
    fn source_filtered_copy_token_does_not_emit_dynamic_qty_warning() {
        let parsed = parse(
            "As this enchantment enters, choose a creature type.\nCreatures you control of the chosen type get +1/+0.\nAt the beginning of your end step, for each token you control of the chosen type that entered this turn, create a token that's a copy of it.",
            "Renewed Solidarity",
            &[],
            &["Enchantment"],
            &[],
        );

        assert!(
            parsed.parse_warnings.iter().all(|warning| warning
                .to_string()
                .split_whitespace()
                .next()
                != Some("Swallow:DynamicQty")),
            "unexpected DynamicQty warning: {:?}",
            parsed.parse_warnings
        );
    }

    #[test]
    fn trigger_persisted_type_choice_reconciles_self_chosen_type_static() {
        let parsed = parse(
            "When ~ enters, choose a creature type.\n~ is the chosen type in addition to its other types.",
            "Synthetic Relic",
            &[],
            &["Artifact"],
            &[],
        );

        assert_eq!(parsed.triggers.len(), 1);
        let static_def = parsed.statics.first().expect("expected static ability");
        assert!(static_def.modifications.iter().any(|modification| matches!(
            modification,
            ContinuousModification::AddChosenSubtype {
                kind: crate::types::ability::ChosenSubtypeKind::CreatureType
            }
        )));
    }

    #[test]
    fn choose_one_of_branch_optional_does_not_emit_you_may_warning() {
        let parsed = parse(
            "Flying\nAt the beginning of your end step, draw a card. Then each opponent faces a villainous choice — That player discards a card, or you may put a Construct, Robot, or Vehicle card from your hand onto the battlefield.",
            "Dr. Eggman",
            &[],
            &["Legendary", "Creature"],
            &["Human", "Scientist"],
        );

        assert!(
            parsed.parse_warnings.iter().all(|warning| warning
                .to_string()
                .split_whitespace()
                .next()
                != Some("Swallow:Optional_YouMay")),
            "unexpected Optional_YouMay warning: {:?}",
            parsed.parse_warnings
        );
    }

    #[test]
    fn alrund_static_sum_for_each_does_not_emit_dynamic_qty_warning() {
        let oracle = "Alrund gets +1/+1 for each card in your hand and each foretold card you own in exile.\n\
             At the beginning of your end step, choose a card type, then reveal the top two cards of your library. \
             Put all cards of the chosen type revealed this way into your hand and the rest on the bottom of your library in any order.";
        let parsed = parse(
            oracle,
            "Alrund, God of the Cosmos",
            &[],
            &["Creature"],
            &["God"],
        );

        assert_eq!(
            parsed.triggers.len(),
            1,
            "end-step trigger must remain parsed"
        );
        assert_eq!(
            parsed.triggers[0].phase,
            Some(crate::types::phase::Phase::End)
        );
        assert_eq!(parsed.statics.len(), 1, "expected Alrund static pump");
        let static_def = &parsed.statics[0];
        assert!(
            static_def
                .modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicPower { value } if matches!(value, QuantityExpr::Sum { exprs } if exprs.len() == 2))),
            "expected dynamic power Sum, got {:?}",
            static_def.modifications
        );
        assert!(
            static_def.modifications.iter().all(|m| !matches!(
                m,
                ContinuousModification::AddPower { .. }
                    | ContinuousModification::AddToughness { .. }
            )),
            "must not emit fixed P/T mods: {:?}",
            static_def.modifications
        );
        assert!(
            parsed.parse_warnings.iter().all(|warning| warning
                .to_string()
                .split_whitespace()
                .next()
                != Some("Swallow:DynamicQty")),
            "unexpected DynamicQty warning: {:?}",
            parsed.parse_warnings
        );
    }

    #[test]
    fn coat_of_arms_velis_vel_static_shared_type_no_dynamic_qty_warning() {
        for (name, types, subtypes, oracle) in [
            (
                "Coat of Arms",
                &["Artifact"][..],
                &[][..],
                "Each creature gets +1/+1 for each other creature on the battlefield that shares at least one creature type with it. (For example, if two Goblin Warriors and a Goblin Shaman are on the battlefield, each gets +2/+2.)",
            ),
            (
                "Velis Vel",
                &["Plane"][..],
                &[][..],
                "Each creature gets +1/+1 for each other creature on the battlefield that shares at least one creature type with it. (For example, if two Elemental Shamans and an Elemental Spirit are on the battlefield, each gets +2/+2.)\nWhenever chaos ensues, target creature gains all creature types until end of turn.",
            ),
        ] {
            let parsed = parse(oracle, name, &[], types, subtypes);
            assert!(
                parsed
                    .parse_warnings
                    .iter()
                    .all(|warning| warning.to_string().split_whitespace().next() != Some("Swallow:DynamicQty")),
                "unexpected DynamicQty warning for {name}: {:?}",
                parsed.parse_warnings
            );

            let mut matching_static = None;
            for static_def in &parsed.statics {
                if static_def.affected == Some(TargetFilter::Typed(TypedFilter::creature())) {
                    matching_static = Some(static_def);
                    break;
                }
            }
            let static_def = matching_static.expect("expected global creature static");
            let expected = QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        controller: None,
                        properties: vec![
                            FilterProp::Another,
                            FilterProp::SharesQuality {
                                quality: SharedQuality::CreatureType,
                                reference: Some(Box::new(TargetFilter::ParentTarget)),
                                relation: SharedQualityRelation::Shares,
                            },
                        ],
                    }),
                },
            };

            assert!(
                static_def.modifications.iter().any(|m| matches!(
                    m,
                    ContinuousModification::AddDynamicPower { value } if value == &expected
                )),
                "expected dynamic power for {name}, got {:?}",
                static_def.modifications
            );
            assert!(
                static_def.modifications.iter().any(|m| matches!(
                    m,
                    ContinuousModification::AddDynamicToughness { value } if value == &expected
                )),
                "expected dynamic toughness for {name}, got {:?}",
                static_def.modifications
            );
            assert!(
                static_def.modifications.iter().all(|m| !matches!(
                    m,
                    ContinuousModification::AddPower { .. }
                        | ContinuousModification::AddToughness { .. }
                )),
                "must not emit fixed P/T mods for {name}: {:?}",
                static_def.modifications
            );
        }
    }

    #[test]
    fn gauntlets_treefolk_umbra_assign_damage_from_toughness_no_dynamic_qty_warning() {
        for (name, oracle) in [
            (
                "Gauntlets of Light",
                "Enchant creature\nEnchanted creature gets +0/+2 and assigns combat damage equal to its toughness rather than its power.\nEnchanted creature has \"{2}{W}: Untap this creature.\"",
            ),
            (
                "Treefolk Umbra",
                "Enchant creature\nEnchanted creature gets +0/+2 and assigns combat damage equal to its toughness rather than its power.\nUmbra armor",
            ),
        ] {
            let parsed = parse(oracle, name, &[], &["Enchantment"], &["Aura"]);
            assert!(
                parsed
                    .parse_warnings
                    .iter()
                    .all(|warning| {
                        let s = warning.to_string();
                        !matches!(
                            s.split_whitespace().next(),
                            Some("Swallow:DynamicQty" | "Swallow:Condition_AsLongAs")
                        )
                    }),
                "unexpected toughness-damage warning for {name}: {:?}",
                parsed.parse_warnings
            );

            let static_def = parsed
                .statics
                .iter()
                .find(|static_def| {
                    static_def.affected
                        == Some(TargetFilter::Typed(
                            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
                        ))
                        && static_def
                            .modifications
                            .contains(&ContinuousModification::AddToughness { value: 2 })
                })
                .expect("expected enchanted creature +0/+2 static");
            assert!(static_def
                .modifications
                .contains(&ContinuousModification::AssignDamageFromToughness));
        }
    }

    #[test]
    fn attached_conditional_toughness_damage_cards_no_dynamic_qty_warning() {
        for (name, types, subtypes, expected_props, oracle) in [
            (
                "Bark of Doran",
                &["Artifact"][..],
                &["Equipment"][..],
                vec![FilterProp::EquippedBy, FilterProp::ToughnessGTPower],
                "Equipped creature gets +0/+1.\nAs long as equipped creature's toughness is greater than its power, it assigns combat damage equal to its toughness rather than its power.\nEquip {1}",
            ),
            (
                "Solid Footing",
                &["Enchantment"][..],
                &["Aura"][..],
                vec![
                    FilterProp::EnchantedBy,
                    FilterProp::WithKeyword {
                        value: Keyword::Vigilance,
                    },
                ],
                "Flash\nEnchant creature\nEnchanted creature gets +1/+1.\nAs long as enchanted creature has vigilance, it assigns combat damage equal to its toughness rather than its power.",
            ),
        ] {
            let parsed = parse(oracle, name, &[], types, subtypes);
            assert!(
                parsed
                    .parse_warnings
                    .iter()
                    .all(|warning| {
                        let s = warning.to_string();
                        !matches!(
                            s.split_whitespace().next(),
                            Some("Swallow:DynamicQty" | "Swallow:Condition_AsLongAs")
                        )
                    }),
                "unexpected toughness-damage warning for {name}: {:?}",
                parsed.parse_warnings
            );

            let static_def = parsed
                .statics
                .iter()
                .find(|static_def| {
                    static_def.affected
                        == Some(TargetFilter::Typed(
                            TypedFilter::creature().properties(expected_props.clone()),
                        ))
                })
                .expect("expected attached conditional toughness-damage static");
            assert!(static_def
                .modifications
                .contains(&ContinuousModification::AssignDamageFromToughness));
        }
    }

    // ------------------------------------------------------------------
    // merge_ability_condition — single-authority merge for ability-word
    // plus literal-if condition composition.
    // ------------------------------------------------------------------

    fn cond_delirium() -> AbilityCondition {
        AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::DistinctCardTypes {
                    source: crate::types::ability::CardTypeSetSource::Zone {
                        zone: crate::types::ability::ZoneRef::Graveyard,
                        scope: crate::types::ability::CountScope::Controller,
                    },
                },
            },
            comparator: crate::types::ability::Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 4 },
        }
    }

    fn cond_your_turn() -> AbilityCondition {
        AbilityCondition::IsYourTurn
    }

    fn cond_max_speed() -> AbilityCondition {
        AbilityCondition::HasMaxSpeed
    }

    #[test]
    fn merge_ability_condition_dedups_structural_equal() {
        // Delirium ability-word + literal "if there are four or more card types..."
        // both emit the same `QuantityCheck` — the merge should collapse to a single
        // leaf condition, not `And(X, X)`.
        let merged = merge_ability_condition(Some(cond_delirium()), cond_delirium());
        assert_eq!(merged, cond_delirium());
    }

    #[test]
    fn merge_ability_condition_wraps_distinct_in_and() {
        let merged = merge_ability_condition(Some(cond_your_turn()), cond_delirium());
        match merged {
            AbilityCondition::And { conditions } => {
                assert_eq!(conditions.len(), 2);
                assert_eq!(conditions[0], cond_your_turn());
                assert_eq!(conditions[1], cond_delirium());
            }
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn merge_ability_condition_flattens_nested_and() {
        // Existing is already `And`: appending a third distinct condition must not
        // produce `And(And(X, Y), Z)` — the result stays flat.
        let existing = AbilityCondition::And {
            conditions: vec![cond_your_turn(), cond_delirium()],
        };
        let merged = merge_ability_condition(Some(existing), cond_max_speed());
        match merged {
            AbilityCondition::And { conditions } => {
                assert_eq!(conditions.len(), 3);
                assert_eq!(conditions[0], cond_your_turn());
                assert_eq!(conditions[1], cond_delirium());
                assert_eq!(conditions[2], cond_max_speed());
            }
            other => panic!("expected flat And(3), got {other:?}"),
        }
    }

    #[test]
    fn merge_ability_condition_dedups_against_and_children() {
        // Appending a condition that already exists in an `And` is a no-op (no duplicate).
        let existing = AbilityCondition::And {
            conditions: vec![cond_your_turn(), cond_delirium()],
        };
        let merged = merge_ability_condition(Some(existing.clone()), cond_delirium());
        assert_eq!(merged, existing);
    }

    #[test]
    fn merge_ability_condition_none_returns_incoming() {
        let merged = merge_ability_condition(None, cond_delirium());
        assert_eq!(merged, cond_delirium());
    }

    /// End-to-end: parse actual Violent Urge Oracle text and assert the 2nd ability's
    /// condition is a single `QuantityCheck`, not `And(X, X)`. Guards against the
    /// ability-word/literal-if duplication bug at the dispatch layer.
    #[test]
    fn delirium_spell_condition_is_single_leaf_not_and() {
        let parsed = parse(
            "Target creature gets +1/+0 and gains first strike until end of turn.\n\
             Delirium — If there are four or more card types among cards in your graveyard, \
             that creature gains double strike until end of turn.",
            "Violent Urge",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(parsed.abilities.len(), 2, "expected two spell abilities");
        let second = &parsed.abilities[1];
        match &second.condition {
            Some(AbilityCondition::QuantityCheck { .. }) => {}
            Some(AbilityCondition::And { conditions }) => {
                panic!(
                    "delirium condition must not be wrapped in And, got And with \
                     {} children: {conditions:?}",
                    conditions.len()
                );
            }
            other => panic!("expected QuantityCheck, got {other:?}"),
        }
    }

    /// Regression: pin Helm of the Host's already-shipped non-legendary token
    /// behavior so a future refactor of `parse_except_clause` /
    /// `become_copy_except` cannot silently drop the `RemoveSupertype`
    /// modification.
    ///
    /// CR 707.9b: "Some copy effects modify a characteristic as part of the
    /// copying process. The final set of values for that characteristic
    /// becomes part of the copiable values of the copy." — "except the token
    /// isn't legendary" is exactly such a modification, lowered to
    /// `ContinuousModification::RemoveSupertype { Legendary }` and stamped
    /// onto the synthesized token at creation time so the legend rule
    /// (CR 704.5j) cannot collapse the token even when its source is a
    /// legendary creature.
    ///
    /// This test pins the parser side only — the resolver side is pinned by
    /// `copy_token_remove_supertype_strips_legendary_from_token` in
    /// `crates/engine/src/game/effects/token_copy.rs`.
    #[test]
    fn helm_of_the_host_emits_remove_supertype_legendary() {
        use crate::types::card_type::Supertype;

        let r = parse(
            "At the beginning of combat on your turn, create a token that's a \
             copy of equipped creature, except the token isn't legendary. That \
             token gains haste.\nEquip {5}",
            "Helm of the Host",
            &[Keyword::Equip(Default::default())],
            &["Artifact"],
            &["Equipment"],
        );

        // One trigger (the begin-combat copy-token trigger) and one activated
        // ability (Equip {5}).
        assert_eq!(
            r.triggers.len(),
            1,
            "expected exactly one trigger, got {}: {:?}",
            r.triggers.len(),
            r.triggers
                .iter()
                .map(|t| t.description.as_deref().unwrap_or(""))
                .collect::<Vec<_>>()
        );

        let trig = &r.triggers[0];
        let exec = trig
            .execute
            .as_ref()
            .expect("begin-combat trigger must have an execute body");

        // CR 707.9b + CR 205.4: top-level effect is `CopyTokenOf` with the
        // `RemoveSupertype { Legendary }` modification baked in. The token
        // copies "equipped creature" — the target filter is internal detail
        // tested elsewhere; this regression test pins ONLY the
        // additional_modifications, which is the load-bearing field for the
        // non-legendary semantic.
        match &*exec.effect {
            Effect::CopyTokenOf {
                additional_modifications,
                ..
            } => {
                assert!(
                    additional_modifications.contains(&ContinuousModification::RemoveSupertype {
                        supertype: Supertype::Legendary,
                    }),
                    "Helm of the Host must emit RemoveSupertype(Legendary); \
                     additional_modifications was {additional_modifications:?}"
                );
            }
            other => panic!("expected CopyTokenOf at trigger.execute.effect, got {other:?}"),
        }
    }

    /// Regression: pin Puresteel Paladin's Metalcraft static-grant-of-equip line
    /// so a future refactor of `try_parse_equip` / Priority 3 dispatch cannot
    /// resurface the `cost: Unimplemented("ment you control...")` misparse.
    ///
    /// CR 207.2c (Metalcraft ability word) + CR 113.3 (granted ability) +
    /// CR 613.1 (continuous effect): "Equipment you control have equip {0}"
    /// must parse as a static (`AddKeyword(Equip {0})` continuous modification),
    /// not as a malformed activated ability whose cost text begins mid-word
    /// inside "Equipment". The defect was a missing word-boundary guard in
    /// `try_parse_equip`: the keyword "equip" must terminate at a recognized
    /// boundary char, not slice off the first 5 bytes of "Equipment".
    #[test]
    fn puresteel_paladin_metalcraft_grant_parses_as_static_not_activated() {
        let r = parse(
            "Whenever an Equipment you control enters, you may draw a card.\n\
             Metalcraft — Equipment you control have equip {0} as long as you \
             control three or more artifacts.",
            "Puresteel Paladin",
            &[],
            &["Creature"],
            &["Human", "Knight"],
        );
        // No malformed activated ability — the granted-equip line is a static.
        assert!(
            r.abilities.is_empty(),
            "expected zero activated abilities (the granted-equip line is a \
             static, not an activation on Puresteel itself); got: {:?}",
            r.abilities
                .iter()
                .map(|a| a.description.as_deref().unwrap_or(""))
                .collect::<Vec<_>>()
        );
        // Exactly one static — the AddKeyword(Equip{0}) Metalcraft grant.
        assert_eq!(
            r.statics.len(),
            1,
            "expected one static (Metalcraft grant); got {}: {:?}",
            r.statics.len(),
            r.statics
                .iter()
                .map(|s| s.description.as_deref().unwrap_or(""))
                .collect::<Vec<_>>()
        );
        let s = &r.statics[0];
        assert!(
            s.condition.is_some(),
            "Metalcraft grant must carry the ability-word condition"
        );
    }

    /// Regression: defensive coverage for `try_parse_equip`'s word-boundary
    /// guard. "Equipment ..." (a sentence opening with the noun, no keyword
    /// "equip") and "Equipped ..." (the static-grant subject) must both
    /// fall through Priority 3 without producing an Activated/Attach ability.
    #[test]
    fn try_parse_equip_word_boundary_rejects_equipment_and_equipped() {
        // "equip" → matches (cost follows)
        assert!(super::try_parse_equip("Equip {2}").is_some());
        assert!(super::try_parse_equip("Equip — {3}").is_some());
        // "equipment" → must NOT match (different word)
        assert!(super::try_parse_equip("Equipment you control have equip {0}.").is_none());
        // "equipped" → caller's separate guard handles this, but defending
        // try_parse_equip itself is fail-safe.
        assert!(super::try_parse_equip("Equipped creature gets +2/+0.").is_none());
    }

    #[test]
    fn restricted_equip_costs_use_embedded_mana_cost() {
        for (line, expected_generic) in [
            ("Equip Elf {2}", 2),
            ("Equip creature token {1}", 1),
            ("Equip legendary creature {3}", 3),
            ("Equip commander {3}", 3),
        ] {
            let ability = super::try_parse_equip(line).expect("restricted equip should parse");
            assert!(
                matches!(
                    ability.cost,
                    Some(AbilityCost::Mana {
                        cost: ManaCost::Cost { generic, .. },
                    }) if generic == expected_generic
                ),
                "{line} parsed unexpected cost: {:?}",
                ability.cost
            );
        }

        // CR 118.12a: "Equip {2} or {B}" is a disjunctive cost — OneOf([Mana({2}), Mana({B})]).
        let ability =
            super::try_parse_equip("Equip {2} or {B}").expect("disjunctive equip should parse");
        match ability.cost {
            Some(AbilityCost::OneOf { ref costs }) => {
                assert_eq!(costs.len(), 2, "expected 2 alternatives, got {:?}", costs);
                assert!(
                    matches!(
                        &costs[0],
                        AbilityCost::Mana {
                            cost: ManaCost::Cost { generic: 2, .. }
                        }
                    ),
                    "left alternative should be Mana({{2}}), got {:?}",
                    costs[0]
                );
                assert!(
                    matches!(&costs[1], AbilityCost::Mana { cost: ManaCost::Cost { shards, generic: 0 } } if shards.len() == 1),
                    "right alternative should be Mana({{B}}), got {:?}",
                    costs[1]
                );
            }
            other => panic!("Expected OneOf for 'Equip {{2}} or {{B}}', got {:?}", other),
        }
    }

    #[test]
    fn restricted_equip_costs_preserve_target_requirement() {
        let legendary = super::try_parse_equip("Equip legendary creature {1}")
            .expect("legendary equip should parse");
        let Effect::Attach { target, .. } = *legendary.effect else {
            panic!("expected Attach, got {:?}", legendary.effect);
        };
        let TargetFilter::Typed(tf) = target else {
            panic!("expected typed target, got {:?}", target);
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf.properties.contains(&FilterProp::HasSupertype {
            value: crate::types::card_type::Supertype::Legendary,
        }));

        let commander =
            super::try_parse_equip("Equip commander {3}").expect("commander equip should parse");
        let Effect::Attach { target, .. } = *commander.effect else {
            panic!("expected Attach, got {:?}", commander.effect);
        };
        let TargetFilter::Typed(tf) = target else {
            panic!("expected typed target, got {:?}", target);
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf.properties.contains(&FilterProp::IsCommander));
    }

    #[test]
    fn restricted_equip_costs_cover_observed_target_classes() {
        for line in [
            "Equip Citizen {1}",
            "Equip Detective {1}",
            "Equip Elf {2}",
            "Equip Halfling {1}",
            "Equip Human {1}",
            "Equip Knight {1}",
            "Equip Pirate {1}",
            "Equip Soldier {W}",
        ] {
            let ability = super::try_parse_equip(line).expect("subtype equip should parse");
            let Effect::Attach { target, .. } = *ability.effect else {
                panic!("expected Attach, got {:?}", ability.effect);
            };
            let TargetFilter::Typed(tf) = target else {
                panic!("expected typed target, got {:?}", target);
            };
            assert_eq!(tf.controller, Some(ControllerRef::You), "{line}");
            assert!(tf.type_filters.contains(&TypeFilter::Creature), "{line}");
            assert!(
                tf.type_filters
                    .iter()
                    .any(|filter| matches!(filter, TypeFilter::Subtype(_))),
                "{line}"
            );
        }

        let class_union = super::try_parse_equip("Equip Shaman, Warlock, or Wizard {1}")
            .expect("multi-subtype equip should parse");
        let Effect::Attach { target, .. } = *class_union.effect else {
            panic!("expected Attach, got {:?}", class_union.effect);
        };
        let TargetFilter::Or { filters } = target else {
            panic!("expected or target, got {:?}", target);
        };
        assert_eq!(filters.len(), 3);
        for expected_subtype in ["Shaman", "Warlock", "Wizard"] {
            assert!(filters.iter().any(|filter| matches!(
                filter,
                TargetFilter::Typed(tf)
                    if tf.controller == Some(ControllerRef::You)
                        && tf.type_filters.contains(&TypeFilter::Creature)
                        && tf
                            .type_filters
                            .contains(&TypeFilter::Subtype(expected_subtype.to_string()))
            )));
        }

        let token = super::try_parse_equip("Equip creature token {1}")
            .expect("creature-token equip should parse");
        let Effect::Attach { target, .. } = *token.effect else {
            panic!("expected Attach, got {:?}", token.effect);
        };
        let TargetFilter::Typed(tf) = target else {
            panic!("expected typed target, got {:?}", target);
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf.properties.contains(&FilterProp::Token));

        let planeswalker = super::try_parse_equip("Equip planeswalker {1}")
            .expect("planeswalker equip should parse");
        let Effect::Attach { target, .. } = *planeswalker.effect else {
            panic!("expected Attach, got {:?}", planeswalker.effect);
        };
        let TargetFilter::Typed(tf) = target else {
            panic!("expected typed target, got {:?}", target);
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf.type_filters.contains(&TypeFilter::Planeswalker));
        assert!(!tf.type_filters.contains(&TypeFilter::Creature));

        let creature_or_planeswalker = super::try_parse_equip("Equip creature or planeswalker {3}")
            .expect("creature-or-planeswalker equip should parse");
        let Effect::Attach { target, .. } = *creature_or_planeswalker.effect else {
            panic!("expected Attach, got {:?}", creature_or_planeswalker.effect);
        };
        let TargetFilter::Or { filters } = target else {
            panic!("expected or target, got {:?}", target);
        };
        assert!(filters.iter().any(|filter| matches!(
            filter,
            TargetFilter::Typed(tf)
                if tf.controller == Some(ControllerRef::You)
                    && tf.type_filters.contains(&TypeFilter::Creature)
        )));
        assert!(filters.iter().any(|filter| matches!(
            filter,
            TargetFilter::Typed(tf)
                if tf.controller == Some(ControllerRef::You)
                    && tf.type_filters.contains(&TypeFilter::Planeswalker)
        )));
    }

    #[test]
    fn equip_cost_modifier_lines_are_not_equip_abilities() {
        for line in [
            "Equip abilities you activate cost {1} less to activate.",
            "Equip costs you pay cost {1} less.",
        ] {
            assert!(
                super::try_parse_equip(line).is_none(),
                "{line} must not parse as an equip activated ability"
            );
        }
    }

    #[test]
    fn equip_once_per_turn_constraint_strips_from_cost() {
        let ability = super::try_parse_equip("Equip {0}. Activate only once each turn.")
            .expect("equip should parse");
        assert_eq!(
            ability.cost,
            Some(AbilityCost::Mana {
                cost: ManaCost::zero(),
            })
        );
        assert!(
            ability
                .activation_restrictions
                .contains(&ActivationRestriction::OnlyOnceEachTurn),
            "expected only-once-each-turn restriction: {:?}",
            ability.activation_restrictions
        );
    }

    #[test]
    fn plate_armor_equip_cost_reduction_stays_on_equip_ability() {
        let result = parse(
            "Equipped creature gets +3/+3 and has ward {1}.\n\
             Equip {3}. This ability costs {1} less to activate for each other Equipment you control.",
            "Plate Armor",
            &[Keyword::Equip(ManaCost::Cost {
                generic: 3,
                shards: vec![],
            })],
            &["Artifact"],
            &["Equipment"],
        );

        assert_eq!(result.abilities.len(), 1);
        let equip = &result.abilities[0];
        assert_eq!(
            equip.cost,
            Some(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 3,
                    shards: vec![],
                },
            })
        );
        let reduction = equip
            .cost_reduction
            .as_ref()
            .expect("equip ability should carry cost reduction");
        assert_eq!(reduction.amount_per, 1);
        match &reduction.count {
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            } => match filter {
                TargetFilter::Typed(tf) => {
                    assert_eq!(
                        tf.controller,
                        Some(crate::types::ability::ControllerRef::You)
                    );
                    assert!(
                        tf.type_filters.iter().any(
                            |filter| matches!(filter, TypeFilter::Subtype(name) if name == "Equipment")
                        ),
                        "expected Equipment subtype, got {:?}",
                        tf.type_filters
                    );
                    assert!(
                        tf.properties
                            .iter()
                            .any(|property| matches!(property, FilterProp::Another)),
                        "expected Another property, got {:?}",
                        tf.properties
                    );
                }
                other => panic!("expected typed ObjectCount filter, got {:?}", other),
            },
            other => panic!("expected ObjectCount cost reduction, got {:?}", other),
        }

        assert_eq!(result.statics.len(), 1);
        let static_def = &result.statics[0];
        assert!(
            static_def.modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::AddPower { value: 3 }
            )),
            "missing +3 power modification: {:?}",
            static_def.modifications
        );
        assert!(
            static_def.modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::AddToughness { value: 3 }
            )),
            "missing +3 toughness modification: {:?}",
            static_def.modifications
        );
        assert!(
            static_def.modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Ward(WardCost::Mana(ManaCost::Cost {
                        generic: 1,
                        shards,
                    })),
                } if shards.is_empty()
            )),
            "missing ward {{1}} modification: {:?}",
            static_def.modifications
        );
        assert!(
            result.parse_warnings.iter().all(|warning| warning
                .to_string()
                .split_whitespace()
                .next()
                != Some("Swallow:DynamicQty")),
            "unexpected DynamicQty warning: {:?}",
            result.parse_warnings
        );
    }

    /// Regression: pin the broader "Equipment you control have equip {N}"
    /// class — Astor (no ability-word prefix, no em-dash on the line) and
    /// Syr Gwyn (Knight-restricted equip {0}) were silently affected by the
    /// same `try_parse_equip` boundary defect. Both must parse cleanly as
    /// statics without producing a malformed activated ability on the source.
    /// CR 113.3 + CR 613.1.
    #[test]
    fn equipment_have_equip_grant_class_parses_as_static() {
        // Astor — bare "Equipment you control have equip {1}." with no
        // ability-word prefix. lower_starts_with("equip") fires here too
        // because "equipment" begins with the same five letters.
        let r = parse(
            "Equipment you control have equip {1}.\nVehicles you control have crew 1.",
            "Astor, Bearer of Blades",
            &[],
            &["Creature"],
            &["Human", "Warrior"],
        );
        assert!(
            r.abilities.is_empty(),
            "Astor: no malformed activated ability expected; got {:?}",
            r.abilities
                .iter()
                .map(|a| a.description.as_deref().unwrap_or(""))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            r.statics.len(),
            2,
            "Astor: expected two statics (equip + crew grants); got {}",
            r.statics.len()
        );

        // Syr Gwyn — "Equipment you control have equip Knight {0}." (Knight
        // sub-restriction on the granted equip ability).
        let r = parse(
            "Equipment you control have equip Knight {0}.",
            "Syr Gwyn, Hero of Ashvale",
            &[],
            &["Creature"],
            &["Human", "Knight"],
        );
        assert!(
            r.abilities.is_empty(),
            "Syr Gwyn: no malformed activated ability expected; got {:?}",
            r.abilities
                .iter()
                .map(|a| a.description.as_deref().unwrap_or(""))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            r.statics.len(),
            1,
            "Syr Gwyn: expected one static (equip Knight grant); got {}",
            r.statics.len()
        );
    }

    #[test]
    fn defiler_single_line_cost_reduction_parses_as_dedicated_static() {
        let r = parse(
            "Flying\nAs an additional cost to cast blue permanent spells, you may pay 2 life. Those spells cost {U} less to cast if you paid life this way. This effect reduces only the amount of blue mana you pay.\nWhenever you cast a blue permanent spell, draw a card.",
            "Defiler of Dreams",
            &[Keyword::Flying],
            &["Creature"],
            &["Phyrexian", "Sphinx"],
        );

        assert_eq!(r.statics.len(), 1, "expected Defiler static: {r:#?}");
        match &r.statics[0].mode {
            StaticMode::DefilerCostReduction {
                color,
                life_cost,
                mana_reduction,
            } => {
                assert_eq!(*color, ManaColor::Blue);
                assert_eq!(*life_cost, 2);
                assert_eq!(
                    mana_reduction,
                    &ManaCost::Cost {
                        shards: vec![ManaCostShard::Blue],
                        generic: 0,
                    }
                );
            }
            other => panic!("expected DefilerCostReduction, got {other:?}"),
        }
        assert!(
            r.parse_warnings.iter().all(|warning| {
                let tag = warning.to_string();
                let tag = tag.split_whitespace().next();
                tag != Some("Swallow:Optional_YouMay") && tag != Some("Swallow:Condition_If")
            }),
            "unexpected Defiler warnings: {:?}",
            r.parse_warnings
        );
    }

    /// CR 614.1a + CR 122.1a: End-to-end check that Vizier of Remedies
    /// parses cleanly through `parse_oracle_text` (the canonical entry
    /// point used by the card-data pipeline) and produces a single
    /// AddCounter replacement gated to -1/-1 counters on creatures the
    /// controller controls. The full card must be fully supported (zero
    /// gaps) — this is what flips the runtime `supported: true` flag in
    /// `card-data.json`.
    #[test]
    fn vizier_of_remedies_parses_to_single_counter_replacement() {
        use crate::game::coverage::{card_face_gaps, card_face_has_unimplemented_parts};
        use crate::types::ability::QuantityModification;
        use crate::types::card::CardFace;
        use crate::types::counter::{CounterMatch, CounterType};

        let oracle = "If one or more -1/-1 counters would be put on a creature you control, that many -1/-1 counters minus one are put on it instead.";
        let parsed = parse_oracle_text(
            oracle,
            "Vizier of Remedies",
            &[],
            &["Creature".to_string()],
            &["Human".to_string(), "Cleric".to_string()],
        );

        assert!(
            parsed.abilities.is_empty(),
            "no spell abilities expected, got {:?}",
            parsed.abilities
        );
        assert!(
            parsed.triggers.is_empty(),
            "no triggered abilities expected, got {:?}",
            parsed.triggers
        );
        assert_eq!(
            parsed.replacements.len(),
            1,
            "expected exactly one replacement, got {:?}",
            parsed.replacements
        );

        let repl = &parsed.replacements[0];
        assert_eq!(repl.event, ReplacementEvent::AddCounter);
        assert_eq!(
            repl.quantity_modification,
            Some(QuantityModification::Minus { value: 1 }),
            "Vizier subtracts 1 from the counter count (saturating at 0 — CR 122.1a)"
        );
        assert_eq!(
            repl.counter_match,
            Some(CounterMatch::OfType(CounterType::Minus1Minus1)),
            "Vizier must be gated to -1/-1 counters specifically"
        );
        assert!(matches!(
            repl.valid_card,
            Some(TargetFilter::Typed(TypedFilter {
                ref type_filters,
                controller: Some(ControllerRef::You),
                ..
            })) if type_filters == &vec![TypeFilter::Creature]
        ));

        // Coverage gate: build a CardFace from the parsed result and verify
        // the engine reports zero gaps (i.e. this is a fully-supported card).
        let face = CardFace {
            name: "Vizier of Remedies".to_string(),
            replacements: parsed.replacements.clone(),
            ..CardFace::default()
        };
        assert!(
            !card_face_has_unimplemented_parts(&face),
            "Vizier of Remedies must report no Unimplemented parts"
        );
        assert!(
            card_face_gaps(&face).is_empty(),
            "Vizier of Remedies must have zero coverage gaps, got: {:?}",
            card_face_gaps(&face)
        );
    }

    /// CR 607.1 + CR 610.3 + #1320: Journey to Nowhere / Oblivion Ring class —
    /// two-trigger exile-return synthesis. The ETB exile ("exile target creature")
    /// has no "until" language, but it's paired with an LTB return trigger. The
    /// synthesis pass must set `Duration::UntilHostLeavesPlay` on the ETB exile
    /// so the engine's ExileLink mechanism returns the card when the source leaves.
    #[test]
    fn journey_to_nowhere_etb_exile_gets_until_host_leaves_duration() {
        let oracle = "When this enchantment enters, exile target creature.\n\
                      When this enchantment leaves the battlefield, return the exiled card \
                      to the battlefield under its owner's control.";
        let result = parse(oracle, "Journey to Nowhere", &[], &["Enchantment"], &[]);

        let etb = result
            .triggers
            .iter()
            .find(|t| {
                t.mode == TriggerMode::ChangesZone && t.destination == Some(Zone::Battlefield)
            })
            .expect("must have ETB trigger");

        let execute = etb.execute.as_deref().expect("ETB must have execute");
        assert_eq!(
            execute.duration,
            Some(crate::types::ability::Duration::UntilHostLeavesPlay),
            "ETB exile must carry UntilHostLeavesPlay so the engine returns the card"
        );
        assert!(
            matches!(
                execute.effect.as_ref(),
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    ..
                }
            ),
            "ETB execute must be ChangeZone→Exile"
        );
    }
}

#[cfg(test)]
mod pipeline_snapshot_tests {
    use super::*;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;

    fn pipeline_parse(
        oracle_text: &str,
        card_name: &str,
        types: &[&str],
        subtypes: &[&str],
    ) -> ParsedAbilities {
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
        parse_oracle_text(oracle_text, card_name, &[], &types, &subtypes)
    }

    #[test]
    fn pipeline_simple_spell() {
        let result = pipeline_parse(
            "Deal 3 damage to any target.",
            "Test Card",
            &["Sorcery"],
            &[],
        );
        insta::assert_json_snapshot!(result);
    }

    #[test]
    fn pipeline_creature_with_keywords_and_trigger() {
        let result = pipeline_parse(
            "Flying\nWhen Test Card enters, draw a card.",
            "Test Card",
            &["Creature"],
            &[],
        );
        insta::assert_json_snapshot!(result);
    }

    #[test]
    fn pipeline_enchantment_with_static_and_replacement() {
        let result = pipeline_parse(
            "Creatures you control get +1/+1.\nIf a creature you control would die, exile it instead.",
            "Test Card",
            &["Enchantment"],
            &[],
        );
        insta::assert_json_snapshot!(result);
    }

    #[test]
    fn pipeline_saga_card() {
        let result = pipeline_parse(
            "I — You draw a card and you lose 1 life.\nII — Create a 2/2 black Zombie creature token.\nIII — Target opponent discards a card.",
            "Test Card",
            &["Enchantment"],
            &["Saga"],
        );
        insta::assert_json_snapshot!(result);
    }

    #[test]
    fn pipeline_class_card() {
        let result = pipeline_parse(
            "Creatures you control get +1/+0.\n{1}{R}: Level 2\nWhenever you attack, target creature you control gains first strike until end of turn.",
            "Test Card",
            &["Enchantment"],
            &["Class"],
        );
        insta::assert_json_snapshot!(result);
    }

    #[test]
    fn pipeline_modal_spell() {
        let result = pipeline_parse(
            "Choose one —\n• Destroy target artifact.\n• Destroy target enchantment.",
            "Test Card",
            &["Instant"],
            &[],
        );
        insta::assert_json_snapshot!(result);
    }

    /// CR 614.1c + CR 502.3: Same-line compound "[~] enters tapped and doesn't
    /// untap during your untap step." must emit BOTH an ETB-tapped replacement
    /// (CR 614.1c) and a CantUntap static (CR 502.3). Regression guard against
    /// the prior bug where the static-pattern classifier consumed the line and
    /// silently dropped the replacement half. Corpus: Traxos, Scourge of Kroog;
    /// Grimgrin, Corpse-Born; Leviathan.
    #[test]
    fn pipeline_etb_tapped_and_cant_untap_compound_emits_both() {
        let result = pipeline_parse(
            "Trample\nTraxos enters tapped and doesn't untap during your untap step.\nWhenever you cast a historic spell, untap Traxos.",
            "Traxos, Scourge of Kroog",
            &["Artifact", "Creature"],
            &["Construct"],
        );
        assert_eq!(
            result.replacements.len(),
            1,
            "expected one ETB-tapped replacement, got {:?}",
            result.replacements
        );
        assert!(
            matches!(result.replacements[0].event, ReplacementEvent::Moved),
            "replacement event must be Moved (ETB), got {:?}",
            result.replacements[0].event
        );
        assert_eq!(
            result.statics.len(),
            1,
            "expected one CantUntap static, got {:?}",
            result.statics
        );
        assert_eq!(
            result.statics[0].mode,
            StaticMode::CantUntap,
            "static mode must be CantUntap"
        );
    }

    // ----------------------------------------------------------------
    // Rocco, Street Chef (issue #412): end-step exile-and-grant +
    // disjunctive play-or-cast payoff triggers.
    // ----------------------------------------------------------------

    /// CR 513.1 + CR 611.2a + CR 108.3 + CR 400.7: Rocco's first trigger
    /// parses to a Phase-mode end-step trigger whose chained sub-ability is
    /// `GrantCastingPermission { permission: PlayFromExile { duration:
    /// UntilNextStepOf { step: End, player: Controller }, ... }, target: TrackedSet(0),
    /// grantee: ObjectOwner }`. CR 305.1 + CR 601.2: the second trigger is
    /// disjunctive on "plays a land from exile" / "casts a spell from
    /// exile" and emits two TriggerDefinitions — one `LandPlayed`, one
    /// `SpellCast` — both with `valid_card.InZone(Exile)` so the
    /// payoff (counter + Food token) fires only on plays-from-exile.
    #[test]
    fn pipeline_rocco_street_chef_emits_three_triggers() {
        use crate::types::ability::{
            CastingPermission, Duration, Effect, FilterProp, PermissionGrantee, PlayerScope,
            TargetFilter, TypedFilter,
        };
        let result = pipeline_parse(
            "At the beginning of your end step, each player exiles the top card of their library. Until your next end step, each player may play the card they exiled this way.\nWhenever a player plays a land from exile or casts a spell from exile, you put a +1/+1 counter on target creature and create a Food token.",
            "Rocco, Street Chef",
            &["Legendary", "Creature"],
            &["Elf", "Druid"],
        );

        assert_eq!(
            result.triggers.len(),
            3,
            "expected 3 triggers (1 end-step + 2 disjunctive payoff), got {:?}",
            result.triggers.iter().map(|t| &t.mode).collect::<Vec<_>>(),
        );

        // Trigger 0: end-step Phase trigger with sub_ability GrantCastingPermission.
        let t0 = &result.triggers[0];
        assert_eq!(t0.mode, TriggerMode::Phase);
        assert_eq!(t0.phase, Some(crate::types::phase::Phase::End));
        let execute = t0.execute.as_deref().expect("trigger has execute");
        let sub = execute.sub_ability.as_deref().expect("sub_ability present");
        match sub.effect.as_ref() {
            Effect::GrantCastingPermission {
                permission,
                target,
                grantee,
            } => {
                match permission {
                    CastingPermission::PlayFromExile {
                        duration:
                            Duration::UntilNextStepOf {
                                step: crate::types::phase::Phase::End,
                            player: PlayerScope::Controller,
                        },
                        ..
                    } => {}
                    _ => panic!(
                        "expected PlayFromExile {{ UntilNextStepOf {{ End, Controller }} }}, got {:?}",
                        permission,
                    ),
                }
                assert!(
                    matches!(
                        target,
                        TargetFilter::TrackedSet {
                            id: crate::types::identifiers::TrackedSetId(0)
                        }
                    ),
                    "target must be TrackedSet(0), got {:?}",
                    target,
                );
                assert_eq!(*grantee, PermissionGrantee::ObjectOwner);
            }
            other => panic!("expected GrantCastingPermission, got {:?}", other),
        }

        // Triggers 1 and 2: disjunctive payoff. Order may vary; collect modes.
        let modes: std::collections::HashSet<_> = result.triggers[1..]
            .iter()
            .map(|t| t.mode.clone())
            .collect();
        assert!(
            modes.contains(&TriggerMode::LandPlayed),
            "expected one LandPlayed trigger, got {:?}",
            modes,
        );
        assert!(
            modes.contains(&TriggerMode::SpellCast),
            "expected one SpellCast trigger, got {:?}",
            modes,
        );

        // Each payoff trigger constrains the event to "from exile" — but
        // through different typed fields per CR 601.2a vs CR 305:
        //   - LandPlayed (CR 305): `valid_card.InZone(Exile)` — the
        //     LandPlayed matcher reads the FilterProp::InZone.
        //   - SpellCast (CR 601.2a): `spell_cast_origin = Equals(Exile)` —
        //     the SpellCast matcher reads the typed origin constraint via
        //     the cast-origin gate, since at fire-time the spell object's
        //     zone is `Stack`, not its cast origin.
        use crate::types::ability::OriginConstraint;
        use crate::types::zones::Zone;
        for trigger in &result.triggers[1..] {
            match trigger.mode {
                TriggerMode::LandPlayed => {
                    let valid_card = trigger
                        .valid_card
                        .as_ref()
                        .expect("LandPlayed payoff trigger has valid_card filter");
                    match valid_card {
                        TargetFilter::Typed(TypedFilter { properties, .. }) => {
                            assert!(
                                properties
                                    .iter()
                                    .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Exile })),
                                "LandPlayed valid_card must carry InZone(Exile), got {:?}",
                                properties,
                            );
                        }
                        other => panic!("expected Typed filter, got {:?}", other),
                    }
                }
                TriggerMode::SpellCast => {
                    assert_eq!(
                        trigger.spell_cast_origin,
                        OriginConstraint::Equals(Zone::Exile),
                        "SpellCast payoff trigger must constrain cast origin to Exile",
                    );
                }
                ref other => panic!("unexpected payoff trigger mode: {:?}", other),
            }
        }
    }

    /// CR 608.2c: Compound "destroy X and up to one other target Y" must parse
    /// both halves as Destroy effects with the verb carried forward to the
    /// "up to" sub-clause. Cards: Relic Crush, Sword of Sinew and Steel.
    #[test]
    fn pipeline_relic_crush_compound_destroy_up_to() {
        use crate::types::ability::{FilterProp, MultiTargetSpec, QuantityExpr, TargetFilter};
        let result = pipeline_parse(
            "Destroy target artifact or enchantment and up to one other target artifact or enchantment.",
            "Relic Crush",
            &["Sorcery"],
            &[],
        );
        assert_eq!(
            result.abilities.len(),
            1,
            "expected one spell ability, got {:?}",
            result.abilities,
        );
        let ab = &result.abilities[0];
        assert!(
            matches!(*ab.effect, Effect::Destroy { .. }),
            "primary effect must be Destroy, got {:?}",
            ab.effect,
        );
        let sub = ab.sub_ability.as_deref().expect("must have sub_ability");
        assert!(
            matches!(*sub.effect, Effect::Destroy { .. }),
            "sub-effect must be Destroy, got {:?}",
            sub.effect,
        );
        // CR 115.6: "up to one" cardinality must be preserved on the sub-ability.
        assert_eq!(
            sub.multi_target,
            Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 })),
            "sub-ability must carry up-to-one multi_target",
        );
        // CR 608.2c: "other" must appear as FilterProp::Another in the sub-effect target.
        match sub.effect.as_ref() {
            Effect::Destroy { target, .. } => {
                // Target may be Typed or Or { filters: [Typed, Typed] } for
                // "artifact or enchantment".
                let typed_filters: Vec<_> = match target {
                    TargetFilter::Typed(tf) => vec![tf],
                    TargetFilter::Or { filters } => filters
                        .iter()
                        .map(|f| match f {
                            TargetFilter::Typed(tf) => tf,
                            other => panic!("expected Typed in Or, got {:?}", other),
                        })
                        .collect(),
                    other => panic!("expected Typed or Or target, got {:?}", other),
                };
                assert!(
                    typed_filters
                        .iter()
                        .all(|tf| tf.properties.contains(&FilterProp::Another)),
                    "all sub-clause target filters must have Another property, got {:?}",
                    typed_filters,
                );
            }
            other => panic!("expected Destroy, got {:?}", other),
        }
    }

    #[test]
    fn pipeline_scheming_aspirant_proliferate_trigger() {
        let result = pipeline_parse(
            "Whenever you proliferate, each opponent loses 2 life and you gain 2 life.",
            "Scheming Aspirant",
            &["Creature"],
            &["Human", "Noble"],
        );
        assert_eq!(result.triggers.len(), 1);
        let trigger = &result.triggers[0];
        assert_eq!(trigger.mode, TriggerMode::PlayerPerformedAction);
        assert_eq!(trigger.valid_target, Some(TargetFilter::Controller));
        assert_eq!(
            trigger.player_actions,
            Some(vec![crate::types::events::PlayerActionKind::Proliferate])
        );
        // Verify the execute body is LoseLife + GainLife
        let exec = trigger.execute.as_ref().expect("execute body");
        assert!(
            matches!(exec.effect.as_ref(), Effect::LoseLife { .. }),
            "expected LoseLife, got {:?}",
            exec.effect
        );
        let sub = exec.sub_ability.as_ref().expect("sub_ability");
        assert!(
            matches!(sub.effect.as_ref(), Effect::GainLife { .. }),
            "expected GainLife, got {:?}",
            sub.effect
        );
    }

    /// CR 608.2c + CR 701.8a: Loyal Sentry — "destroy that creature and ~"
    /// compound action with self-reference carry-forward.
    #[test]
    fn pipeline_loyal_sentry_compound_destroy_self_ref() {
        use crate::types::ability::TargetFilter;
        use crate::types::triggers::TriggerMode;
        let result = pipeline_parse(
            "When this creature blocks a creature, destroy that creature and ~.",
            "Loyal Sentry",
            &["Creature"],
            &[],
        );
        // Should have one triggered ability.
        assert_eq!(
            result.triggers.len(),
            1,
            "expected one trigger, got {:?}",
            result.triggers,
        );
        let trig = &result.triggers[0];
        // CR 509.1g: Trigger mode must be Blocks.
        assert_eq!(
            trig.mode,
            TriggerMode::Blocks,
            "trigger mode must be Blocks",
        );
        // The execute field holds the AbilityDefinition for the triggered effect.
        let exec = trig.execute.as_deref().expect("trigger must have execute");
        // CR 608.2c: Primary effect is Destroy targeting the blocked creature.
        // The anaphoric "that creature" resolves to ParentTarget (inherits the
        // trigger's target binding via try_split_targeted_compound).
        match exec.effect.as_ref() {
            Effect::Destroy { target, .. } => {
                assert_eq!(
                    target.clone(),
                    TargetFilter::ParentTarget,
                    "primary target must be ParentTarget (the blocked creature)",
                );
            }
            other => panic!("primary effect must be Destroy, got {:?}", other),
        }
        // CR 608.2c + CR 701.8a: Sub-clause is Destroy { SelfRef } for '~'.
        let sub = exec.sub_ability.as_deref().expect("must have sub_ability");
        match sub.effect.as_ref() {
            Effect::Destroy { target, .. } => {
                assert_eq!(
                    target.clone(),
                    TargetFilter::SelfRef,
                    "sub-clause target must be SelfRef for '~'",
                );
            }
            other => panic!("sub-clause must be Destroy, got {:?}", other),
        }
    }

    // ── Well of Lost Dreams: pay {X} ≤ life gained, draw X cards ─────────────

    #[test]
    fn well_of_lost_dreams_draw_count_is_variable_x() {
        // CR 107.3i: "where X is less than or equal to <bound>" is a player-
        // chosen constraint, not a definition of X. The draw count must resolve
        // to Variable("X") so the PayAmountChoice → chosen_x → draw path
        // produces X drawn cards (not 0 from a stale QuantityRef string).
        let r = pipeline_parse(
            "Whenever you gain life, you may pay {X}, where X is less than or equal to the amount of life you gained. If you do, draw X cards.",
            "Well of Lost Dreams",
            &["Artifact"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1, "should have one trigger");
        let exec = r.triggers[0]
            .execute
            .as_ref()
            .expect("trigger must have execute");
        assert!(
            matches!(*exec.effect, Effect::PayCost { .. }),
            "first effect should be PayCost, got {:?}",
            exec.effect,
        );
        let sub = exec
            .sub_ability
            .as_deref()
            .expect("PayCost must have sub_ability");
        match sub.effect.as_ref() {
            Effect::Draw { count, .. } => {
                assert_eq!(
                    count.clone(),
                    crate::types::ability::QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    "draw count must be Variable(\"X\") so chosen_x resolves it, not a stale bound string"
                );
            }
            other => panic!("sub-ability must be Draw, got {:?}", other),
        }
    }
}
