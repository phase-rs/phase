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
    ChosenSubtypeKind, ContinuousModification, ControllerRef, CostReduction,
    DelayedTriggerCondition, Duration, Effect, EffectScope, FilterProp, ManaProduction,
    ModalChoice, ParsedCondition, PlayerFilter, QuantityExpr, QuantityRef, ReplacementDefinition,
    SolveCondition, SpellCastingOption, StaticCondition, StaticDefinition, TapStateChange,
    TargetFilter, TriggerCondition, TriggerDefinition, TypedFilter,
};
use crate::types::format::DeckCopyLimit;
use crate::types::keywords::{EscapeCost, FlashbackCost, Keyword, KeywordKind};
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::replacements::ReplacementEvent;
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::oracle_nom::bridge::{nom_on_lower, split_once_on_lower};
use super::oracle_nom::condition::{parse_graveyard_keyword_grant_sentence, parse_inner_condition};
use super::oracle_nom::primitives::parse_number as nom_parse_number;
use super::oracle_nom::primitives::scan_contains;

use super::oracle_attraction::parse_attraction_visit_triggers;
use super::oracle_casting::{
    parse_additional_cost_line, parse_casting_restriction_line, parse_spell_casting_option_line,
    split_additional_cost_trailing_spell_reduction,
};
use super::oracle_class::parse_class_oracle_text;
use super::oracle_classifier::{
    has_roll_die_pattern, has_trigger_prefix, is_ability_activate_cost_static,
    is_alternative_keyword_cost_pattern, is_as_enters_becomes_choice_pattern,
    is_cant_win_lose_compound, is_cast_spells_alternative_cost_pattern,
    is_collect_evidence_alt_cost_pattern, is_compound_turn_limit, is_defiler_cost_pattern,
    is_enters_tapped_cant_untap_compound, is_enters_with_counter_replacement_line,
    is_enters_with_counter_trigger, is_flashback_equal_mana_cost, is_granted_static_line,
    is_instead_replacement_line, is_opening_hand_begin_game, is_pay_life_as_colored_mana_pattern,
    is_replacement_pattern, is_spells_alternative_cost_pattern, is_static_pattern,
    is_vehicle_tier_line, lower_starts_with, should_defer_spell_to_effect,
    split_flashback_trailing_self_spell_cost_reduction,
};
use super::oracle_condition::parse_restriction_condition;
use super::oracle_cost::{parse_oracle_cost, parse_single_cost, try_parse_cost_reduction};
use super::oracle_dispatch::dispatch_line_nom;
use super::oracle_effect::sequence::try_parse_same_is_true_continuation;
use super::oracle_effect::{
    lower_effect_chain_ir, parse_additional_cost_instead_condition_fragment, parse_effect_chain,
    parse_effect_chain_with_context, rewrite_condition_keyword,
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
    extract_ability_word_reminder_body, lower_oracle_block, parse_oracle_block,
    split_short_label_prefix, strip_ability_word, strip_ability_word_with_name,
    strip_flavor_word_with_name, FLAVOR_WORD_COST_LABEL_MAX_WORDS,
};
use super::oracle_replacement::{
    find_copy_verb_present, lower_as_enters_becomes_choice_modal,
    lower_as_enters_or_face_up_counters, lower_replacement_ir, parse_replacement_line,
};
use super::oracle_saga::{is_saga_chapter, parse_saga_chapters};
use super::oracle_spacecraft::parse_spacecraft_threshold_lines;
use super::oracle_special::{
    attach_die_result_branches_to_chain, normalize_self_refs_for_static,
    parse_cumulative_upkeep_keyword, parse_defiler_cost_reduction, parse_harmonize_keyword,
    parse_mayhem_keyword, parse_solve_condition, try_parse_die_roll_table,
};
use super::oracle_static::{
    is_speed_unlock_sentence, lower_static_ir, parse_alternative_keyword_cost,
    parse_cast_spells_alternative_cost_multi, parse_chosen_creature_type_static_prefix,
    parse_collect_evidence_alt_cost, parse_every_creature_type_static_prefix,
    parse_flashback_trailing_self_spell_cost_reduction, parse_spells_alternative_cost,
    parse_static_line, parse_static_line_multi, try_parse_graveyard_keyword_grant_clause,
    try_parse_graveyard_keyword_grant_static, try_parse_top_of_library_cast_permission,
    GrantedCastKeywordKind,
};
use super::oracle_trigger::{lower_trigger_ir, parse_trigger_lines_at_index};
use super::oracle_util::{
    normalize_card_name_refs, parse_mana_symbols, parse_number, split_same_is_true_static_tail,
    strip_reminder_text, TextPair, GRANTING_SELF_PLACEHOLDER,
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
            repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
        } => {
            if let Some(AdditionalCost::Kicker {
                costs,
                repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
            }) = slot.as_mut()
            {
                costs.extend(incoming_costs);
            } else {
                *slot = Some(AdditionalCost::Kicker {
                    costs: incoming_costs,
                    repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
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

/// Recognizer for draft-time procedural sentences on Conspiracy / "draft
/// matters" cards (CR 905). These instruct the booster draft itself — "Draft
/// this card face up.", "As you draft a card, …", "During the draft, …",
/// "Immediately after the draft, …", "Instead of drafting …", "As long as this
/// card is face up during the draft, …" — and have no function during normal
/// play, where the engine never simulates a draft. Consumed silently so they
/// do not fall through to `Effect::Unimplemented`; any constructed-play
/// abilities printed on the same card (keywords, ETBs, activated abilities)
/// still parse through the normal line dispatch.
pub(crate) fn is_draft_matters_sentence(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    lower_starts_with(&lower, "draft this card face up")
        || lower_starts_with(&lower, "as you draft ")
        || lower_starts_with(&lower, "during the draft")
        || lower_starts_with(&lower, "immediately after the draft")
        || lower_starts_with(&lower, "instead of drafting ")
        || lower_starts_with(&lower, "as long as this card is face up during the draft")
        || lower_starts_with(&lower, "each player passes the last card")
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
                    gate: crate::types::ability::TurnGate::None,
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
    // (not_starting_player, counters); the original-case remainder (mapped back
    // by `nom_on_lower`) is the "If you do, [effect]" tail — empty when absent.
    let ((not_starting_player, enter_with_counters), effect_text) = nom_on_lower(
        line,
        lower,
        |input| {
            // Preamble — explicit known forms, each ending in "you may ".
            // CR 103.6a (begin the game with that card on the battlefield);
            // Gemstone Caverns additionally gates on not being the starting player
            // (CR 103.1), captured as a bool so the condition is encoded below.
            let (input, not_starting_player) = alt((
                value(
                    true,
                    tag(
                        "if this card is in your opening hand and you're not the starting player, you may ",
                    ),
                ),
                value(false, tag("if this card is in your opening hand, you may ")),
                value(false, tag("if ~ is in your opening hand, you may ")),
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

            Ok((input, (not_starting_player, counters.unwrap_or_default())))
        },
    )?;

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
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            // CR 122.1: entry counters parsed from "with [N] [type] counter(s) on it".
            enter_with_counters,
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
    )
    .description(line.to_string());
    def.optional = true;

    // CR 103.1: the starting player is determined before mulligans. Gemstone
    // Caverns gates its begin-game ability on NOT being the starting player.
    if not_starting_player {
        def = def.condition(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::WasStartingPlayer {
                controller: ControllerRef::You,
            }),
        });
    }

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

pub(crate) fn parse_graveyard_keyword_continuation(
    text: &str,
    kind: GrantedCastKeywordKind,
) -> Option<Keyword> {
    fn continuation_fully_consumed(rest: &str) -> bool {
        rest.trim().trim_end_matches('.').trim().is_empty()
    }

    fn parse_self_mana_cost_suffix(text: &str) -> Option<&str> {
        let lower = text.to_lowercase();
        let (_, rest) = nom_on_lower(text, &lower, |i| {
            let (i, _) = alt((tag("that card's"), tag("the card's"), tag("its"))).parse(i)?;
            let (i, _) = tag(" mana cost").parse(i)?;
            Ok((i, ()))
        })?;
        Some(rest)
    }

    /// CR 601.2f: Parse an optional "reduced by {N}" suffix on a granted
    /// "[keyword] cost is equal to its mana cost" continuation (Dream Devourer's
    /// "reduced by {2}", Aminatou's "reduced by {4}"). Returns the GENERIC
    /// component of the parsed cost as the reduction (colored pips in the
    /// reduction phrase would be non-generic and ignored — real cards state
    /// generic-only reductions), or `0` when the suffix is absent. The remaining
    /// slice after the (optional) suffix is returned so the caller can enforce
    /// `continuation_fully_consumed`.
    fn parse_reduced_by_generic_suffix(text: &str) -> (u32, &str) {
        let lower = text.to_lowercase();
        nom_on_lower(text, &lower, |i| {
            let (i, reduction) = opt(preceded(
                (
                    nom::character::complete::space0,
                    tag("reduced by "),
                    nom::character::complete::space0,
                ),
                super::oracle_nom::primitives::parse_mana_cost,
            ))
            .parse(i)?;
            let generic = match reduction {
                Some(ManaCost::Cost { generic, .. }) => generic,
                _ => 0,
            };
            Ok((i, generic))
        })
        .map_or((0, text), |(generic, rest)| (generic, rest))
    }

    let lower = text.to_lowercase();

    match kind {
        GrantedCastKeywordKind::Flashback => {
            let (_, rest) = nom_on_lower(text, &lower, |i| {
                value((), tag("the flashback cost is equal to ")).parse(i)
            })?;
            let rest = parse_self_mana_cost_suffix(rest)?;
            if !continuation_fully_consumed(rest) {
                return None;
            }
            Some(Keyword::Flashback(FlashbackCost::Mana(
                ManaCost::SelfManaCost,
            )))
        }
        GrantedCastKeywordKind::Escape => {
            let (_, rest) = nom_on_lower(text, &lower, |i| {
                value((), tag("the escape cost is equal to ")).parse(i)
            })?;
            let rest = parse_self_mana_cost_suffix(rest)?;
            let rest_lower = rest.to_lowercase();
            let (_, rest) = nom_on_lower(rest, &rest_lower, |i| {
                value((), tag(" plus exile ")).parse(i)
            })?;
            let (exile_count, rest) = parse_number(rest)?;
            let rest_lower = rest.to_lowercase();
            let (_, rest) = nom_on_lower(rest, &rest_lower, |i| {
                value((), tag("other cards from your graveyard")).parse(i)
            })?;
            if !continuation_fully_consumed(rest) {
                return None;
            }
            // CR 702.138a: The granted escape cost is "[card's mana cost] plus
            // exile N other cards from your graveyard". Build the compound
            // `EscapeCost::NonMana(Composite[Mana(SelfManaCost), Exile{N,gy}])`
            // so the runtime split (`split_escape_cost_components`) extracts the
            // mana sub-cost for normal payment and routes the exile residual
            // through `pay_additional_cost`.
            Some(Keyword::Escape(EscapeCost::NonMana(
                AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Mana {
                            cost: ManaCost::SelfManaCost,
                        },
                        AbilityCost::Exile {
                            count: exile_count,
                            zone: Some(Zone::Graveyard),
                            filter: None,
                        },
                    ],
                },
            )))
        }
        GrantedCastKeywordKind::Mayhem => {
            // CR 702.187b: "The mayhem cost is equal to [its/that card's/the
            // card's] mana cost." (Green Goblin's Goblin Formula). Mirrors the
            // Flashback continuation; the cost resolves to the card's own mana
            // cost via `ManaCost::SelfManaCost`.
            let (_, rest) = nom_on_lower(text, &lower, |i| {
                value((), tag("the mayhem cost is equal to ")).parse(i)
            })?;
            let rest = parse_self_mana_cost_suffix(rest)?;
            if !continuation_fully_consumed(rest) {
                return None;
            }
            Some(Keyword::Mayhem(ManaCost::SelfManaCost))
        }
        GrantedCastKeywordKind::Scavenge => {
            // CR 702.97a: "The scavenge cost is equal to its mana cost." (Varolz,
            // the Scar-Striped; Young Deathclaws; The Cave of Skulls). Mirrors the
            // Flashback continuation; cost resolves to the card's own mana cost.
            let (_, rest) = nom_on_lower(text, &lower, |i| {
                value(
                    (),
                    alt((
                        tag("the scavenge cost is equal to "),
                        tag("its scavenge cost is equal to "),
                    )),
                )
                .parse(i)
            })?;
            let rest = parse_self_mana_cost_suffix(rest)?;
            if !continuation_fully_consumed(rest) {
                return None;
            }
            Some(Keyword::Scavenge(ManaCost::SelfManaCost))
        }
        GrantedCastKeywordKind::Encore => {
            // CR 702.141a: "Its encore cost is equal to its mana cost." (Wire
            // Surgeons). Same shape as scavenge.
            let (_, rest) = nom_on_lower(text, &lower, |i| {
                value(
                    (),
                    alt((
                        tag("its encore cost is equal to "),
                        tag("the encore cost is equal to "),
                    )),
                )
                .parse(i)
            })?;
            let rest = parse_self_mana_cost_suffix(rest)?;
            if !continuation_fully_consumed(rest) {
                return None;
            }
            Some(Keyword::Encore(ManaCost::SelfManaCost))
        }
        GrantedCastKeywordKind::Embalm => {
            // CR 702.128a: "Its embalm cost is equal to its mana cost."
            // (Naktamun). Same shape as scavenge/encore.
            let (_, rest) = nom_on_lower(text, &lower, |i| {
                value(
                    (),
                    alt((
                        tag("its embalm cost is equal to "),
                        tag("the embalm cost is equal to "),
                    )),
                )
                .parse(i)
            })?;
            let rest = parse_self_mana_cost_suffix(rest)?;
            if !continuation_fully_consumed(rest) {
                return None;
            }
            Some(Keyword::Embalm(crate::types::keywords::EmbalmCost::Mana(
                ManaCost::SelfManaCost,
            )))
        }
        GrantedCastKeywordKind::Foretell => {
            // CR 702.143a + CR 601.2f: "Its foretell cost is equal to its mana
            // cost reduced by {N}." (Dream Devourer, reduced by {2}). The bare
            // "reduced by {0}"-absent form yields `SelfManaCost`; a nonzero
            // reduction yields `SelfManaCostReduced`, both concretized at the
            // runtime stamp point (`resolve_keyword_mana_cost`).
            let (_, rest) = nom_on_lower(text, &lower, |i| {
                value(
                    (),
                    alt((
                        tag("its foretell cost is equal to "),
                        tag("the foretell cost is equal to "),
                    )),
                )
                .parse(i)
            })?;
            let rest = parse_self_mana_cost_suffix(rest)?;
            let (reduction, rest) = parse_reduced_by_generic_suffix(rest);
            if !continuation_fully_consumed(rest) {
                return None;
            }
            Some(Keyword::Foretell(if reduction == 0 {
                ManaCost::SelfManaCost
            } else {
                ManaCost::SelfManaCostReduced { reduction }
            }))
        }
        GrantedCastKeywordKind::Miracle => {
            // CR 702.94a + CR 601.2f: "Its miracle cost is equal to its mana cost
            // reduced by {N}." (Aminatou, Veil Piercer, reduced by {4}). Same
            // shape as the foretell continuation.
            let (_, rest) = nom_on_lower(text, &lower, |i| {
                value(
                    (),
                    alt((
                        tag("its miracle cost is equal to "),
                        tag("the miracle cost is equal to "),
                    )),
                )
                .parse(i)
            })?;
            let rest = parse_self_mana_cost_suffix(rest)?;
            let (reduction, rest) = parse_reduced_by_generic_suffix(rest);
            if !continuation_fully_consumed(rest) {
                return None;
            }
            Some(Keyword::Miracle(if reduction == 0 {
                ManaCost::SelfManaCost
            } else {
                ManaCost::SelfManaCostReduced { reduction }
            }))
        }
    }
}

fn try_parse_graveyard_keyword_static_with_continuation(line: &str) -> Option<StaticDefinition> {
    let lower = line.to_lowercase();
    let (prefix, continuation) = split_once_on_lower(line, &lower, ". ")?;
    let prefix_lower = prefix.to_lowercase();
    let (turn_condition, grant_prefix) = nom_on_lower(prefix, &prefix_lower, |input| {
        value(StaticCondition::DuringYourTurn, tag("during your turn, ")).parse(input)
    })
    .map_or((None, prefix), |(condition, rest)| (Some(condition), rest));
    let (affected, kind, _) = try_parse_graveyard_keyword_grant_clause(grant_prefix)?;
    let keyword = parse_graveyard_keyword_continuation(continuation, kind)?;
    if !kind.matches_keyword(&keyword) {
        return None;
    }
    let mut def = StaticDefinition::continuous()
        .affected(affected)
        .modifications(vec![ContinuousModification::AddKeyword { keyword }])
        .description(line.to_string());
    if let Some(condition) = turn_condition {
        def = def.condition(condition);
    }
    Some(def)
}

/// Returns every `StaticDefinition` produced by `line`, with the
/// graveyard-keyword-continuation front door checked first (CR 702.99 etc.)
/// and then delegating to `parse_static_line_multi` so compound forms
/// (e.g., cross-mode conjunctions) emit all their constituent statics
/// rather than silently dropping the extras.
///
/// When `raw_line_for_cant_cast_gates` is set (oracle dispatch only), cant-cast
/// gate parentheticals stripped by `strip_reminder_text` are re-applied without
/// passing raw reminder parentheticals through the general static parser.
fn parse_static_line_with_graveyard_keyword_continuation(
    line: &str,
    raw_line_for_cant_cast_gates: Option<&str>,
    card_name_for_cant_cast_gates: Option<&str>,
) -> Vec<StaticDefinition> {
    let mut defs = if let Some(def) = try_parse_graveyard_keyword_static_with_continuation(line) {
        vec![def]
    } else if let Some(def) = try_parse_graveyard_keyword_grant_static(line) {
        vec![def]
    } else {
        parse_static_line_multi(line)
    };
    if let (Some(raw), Some(card_name)) =
        (raw_line_for_cant_cast_gates, card_name_for_cant_cast_gates)
    {
        defs = crate::parser::oracle_static::apply_raw_parenthetical_cant_cast_gate(
            defs, raw, card_name,
        );
    }
    defs
}

/// CR 614.6 + CR 701.26b: A single `<subject> can't <P1> and can't <P2>`
/// prohibition whose two conjuncts belong to DIFFERENT parser layers — the
/// static layer and/or the replacement layer. Blossombind ("Enchanted creature
/// can't become untapped and can't have counters put on it.") joins an
/// untap-event prevention (CR 701.26b) and an `AddCounter`-prevention
/// replacement (CR 614.6). Because the counter-prohibition substring trips
/// `is_static_pattern`, the whole line would otherwise be claimed by the static
/// parser, silently dropping the second conjunct. Split on the conjunction,
/// re-attach the shared subject to each clause, route each to BOTH layer parsers,
/// and adopt the split only when every conjunct is claimed by at least one layer
/// AND at least one replacement is produced (a pure-static compound keeps its
/// existing single-layer multi-static path). `line` is already
/// self-ref-normalized for static parsing.
fn parse_static_replacement_compound(
    line: &str,
    lower: &str,
    card_name: &str,
) -> Option<(Vec<StaticDefinition>, Vec<ReplacementDefinition>)> {
    // Re-attach the shared subject to each conjunct so each clause parses
    // independently (Oracle text drops the subject on the second conjunct).
    let (subject, p1, p2) = split_dual_cant_clause(line, lower)?;
    let left = format!("{subject} can't {p1}");
    let right = format!("{subject} can't {p2}");

    let left_statics = parse_static_line_with_graveyard_keyword_continuation(&left, None, None);
    let right_statics = parse_static_line_with_graveyard_keyword_continuation(&right, None, None);
    let left_repl = parse_replacement_line(&left, card_name);
    let right_repl = parse_replacement_line(&right, card_name);

    // Each conjunct must be claimed by at least one layer; otherwise this is not
    // a clean cross-layer compound and the line belongs to the single-layer
    // fallbacks.
    let left_claimed = left_repl.is_some() || !left_statics.is_empty();
    let right_claimed = right_repl.is_some() || !right_statics.is_empty();
    if !left_claimed || !right_claimed {
        return None;
    }

    let mut replacements = Vec::new();
    replacements.extend(left_repl);
    replacements.extend(right_repl);
    // At least one conjunct must be a replacement — pure-static compounds have
    // their own multi-static splitters and must not be diverted here.
    if replacements.is_empty() {
        return None;
    }

    let mut statics = left_statics;
    statics.extend(right_statics);
    Some((statics, replacements))
}

/// CR 614.6: Split `<subject> can't <P1> and can't <P2>` into the shared subject
/// and the two bare predicates (the leading `can't ` already stripped). Operates
/// on the lowercase view for matching but returns ORIGINAL-case slices of `line`.
///
/// Robust against a subject that itself contains "can't" (e.g. "A creature that
/// can't block can't become untapped and can't …"): the conjunction `" and can't
/// "` is the unambiguous structural boundary between the two prohibitions, so we
/// split there FIRST to isolate P2, then take the LAST `" can't "` within the
/// left half as the P1 boundary. `rfind` here is a deliberate structural
/// last-boundary scan, not a parsing-dispatch substring test — the predicate
/// tokens themselves are parsed by the layer parsers the caller invokes.
fn split_dual_cant_clause<'a>(line: &'a str, lower: &str) -> Option<(&'a str, &'a str, &'a str)> {
    const CONJ: [&str; 2] = [" and can't ", " and can\u{2019}t "];
    const CANT: [&str; 2] = [" can't ", " can\u{2019}t "];

    // Trim a single trailing period (on both views, so byte offsets stay aligned).
    // allow-noncombinator: structural trailing-punctuation trim on a whole line, not parsing dispatch.
    let lower = lower.strip_suffix('.').unwrap_or(lower);
    let line = &line[..lower.len()];

    // Conjunction boundary: "<left> and can't <P2>". The conjunction divider is
    // located structurally so the two prohibition predicates can each be handed to
    // the layer parsers; the predicate tokens themselves are parsed there.
    // allow-noncombinator: structural conjunction-boundary scan, not parsing dispatch.
    let (conj_pos, conj_len) = CONJ
        .iter()
        .find_map(|needle| lower.find(needle).map(|pos| (pos, needle.len())))?;
    let left_lower = &lower[..conj_pos];
    let p2 = line[conj_pos + conj_len..].trim();

    // P1 boundary: the LAST " can't " inside the left half, so a subject that
    // itself contains "can't" (e.g. "A creature that can't block …") is not
    // truncated. The subject is everything before it; P1 everything after.
    // allow-noncombinator: structural last-boundary scan, not parsing dispatch.
    let (cant_pos, cant_len) = CANT
        .iter()
        .find_map(|needle| left_lower.rfind(needle).map(|pos| (pos, needle.len())))?;
    let subject = line[..cant_pos].trim();
    let p1 = line[cant_pos + cant_len..conj_pos].trim();

    if subject.is_empty() || p1.is_empty() || p2.is_empty() {
        return None;
    }
    Some((subject, p1, p2))
}

/// CR 607.2d: Reconcile self-chosen type statics with the source's linked
/// persisted choice.
/// CR 614.1c + CR 608.2d: Cards like Banner of Kinship parse "as ~ enters,
/// choose a creature type" and "~ enters with a fellowship counter … for each
/// creature you control of the chosen type" as two Moved replacements. The
/// counter count depends on the persisted choice, so it must chain after the
/// `Choose` post-entry effect — not fold into `enter_with_counters` during the
/// replacement pipeline.
fn reconcile_choose_then_chosen_dependent_etb_counters(result: &mut ParsedAbilities) {
    let choose_idx = result.replacements.iter().position(|replacement| {
        replacement.event == ReplacementEvent::Moved
            && replacement
                .execute
                .as_ref()
                .is_some_and(|def| is_persisted_as_enters_choice(def))
    });
    let counter_idx = result.replacements.iter().position(|replacement| {
        replacement.event == ReplacementEvent::Moved
            && replacement
                .execute
                .as_ref()
                .is_some_and(|def| is_chosen_dependent_self_etb_counter(def))
    });
    let (Some(choose_idx), Some(counter_idx)) = (choose_idx, counter_idx) else {
        return;
    };
    if choose_idx == counter_idx {
        return;
    }

    let counter_repl = result.replacements.remove(counter_idx);
    let choose_idx = if counter_idx < choose_idx {
        choose_idx - 1
    } else {
        choose_idx
    };
    let Some(counter_exec) = counter_repl.execute else {
        return;
    };
    let choose_repl = &mut result.replacements[choose_idx];
    if let Some(ref mut choose_exec) = choose_repl.execute {
        append_sub_ability(choose_exec, *counter_exec);
    }
}

fn is_persisted_as_enters_choice(def: &AbilityDefinition) -> bool {
    matches!(&*def.effect, Effect::Choose { persist: true, .. })
}

fn is_chosen_dependent_self_etb_counter(def: &AbilityDefinition) -> bool {
    match &*def.effect {
        Effect::PutCounter {
            target: TargetFilter::SelfRef,
            count,
            ..
        } => quantity_expr_uses_chosen_filter(count),
        _ => false,
    }
}

fn quantity_expr_uses_chosen_filter(expr: &QuantityExpr) -> bool {
    quantity_expr_uses_filter_prop(expr, &|prop| {
        matches!(
            prop,
            FilterProp::IsChosenCreatureType | FilterProp::IsChosenColor
        )
    })
}

fn quantity_expr_uses_filter_prop(
    expr: &QuantityExpr,
    pred: &impl Fn(&FilterProp) -> bool,
) -> bool {
    match expr {
        QuantityExpr::Ref { qty } => quantity_ref_uses_filter_prop(qty, pred),
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => quantity_expr_uses_filter_prop(inner, pred),
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => exprs
            .iter()
            .any(|inner| quantity_expr_uses_filter_prop(inner, pred)),
        QuantityExpr::Fixed { .. } => false,
        QuantityExpr::UpTo { max } => quantity_expr_uses_filter_prop(max, pred),
        QuantityExpr::Power { exponent, .. } => quantity_expr_uses_filter_prop(exponent, pred),
        QuantityExpr::Difference { left, right } => {
            quantity_expr_uses_filter_prop(left, pred)
                || quantity_expr_uses_filter_prop(right, pred)
        }
    }
}

fn quantity_ref_uses_filter_prop(qty: &QuantityRef, pred: &impl Fn(&FilterProp) -> bool) -> bool {
    match qty {
        QuantityRef::ObjectCount { filter }
        | QuantityRef::ObjectCountDistinct { filter, .. }
        | QuantityRef::ObjectCountBySharedQuality { filter, .. }
        | QuantityRef::CountersOnObjects { filter, .. }
        | QuantityRef::Aggregate { filter, .. }
        | QuantityRef::ControlledByEachPlayer { filter, .. }
        | QuantityRef::DistinctColorsAmongPermanents { filter }
        | QuantityRef::DistinctCounterKindsAmong { filter }
        | QuantityRef::EnteredThisTurn { filter } => target_filter_uses_filter_prop(filter, pred),
        QuantityRef::DistinctCardTypes {
            source: crate::types::ability::CardTypeSetSource::Objects { filter },
        }
        | QuantityRef::DistinctSubtypes {
            source: crate::types::ability::CardTypeSetSource::Objects { filter },
            ..
        } => target_filter_uses_filter_prop(filter, pred),
        _ => false,
    }
}

fn target_filter_uses_filter_prop(
    filter: &TargetFilter,
    pred: &impl Fn(&FilterProp) -> bool,
) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.properties.iter().any(pred),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => filters
            .iter()
            .any(|inner| target_filter_uses_filter_prop(inner, pred)),
        TargetFilter::Not { filter } => target_filter_uses_filter_prop(filter, pred),
        _ => false,
    }
}

fn append_sub_ability(chain: &mut AbilityDefinition, tail: AbilityDefinition) {
    let mut cursor = chain;
    while let Some(ref mut next) = cursor.sub_ability {
        cursor = next;
    }
    cursor.sub_ability = Some(Box::new(tail));
}

fn reconcile_self_chosen_type_statics(result: &mut ParsedAbilities, types: &[String]) {
    let persisted_kind = chosen_subtype_kind_from_persisted_choice(result);

    // CR 607.2d + CR 205.3 + CR 601.2f: A cost reducer that refers to "the chosen
    // type" ("Spells of the chosen type you cast cost {W}{U}{B}{R}{G} less",
    // Morophon) is LINKED to the same card's "choose a [value]" clause and must
    // match whatever that clause picks. `static_helpers` defaults a bare-"spells"
    // base (no creature type word) to `IsChosenCardType` — correct only for
    // card-type choosers (Cloud Key / Umori / Stenn) — so a creature-type chooser
    // is mis-discriminated and the reduction never matches a spell. Realign here,
    // the only point with cross-clause visibility, keying STRICTLY on the
    // persisted choice: a creature that chooses a CARD type (Umori) returns a
    // card-type kind and must keep `IsChosenCardType`, so the creature-card-type
    // fallback below must not drive this.
    if matches!(persisted_kind, Some(ChosenSubtypeKind::CreatureType)) {
        for static_def in &mut result.statics {
            if let crate::types::statics::StaticMode::ModifyCost {
                spell_filter: Some(filter),
                ..
            } = &mut static_def.mode
            {
                retarget_chosen_card_type_to_creature_type(filter);
            }
        }
        retarget_creature_type_choice_dig_filters(result);
    }

    let Some(chosen_kind) = persisted_kind.or_else(|| chosen_kind_from_card_types(types)) else {
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

/// CR 607.2d: Within a creature-type chooser's cost-modifier spell filter,
/// rewrite the card-type chosen-discriminator (`IsChosenCardType`) to the
/// creature-type one (`IsChosenCreatureType`) so "the chosen type" matches the
/// linked creature-type choice. CR 205.3: the linked choice is a creature
/// subtype, so it must be matched against subtypes. Recurses through every
/// nested-filter `TargetFilter` variant (`And`/`Or`/`Not`/`TrackedSetFiltered`),
/// e.g. a typed filter ANDed with `HasChosenName`.
fn retarget_chosen_card_type_to_creature_type(filter: &mut TargetFilter) {
    use crate::types::ability::FilterProp;
    match filter {
        TargetFilter::Typed(tf) => {
            for prop in &mut tf.properties {
                if matches!(prop, FilterProp::IsChosenCardType) {
                    *prop = FilterProp::IsChosenCreatureType;
                }
            }
        }
        TargetFilter::And { filters } | TargetFilter::Or { filters } => filters
            .iter_mut()
            .for_each(retarget_chosen_card_type_to_creature_type),
        TargetFilter::Not { filter } | TargetFilter::TrackedSetFiltered { filter, .. } => {
            retarget_chosen_card_type_to_creature_type(filter)
        }
        _ => {}
    }
}

/// CR 608.2c: Dig/reveal continuations after "Choose a creature type" refer to
/// creature subtypes ("cards of the chosen type", For the Ancestors). The bare
/// "cards" base defaults to `IsChosenCardType`; realign those dig filters once
/// the persisted choice is known to be creature-type.
fn retarget_creature_type_choice_dig_filters(result: &mut ParsedAbilities) {
    for ability in &mut result.abilities {
        retarget_creature_type_choice_dig_filters_in_ability(ability);
    }
    for trigger in &mut result.triggers {
        if let Some(execute) = trigger.execute.as_mut() {
            retarget_creature_type_choice_dig_filters_in_ability(execute);
        }
    }
}

fn retarget_creature_type_choice_dig_filters_in_ability(def: &mut AbilityDefinition) {
    if let Effect::Dig { filter, .. } = &mut *def.effect {
        retarget_chosen_card_type_to_creature_type(filter);
    }
    if let Some(sub) = def.sub_ability.as_mut() {
        retarget_creature_type_choice_dig_filters_in_ability(sub);
    }
}

/// CR 702.26a + CR 603.7c: Upgrade bare one-shot `PhaseOut` ETB effects that
/// carry a host-bound re-entry rider ("Tap that creature as it phases in this
/// way", Oubliette) into PhaseOut + CantPhaseIn + delayed PhaseIn/Tap.
fn reconcile_host_bound_phase_outs(result: &mut ParsedAbilities) {
    for ability in &mut result.abilities {
        reconcile_host_bound_phase_outs_in_ability(ability);
    }
    for trigger in &mut result.triggers {
        if let Some(execute) = trigger.execute.as_mut() {
            reconcile_host_bound_phase_outs_in_ability(execute);
        }
    }
}

fn reconcile_host_bound_phase_outs_in_ability(def: &mut AbilityDefinition) {
    let should_upgrade = matches!(*def.effect, Effect::PhaseOut { .. })
        && def
            .sub_ability
            .as_ref()
            .is_some_and(|sub| chain_contains_host_bound_tap_rider(sub.as_ref()));
    if should_upgrade {
        upgrade_host_bound_phase_out_at_head(def);
        return;
    }
    if let Some(sub) = def.sub_ability.as_mut() {
        reconcile_host_bound_phase_outs_in_ability(sub);
    }
}

fn chain_contains_host_bound_tap_rider(def: &AbilityDefinition) -> bool {
    if is_host_bound_phase_in_tap_rider_node(def) {
        return true;
    }
    def.sub_ability
        .as_ref()
        .is_some_and(|sub| chain_contains_host_bound_tap_rider(sub.as_ref()))
}

fn upgrade_host_bound_phase_out_at_head(def: &mut AbilityDefinition) {
    let Effect::PhaseOut { target } = *def.effect.clone() else {
        return;
    };

    let (tail, removed_rider) = remove_host_bound_tap_rider_from_chain(def.sub_ability.take());
    if !removed_rider {
        def.sub_ability = tail;
        return;
    }

    let cant_phase_in = Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::new(StaticMode::CantPhaseIn)
            .affected(TargetFilter::ParentTarget)
            .modifications(vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::CantPhaseIn,
            }])],
        duration: Some(Duration::UntilHostLeavesPlay),
        target: Some(TargetFilter::ParentTarget),
    };

    let mut return_ability = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PhaseIn {
            target: TargetFilter::ParentTarget,
        },
    );
    return_ability.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SetTapState {
            target: TargetFilter::ParentTarget,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        },
    )));

    let mut delayed = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::WhenLeavesPlayFiltered {
                filter: TargetFilter::SelfRef,
            },
            effect: Box::new(return_ability),
            uses_tracked_set: false,
        },
    );
    delayed.sub_ability = tail;

    let mut lock = AbilityDefinition::new(AbilityKind::Spell, cant_phase_in);
    lock.sub_ability = Some(Box::new(delayed));

    *def.effect = Effect::PhaseOut { target };
    def.sub_ability = Some(Box::new(lock));
}

/// Remove only the host-bound tap rider node, preserving any intervening siblings.
fn remove_host_bound_tap_rider_from_chain(
    chain: Option<Box<AbilityDefinition>>,
) -> (Option<Box<AbilityDefinition>>, bool) {
    let Some(mut node) = chain else {
        return (None, false);
    };

    if is_host_bound_phase_in_tap_rider_node(&node) {
        return (node.sub_ability.take(), true);
    }

    if let Some(sub) = node.sub_ability.take() {
        let (new_sub, found) = remove_host_bound_tap_rider_from_chain(Some(sub));
        node.sub_ability = new_sub;
        if found {
            return (Some(node), true);
        }
    }

    (Some(node), false)
}

fn is_host_bound_phase_in_tap_rider_node(def: &AbilityDefinition) -> bool {
    if !matches!(
        def.effect.as_ref(),
        Effect::SetTapState {
            state: TapStateChange::Tap,
            target: TargetFilter::ParentTarget,
            ..
        }
    ) {
        return false;
    }
    def.description
        .as_deref()
        .is_some_and(host_bound_phase_in_tap_phrase)
}

fn host_bound_phase_in_tap_phrase(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    scan_contains(&lower, "as it phases in this way")
        || scan_contains(&lower, "as that creature phases in this way")
        || scan_contains(&lower, "as that permanent phases in this way")
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
            choice_type: ChoiceType::CreatureType { .. },
            persist: true,
            ..
        } => Some(ChosenSubtypeKind::CreatureType),
        Effect::Choose {
            choice_type: ChoiceType::BasicLandType,
            persist: true,
            ..
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
                None,
                None,
            ));
        result.abilities.push(make_unimplemented(unmodeled_tail));
        return true;
    }

    false
}

/// CR 611.3a + CR 702: Distribute a "The same is true for <keyword list>"
/// continuation across a graveyard-keyword-gated static grant (Cairn Wanderer).
///
/// The modeled sentence "As long as a creature card with <kw> is in a graveyard,
/// this creature has <kw>" parses to ONE gated `StaticDefinition` (grant
/// `AddKeyword { kw }` gated on `IsPresent(<kw>-card in a graveyard)`). Each
/// keyword in the trailing list clones that template, swapping BOTH the granted
/// keyword and the gate condition's `WithKeyword`, so each keyword is granted
/// independently — only while a creature card WITH that keyword is in a graveyard
/// (CR 611.3a per-keyword conditional continuous static). This is the plain-
/// `StaticDefinition` analogue of `attach_same_is_true_keywords` (which operates
/// on the trigger path's `GenericEffect`), reusing the same keyword-list parser
/// (`try_parse_same_is_true_continuation`) and keyword-rewrite building block
/// (`rewrite_condition_keyword`) so it covers the whole class, not one card.
///
/// Returns `false` (leaving the line for the generic dispatch) when the modeled
/// sentence, the keyword list, or the gated template cannot be recovered. Any
/// continuation keyword that resolves to `Keyword::Unknown` (unqualified
/// `protection` / `landwalk`) is emitted as an explicit `Unimplemented` residual
/// rather than an inert `AddKeyword(Unknown)` static, so it stays a loud
/// unsupported gap in coverage instead of silently reading as supported.
fn push_graveyard_keyword_same_is_true_tail(
    result: &mut ParsedAbilities,
    line: &str,
    lower: &str,
) -> bool {
    let Some((modeled_sentence, tail)) =
        split_same_is_true_static_tail(line, lower, parse_graveyard_keyword_grant_sentence)
    else {
        return false;
    };
    // No cant-cast gate applies to a graveyard-keyword grant, so the raw-line /
    // card-name gate params are None (matching the other non-cant-cast callers).
    let mut statics =
        parse_static_line_with_graveyard_keyword_continuation(modeled_sentence, None, None);
    // The modeled sentence must yield exactly the gated keyword grant to clone.
    let Some(template) = statics.first().cloned() else {
        return false;
    };
    // CR 611.3a: only distribute a genuinely gated grant. If the modeled sentence
    // ever parsed without its graveyard-presence condition, fall through to the
    // generic path rather than cloning an UNGATED grant per keyword — that would
    // reintroduce the unconditional over-grant this distribution exists to remove.
    if template.condition.is_none() {
        return false;
    }
    let Some(keywords) = try_parse_same_is_true_continuation(tail) else {
        return false;
    };
    // CR 611.3a coverage-honesty: a continuation keyword that resolves to
    // `Keyword::Unknown` (an unqualified `protection` / `landwalk` — those keyword
    // abilities require a quality/subtype that a bare continuation clause does not
    // supply) is NOT semantically modeled. Cloning it into an
    // `AddKeyword(Keyword::Unknown(_))` static would still read as Continuous-mode
    // "supported" in `game/coverage.rs` (which checks static mode + child
    // grant-abilities/triggers, not the granted keyword's identity), letting the
    // card become coverage-supported while that clause does nothing. Keep those as
    // an explicit `Unimplemented` residual so they remain a loud unsupported gap.
    let mut unqualified: Vec<String> = Vec::new();
    for keyword in &keywords {
        if let Keyword::Unknown(name) = keyword {
            unqualified.push(name.clone());
            continue;
        }
        let mut new_def = template.clone();
        for modification in &mut new_def.modifications {
            if let ContinuousModification::AddKeyword { keyword: kw } = modification {
                *kw = keyword.clone();
            }
        }
        if let Some(condition) = &mut new_def.condition {
            rewrite_condition_keyword(condition, keyword);
        }
        statics.push(new_def);
    }
    result.statics.extend(statics);
    if !unqualified.is_empty() {
        result.abilities.push(make_unimplemented(&format!(
            "the same is true for {}",
            unqualified.join(", ")
        )));
    }
    true
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
        // CR 608.2c + CR 601.2b: An inverted additional-cost / gift "instead if"
        // is folded to the dedicated `AdditionalCostPaidInstead` by the chain's
        // `strip_additional_cost_conditional`. Defer the whole line so the chain
        // builds the conditional else_ability (Cinder Strike) rather than the
        // line-level path dropping the unrecognized condition here.
        if parse_additional_cost_instead_condition_fragment(condition_text).is_some() {
            return (text.to_string(), None, false);
        }
        // CR 614.1a + CR 608.2c: an inverted "instead if <cond>" followed by a further
        // printed instruction (Throw from the Saddle: "… instead if it's a Mount. Then it
        // deals damage …") is an INTRA-CHAIN override, not a whole-line replacement. The
        // condition region then carries an internal sentence boundary; defer the whole line
        // to the chain parser (mirrors the pattern-3 intra-chain guard at the `" instead"`
        // branch below) so the trailing independent clause is not swallowed. Operates on the
        // post-keyword-exclusion joined `effect_line`, so trailing keyword lines (Flashback)
        // never reach here.
        if condition_text.contains('.') {
            // allow-noncombinator: structural sentence-boundary split (mirrors the
            // pattern-3 `before_trim.contains('.')` guard below), not parsing dispatch
            return (text.to_string(), None, false);
        }
        // CR 608.2c + CR 614.1a: A multi-sentence effect line
        // ("[prior sentence]. [effect] instead if <cond>", e.g. Steer Clear) is an
        // INTRA-CHAIN override — the "instead" replaces only the trailing sentence's
        // effect, not the whole line, and its condition ("you controlled a Mount as
        // you cast this spell") is owned by the chain-level `parse_condition_text`
        // recognizers, not the line-level `parse_inner_condition`. Defer the whole
        // line to the chain parser (mirrors the pattern-3 `before_trim.contains('.')`
        // guard below) so `try_parse_generic_instead_clause` builds the conditional
        // sub-ability and the prior sentence is preserved.
        if before.original.trim().trim_end_matches('.').contains('.') {
            // allow-noncombinator: structural sentence-boundary split, not parsing dispatch
            return (text.to_string(), None, false);
        }
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

    if is_speed_unlock_sentence(&lower) {
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
        || is_draft_matters_sentence(line)
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

    // CR 111.3 + CR 111.4: mask double-quoted spans (a created token/permanent's
    // defined inline ability text) before spell-line static classification, so a
    // token's quoted "can't block" etc. doesn't mark this resolution line static.
    // This function is already spell-scoped (caller is inside `if is_spell {`).
    // The adjacent is_replacement_pattern check below stays on the UNMASKED text.
    //
    // Gate the mask on a token/permanent-creation verb being present: only then is
    // a quoted span an inline ability *of the created object* ("create ... with
    // \"…\""). On a line with no creation verb the quote is instead a granted-
    // ability payload ("…perpetually gain \"This spell costs {1} less\""), whose
    // inner static shape is load-bearing for routing — masking it there misroutes
    // the grant (coverage regression: Circadian Struggle, Absorb Energy).
    let static_view = if scan_contains(&effect_lower, "create") {
        crate::parser::oracle_nom::primitives::strip_double_quoted_spans(&effect_lower)
    } else {
        std::borrow::Cow::Borrowed(effect_lower.as_str())
    };
    if is_static_pattern(&static_view) && !should_defer_spell_to_effect(&effect_lower) {
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
        || lower_starts_with(&lower, "mayhem ")
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
        // CR 702.186a/b: "∞ — [Ability]" is the Infinity static ability; the
        // ∞ keyword maps to the harnessed gate ("as long as this permanent is
        // harnessed, it has [Ability]"). `strip_ability_word_with_name` already
        // splits the `∞ — ` prefix generically, so this only needs the mapping.
        // allow-noncombinator: semantic mapping after ability-word parser has classified the word
        "∞" => Some(StaticCondition::SourceIsHarnessed),
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
                    filter: None,
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
        // CR 702.186b: the ∞ ability word gates its triggered ability on the
        // harnessed designation.
        StaticCondition::SourceIsHarnessed => Some(TriggerCondition::SourceIsHarnessed),
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

fn parse_strive_cost_line(line: &str) -> Option<ManaCost> {
    let stripped = strip_reminder_text(line.trim());
    let (ability_word, effect_text) = strip_ability_word_with_name(&stripped)?;
    if ability_word != "strive" {
        return None;
    }

    let effect_lower = effect_text.to_lowercase();
    let ((), rest_original) = nom_on_lower(&effect_text, &effect_lower, |i| {
        value((), tag("this spell costs ")).parse(i)
    })?;
    let (cost, rest_original) = parse_mana_symbols(rest_original)?;
    let rest_lower = rest_original.to_lowercase();
    nom_on_lower(rest_original, &rest_lower, |i| {
        value(
            (),
            all_consuming((
                tag(" more to cast for each target beyond the first"),
                opt(tag(".")),
                multispace0,
            )),
        )
        .parse(i)
    })?;
    Some(cost)
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
            let mut restrictions = constraints.restrictions;
            restrictions.push(ActivationRestriction::LevelCounterRange { minimum, maximum });
            def.activation_restrictions = restrictions;
            extract_cost_reduction_from_chain(&mut def);
            extract_mana_spend_trigger_from_chain(&mut def);
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
            extract_mana_spend_trigger_from_chain(&mut def);
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
        if let Some(cost) = parse_strive_cost_line(raw) {
            result.strive_cost = Some(cost);
            break;
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
        let activation_timing_parenthetical_owned =
            preserve_activation_timing_parenthetical(raw_line);
        let raw_line: &str = activation_timing_parenthetical_owned
            .as_deref()
            .unwrap_or(raw_line);

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

        let lower = line.to_lowercase();

        // Priority 8b (early): "As an additional cost to cast this spell" — must
        // precede static-pattern classifiers (Priority 7) that match embedded
        // "This spell costs {N} less..." tails on combined lines (Rottenmouth
        // Viper class). Defiler cycle lines share the prefix but route at
        // Priority 6c-defiler instead.
        if lower_starts_with(&lower, "as an additional cost") && !is_defiler_cost_pattern(&lower) {
            let (cost_line, trailing_reduction) =
                split_additional_cost_trailing_spell_reduction(&line, &lower);
            let cost_lower = cost_line.to_lowercase();
            result.additional_cost = parse_additional_cost_line(&cost_lower, cost_line);
            if let Some(reduction_text) = trailing_reduction {
                if let Some(mut def) = parse_static_line(reduction_text) {
                    // CR 702.166a analogue: reduction only applies when the optional
                    // additional cost is declared, not when the player declines it.
                    def.condition = Some(match def.condition {
                        Some(existing) => StaticCondition::And {
                            conditions: vec![existing, StaticCondition::AdditionalCostPaid],
                        },
                        None => StaticCondition::AdditionalCostPaid,
                    });
                    result.statics.push(def);
                }
            }
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

        // Normalize card self-references for static parsing (replace card name with ~).
        let static_line = normalize_self_refs_for_static(&line, card_name);
        let static_line_lower = static_line.to_lowercase();
        // CR 611.3a + CR 702: "As long as a creature card with <kw> is in a
        // graveyard, this creature has <kw>. The same is true for <keyword list>."
        // (Cairn Wanderer) — distribute the gated grant per keyword before the
        // chosen/every-type same-is-true arms (which gap the tail) or the generic
        // static path (which mis-tokenizes the keyword list) can claim the line.
        if push_graveyard_keyword_same_is_true_tail(&mut result, &static_line, &static_line_lower) {
            i += 1;
            continue;
        }
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
            let defs = parse_static_line_with_graveyard_keyword_continuation(
                &static_line,
                Some(raw_line),
                Some(card_name),
            );
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

        if is_speed_unlock_sentence(&lower) {
            let defs = parse_static_line_with_graveyard_keyword_continuation(
                &static_line,
                Some(raw_line),
                Some(card_name),
            );
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

        if is_draft_matters_sentence(&line) {
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
                            parse_static_line_with_graveyard_keyword_continuation(
                                &clause_dot,
                                None,
                                None,
                            ),
                        );
                    }
                }
                i += 1;
                continue;
            }
            // Compound detection (CR 602.5 can't-be-activated, cross-mode conjunctions,
            // life-total locks, etc.) is already owned by `parse_static_line_multi`,
            // which the wrapper below delegates to.
            let defs = parse_static_line_with_graveyard_keyword_continuation(
                &static_line,
                Some(raw_line),
                Some(card_name),
            );
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
            if let Some(colon_pos) = find_activated_colon(rest_original) {
                let cost_text = rest_original[..colon_pos].trim();
                let effect_text = rest_original[colon_pos + 1..].trim();
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
                if !constraints.restrictions.is_empty() {
                    def.activation_restrictions = constraints.restrictions;
                }
                // CR 601.2f: Extract self-referential cost reduction from the terminal
                // sub_ability in the chain (it may be several levels deep).
                extract_cost_reduction_from_chain(&mut def);
                extract_mana_spend_trigger_from_chain(&mut def);
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
            if let Some(colon_pos) = find_activated_colon(rest_original) {
                let cost_text = rest_original[..colon_pos].trim();
                let effect_text = rest_original[colon_pos + 1..].trim();
                let (effect_text, constraints) = strip_activated_constraints(effect_text);
                let cost = parse_oracle_cost(cost_text);
                ctx.subject = None;
                ctx.actor = None;
                let mut def =
                    parse_effect_chain_with_context(&effect_text, AbilityKind::Activated, &mut ctx);
                def.cost = Some(cost);
                def.description = Some(line.to_string());
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
                extract_mana_spend_trigger_from_chain(&mut def);
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
            if let Some(colon_pos) = find_activated_colon(rest_original) {
                let cost_text = rest_original[..colon_pos].trim();
                let effect_text = rest_original[colon_pos + 1..].trim();
                let (effect_text, constraints) = strip_activated_constraints(effect_text);
                let cost = parse_oracle_cost(cost_text);
                ctx.subject = None;
                ctx.actor = None;
                let mut def =
                    parse_effect_chain_with_context(&effect_text, AbilityKind::Activated, &mut ctx);
                def.cost = Some(cost);
                def.description = Some(line.to_string());
                def.activation_restrictions.extend(constraints.restrictions);
                def.activation_restrictions
                    .push(ActivationRestriction::OnlyOnce);
                def.ability_tag = Some(AbilityTag::Exhaust);
                extract_cost_reduction_from_chain(&mut def);
                extract_mana_spend_trigger_from_chain(&mut def);
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 3e2: Power-up — "Power-up — {cost}: {effect}" (CR 702.193a, CR 602.5b).
        // Power-up is a keyword-labeled activated ability (like Exhaust): it can
        // be activated only once per game, and its cost is reduced by the source's
        // mana value if it entered the battlefield this turn. The cost reduction is
        // set from the keyword definition (not parsed from reminder text, which
        // `strip_reminder_text` removes).
        if let Some(((), rest_original)) = nom_on_lower(&line, &lower, |i| {
            value((), alt((tag("power-up \u{2014} "), tag("power-up -- ")))).parse(i)
        }) {
            if let Some(colon_pos) = find_activated_colon(rest_original) {
                let cost_text = rest_original[..colon_pos].trim();
                let effect_text = rest_original[colon_pos + 1..].trim();
                let (effect_text, constraints) = strip_activated_constraints(effect_text);
                let cost = parse_oracle_cost(cost_text);
                ctx.subject = None;
                ctx.actor = None;
                let mut def =
                    parse_effect_chain_with_context(&effect_text, AbilityKind::Activated, &mut ctx);
                def.cost = Some(cost);
                def.description = Some(line.to_string());
                def.activation_restrictions.extend(constraints.restrictions);
                // CR 702.193a: power-up may be activated only once.
                def.activation_restrictions
                    .push(ActivationRestriction::OnlyOnce);
                def.ability_tag = Some(AbilityTag::PowerUp);
                // CR 702.193b + CR 602.2b + CR 601.2f + CR 302.6: the activation cost's
                // generic mana is reduced by the source's mana value if it entered this turn.
                def.cost_reduction = Some(CostReduction {
                    amount_per: 1,
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::SelfManaValue,
                    },
                    condition: Some(ParsedCondition::SourceEnteredThisTurn),
                });
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 3f: Forecast — "Forecast — {cost}: {effect}" (CR 702.57).
        // A forecast ability is an activated ability with three implicit
        // restrictions (CR 702.57a-b): it can be activated only from the card's
        // owner's hand, only during that player's upkeep, and only once each
        // turn. Must run before `is_keyword_cost_line` (which lists "forecast"):
        // there is no `Keyword::Forecast` synthesizer, so without this branch the
        // line is skipped and the ability is silently dropped. Mirrors the
        // Boast/Channel/Exhaust em-dash activated-ability handlers above.
        if let Some(((), rest_original)) = nom_on_lower(&line, &lower, |i| {
            value((), alt((tag("forecast \u{2014} "), tag("forecast -- ")))).parse(i)
        }) {
            if let Some(colon_pos) = find_activated_colon(rest_original) {
                let cost_text = rest_original[..colon_pos].trim();
                let effect_text = rest_original[colon_pos + 1..].trim();
                let (effect_text, constraints) = strip_activated_constraints(effect_text);
                let cost = parse_oracle_cost(cost_text);
                ctx.subject = None;
                ctx.actor = None;
                let mut def =
                    parse_effect_chain_with_context(&effect_text, AbilityKind::Activated, &mut ctx);
                def.cost = Some(cost);
                def.description = Some(line.to_string());
                // CR 702.57a: a forecast ability is activated only from hand.
                def.activation_zone = Some(Zone::Hand);
                def.activation_restrictions.extend(constraints.restrictions);
                // CR 702.57b: only during the owner's upkeep, only once each turn.
                def.activation_restrictions
                    .push(ActivationRestriction::DuringYourUpkeep);
                def.activation_restrictions
                    .push(ActivationRestriction::OnlyOnceEachTurn);
                extract_cost_reduction_from_chain(&mut def);
                extract_mana_spend_trigger_from_chain(&mut def);
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 4: Activated ability — contains ":" with cost-like prefix
        if let Some(colon_pos) = find_activated_colon(&line) {
            let cost_text = line[..colon_pos].trim();
            let effect_text = line[colon_pos + 1..].trim();
            // CR 207.2c (shared label-prefix mechanism, used by ability words
            // like Threshold) + CR 702.186a: the ∞ keyword (NOT an ability word —
            // it is absent from the CR 207.2c list) is likewise followed by
            // ability text after an em-dash and can prefix an activation cost
            // ("∞ — {T}: ..."). `find_activated_colon` strips the label only to
            // locate the colon; the prefix is still in `cost_text` here, so
            // recover the typed gate condition (shared `strip_ability_word_with_name`
            // path serves both forms) to gate this ability.
            let aw_condition = strip_ability_word_with_name(cost_text)
                .and_then(|(aw_name, _)| ability_word_to_condition(&aw_name));
            let (mut def, effect_text) = parse_activated_ability_definition(
                cost_text,
                effect_text,
                &line,
                card_name,
                Some(result.abilities.len()),
                &mut ctx,
            );
            // CR 702.186b: ∞ ("As long as harnessed, it has [ability]") gates an
            // activated ability's legality (the ability is absent while
            // unharnessed) — an activation restriction, NOT an intervening-if
            // `condition` (a resolution-time gate, CR 608.2c + Shelldock Isle
            // ruling, which the engine deliberately does not use for activation
            // legality). Applied AFTER the call because
            // `parse_activated_ability_definition` overwrites
            // `activation_restrictions` from the cost-text constraints.
            if matches!(aw_condition, Some(StaticCondition::SourceIsHarnessed)) {
                def.activation_restrictions
                    .push(ActivationRestriction::SourceIsHarnessed);
            }
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
        // CR 608.2c: "If a [type] enters this way, it enters with …" is a reflexive
        // conditional rider on a non-ETB trigger (Winter Soldier, Reborn Avenger),
        // not a CR 614.1c enters-with replacement head. Skip the replacement
        // interceptor so the line routes through trigger dispatch.
        if has_trigger_prefix(&lower)
            && !is_enters_with_counter_trigger(&lower)
            && scan_contains(&lower, "enters with")
            && !scan_contains(&lower, "enters this way,")
        {
            if let Some(rep_def) = parse_replacement_line(&line, card_name) {
                result.replacements.push(rep_def);
                i += 1;
                continue;
            }
        }

        // CR 603.7a-b: Instant/sorcery text like "Whenever [event] this turn, ..."
        // or "At the beginning of your next upkeep, ..." creates a delayed
        // triggered ability during resolution. It is not a permanent's printed
        // triggered ability, so spell cards must get one chance to route
        // trigger-shaped temporal text through the effect parser before generic
        // trigger dispatch.
        if is_spell && has_trigger_prefix(&lower) {
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
        // the line. Uses the wider flavor-word cap (CR 207.2c) so Universes-Beyond
        // 5-6 word flavor names ("Woman Who Walked the Earth", "Deal with the Black
        // Guardian") strip; the activated branch stays gated on ability-word
        // recognition and the trigger branch re-validates via has_trigger_prefix.
        if let Some((aw_name, effect_text)) = strip_flavor_word_with_name(&line) {
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
                        Some(result.abilities.len()),
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
        // cost for [filter] spells you cast." Alternative-cost-grant static
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

        // Priority 6c-altcost-b: CR 118.9 — "You may cast [filter] by paying {X}
        // rather than paying their mana costs." (Primal Prayers). May also carry a
        // flash rider on the same line.
        if is_cast_spells_alternative_cost_pattern(&lower) {
            let defs = parse_cast_spells_alternative_cost_multi(&line);
            if !defs.is_empty() {
                result.statics.extend(defs);
                i += 1;
                continue;
            }
        }

        // Priority 6c-altcost-c: CR 118.9 + CR 701.59a — "You may collect evidence N
        // rather than pay the mana cost for [filter] spells you cast."
        // Conspiracy Unraveler class. Must run before Priority 7 because
        // `is_spells_alternative_cost_pattern` requires "you may pay " prefix
        // and would miss this verb form.
        if is_collect_evidence_alt_cost_pattern(&lower) {
            if let Some(static_def) = parse_collect_evidence_alt_cost(&line) {
                result.statics.push(static_def);
                i += 1;
                continue;
            }
        }

        // Priority 6c-altcost-d: CR 107.4f — "For each {C} in a cost, you may pay
        // 2 life rather than pay that mana." K'rrik class. Must run before Priority 7
        // because is_static_pattern does not classify this shape.
        if is_pay_life_as_colored_mana_pattern(&lower) {
            let defs = parse_static_line_with_graveyard_keyword_continuation(
                &static_line,
                Some(raw_line),
                Some(card_name),
            );
            if !defs.is_empty() {
                result.statics.extend(defs);
                i += 1;
                continue;
            }
        }

        // Priority 6c-altcost-e: CR 118.9 + CR 702.29a + CR 702.122a —
        // "You may [cost] rather than pay [keyword] cost[s]."
        // New Perspectives (cycling) / Heart of Kiran (crew) / Gavi class.
        if is_alternative_keyword_cost_pattern(&lower) {
            if let Some(static_def) = parse_alternative_keyword_cost(&line) {
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
            let defs = parse_static_line_with_graveyard_keyword_continuation(
                &static_line,
                Some(raw_line),
                Some(card_name),
            );
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

        // Priority 6e: Compound `<subject> can't <P1> and can't <P2>` prohibition
        // whose conjuncts cross parser layers (static and/or replacement).
        // CR 701.26b + CR 614.6: Blossombind class — "Enchanted creature can't
        // become untapped and can't have counters put on it" is two replacement
        // effects (an Untap prevention and an AddCounter prevention). The "can't
        // have counters put on" substring makes Priority 7's `is_static_pattern`
        // fire and consume the whole line, dropping a conjunct. Split on the
        // " and can't " conjunction so each clause reaches BOTH layer parsers and
        // every conjunct is claimed.
        if let Some((statics, replacements)) =
            parse_static_replacement_compound(&static_line, &static_line_lower, card_name)
        {
            result.statics.extend(statics);
            result.replacements.extend(replacements);
            i += 1;
            continue;
        }

        // CR 207.2c + CR 401.5 + CR 601.1a + CR 603.12: an (optionally
        // ability-word-prefixed) top-of-library play/cast permission carrying a
        // reflexive "When you do, <effect>" rider (The Fourth Doctor). Emits
        // the permission static so play-from-library works, and marks the rider
        // as an honest unsupported gap (TriggerMode::Unknown) — the reflexive
        // trigger cannot be correctly scoped until the casting/land-play
        // pipeline records which permission authorized each play (CR 603.12
        // provenance limitation: a global PlayCard trigger cannot distinguish
        // WHICH permission authorized a given play). Must precede Priority 7
        // (the static-only path would silently drop the rider, hiding the gap).
        {
            let permission_line = strip_ability_word(&line).unwrap_or_else(|| line.clone());
            let permission_lower = permission_line.to_lowercase();
            if let Some((perm_text, _)) =
                split_once_on_lower(&permission_line, &permission_lower, ". when you do, ")
            {
                let perm_lower = perm_text.to_lowercase();
                if let Some(static_def) =
                    try_parse_top_of_library_cast_permission(perm_text, &perm_lower)
                {
                    // CR 603.12 (deferred): emit TriggerMode::Unknown so the
                    // rider gap is visible in coverage instead of approximating
                    // incorrect provenance with a rules-incorrect PlayCard
                    // trigger. No context mutation: we do not parse the rider
                    // body here (avoids ctx.subject/actor leakage into
                    // subsequent lines).
                    let rider_gap =
                        TriggerDefinition::new(TriggerMode::Unknown("when you do".to_string()))
                            .description(line.to_string());
                    result
                        .statics
                        .push(static_def.description(line.to_string()));
                    result.triggers.push(rider_gap);
                    i += 1;
                    continue;
                }
            }
        }

        // CR 702.34a: Flashback em-dash / compound self-spell cost-reduction lines.
        // Must run before Priority 7 static patterns: "This spell costs {X} less
        // to cast this way" matches `is_static_pattern` and would swallow the
        // flashback keyword on Visions of Ruin class cards.
        if lower_starts_with(&lower, "flashback") {
            if line.contains('\u{2014}') {
                let lower_clean = lower.trim_end_matches('.').trim();
                if let Some(kw) = parse_keyword_from_oracle(lower_clean) {
                    result.extracted_keywords.push(kw);
                    i += 1;
                    continue;
                }
            } else if let Some((flashback_part, reduction_part)) =
                split_flashback_trailing_self_spell_cost_reduction(&line, &lower)
            {
                let flashback_lower = flashback_part.to_lowercase();
                if let Some(kw) = parse_keyword_from_oracle(&flashback_lower) {
                    result.extracted_keywords.push(kw);
                }
                if let Some(def) =
                    parse_flashback_trailing_self_spell_cost_reduction(reduction_part)
                {
                    result.statics.push(def);
                }
                i += 1;
                continue;
            }
        }

        // Priority 7: Static/continuous patterns
        // CR 611.2a + CR 611.3a: On permanents, "creatures you control get +1/+1"
        // is a static ability (CR 611.3a). On instants/sorceries, lines with an
        // explicit duration ("until end of turn", "this turn") are one-shot
        // continuous effects from spell resolution (CR 611.2a) and must reach the
        // effect parser at Priority 9. Damage-verb lines are also deferred because
        // parse_effect_chain handles embedded statics via split_clause_sequence.
        //
        // CR 111.3 + CR 111.4: a double-quoted span is an inline granted ability of
        // a created token/permanent (the token's defined "text"), not the host
        // line's own static clause; mask it before spell-line static classification
        // so e.g. a token's "This token can't block." doesn't route the whole
        // sorcery to the static parser. Spell-scoped only — the masked view feeds
        // the gate predicate exclusively; every replacement gate below and the
        // static_line passed to parse_static_line* stay on the UNMASKED text.
        //
        // Gate on a creation verb: only "create ... with \"…\"" makes the quote an
        // inline ability of the created object. Without one, the quote is a granted-
        // ability payload ("…perpetually gain \"This spell costs {1} less\"") whose
        // inner static shape is load-bearing for routing — masking it there
        // misroutes the grant (coverage regression: Circadian Struggle, Absorb
        // Energy). Non-creation lines therefore keep the UNMASKED baseline view.
        let static_classify_view = if is_spell && scan_contains(&lower, "create") {
            crate::parser::oracle_nom::primitives::strip_double_quoted_spans(&lower)
        } else {
            std::borrow::Cow::Borrowed(lower.as_str())
        };
        if is_static_pattern(&static_classify_view) {
            if result.strive_cost.is_some() && parse_strive_cost_line(&line).is_some() {
                i += 1;
                continue;
            }
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
            } else if is_enters_with_counter_replacement_line(&lower) {
                // CR 614.1c + CR 614.12: distributive "[Other/each] [type] you
                // control enter(s) with [an additional] [counter] on them [for
                // each …]" lines (Gev, Scaled Scorch) are ETB-with-counter
                // replacement effects, but their leading "[type] you control …"
                // subject also matches `is_static_pattern`. Route them to the
                // replacement parser first; a line that is not actually an
                // enters-with-counter replacement returns `None` and falls
                // through to the static parser below.
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
                    let mut defs = parse_static_line_with_graveyard_keyword_continuation(
                        &effect_static,
                        None,
                        None,
                    );
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
                                parse_static_line_with_graveyard_keyword_continuation(
                                    &clause_dot,
                                    None,
                                    None,
                                ),
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
                                parse_static_line_with_graveyard_keyword_continuation(
                                    &clause_dot,
                                    None,
                                    None,
                                ),
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
                let defs = parse_static_line_with_graveyard_keyword_continuation(
                    &static_line,
                    Some(raw_line),
                    Some(card_name),
                );
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
                    selection: crate::types::ability::TargetSelectionMode::Chosen,
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
                    prevention_duration: None,
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
            // CR 208.2b + CR 614.1c + CR 614.12a: modal "As ~ enters, it becomes
            // your choice of [P/T profiles]" as-enters replacement (Primal Plasma,
            // Primal Clay, Corrupted Shapeshifter, Aquamorph Entity). This is a
            // single-sentence replacement line with NO bullet block, so the
            // Priority-1 `OracleBlockAst::AsEntersAnchorWordModal` block parser
            // never fires — it must be lowered here. [G2] It MUST run BEFORE the
            // generic `parse_replacement_sentence_sequence` / `parse_replacement_line`
            // parsers so those don't claim the "becomes your choice of" line as a
            // plain choice/animate and drop the per-mode gated statics.
            if is_as_enters_becomes_choice_pattern(&lower)
                && lower_as_enters_becomes_choice_modal(&line, &mut result)
            {
                i += 1;
                continue;
            }
            // CR 614.1c + CR 708.11: dual "As ~ enters[ or is turned face up],
            // put X +1/+1 counters on it, where X is …" (Crowd-Control Warden).
            // A single sentence that never splits on `.`, so it needs a
            // multi-emit-into-`result` path (one `Moved`/Battlefield + one
            // `TurnFaceUp` replacement sharing the PutCounter execute). Runs BEFORE
            // the generic sequence/line parsers, whose per-arm parsers each return
            // one definition and cannot emit the dual pair. The tight
            // PutCounter-SelfRef guard makes it fall through on any non-counter
            // as-enters line, so the choose/becomes/enters-with siblings are safe.
            if lower_as_enters_or_face_up_counters(&line, &mut result) {
                i += 1;
                continue;
            }
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
            // CR 207.2c: An ability word (e.g. "Venom Blast —") is an italicized
            // flavor marker with no rules meaning — its replacement body must
            // parse through the ordinary replacement machinery. Strip the
            // prefix and retry so named static-replacement ability words
            // (Spider-Woman's "Venom Blast — Artifacts and creatures your
            // opponents control enter tapped.") reach the external-entry parser
            // exactly as the unprefixed Blind Obedience / Authority of the
            // Consuls lines do.
            if let Some(effect_text) = strip_ability_word(&line) {
                if let Some(rep_defs) = parse_replacement_sentence_sequence(&effect_text, card_name)
                {
                    result.replacements.extend(rep_defs);
                    i += 1;
                    continue;
                }
                if let Some(rep_def) = parse_replacement_line(&effect_text, card_name) {
                    result.replacements.push(rep_def);
                    i += 1;
                    continue;
                }
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

        // Priority 8c-strive: Skip strive lines (cost already extracted in pre-parse above).
        // Must run before Priority 9 (spell imperative catch-all) which would otherwise
        // consume the entire "Strive — This spell costs..." line as an unimplemented ability.
        if result.strive_cost.is_some() && parse_strive_cost_line(&line).is_some() {
            i += 1;
            continue;
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

        // CR 702.187b: Mayhem {cost} — parse mana cost from Oracle text, same as
        // Harmonize. MTGJSON's keywords array carries only the bare "Mayhem"
        // name, so the cost is extracted here. Must run before the spell
        // imperative catch-all so the line is a keyword, not an effect.
        if lower_starts_with(&lower, "mayhem ") {
            if let Some(mayhem_kw) = parse_mayhem_keyword(&line) {
                result.extracted_keywords.push(mayhem_kw);
                i += 1;
                continue;
            }
        }

        // Priority 8f: Kicker / Multikicker / Replicate cost lines — must run BEFORE Priority 9
        // (spell catch-all) so these keyword declarations on spell cards don't become Unimplemented.
        // We cannot use is_keyword_cost_line here because it would also catch "flashback"
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
            // CR 702.29a/e + CR 702.27a: Keyword-cost lines (cycling, flashback,
            // suspend, …) are not spell resolution instructions. Without this
            // guard, a sorcery whose Oracle text prints a spell effect followed
            // by a cycling line (Fractured Sanity, Decree of Justice) routes
            // "Cycling {cost}" through the spell catch-all and produces an
            // `Unimplemented` spell ability instead of extracting the keyword
            // for `synthesize_cycling`. Continuation-line protection already
            // lives in `is_spell_resolution_instruction_line`; this covers the
            // case where the keyword-cost line is its own main-loop iteration.
            if is_keyword_cost_line(&lower) {
                if let Some(kw) = parse_keyword_from_oracle(&lower) {
                    result.extracted_keywords.push(kw);
                    i += 1;
                    continue;
                }
            }

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
                    || lower_starts_with(&next_prepared.effect_text.to_lowercase(), "flashback")
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

        // CR 702.138a: Escape is extracted by the generic keyword-cost guards —
        // the `is_spell` guard above (Priority 9) for instants/sorceries and the
        // `is_keyword_cost_line` guard below (Priority 13) for permanents — via the
        // `escape—` branch registered in `parse_keyword_from_oracle`, alongside its
        // evoke/embalm/eternalize/escalate em-dash siblings. No dedicated intercept
        // is needed here.

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
        if lower_starts_with(&lower, "flashback") {
            if let Some((flashback_part, reduction_part)) =
                split_flashback_trailing_self_spell_cost_reduction(&line, &lower)
            {
                let flashback_lower = flashback_part.to_lowercase();
                if let Some(kw) = parse_keyword_from_oracle(&flashback_lower) {
                    result.extracted_keywords.push(kw);
                }
                if let Some(def) =
                    parse_flashback_trailing_self_spell_cost_reduction(reduction_part)
                {
                    result.statics.push(def);
                }
                i += 1;
                continue;
            }
        }
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
                let mut defs = parse_static_line_with_graveyard_keyword_continuation(
                    &effect_static,
                    None,
                    None,
                );
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
        let defs = parse_static_line_with_graveyard_keyword_continuation(
            &static_line,
            Some(raw_line),
            Some(card_name),
        );
        if !defs.is_empty() {
            result.statics.extend(defs);
            i += 1;
            continue;
        }

        // Priority 14a: Nom dispatch — try effect, trigger, static, and replacement
        // sub-parsers. Returns the full AbilityDefinition so that fields beyond
        // `effect` (e.g. `distribute`, `multi_target`) are preserved.
        let nom_def = dispatch_line_nom(&line, card_name, ctx.host_self_reference.clone());
        if !matches!(*nom_def.effect, Effect::Unimplemented { .. }) {
            result.abilities.push(nom_def);
            i += 1;
            continue;
        }

        // Priority 15: Final fallback — the unimplemented def already carries
        // diagnostic info from dispatch_line_nom; push it as-is.
        tracing::debug!(oracle_text = line, "unimplemented ability line");
        result.abilities.push(nom_def);
        i += 1;
    }

    reconcile_choose_then_chosen_dependent_etb_counters(&mut result);
    reconcile_self_chosen_type_statics(&mut result, types);
    reconcile_host_bound_phase_outs(&mut result);

    // Architectural rule: the parser must never silently discard Oracle
    // text. Run the swallow audit against the parsed result so any unrep-
    // resented clause surfaces as a parse_warning instead of disappearing
    // (Phase 1: observability only — see swallow_check.rs for detector
    // catalog and Phase 2 demotion plan).
    let mut swallow_diags = Vec::new();
    // Draft-time "draft matters" lines (CR 905) are intentionally consumed as
    // no-ops by `is_draft_matters_sentence` — they never produce a parsed
    // ability, so the swallow detectors must not scan them (their "you may",
    // "if you do", and "as long as" markers would otherwise be reported as
    // swallowed clauses). Strip them before the audit; constructed-play lines
    // on the same card remain and are still checked. Cards with no draft text
    // (the overwhelming majority) feed the unmodified Oracle text unchanged.
    let swallow_text;
    let swallow_input: &str = if oracle_text.lines().any(is_draft_matters_sentence) {
        swallow_text = oracle_text
            .lines()
            .filter(|line| !is_draft_matters_sentence(line))
            .collect::<Vec<_>>()
            .join("\n");
        &swallow_text
    } else {
        oracle_text
    };
    super::swallow_check::check_swallowed_clauses(swallow_input, &result, &mut swallow_diags);
    for d in swallow_diags {
        ctx.push_diagnostic(d);
    }

    parsed_abilities_to_doc_ir(result, oracle_text, card_name, &mut ctx)
}

fn activation_zone_from_self_cost(cost: &AbilityCost) -> Option<Zone> {
    match cost {
        AbilityCost::Discard {
            self_scope: crate::types::ability::DiscardSelfScope::SourceCard,
            ..
        } => Some(Zone::Hand),
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
    current_ability_index: Option<usize>,
    ctx: &mut ParseContext,
) -> (AbilityDefinition, String) {
    let (effect_text, constraints) = strip_activated_constraints(effect_text);
    // CR 207.2c / CR 207.2d: drop a leading ability-/flavor-word label so the cost
    // after the em-dash parses (covers 5–6-word Universes-Beyond flavor names that
    // exceed the 4-word ability-word cap, e.g. "The Most Important Punch in History
    // — {1}{G}, {T}"). No-op when the label was already stripped upstream
    // (Priority-6b path) or absent.
    let cost_text = strip_activated_cost_label(cost_text).unwrap_or(cost_text);
    let normalized_cost_text = normalize_self_refs_for_static(cost_text, card_name);
    let cost = parse_oracle_cost(&normalized_cost_text);

    // CR 608.2k: expose this ability's exile-cost source zone so the effect
    // parser can disambiguate "the exiled card" as a cost-paid-object
    // reference. Restored after the effect parse — no leak to sibling abilities.
    let prev_exile_zone = ctx.current_ability_exile_cost_zone.take();
    ctx.current_ability_exile_cost_zone = non_self_exile_cost_zone(&cost);
    // CR 707.9a: thread the activated-ability index so "except it has this
    // ability" inside the effect body resolves to RetainPrintedAbilityFromSource.
    let prev_ability_index = ctx.current_ability_index;
    ctx.current_ability_index = current_ability_index;

    // Retry with `~` normalization if the first pass left an Unimplemented node
    // or emitted a target-fallback warning.
    let mut def = parse_activated_with_self_ref_fallback(&effect_text, card_name, ctx);

    ctx.current_ability_exile_cost_zone = prev_exile_zone;
    ctx.current_ability_index = prev_ability_index;
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
    if !constraints.restrictions.is_empty() {
        def.activation_restrictions = constraints.restrictions;
    }
    def.activator_filter = constraints.activator_filter.or_else(|| {
        constraints
            .any_player_may_activate
            .then_some(PlayerFilter::All)
    });
    extract_cost_reduction_from_chain(&mut def);
    extract_mana_spend_trigger_from_chain(&mut def);
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
    let mut parsed = lower_oracle_ir(&ir);
    scrub_granting_placeholder_descriptions(&mut parsed);
    parsed
}

/// CR 201.5a: Single post-parse degrade net for [`GRANTING_SELF_PLACEHOLDER`].
/// The masker inserts the placeholder into verb-object self-ref positions so the
/// self-ref combinators can map it to `TargetFilter::GrantingObject`. After
/// parsing, any residual placeholder lives ONLY in display `description` strings
/// (which embed the raw quoted text, e.g. an equipment's outer "…has \"…\""
/// static description); it must render as `~` and never leak the raw private-use
/// char into card-data. This is the shared-parse-entry cleanup the plan promised
/// — one sweep, not a per-consumer patch.
fn scrub_granting_placeholder_descriptions(parsed: &mut ParsedAbilities) {
    for def in &mut parsed.abilities {
        scrub_ability_descriptions(def);
    }
    for trig in &mut parsed.triggers {
        scrub_trigger_descriptions(trig);
    }
    for st in &mut parsed.statics {
        scrub_static_descriptions(st);
    }
    for rep in &mut parsed.replacements {
        scrub_replacement_descriptions(rep);
    }
}

fn scrub_description(desc: &mut Option<String>) {
    if let Some(s) = desc {
        if s.contains(GRANTING_SELF_PLACEHOLDER) {
            *s = s.replace(GRANTING_SELF_PLACEHOLDER, "~");
        }
    }
}

fn scrub_ability_descriptions(def: &mut AbilityDefinition) {
    scrub_description(&mut def.description);
    // allow-noncombinator: destructure-read of the Unimplemented gap description
    // (a display string), not a hand-constructed literal or parsing dispatch.
    if let Effect::Unimplemented { description, .. } = def.effect.as_mut() {
        scrub_description(description);
    }
    if let Some(sub) = def.sub_ability.as_mut() {
        scrub_ability_descriptions(sub);
    }
    if let Some(els) = def.else_ability.as_mut() {
        scrub_ability_descriptions(els);
    }
    for mode in def.mode_abilities.iter_mut() {
        scrub_ability_descriptions(mode);
    }
}

fn scrub_trigger_descriptions(trig: &mut TriggerDefinition) {
    scrub_description(&mut trig.description);
    if let Some(execute) = trig.execute.as_mut() {
        scrub_ability_descriptions(execute);
    }
}

fn scrub_static_descriptions(st: &mut StaticDefinition) {
    scrub_description(&mut st.description);
    for modification in st.modifications.iter_mut() {
        match modification {
            ContinuousModification::GrantAbility { definition } => {
                scrub_ability_descriptions(definition)
            }
            ContinuousModification::GrantTrigger { trigger } => scrub_trigger_descriptions(trigger),
            ContinuousModification::GrantStaticAbility { definition } => {
                scrub_static_descriptions(definition)
            }
            _ => {}
        }
    }
}

fn scrub_replacement_descriptions(rep: &mut ReplacementDefinition) {
    scrub_description(&mut rep.description);
    if let Some(execute) = rep.execute.as_mut() {
        scrub_ability_descriptions(execute);
    }
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
pub(crate) fn try_parse_equip(line: &str) -> Option<AbilityDefinition> {
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
    ability.ability_tag = Some(AbilityTag::Equip);
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

/// CR 606.5 + CR 107.3: True when a loyalty-cost inner token is the variable
/// `−X` form (any minus glyph followed by a lone `X`), e.g. the inner of
/// `[−X]:`. The fixed `[−N]` forms are handled by `parse_loyalty_number`.
fn is_minus_x_loyalty(inner: &str) -> bool {
    let trimmed = inner.trim();
    let mut chars = trimmed.chars();
    match chars.next() {
        // U+2212 minus, en dash, ASCII hyphen — mirrors `parse_loyalty_number`.
        Some('−') | Some('–') | Some('-') => {}
        _ => return false,
    }
    chars.as_str().trim().eq_ignore_ascii_case("x")
}

/// CR 606.5 + CR 107.3: Build the cost for a `[−X]` loyalty ability — remove X
/// loyalty counters, where the controller chooses X at activation. Modeled as a
/// chosen-X `RemoveCounter` of `Loyalty` counters so it reuses the existing
/// chosen-X announcement (`max` derives from the source's loyalty counters),
/// concretization (`count` → chosen X), and replacement-aware payment (which
/// keeps `obj.loyalty` in sync per CR 306.5b). The chosen X is stamped to
/// `cost_x_paid`, so `X` references in the effect resolve to it. `is_loyalty_ability_cost`
/// recognizes this shape so the CR 606.3 once-per-turn gate still applies.
fn minus_x_loyalty_cost() -> AbilityCost {
    AbilityCost::RemoveCounter {
        count: crate::types::ability::REMOVE_COUNTER_COST_X,
        counter_type: crate::types::counter::CounterMatch::OfType(
            crate::types::counter::CounterType::Loyalty,
        ),
        target: None,
        selection: crate::types::ability::CounterCostSelection::default(),
    }
}

/// Try to parse a planeswalker loyalty line: "+N:", "−N:", "0:", "[+N]:", "[−N]:", "[0]:", "[−X]:"
fn try_parse_loyalty_line(line: &str, ctx: &mut ParseContext) -> Option<AbilityDefinition> {
    let trimmed = line.trim();

    // Try bracket format first: [+2]: ..., [−1]: ..., [0]: ..., [−X]: ...
    if let Some(after_open) = trimmed.strip_prefix('[') {
        if let Some((inner, rest)) = after_open.split_once(']') {
            if let Some(effect_text) = rest.trim().strip_prefix(':') {
                // CR 606.5 + CR 107.3: "[−X]" variable-loyalty ability — the
                // controller chooses X at activation (0..=current loyalty) and X
                // feeds the effect via `cost_x_paid`. Checked before
                // `parse_loyalty_number`, which only handles fixed amounts.
                if is_minus_x_loyalty(inner) {
                    let effect_text = effect_text.trim();
                    ctx.subject = None;
                    ctx.actor = None;
                    let mut def =
                        parse_effect_chain_with_context(effect_text, AbilityKind::Activated, ctx);
                    def.cost = Some(minus_x_loyalty_cost());
                    def.description = Some(trimmed.to_string());
                    apply_loyalty_restrictions(&mut def);
                    return Some(def);
                }
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

    // Try bare format: +2: ..., −1: ..., 0: ..., −X: ...
    if let Some((prefix, effect_text)) = trimmed.split_once(':') {
        // CR 606.5 + CR 107.3: bare "−X:" variable-loyalty ability (mirrors the
        // bracket branch). `parse_loyalty_number` rejects "X", so this must be
        // checked first.
        if is_minus_x_loyalty(prefix) {
            let effect_text = effect_text.trim();
            ctx.subject = None;
            ctx.actor = None;
            let mut def = parse_effect_chain_with_context(effect_text, AbilityKind::Activated, ctx);
            def.cost = Some(minus_x_loyalty_cost());
            def.description = Some(trimmed.to_string());
            apply_loyalty_restrictions(&mut def);
            return Some(def);
        }
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
pub(crate) fn extract_cost_reduction_from_chain(def: &mut AbilityDefinition) {
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

/// CR 106.6 + CR 603.3: Fold a trailing "When you spend this mana to cast a
/// [filter] spell, [effect]" sub-ability into the parent mana effect's `grants`
/// as a `ManaSpellGrant::TriggerOnSpend` (Lapis Orb of Dragonkind, Scaled
/// Nurturer, Gilanra). Only applies to mana abilities; otherwise the clause
/// drops to an `Effect:when` gap.
fn extract_mana_spend_trigger_from_chain(def: &mut AbilityDefinition) {
    if !matches!(&*def.effect, Effect::Mana { .. }) {
        return;
    }
    if let Some(grant) = strip_mana_spend_trigger_node(&mut def.sub_ability) {
        if let Effect::Mana { grants, .. } = &mut *def.effect {
            grants.push(grant);
        }
    }
}

/// Recursively walk the sub_ability chain. If a node is an `Unimplemented`
/// "When you spend this mana to cast …" clause, remove it and return the parsed
/// `ManaSpellGrant`.
fn strip_mana_spend_trigger_node(
    slot: &mut Option<Box<AbilityDefinition>>,
) -> Option<crate::types::mana::ManaSpellGrant> {
    let sub = slot.as_mut()?;
    // Re-parse the gap node's text via the `Effect` accessor (rather than a
    // hand-matched `Effect::Unimplemented` literal, which the parser-combinator
    // gate forbids in parser modules).
    if let Some(desc) = sub.effect.unimplemented_description() {
        if let Some(grant) =
            super::oracle_effect::mana::parse_mana_spend_trigger(&desc.to_lowercase())
        {
            // Remove this node, promote its child (usually None).
            *slot = sub.sub_ability.take();
            return Some(grant);
        }
    }
    strip_mana_spend_trigger_node(&mut sub.sub_ability)
}

/// Find the position of ":" that indicates an activated ability cost/effect split.
/// The left side must look like a cost (contains "{", or starts with cost-like words,
/// or is a loyalty marker).
pub(super) fn find_activated_colon(line: &str) -> Option<usize> {
    let colon_pos = find_top_level_colon(line)?;
    let prefix = &line[..colon_pos];

    if cost_prefix_is_activated(prefix) {
        return Some(colon_pos);
    }

    // CR 207.2c / CR 207.2d + CR 602.1: an ability-word (<=4 words) or flavor-word
    // (Universes Beyond, any length) label may precede the activation cost
    // ("Mental Organism — Pay 3 life: ~ connives" — M.O.D.O.K.; "I've Come Up with
    // a New Recipe! — {1}{G}{U}, {T}: ..." — Ignis Scientia). Labels have no rules
    // meaning, so strip the italic label and re-test the remaining cost prefix.
    // `strip_activated_cost_label` re-validates via `cost_prefix_is_activated` and
    // `split_short_label_prefix` rejects prefixes containing `{` or `:`, so this
    // never misclassifies an em-dash that lives inside the cost itself.
    if strip_activated_cost_label(prefix).is_some() {
        return Some(colon_pos);
    }

    None
}

/// Whether the text preceding a top-level colon reads as an activation cost.
/// Shared by `find_activated_colon` (and, transitively,
/// `strip_activated_cost_label`) so the bare and ability-word-prefixed paths
/// apply identical cost recognition.
///
/// Three admitting signals, in precedence order:
///  1. mana symbols (`'{'` fast path);
///  2. a cost-starter verb prefix (the original allowlist) — admits the
///     effect-shaped activation costs ("Put a -1/-1 counter on ~", "Return ~ to
///     its owner's hand") whose `parse_single_cost` form is an `EffectCost`;
///  3. a `parse_single_cost` probe that yields a concrete cost which is neither
///     `Unimplemented` nor `EffectCost`.
///
/// Signal 3 is the K0 widening: it admits keyword-action and other named
/// activation costs — "Collect evidence N" (CR 701.59a, Kylox's Voltstrider line
/// 0), Mill, Exert, Behold, Reveal, etc. — that a hardcoded verb list would
/// miss, without a fixed keyword list. `EffectCost` is deliberately EXCLUDED
/// from signal 3: that arm of `parse_single_cost` parses an arbitrary effect, so
/// an effect fragment that happens to precede an EMBEDDED colon — "...or Magic:
/// The Gathering Online avatar" (Clear, Fair Magic), "...at random: Ash Zealot;
/// ..." (The Ash Lizard) — would otherwise be misread as a cost and split the
/// line at the wrong colon. The legitimate effect-shaped costs are recovered by
/// signal 2's verb allowlist (those prefixes start with a cost verb; the
/// embedded-colon effect fragments do not). The card-data coverage-regression
/// gate covers the corpus-wide blast radius of signal 3.
fn cost_prefix_is_activated(prefix: &str) -> bool {
    // Contains mana symbols — fast path (skips the lowercase/parse work below).
    if prefix.contains('{') {
        return true;
    }
    let trimmed = prefix.trim();
    // CR 602.1: cost-starter verbs — the effect-shaped activation costs whose
    // `parse_single_cost` form is an `EffectCost` (excluded from the probe).
    // Recognized with a nom `alt`/`tag` combinator over the lowercased prefix
    // (the parser is the detector; no `starts_with` dispatch).
    let starts_with_cost_verb = nom_on_lower(trimmed, &trimmed.to_lowercase(), |i| {
        value(
            (),
            alt((
                tag("sacrifice"),
                tag("discard"),
                tag("pay"),
                tag("remove"),
                tag("exile"),
                tag("return"),
                tag("tap"),
                tag("untap"),
                tag("put"),
            )),
        )
        .parse(i)
    })
    .is_some();
    if starts_with_cost_verb {
        return true;
    }
    !matches!(
        parse_single_cost(trimmed),
        AbilityCost::Unimplemented { .. } | AbilityCost::EffectCost { .. }
    )
}

/// CR 207.2c / CR 207.2d: an ability word (<=4 words) or a flavor word (Universes
/// Beyond, any length) may label an activated ability — e.g. "The Most Important
/// Punch in History — {1}{G}, {T}: ..." (6 words, Duggan) or "I've Come Up with a
/// New Recipe! — {1}{G}{U}, {T}: ..." (7 words, Ignis Scientia). Labels have no
/// rules meaning, so strip the "<label> — " prefix before parsing the activation
/// cost. Returns the cost remainder ONLY when it reads as a genuine activation
/// cost (`cost_prefix_is_activated`); this guarantees a real em-dash-bearing cost
/// is never mistaken for a label, and an un-labeled cost is reported via `None`
/// (the caller keeps the original text untouched). `cost_prefix_is_activated` —
/// not a word count — is the discriminator, so the label strip is uncapped
/// (`FLAVOR_WORD_COST_LABEL_MAX_WORDS`); a longer flavor name can never widen the
/// set of em-dash lines accepted, only the labels that reach the cost validator.
fn strip_activated_cost_label(cost_text: &str) -> Option<&str> {
    let (_label, rest) = split_short_label_prefix(cost_text, FLAVOR_WORD_COST_LABEL_MAX_WORDS)?;
    cost_prefix_is_activated(rest).then_some(rest)
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

/// CR 602.5: Map a trailing activation-timing phrase to its
/// `ActivationRestriction`(s). Used for the "Any player may activate this ability
/// but only <phrase>" form (and composable with other timing-suffix handlers).
/// Returns `None` for phrases without a recognized timing gate so the caller can
/// decline rather than mis-classify.
fn parse_activation_timing_restriction(phrase: &str) -> Option<Vec<ActivationRestriction>> {
    let phrase = phrase.trim().trim_end_matches('.').trim();
    let lower = phrase.to_lowercase();
    // Speed / turn / upkeep gates — case-insensitive value matches. "their" is the
    // activating player's possessive, equivalent to "your" once an activator is fixed.
    let gate = alt((
        value(
            ActivationRestriction::AsSorcery,
            tag::<_, _, OracleError<'_>>("as a sorcery"),
        ),
        value(ActivationRestriction::AsInstant, tag("as an instant")),
        value(
            opponents_turn_activation_restriction(),
            alt((
                tag("during an opponent's turn"),
                tag("during an opponents turn"),
            )),
        ),
        value(
            ActivationRestriction::DuringYourTurn,
            alt((tag("during your turn"), tag("during their turn"))),
        ),
        value(
            ActivationRestriction::DuringYourUpkeep,
            alt((tag("during your upkeep"), tag("during their upkeep"))),
        ),
    ))
    .parse(lower.as_str());
    if let Ok((rest, restr)) = gate {
        if rest.trim().is_empty() {
            return Some(vec![restr]);
        }
    }
    // CR 602.5: "if <condition>" gate (Lightning Storm "if ~ is on the stack").
    if let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>("if ")).parse(lower.as_str()) {
        let condition_start = phrase.len() - rest.len();
        let condition_text = phrase[condition_start..].trim();
        return Some(vec![ActivationRestriction::RequiresCondition {
            condition: parse_restriction_condition(condition_text),
        }]);
    }
    None
}

// CR 602.1b: Activation instructions after the colon restrict when an ability
// can be activated and are not part of the ability's effect.
// CR 304.5 / CR 307.5: "Only as an instant" and "only as a sorcery" define
// the priority and timing permissions for activating the ability.
fn parse_activation_speed_parenthetical_body(phrase: &str) -> Option<Vec<ActivationRestriction>> {
    let lower = phrase.to_lowercase();
    let (_, rest_original) = nom_on_lower(phrase, &lower, |i| {
        value((), tag("activate only ")).parse(i)
    })?;
    let restrictions = parse_activation_timing_restriction(rest_original)?;
    restrictions
        .iter()
        .all(|restriction| {
            matches!(
                restriction,
                ActivationRestriction::AsInstant | ActivationRestriction::AsSorcery
            )
        })
        .then_some(restrictions)
}

// CR 602.1b: A parenthesized speed instruction after an activated ability is
// still an activation restriction, so keep it visible before reminder stripping.
fn preserve_activation_timing_parenthetical(raw_line: &str) -> Option<String> {
    let lower = raw_line.to_lowercase();
    let (_, parenthetical_original) = nom_on_lower(raw_line, &lower, |i| {
        let (i, _) = take_until::<_, _, OracleError<'_>>(" (activate only ").parse(i)?;
        let (i, _) = tag::<_, _, OracleError<'_>>(" (").parse(i)?;
        Ok((i, ()))
    })?;
    let prefix_len = raw_line.len() - parenthetical_original.len() - " (".len();
    let prefix = raw_line[..prefix_len].trim_end();

    let Ok((tail, inner_original)) = terminated(take_until::<_, _, OracleError<'_>>(")"), tag(")"))
        .parse(parenthetical_original)
    else {
        return None;
    };
    if !tail.trim().trim_end_matches('.').trim().is_empty() {
        return None;
    }

    let timing_text = inner_original.trim().trim_end_matches('.').trim();
    parse_activation_speed_parenthetical_body(timing_text)?;
    Some(format!("{prefix} {timing_text}."))
}

fn opponents_turn_activation_restriction() -> ActivationRestriction {
    ActivationRestriction::RequiresCondition {
        condition: Some(opponents_turn_activation_condition()),
    }
}

/// CR 602.5b + CR 102.1 + CR 109.5: "Activate only during an opponent's turn"
/// gates activation to turns where the activator is not the active player.
fn opponents_turn_activation_condition() -> ParsedCondition {
    ParsedCondition::Not {
        condition: Box::new(ParsedCondition::IsYourTurn),
    }
}

pub(super) fn strip_activated_constraints(text: &str) -> (String, ActivatedConstraintAst) {
    let mut remaining = text.trim().trim_end_matches('.').trim().to_string();
    let mut constraints = ActivatedConstraintAst::default();

    'parse_constraints: loop {
        let lower = remaining.to_lowercase();
        let tp = TextPair::new(&remaining, &lower);

        // CR 602.5b: A printed "Once each turn" activation restriction stays
        // attached to this activated ability even if the object changes control.
        if let Some(((), rest_original)) = nom_on_lower(&remaining, &lower, |i| {
            value((), tag("once each turn, ")).parse(i)
        }) {
            constraints
                .restrictions
                .push(ActivationRestriction::OnlyOnceEachTurn);
            remaining = rest_original.trim().to_string();
            continue;
        }

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

        // CR 602.2 + CR 602.5: "Any player may activate this ability but only
        // <restriction>" combines the any-player permission with an activation
        // timing restriction (Endbringer's Revel "as a sorcery", Volrath's Dungeon
        // "during their turn", Lightning Storm "if ~ is on the stack"). Split so
        // BOTH are recorded; otherwise the whole trailing sentence is dropped and
        // the runtime-enforced timing restriction is silently lost. Must precede
        // the terminal "any player may activate this ability" strip below, which
        // would not match because the sentence continues past that phrase.
        if let Some((before, restriction)) =
            tp.rsplit_around("any player may activate this ability but only ")
        {
            if let Some(parsed) = parse_activation_timing_restriction(restriction.original) {
                constraints.any_player_may_activate = true;
                constraints.restrictions.extend(parsed);
                remaining = before
                    .original
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                if remaining.trim().is_empty() {
                    break;
                }
                continue;
            }
        }

        // CR 602.2a + CR 602.5: "Only your opponents may activate this ability and only
        // <restriction>" — mirror the any-player composition: record the opponent
        // permission and delegate the timing axis to `parse_activation_timing_restriction`.
        if let Some((before, restriction)) =
            tp.rsplit_around("only your opponents may activate this ability and only ")
        {
            if let Some(parsed) = parse_activation_timing_restriction(restriction.original) {
                constraints.activator_filter = Some(PlayerFilter::Opponent);
                constraints.restrictions.extend(parsed);
                remaining = before
                    .original
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                if remaining.trim().is_empty() {
                    break;
                }
                continue;
            }
        }

        if let Some((before, restrictions)) = split_legacy_play_this_ability_timing(&remaining) {
            remaining = before
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints.restrictions.extend(restrictions);
            if remaining.is_empty() {
                break;
            }
            continue;
        }

        const OPPONENTS_ACTIVATE_SUFFIX: &str = "only your opponents may activate this ability";
        if lower.ends_with(OPPONENTS_ACTIVATE_SUFFIX) {
            let end = remaining.len() - OPPONENTS_ACTIVATE_SUFFIX.len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints.activator_filter = Some(PlayerFilter::Opponent);
            if remaining.is_empty() {
                break 'parse_constraints;
            }
            continue 'parse_constraints;
        }

        // CR 602.2: "Any player may activate this ability." — strip as a recognized
        // annotation. This appears as a trailing sentence on activated abilities.
        const ANY_PLAYER_ACTIVATE_SUFFIX: &str = "any player may activate this ability";
        let any_player_suffix = all_consuming(terminated(
            take_until::<_, _, OracleError<'_>>(ANY_PLAYER_ACTIVATE_SUFFIX),
            tag::<_, _, OracleError<'_>>(ANY_PLAYER_ACTIVATE_SUFFIX),
        ))
        .parse(lower.as_str())
        .is_ok();
        if any_player_suffix {
            let end = remaining.len() - ANY_PLAYER_ACTIVATE_SUFFIX.len();
            let prefix = lower[..end].trim();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints.any_player_may_activate = true;
            if prefix.is_empty() {
                break;
            }
            continue;
        }

        // CR 602.5b: Delegate bare "Activate only <timing>" phrases to the same
        // timing parser used by the "Any player may activate ... but only"
        // composition path. The condition-only form stays on its specialized
        // branch below so the once-per-turn rider is stripped before condition
        // parsing.
        if let Some((before, restriction)) = tp.rsplit_around("activate only ") {
            if tag::<_, _, OracleError<'_>>("if ")
                .parse(restriction.lower.trim_start())
                .is_err()
            {
                if let Some(parsed) = parse_activation_timing_restriction(restriction.original) {
                    constraints.restrictions.extend(parsed);
                    remaining = before
                        .original
                        .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                        .to_string();
                    if remaining.is_empty() {
                        break;
                    }
                    continue;
                }
            }
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

        // CR 602.5b + CR 602.5c: An "... and only once [each turn]" activation-limit
        // rider can trail any timing restriction — "Activate only during your turn
        // and only once" (Loch Larent), "... and only once each turn", etc. Each
        // "activate only <timing>" arm above anchors on the literal "activate", so a
        // conjoined rider is left stranded and the whole sentence would be dropped
        // (the swallowed `ActivateOnlyDuring` clause, issue #2238). Peel the rider
        // here and loop so the bare "activate only <timing>" core matches its own
        // arm next pass — composing the limit and timing axes rather than
        // enumerating every timing × limit pairing. Guarded on a preceding
        // "activate only" clause so an effect sentence that merely ends in "and only
        // once" is never mis-stripped. ("each turn" form first: longest match.)
        if let Some((kept_len, restriction)) = peel_only_once_rider(&lower) {
            remaining = remaining[..kept_len]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints.restrictions.push(restriction);
            continue 'parse_constraints;
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

// CR 602.1d: Older cards referred to activating an activated ability as
// "playing" that ability.
// CR 602.1b + CR 307.5: "Play this ability as a sorcery" is a trailing
// activation instruction, not part of the ability's effect.
fn split_legacy_play_this_ability_timing(text: &str) -> Option<(&str, Vec<ActivationRestriction>)> {
    let lower = text.to_lowercase();
    let (_, restriction_original) = nom_on_lower(text, &lower, |i| {
        let (i, before) = take_until::<_, _, OracleError<'_>>("play this ability ").parse(i)?;
        if !is_empty_or_sentence_boundary(before) {
            return Err(nom::Err::Error(OracleError::new(
                before,
                nom::error::ErrorKind::Tag,
            )));
        }
        let (i, _) = tag::<_, _, OracleError<'_>>("play this ability ").parse(i)?;
        Ok((i, ()))
    })?;
    let prefix_len = text.len() - restriction_original.len() - "play this ability ".len();
    let restrictions = parse_activation_timing_restriction(restriction_original)?;
    Some((&text[..prefix_len], restrictions))
}

fn is_empty_or_sentence_boundary(text: &str) -> bool {
    text.trim_end()
        .chars()
        .next_back()
        .is_none_or(|ch| ch == '.')
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
/// `ActivationRestriction::OnlyOnce`, which the once-each-turn cadence does not
/// model).
fn recognize_once_each_turn_cadence(text: &str) -> bool {
    let lower = text.trim().trim_end_matches('.').to_lowercase();
    let matched = all_consuming(tag::<_, _, OracleError<'_>>("activate only once each turn"))
        .parse(lower.as_str())
        .is_ok();
    matched
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
    if recognize_once_each_turn_cadence(tail) {
        Some(Keyword::Crew {
            power,
            // CR 602.5b: "Activate only once each turn."
            once_per_turn: Some(Box::new(ActivationRestriction::OnlyOnceEachTurn)),
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

/// CR 602.5b + CR 602.5c: Peel a trailing "and only once [each turn]"
/// activation-limit rider that conjoins onto an "activate only <timing>" clause
/// ("Activate only during your turn and only once", Loch Larent). Forward nom
/// combinators locate the rider (`take_until`) and confirm it trails an
/// "activate only" clause, composing the limit axis with the timing axis rather
/// than enumerating every timing × limit pairing. Returns the byte length of the
/// text to keep (everything before the rider) and the limit restriction.
fn peel_only_once_rider(lower: &str) -> Option<(usize, ActivationRestriction)> {
    let (rider_onward, before) = take_until::<_, _, OracleError<'_>>(" and only once")
        .parse(lower)
        .ok()?;
    // The rider must trail an "activate only ..." clause, never an effect
    // sentence that merely ends in "and only once".
    take_until::<_, _, OracleError<'_>>("activate only")
        .parse(before)
        .ok()?;
    // "each turn" is the optional longest-match tail; the rider must end the line.
    let (rest, each_turn) = preceded(
        tag::<_, _, OracleError<'_>>(" and only once"),
        opt(tag::<_, _, OracleError<'_>>(" each turn")),
    )
    .parse(rider_onward)
    .ok()?;
    if !rest.is_empty() {
        return None;
    }
    let restriction = if each_turn.is_some() {
        ActivationRestriction::OnlyOnceEachTurn
    } else {
        ActivationRestriction::OnlyOnce
    };
    Some((before.len(), restriction))
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
    // Suffix / mid-line case: the "X can't be 0." annotation is EXCISED in place,
    // never truncated. Everything before it is kept, and any sentence(s) that
    // follow it on the same line are re-attached. Katara, Water Tribe's Hope is
    // the witness (#2238): "Waterbend {X}: … until end of turn. X can't be 0.
    // Activate only during your turn." — the trailing "Activate only during your
    // turn." must survive so the activated-ability parser still sees its timing
    // restriction. (Reminder text is already stripped by the caller, so a
    // trailing parenthetical never reaches here.) The annotation is located with
    // a forward `take_until` combinator (longest "this ability..." form first),
    // not a string-method scan.
    for (annotation, had_period) in [
        (". this ability can't be copied and x can't be 0", true),
        (" this ability can't be copied and x can't be 0", false),
        (". x can't be 0", true),
        (" x can't be 0", false),
    ] {
        if let Ok((_, before)) = take_until::<_, _, OracleError<'_>>(annotation).parse(trimmed) {
            let pos = before.len();
            let mut result = line[..pos].trim_end().to_string();
            // Preserve the sentence boundary the annotation occupied.
            if had_period {
                result.push('.');
            }
            // Re-attach any sentence that followed the annotation. The annotation
            // ends at `pos + annotation.len()`, optionally followed by its own
            // sentence-terminating '.' (peeled with a nom `opt(tag("."))`).
            let after = line.get(pos + annotation.len()..).unwrap_or("");
            let after = opt(tag::<_, _, OracleError<'_>>("."))
                .parse(after)
                .map(|(rest, _)| rest)
                .unwrap_or(after)
                .trim_start();
            if !after.is_empty() {
                result.push(' ');
                result.push_str(after);
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
#[path = "oracle_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "oracle_pipeline_snapshot_tests.rs"]
mod pipeline_snapshot_tests;
