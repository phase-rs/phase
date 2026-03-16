use std::str::FromStr;

use super::oracle_static::parse_continuous_modifications;
use super::oracle_target::{parse_target, parse_type_phrase};
use super::oracle_util::{
    contains_object_pronoun, contains_possessive, parse_mana_production, parse_number,
    starts_with_possessive, strip_reminder_text,
};
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, ChoiceType, CountValue,
    DamageAmount, Duration, Effect, FilterProp, GainLifePlayer, LifeAmount, ManaProduction,
    ManaSpendRestriction, PtValue, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedEffectClause {
    effect: Effect,
    duration: Option<Duration>,
    /// Compound "and" remainder parsed into a sub_ability chain.
    sub_ability: Option<Box<AbilityDefinition>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SubjectApplication {
    affected: TargetFilter,
    target: Option<TargetFilter>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TokenDescription {
    name: String,
    power: Option<PtValue>,
    toughness: Option<PtValue>,
    types: Vec<String>,
    colors: Vec<ManaColor>,
    keywords: Vec<Keyword>,
    tapped: bool,
    count: CountValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct AnimationSpec {
    power: Option<i32>,
    toughness: Option<i32>,
    colors: Option<Vec<ManaColor>>,
    keywords: Vec<Keyword>,
    types: Vec<String>,
    remove_all_abilities: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchLibraryDetails {
    filter: TargetFilter,
    count: u32,
    reveal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClauseAst {
    Imperative {
        text: String,
    },
    SubjectPredicate {
        subject: SubjectPhraseAst,
        predicate: Box<PredicateAst>,
    },
    Conditional {
        clause: Box<ClauseAst>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SubjectPhraseAst {
    affected: TargetFilter,
    target: Option<TargetFilter>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PredicateAst {
    Continuous {
        effect: Effect,
        duration: Option<Duration>,
        sub_ability: Option<Box<AbilityDefinition>>,
    },
    Become {
        effect: Effect,
        duration: Option<Duration>,
    },
    Restriction {
        effect: Effect,
        duration: Option<Duration>,
    },
    ImperativeFallback {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ContinuationAst {
    SearchDestination { destination: Zone },
    RevealHandFilter { card_filter: TargetFilter },
    ManaRestriction { restriction: ManaSpendRestriction },
    CounterSourceStatic { source_static: StaticDefinition },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ImperativeAst {
    Numeric(NumericImperativeAst),
    Targeted(TargetedImperativeAst),
    SearchCreation(SearchCreationImperativeAst),
    HandReveal(HandRevealImperativeAst),
    Choose(ChooseImperativeAst),
    Utility(UtilityImperativeAst),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ImperativeFamilyAst {
    Structured(ImperativeAst),
    CostResource(CostResourceImperativeAst),
    ZoneCounter(ZoneCounterImperativeAst),
    Explore,
    Proliferate,
    Shuffle(ShuffleImperativeAst),
    Put(PutImperativeAst),
    YouMay { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NumericImperativeAst {
    Draw { count: u32 },
    GainLife { amount: i32 },
    LoseLife { amount: i32 },
    Pump { power: PtValue, toughness: PtValue },
    Scry { count: u32 },
    Surveil { count: u32 },
    Mill { count: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetedImperativeAst {
    Tap { target: TargetFilter },
    Untap { target: TargetFilter },
    Sacrifice { target: TargetFilter },
    Discard { count: u32 },
    Return { target: TargetFilter },
    Fight { target: TargetFilter },
    GainControl { target: TargetFilter },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SearchCreationImperativeAst {
    SearchLibrary {
        filter: TargetFilter,
        count: u32,
        reveal: bool,
    },
    Dig {
        count: u32,
    },
    CopyTokenOf {
        target: TargetFilter,
    },
    Token {
        token: TokenDescription,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UtilityImperativeAst {
    Prevent { text: String },
    Regenerate { text: String },
    Copy { target: TargetFilter },
    Transform { target: TargetFilter },
    Attach { target: TargetFilter },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HandRevealImperativeAst {
    LookAtHand { target: TargetFilter },
    RevealHand,
    RevealTop { count: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChooseImperativeAst {
    TargetOnly { target: TargetFilter },
    Reparse { text: String },
    NamedChoice { choice_type: ChoiceType },
    RevealHandFilter { card_filter: TargetFilter },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PutImperativeAst {
    Mill {
        count: u32,
    },
    ZoneChange {
        origin: Option<Zone>,
        destination: Zone,
        target: TargetFilter,
    },
    TopOfLibrary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShuffleImperativeAst {
    ShuffleLibrary { target: TargetFilter },
    ChangeZoneToLibrary,
    ChangeZoneAllToLibrary { origin: Zone },
    Unimplemented { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CostResourceImperativeAst {
    ActivateOnlyIfControlsLandSubtypeAny {
        subtypes: Vec<String>,
    },
    Mana {
        produced: ManaProduction,
        restrictions: Vec<ManaSpendRestriction>,
    },
    Damage {
        amount: DamageAmount,
        target: TargetFilter,
        all: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ZoneCounterImperativeAst {
    Destroy {
        target: TargetFilter,
        all: bool,
    },
    Exile {
        origin: Option<Zone>,
        target: TargetFilter,
        all: bool,
    },
    Counter {
        target: TargetFilter,
        source_static: Option<StaticDefinition>,
    },
    PutCounter {
        counter_type: String,
        count: i32,
        target: TargetFilter,
    },
    RemoveCounter {
        counter_type: String,
        count: i32,
        target: TargetFilter,
    },
}

/// Parse an effect clause from Oracle text into an Effect enum.
/// This handles the verb-based matching for spell effects, activated ability effects,
/// and the effect portion of triggered abilities.
///
/// For compound effects ("Gain 3 life. Draw a card."), call `parse_effect_chain`
/// which splits on sentence boundaries and chains via AbilityDefinition::sub_ability.
pub fn parse_effect(text: &str) -> Effect {
    parse_effect_clause(text).effect
}

fn parse_effect_clause(text: &str) -> ParsedEffectClause {
    let text = strip_leading_sequence_connector(text)
        .trim()
        .trim_end_matches('.');
    if text.is_empty() {
        return parsed_clause(Effect::Unimplemented {
            name: "empty".to_string(),
            description: None,
        });
    }

    if let Some((duration, rest)) = strip_leading_duration(text) {
        return with_clause_duration(parse_effect_clause(rest), duration);
    }

    let ast = parse_clause_ast(text);
    lower_clause_ast(ast)
}

fn parse_clause_ast(text: &str) -> ClauseAst {
    let text = text.trim();

    // Mirror the CubeArtisan grammar's high-level sentence shapes:
    // 1) conditionals ("if X, Y"), 2) subject + verb phrase, 3) bare imperative.
    if let Some((condition_text, remainder)) = split_leading_conditional(text) {
        let _ = condition_text;
        return ClauseAst::Conditional {
            clause: Box::new(parse_clause_ast(&remainder)),
        };
    }

    if let Some(ast) = try_parse_subject_predicate_ast(text) {
        return ast;
    }

    ClauseAst::Imperative {
        text: text.to_string(),
    }
}

fn lower_clause_ast(ast: ClauseAst) -> ParsedEffectClause {
    match ast {
        ClauseAst::Imperative { text } => lower_imperative_clause(&text),
        ClauseAst::SubjectPredicate { subject, predicate } => {
            lower_subject_predicate_ast(subject, *predicate)
        }
        ClauseAst::Conditional { clause } => {
            // Phase 2 preserves current semantics for generic leading conditionals:
            // recognize the structure explicitly, but lower only the body.
            lower_clause_ast(*clause)
        }
    }
}

fn lower_imperative_clause(text: &str) -> ParsedEffectClause {
    // "Its controller gains life equal to its power/toughness" — subject must be preserved
    // because the life recipient is not the caster but the targeted permanent's controller.
    if let Some(clause) = try_parse_targeted_controller_gain_life(text) {
        return clause;
    }

    let (stripped, duration) = strip_trailing_duration(text);
    let mut clause = parsed_clause(parse_imperative_effect(stripped));
    if clause.duration.is_none() {
        clause.duration = duration;
    }
    clause
}

fn lower_subject_predicate_ast(
    subject: SubjectPhraseAst,
    predicate: PredicateAst,
) -> ParsedEffectClause {
    match predicate {
        PredicateAst::Continuous {
            effect,
            duration,
            sub_ability,
        } => ParsedEffectClause {
            effect,
            duration,
            sub_ability,
        },
        PredicateAst::Become { effect, duration }
        | PredicateAst::Restriction { effect, duration } => ParsedEffectClause {
            effect,
            duration,
            sub_ability: None,
        },
        PredicateAst::ImperativeFallback { text } => {
            if matches!(text.to_lowercase().as_str(), "shuffle" | "shuffles")
                && matches!(
                    subject.affected,
                    TargetFilter::Player | TargetFilter::Controller
                )
            {
                return parsed_clause(Effect::Shuffle {
                    target: subject.affected,
                });
            }
            lower_imperative_clause(&text)
        }
    }
}

fn parse_imperative_effect(text: &str) -> Effect {
    let lower = text.to_lowercase();
    if let Some(ast) = parse_imperative_family_ast(text, &lower) {
        return lower_imperative_family_ast(ast);
    }

    // --- Fallback ---
    let verb = lower.split_whitespace().next().unwrap_or("unknown");
    Effect::Unimplemented {
        name: verb.to_string(),
        description: Some(text.to_string()),
    }
}

/// Determines if text after "choose " is a targeting synonym rather than
/// a modal choice ("choose one —"), color choice, or creature type choice.
///
/// Returns true when the text contains "target" (indicating a targeting phrase)
/// or uses "a/an {type} you/opponent control(s)" (selection-as-targeting).
///
/// Returns false for:
///   - "card from it" — handled separately as RevealHand filter
///   - "a color" / "a creature type" / "a card type" / "a card name" — different mechanics
fn is_choose_as_targeting(rest: &str) -> bool {
    // Already handled elsewhere
    if rest.contains("card from it") {
        return false;
    }

    // If try_parse_named_choice would match "choose {rest}", it's a named choice, not targeting
    let as_full = format!("choose {rest}");
    if try_parse_named_choice(&as_full).is_some() {
        return false;
    }

    // Any phrase containing "target" is a targeting synonym
    if rest.contains("target") {
        return true;
    }

    // "choose up to N" without "target" (e.g. "choose up to two creatures")
    if rest.starts_with("up to ") {
        return true;
    }

    // "choose a/an {type} ... you control / an opponent controls"
    if let Some(after_article) = rest.strip_prefix("a ").or_else(|| rest.strip_prefix("an ")) {
        // Exclude patterns not yet in try_parse_named_choice but still not targeting
        if after_article.starts_with("nonbasic land type") || after_article.starts_with("number") {
            return false;
        }
        // Must reference controller to be targeting-like
        if after_article.contains("you control")
            || after_article.contains("opponent controls")
            || after_article.contains("an opponent controls")
        {
            return true;
        }
    }

    false
}

/// Match "choose a creature type", "choose a color", "choose odd or even",
/// "choose a basic land type", "choose a card type" from lowercased Oracle text.
pub(crate) fn try_parse_named_choice(lower: &str) -> Option<ChoiceType> {
    if !lower.starts_with("choose ") {
        return None;
    }
    let rest = &lower[7..]; // skip "choose "
    if rest.starts_with("a creature type") {
        Some(ChoiceType::CreatureType)
    } else if rest.starts_with("a color") {
        Some(ChoiceType::Color)
    } else if rest.starts_with("odd or even") {
        Some(ChoiceType::OddOrEven)
    } else if rest.starts_with("a basic land type") {
        Some(ChoiceType::BasicLandType)
    } else if rest.starts_with("a card type") {
        Some(ChoiceType::CardType)
    } else if rest.starts_with("a card name")
        || rest.starts_with("a nonland card name")
        || rest.starts_with("a creature card name")
    {
        Some(ChoiceType::CardName)
    } else if let Some(range_rest) = rest.strip_prefix("a number between ") {
        // "choose a number between 0 and 13"
        let mut parts = range_rest.splitn(3, ' ');
        let min = parts.next().and_then(|s| s.parse::<u8>().ok()).unwrap_or(0);
        let and = parts.next();
        let max = parts
            .next()
            .and_then(|s| {
                s.trim_end_matches(|c: char| !c.is_ascii_digit())
                    .parse::<u8>()
                    .ok()
            })
            .unwrap_or(20);
        if and == Some("and") {
            Some(ChoiceType::NumberRange { min, max })
        } else {
            None
        }
    } else if let Some(gt_rest) = rest.strip_prefix("a number greater than ") {
        // "choose a number greater than 0" — open-ended, cap at 20
        let n = gt_rest
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(0);
        Some(ChoiceType::NumberRange {
            min: n + 1,
            max: 20,
        })
    } else if rest == "a number" || rest.starts_with("a number ") {
        // Generic "choose a number" — default range 0-20
        Some(ChoiceType::NumberRange { min: 0, max: 20 })
    } else if rest.starts_with("a land type") || rest.starts_with("a nonbasic land type") {
        Some(ChoiceType::LandType)
    } else {
        // Generic "X or Y" pattern — must come AFTER all specific patterns above
        try_parse_binary_choice(rest).map(|options| ChoiceType::Labeled { options })
    }
}

/// Try to parse "X or Y" as a binary labeled choice.
/// Only matches simple one-or-two-word labels separated by " or ".
/// Returns capitalized labels.
/// This must come AFTER all specific patterns in try_parse_named_choice to avoid
/// accidentally matching "choose left or right" against targeting patterns.
fn try_parse_binary_choice(rest: &str) -> Option<Vec<String>> {
    let (left, right) = rest.split_once(" or ")?;
    let left = left.trim();
    let right = right.trim();

    // Labels must be short (≤2 words) — longer phrases are likely clauses, not choices
    if left.split_whitespace().count() > 2 || right.split_whitespace().count() > 2 {
        return None;
    }
    // Reject known non-choice patterns
    if left.contains("target") || right.contains("target") {
        return None;
    }
    if right == "more" || left == "both" || right == "both" {
        return None;
    }

    Some(vec![capitalize(left), capitalize(right)])
}

fn parse_choose_filter(lower: &str) -> TargetFilter {
    // Extract type info between "choose" and "card from it"
    // Handle both "choose X" and "you choose X" forms
    let after_choose = lower
        .strip_prefix("you choose ")
        .or_else(|| lower.strip_prefix("you may choose "))
        .or_else(|| lower.strip_prefix("choose "))
        .unwrap_or(lower);
    let before_card = after_choose.split("card").next().unwrap_or("");
    let cleaned = before_card
        .trim()
        .trim_start_matches("a ")
        .trim_start_matches("an ")
        .trim();

    let parts: Vec<&str> = cleaned.split(" or ").collect();
    if parts.len() > 1 {
        let filters: Vec<TargetFilter> = parts
            .iter()
            .filter_map(|p| type_str_to_target_filter(p.trim()))
            .collect();
        if filters.len() > 1 {
            return TargetFilter::Or { filters };
        }
        if let Some(f) = filters.into_iter().next() {
            return f;
        }
    }
    if let Some(f) = type_str_to_target_filter(cleaned) {
        return f;
    }
    TargetFilter::Any
}

fn type_str_to_target_filter(s: &str) -> Option<TargetFilter> {
    let card_type = match s {
        "artifact" => Some(TypeFilter::Artifact),
        "creature" => Some(TypeFilter::Creature),
        "enchantment" => Some(TypeFilter::Enchantment),
        "instant" => Some(TypeFilter::Instant),
        "sorcery" => Some(TypeFilter::Sorcery),
        "planeswalker" => Some(TypeFilter::Planeswalker),
        "land" => Some(TypeFilter::Land),
        _ => None,
    };
    card_type.map(|ct| TargetFilter::Typed(TypedFilter::new(ct)))
}

/// Extract card type filter from a sub-ability sentence containing "card from it/among".
/// Handles forms like "exile a nonland card from it", "discard a creature card from it".
fn parse_choose_filter_from_sentence(lower: &str) -> TargetFilter {
    let card_pos = match lower.find("card from") {
        Some(pos) => pos,
        None => return TargetFilter::Any,
    };
    // The word immediately before "card from" is the type descriptor
    let word = lower[..card_pos].trim().rsplit(' ').next().unwrap_or("");
    if let Some(negated) = word.strip_prefix("non") {
        if let Some(TargetFilter::Typed(TypedFilter { card_type, .. })) =
            type_str_to_target_filter(negated)
        {
            return TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::NonType {
                    value: card_type.map(|ct| format!("{ct:?}")).unwrap_or_default(),
                },
            ]));
        }
    }
    type_str_to_target_filter(word).unwrap_or(TargetFilter::Any)
}

fn try_parse_add_mana_effect(text: &str) -> Option<Effect> {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    if !lower.starts_with("add ") {
        return None;
    }

    let clause = trimmed[4..].trim();
    let (without_where_x, where_x_expression) = strip_trailing_where_x(clause);
    let clause = without_where_x.trim().trim_end_matches(['.', '"']);

    if let Some(produced) = parse_mana_production_clause(clause) {
        return Some(Effect::Mana {
            produced,
            restrictions: vec![],
        });
    }

    if let Some((count, rest)) = parse_mana_count_prefix(clause) {
        let count = apply_where_x_count_expression(count, where_x_expression.as_deref());
        let rest = rest.trim().trim_end_matches(['.', '"']).trim();
        let rest_lower = rest.to_lowercase();

        if rest_lower.starts_with("mana of any one color")
            || rest_lower.starts_with("mana of any color")
        {
            return Some(Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count,
                    color_options: all_mana_colors(),
                },
                restrictions: vec![],
            });
        }

        if rest_lower.starts_with("mana in any combination of colors") {
            return Some(Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count,
                    color_options: all_mana_colors(),
                },
                restrictions: vec![],
            });
        }

        if rest_lower.starts_with("mana of the chosen color")
            || rest_lower.starts_with("mana of that color")
        {
            return Some(Effect::Mana {
                produced: ManaProduction::ChosenColor { count },
                restrictions: vec![],
            });
        }

        const ANY_COMBINATION_PREFIX: &str = "mana in any combination of ";
        if rest_lower.starts_with(ANY_COMBINATION_PREFIX) {
            let color_set_text = rest[ANY_COMBINATION_PREFIX.len()..].trim();
            if let Some(color_options) = parse_mana_color_set(color_set_text) {
                return Some(Effect::Mana {
                    produced: ManaProduction::AnyCombination {
                        count,
                        color_options,
                    },
                    restrictions: vec![],
                });
            }
        }
    }

    let clause_lower = clause.to_lowercase();
    let fallback_count = parse_mana_count_prefix(clause)
        .map(|(count, _)| count)
        .unwrap_or(CountValue::Fixed(1));
    let fallback_count =
        apply_where_x_count_expression(fallback_count, where_x_expression.as_deref());

    if clause_lower.contains("mana of any one color") || clause_lower.contains("mana of any color")
    {
        return Some(Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: fallback_count,
                color_options: all_mana_colors(),
            },
            restrictions: vec![],
        });
    }

    if clause_lower.contains("mana in any combination of colors") {
        return Some(Effect::Mana {
            produced: ManaProduction::AnyCombination {
                count: fallback_count,
                color_options: all_mana_colors(),
            },
            restrictions: vec![],
        });
    }

    if clause_lower.contains("mana of the chosen color")
        || clause_lower.contains("mana of that color")
    {
        return Some(Effect::Mana {
            produced: ManaProduction::ChosenColor {
                count: fallback_count,
            },
            restrictions: vec![],
        });
    }

    None
}

fn try_parse_activate_only_condition(text: &str) -> Option<Effect> {
    let trimmed = text.trim().trim_end_matches('.');
    let lower = trimmed.to_ascii_lowercase();
    let prefix = "activate only if you control ";
    if !lower.starts_with(prefix) {
        return None;
    }

    let raw = &lower[prefix.len()..];
    let mut subtypes = Vec::new();
    for part in raw.split(" or ") {
        let token = part
            .trim()
            .trim_start_matches("a ")
            .trim_start_matches("an ")
            .trim();
        let subtype = match token {
            "plains" => "Plains",
            "island" => "Island",
            "swamp" => "Swamp",
            "mountain" => "Mountain",
            "forest" => "Forest",
            _ => return None,
        };
        if !subtypes.contains(&subtype) {
            subtypes.push(subtype);
        }
    }

    if subtypes.is_empty() {
        return None;
    }

    Some(Effect::Unimplemented {
        name: "activate_only_if_controls_land_subtype_any".to_string(),
        description: Some(subtypes.join("|")),
    })
}

fn parse_mana_production_clause(text: &str) -> Option<ManaProduction> {
    if let Some(color_options) = parse_mana_color_set(text) {
        if color_options.len() > 1 {
            return Some(ManaProduction::AnyOneColor {
                count: CountValue::Fixed(1),
                color_options,
            });
        }
    }

    if let Some((colors, _)) = parse_mana_production(text) {
        return Some(ManaProduction::Fixed { colors });
    }

    if let Some((count, _)) = parse_colorless_mana_production(text) {
        return Some(ManaProduction::Colorless { count });
    }

    None
}

fn parse_colorless_mana_production(text: &str) -> Option<(CountValue, &str)> {
    let mut rest = text.trim_start();
    let mut count = 0u32;

    while rest.starts_with('{') {
        let end = rest.find('}')?;
        let symbol = &rest[1..end];
        if !symbol.eq_ignore_ascii_case("C") {
            break;
        }
        count += 1;
        rest = rest[end + 1..].trim_start();
    }

    if count == 0 {
        return None;
    }

    Some((CountValue::Fixed(count), rest))
}

fn parse_mana_count_prefix(text: &str) -> Option<(CountValue, &str)> {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("X ") {
        return Some((CountValue::Variable("X".to_string()), rest.trim_start()));
    }
    if let Some(rest) = trimmed.strip_prefix("x ") {
        return Some((CountValue::Variable("X".to_string()), rest.trim_start()));
    }
    let (count, rest) = parse_number(trimmed)?;
    Some((CountValue::Fixed(count), rest))
}

fn apply_where_x_count_expression(
    count: CountValue,
    where_x_expression: Option<&str>,
) -> CountValue {
    match (count, where_x_expression) {
        (CountValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("X") => {
            CountValue::Variable(expression.to_string())
        }
        (count, _) => count,
    }
}

fn parse_mana_color_set(text: &str) -> Option<Vec<ManaColor>> {
    let mut rest = text.trim().trim_end_matches(['.', '"']).trim();
    if rest.is_empty() {
        return None;
    }

    let mut colors = Vec::new();
    loop {
        let (parsed, after_symbol) = parse_mana_color_symbol(rest)?;
        for color in parsed {
            if !colors.contains(&color) {
                colors.push(color);
            }
        }

        let next = after_symbol.trim_start();
        if next.is_empty() {
            break;
        }

        if let Some(stripped) = next.strip_prefix("and/or ") {
            rest = stripped.trim_start();
            continue;
        }
        if let Some(stripped) = next.strip_prefix("or ") {
            rest = stripped.trim_start();
            continue;
        }
        if let Some(stripped) = next.strip_prefix("and ") {
            rest = stripped.trim_start();
            continue;
        }
        if let Some(stripped) = next.strip_prefix(',') {
            let stripped = stripped.trim_start();
            if let Some(after_or) = stripped.strip_prefix("or ") {
                rest = after_or.trim_start();
                continue;
            }
            if let Some(after_and_or) = stripped.strip_prefix("and/or ") {
                rest = after_and_or.trim_start();
                continue;
            }
            if let Some(after_and) = stripped.strip_prefix("and ") {
                rest = after_and.trim_start();
                continue;
            }
            rest = stripped;
            continue;
        }
        if let Some(stripped) = next.strip_prefix('/') {
            rest = stripped.trim_start();
            continue;
        }

        return None;
    }

    if colors.is_empty() {
        None
    } else {
        Some(colors)
    }
}

fn parse_mana_color_symbol(text: &str) -> Option<(Vec<ManaColor>, &str)> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let end = trimmed.find('}')?;
    let symbol = &trimmed[1..end];
    let colors = parse_mana_color_symbol_set(symbol)?;
    Some((colors, &trimmed[end + 1..]))
}

fn parse_mana_color_symbol_set(symbol: &str) -> Option<Vec<ManaColor>> {
    fn parse_single(code: &str) -> Option<ManaColor> {
        match code {
            "W" => Some(ManaColor::White),
            "U" => Some(ManaColor::Blue),
            "B" => Some(ManaColor::Black),
            "R" => Some(ManaColor::Red),
            "G" => Some(ManaColor::Green),
            _ => None,
        }
    }

    let symbol = symbol.trim().to_ascii_uppercase();
    if let Some(color) = parse_single(&symbol) {
        return Some(vec![color]);
    }

    let mut colors = Vec::new();
    for part in symbol.split('/') {
        let color = parse_single(part.trim())?;
        if !colors.contains(&color) {
            colors.push(color);
        }
    }

    if colors.is_empty() {
        None
    } else {
        Some(colors)
    }
}

fn all_mana_colors() -> Vec<ManaColor> {
    vec![
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClauseBoundary {
    Sentence,
    Then,
    Comma,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClauseChunk {
    text: String,
    boundary_after: Option<ClauseBoundary>,
}

/// Parse a compound effect chain into an `AbilityDefinition` sub-ability chain.
///
/// Phase 1 keeps the existing clause/effect semantics but replaces the fragile
/// textual `replace(", then ", ". ").split(". ")` logic with a boundary-aware
/// splitter that preserves whether a chunk ended a sentence or was linked by
/// `, then`.
pub fn parse_effect_chain(text: &str, kind: AbilityKind) -> AbilityDefinition {
    let chunks = split_clause_sequence(text);
    let mut defs: Vec<AbilityDefinition> = Vec::new();

    for chunk in &chunks {
        let normalized_text = strip_leading_sequence_connector(&chunk.text).trim();
        if normalized_text.is_empty() {
            continue;
        }

        let (condition, text) = strip_additional_cost_conditional(normalized_text);
        let clause = parse_effect_clause(&text);
        let mut def = AbilityDefinition::new(kind, clause.effect);
        if let Some(duration) = clause.duration {
            def = def.duration(duration);
        }
        if let Some(condition) = condition {
            def = def.condition(condition);
        }

        let mut current_defs = vec![def];
        if let Some(sub) = clause.sub_ability {
            current_defs.push(*sub);
        }

        let followup_continuation = defs.last().and_then(|previous| {
            parse_followup_continuation_ast(normalized_text, &previous.effect)
        });
        let absorb_followup = followup_continuation.as_ref().is_some_and(|continuation| {
            current_defs
                .first()
                .is_some_and(|current| continuation_absorbs_current(continuation, &current.effect))
        });
        if let Some(continuation) = followup_continuation {
            apply_clause_continuation(&mut defs, continuation, kind);
        }
        if absorb_followup {
            continue;
        }

        let intrinsic_continuation =
            parse_intrinsic_continuation_ast(normalized_text, &current_defs[0].effect);
        defs.extend(current_defs);

        if let Some(continuation) = intrinsic_continuation {
            apply_clause_continuation(&mut defs, continuation, kind);
        }
    }

    // Chain: last has no sub_ability, each earlier one chains to next
    if defs.len() > 1 {
        let last = defs.pop().unwrap();
        let mut chain = last;
        while let Some(mut prev) = defs.pop() {
            prev.sub_ability = Some(Box::new(chain));
            chain = prev;
        }
        chain
    } else {
        defs.pop().unwrap_or_else(|| {
            AbilityDefinition::new(
                kind,
                Effect::Unimplemented {
                    name: "empty".to_string(),
                    description: None,
                },
            )
        })
    }
}

fn parse_intrinsic_continuation_ast(text: &str, effect: &Effect) -> Option<ContinuationAst> {
    match effect {
        Effect::SearchLibrary { .. } => Some(ContinuationAst::SearchDestination {
            destination: parse_search_destination(&text.to_lowercase()),
        }),
        _ => None,
    }
}

fn parse_followup_continuation_ast(
    text: &str,
    previous_effect: &Effect,
) -> Option<ContinuationAst> {
    let lower = text.to_lowercase();

    match previous_effect {
        Effect::RevealHand { .. }
            if lower.contains("card from it") || lower.contains("card from among") =>
        {
            let card_filter = if lower.starts_with("you choose ") || lower.starts_with("choose ") {
                parse_choose_filter(&lower)
            } else {
                parse_choose_filter_from_sentence(&lower)
            };
            Some(ContinuationAst::RevealHandFilter { card_filter })
        }
        Effect::Mana { .. } => parse_mana_spend_restriction(&lower)
            .map(|restriction| ContinuationAst::ManaRestriction { restriction }),
        Effect::Counter { .. }
            if lower.contains("countered this way") && lower.contains("loses all abilities") =>
        {
            Some(ContinuationAst::CounterSourceStatic {
                source_static: StaticDefinition::continuous().modifications(vec![
                    crate::types::ability::ContinuousModification::RemoveAllAbilities,
                ]),
            })
        }
        _ => None,
    }
}

fn parse_destroy_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    if lower.starts_with("destroy all ") || lower.starts_with("destroy each ") {
        let (target, _) = parse_target(&text[8..]);
        return Some(ZoneCounterImperativeAst::Destroy { target, all: true });
    }
    if lower.starts_with("destroy ") {
        let (target, _) = parse_target(&text[8..]);
        return Some(ZoneCounterImperativeAst::Destroy { target, all: false });
    }
    None
}

fn parse_exile_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    if lower.starts_with("exile all ") || lower.starts_with("exile each ") {
        let (target, _) = parse_target(&text[6..]);
        return Some(ZoneCounterImperativeAst::Exile {
            origin: None,
            target,
            all: true,
        });
    }

    let rest_lower = lower.strip_prefix("exile ")?;
    let (target, _) = parse_target(&text[6..]);
    let origin = infer_origin_zone(rest_lower);
    Some(ZoneCounterImperativeAst::Exile {
        origin,
        target,
        all: false,
    })
}

fn parse_counter_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    let rest = lower.strip_prefix("counter ")?;
    if rest.contains("activated or triggered ability") {
        return Some(ZoneCounterImperativeAst::Counter {
            target: TargetFilter::StackAbility,
            source_static: None,
        });
    }

    let (target, _) = parse_target(&text[8..]);
    let target = if rest.contains("spell") {
        constrain_filter_to_stack(target)
    } else {
        target
    };
    Some(ZoneCounterImperativeAst::Counter {
        target,
        source_static: None,
    })
}

fn parse_cost_resource_ast(text: &str, lower: &str) -> Option<CostResourceImperativeAst> {
    if let Some(Effect::Unimplemented {
        name,
        description: Some(description),
    }) = try_parse_activate_only_condition(text)
    {
        if name == "activate_only_if_controls_land_subtype_any" {
            return Some(
                CostResourceImperativeAst::ActivateOnlyIfControlsLandSubtypeAny {
                    subtypes: description.split('|').map(ToString::to_string).collect(),
                },
            );
        }
    }
    if lower.starts_with("add ") {
        return match try_parse_add_mana_effect(text) {
            Some(Effect::Mana {
                produced,
                restrictions,
            }) => Some(CostResourceImperativeAst::Mana {
                produced,
                restrictions,
            }),
            _ => None,
        };
    }
    if let Some(effect) = try_parse_damage(lower, text) {
        return match effect {
            Effect::DealDamage { amount, target } => Some(CostResourceImperativeAst::Damage {
                amount,
                target,
                all: false,
            }),
            Effect::DamageAll { amount, target } => Some(CostResourceImperativeAst::Damage {
                amount,
                target,
                all: true,
            }),
            _ => None,
        };
    }
    None
}

fn lower_cost_resource_ast(ast: CostResourceImperativeAst) -> Effect {
    match ast {
        CostResourceImperativeAst::ActivateOnlyIfControlsLandSubtypeAny { subtypes } => {
            Effect::Unimplemented {
                name: "activate_only_if_controls_land_subtype_any".to_string(),
                description: Some(subtypes.join("|")),
            }
        }
        CostResourceImperativeAst::Mana {
            produced,
            restrictions,
        } => Effect::Mana {
            produced,
            restrictions,
        },
        CostResourceImperativeAst::Damage {
            amount,
            target,
            all,
        } => {
            if all {
                Effect::DamageAll { amount, target }
            } else {
                Effect::DealDamage { amount, target }
            }
        }
    }
}

fn parse_imperative_family_ast(text: &str, lower: &str) -> Option<ImperativeFamilyAst> {
    if let Some(ast) = parse_cost_resource_ast(text, lower) {
        return Some(ImperativeFamilyAst::CostResource(ast));
    }
    if let Some(ast) = parse_zone_counter_ast(text, lower) {
        return Some(ImperativeFamilyAst::ZoneCounter(ast));
    }
    if let Some(ast) = parse_numeric_imperative_ast(text, lower) {
        return Some(ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)));
    }
    if let Some(ast) = parse_targeted_action_ast(text, lower) {
        return Some(ImperativeFamilyAst::Structured(ImperativeAst::Targeted(
            ast,
        )));
    }
    if let Some(ast) = parse_search_and_creation_ast(text, lower) {
        return Some(ImperativeFamilyAst::Structured(
            ImperativeAst::SearchCreation(ast),
        ));
    }
    if lower == "explore" || lower.starts_with("explore.") {
        return Some(ImperativeFamilyAst::Explore);
    }
    if lower == "proliferate" || lower.starts_with("proliferate.") {
        return Some(ImperativeFamilyAst::Proliferate);
    }
    if let Some(ast) = parse_shuffle_ast(text, lower) {
        return Some(ImperativeFamilyAst::Shuffle(ast));
    }
    if let Some(ast) = parse_hand_reveal_ast(text, lower) {
        return Some(ImperativeFamilyAst::Structured(ImperativeAst::HandReveal(
            ast,
        )));
    }
    if let Some(ast) = parse_utility_imperative_ast(text, lower) {
        return Some(ImperativeFamilyAst::Structured(ImperativeAst::Utility(ast)));
    }
    if let Some(ast) = parse_put_ast(text, lower) {
        return Some(ImperativeFamilyAst::Put(ast));
    }
    if let Some(stripped) = text.strip_prefix("you may ") {
        return Some(ImperativeFamilyAst::YouMay {
            text: stripped.to_string(),
        });
    }
    if let Some(ast) = parse_choose_ast(text, lower) {
        return Some(ImperativeFamilyAst::Structured(ImperativeAst::Choose(ast)));
    }
    None
}

fn lower_imperative_family_ast(ast: ImperativeFamilyAst) -> Effect {
    match ast {
        ImperativeFamilyAst::Structured(ast) => lower_imperative_ast(ast),
        ImperativeFamilyAst::CostResource(ast) => lower_cost_resource_ast(ast),
        ImperativeFamilyAst::ZoneCounter(ast) => lower_zone_counter_ast(ast),
        ImperativeFamilyAst::Explore => Effect::Explore,
        ImperativeFamilyAst::Proliferate => Effect::Proliferate,
        ImperativeFamilyAst::Shuffle(ast) => lower_shuffle_ast(ast),
        ImperativeFamilyAst::Put(ast) => lower_put_ast(ast),
        ImperativeFamilyAst::YouMay { text } => parse_effect(&text),
    }
}

fn parse_zone_counter_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    if let Some(ast) = parse_destroy_ast(text, lower) {
        return Some(ast);
    }
    if let Some(ast) = parse_exile_ast(text, lower) {
        return Some(ast);
    }
    if let Some(ast) = parse_counter_ast(text, lower) {
        return Some(ast);
    }
    if lower.starts_with("put ") && lower.contains("counter") {
        return match try_parse_put_counter(lower, text) {
            Some(Effect::PutCounter {
                counter_type,
                count,
                target,
            }) => Some(ZoneCounterImperativeAst::PutCounter {
                counter_type,
                count,
                target,
            }),
            _ => None,
        };
    }
    if lower.starts_with("remove ") && lower.contains("counter") {
        return match try_parse_remove_counter(lower) {
            Some(Effect::RemoveCounter {
                counter_type,
                count,
                target,
            }) => Some(ZoneCounterImperativeAst::RemoveCounter {
                counter_type,
                count,
                target,
            }),
            _ => None,
        };
    }
    None
}

fn lower_zone_counter_ast(ast: ZoneCounterImperativeAst) -> Effect {
    match ast {
        ZoneCounterImperativeAst::Destroy { target, all } => {
            if all {
                Effect::DestroyAll { target }
            } else {
                Effect::Destroy { target }
            }
        }
        ZoneCounterImperativeAst::Exile {
            origin,
            target,
            all,
        } => {
            if all {
                Effect::ChangeZoneAll {
                    origin,
                    destination: Zone::Exile,
                    target,
                }
            } else {
                Effect::ChangeZone {
                    origin,
                    destination: Zone::Exile,
                    target,
                }
            }
        }
        ZoneCounterImperativeAst::Counter {
            target,
            source_static,
        } => Effect::Counter {
            target,
            source_static,
        },
        ZoneCounterImperativeAst::PutCounter {
            counter_type,
            count,
            target,
        } => Effect::PutCounter {
            counter_type,
            count,
            target,
        },
        ZoneCounterImperativeAst::RemoveCounter {
            counter_type,
            count,
            target,
        } => Effect::RemoveCounter {
            counter_type,
            count,
            target,
        },
    }
}

fn parse_numeric_imperative_ast(text: &str, lower: &str) -> Option<NumericImperativeAst> {
    if lower.starts_with("draw ") {
        let count = parse_number(&text[5..]).map(|(n, _)| n).unwrap_or(1);
        return Some(NumericImperativeAst::Draw { count });
    }

    if lower.contains("gain") && lower.contains("life") {
        let after_gain = if lower.starts_with("you gain ") {
            &text[9..]
        } else if lower.starts_with("gain ") {
            &text[5..]
        } else {
            ""
        };
        if !after_gain.is_empty() {
            let amount = parse_number(after_gain).map(|(n, _)| n as i32).unwrap_or(1);
            return Some(NumericImperativeAst::GainLife { amount });
        }
    }

    if lower.contains("lose") && lower.contains("life") {
        let amount = extract_number_before(lower, "life").unwrap_or(1) as i32;
        return Some(NumericImperativeAst::LoseLife { amount });
    }

    if lower.contains("gets +")
        || lower.contains("gets -")
        || lower.contains("get +")
        || lower.contains("get -")
    {
        if let Some(Effect::Pump {
            power,
            toughness,
            target: TargetFilter::Any,
        }) = try_parse_pump(lower, text)
        {
            return Some(NumericImperativeAst::Pump { power, toughness });
        }
    }

    if lower.starts_with("scry ") {
        let count = parse_number(&text[5..]).map(|(n, _)| n).unwrap_or(1);
        return Some(NumericImperativeAst::Scry { count });
    }
    if lower.starts_with("surveil ") {
        let count = parse_number(&text[8..]).map(|(n, _)| n).unwrap_or(1);
        return Some(NumericImperativeAst::Surveil { count });
    }
    if lower.starts_with("mill ") {
        let count = parse_number(&text[5..]).map(|(n, _)| n).unwrap_or(1);
        return Some(NumericImperativeAst::Mill { count });
    }

    None
}

fn parse_targeted_action_ast(text: &str, lower: &str) -> Option<TargetedImperativeAst> {
    if lower.starts_with("tap ") {
        let (target, _) = parse_target(&text[4..]);
        return Some(TargetedImperativeAst::Tap { target });
    }
    if lower.starts_with("untap ") {
        let (target, _) = parse_target(&text[6..]);
        return Some(TargetedImperativeAst::Untap { target });
    }
    if lower.starts_with("sacrifice ") {
        let (target, _) = parse_target(&text[10..]);
        return Some(TargetedImperativeAst::Sacrifice { target });
    }
    if lower.starts_with("discard ") {
        let count = parse_number(&text[8..]).map(|(n, _)| n).unwrap_or(1);
        return Some(TargetedImperativeAst::Discard { count });
    }
    if lower.starts_with("return ") {
        let (target, _) = parse_target(&text[7..]);
        return Some(TargetedImperativeAst::Return { target });
    }
    if lower.starts_with("fight ") {
        let (target, _) = parse_target(&text[6..]);
        return Some(TargetedImperativeAst::Fight { target });
    }
    if lower.starts_with("gain control of ") {
        let (target, _) = parse_target(&text[16..]);
        return Some(TargetedImperativeAst::GainControl { target });
    }
    None
}

fn lower_numeric_imperative_ast(ast: NumericImperativeAst) -> Effect {
    match ast {
        NumericImperativeAst::Draw { count } => Effect::Draw { count },
        NumericImperativeAst::GainLife { amount } => Effect::GainLife {
            amount: LifeAmount::Fixed(amount),
            player: GainLifePlayer::Controller,
        },
        NumericImperativeAst::LoseLife { amount } => Effect::LoseLife { amount },
        NumericImperativeAst::Pump { power, toughness } => Effect::Pump {
            power,
            toughness,
            target: TargetFilter::Any,
        },
        NumericImperativeAst::Scry { count } => Effect::Scry { count },
        NumericImperativeAst::Surveil { count } => Effect::Surveil { count },
        NumericImperativeAst::Mill { count } => Effect::Mill {
            count,
            target: TargetFilter::Any,
        },
    }
}

fn lower_targeted_action_ast(ast: TargetedImperativeAst) -> Effect {
    match ast {
        TargetedImperativeAst::Tap { target } => Effect::Tap { target },
        TargetedImperativeAst::Untap { target } => Effect::Untap { target },
        TargetedImperativeAst::Sacrifice { target } => Effect::Sacrifice { target },
        TargetedImperativeAst::Discard { count } => Effect::Discard {
            count,
            target: TargetFilter::Any,
        },
        TargetedImperativeAst::Return { target } => Effect::Bounce {
            target,
            destination: None,
        },
        TargetedImperativeAst::Fight { target } => Effect::Fight { target },
        TargetedImperativeAst::GainControl { target } => Effect::GainControl { target },
    }
}

fn parse_search_and_creation_ast(text: &str, lower: &str) -> Option<SearchCreationImperativeAst> {
    if starts_with_possessive(lower, "search", "library") {
        let details = parse_search_library_details(lower);
        return Some(SearchCreationImperativeAst::SearchLibrary {
            filter: details.filter,
            count: details.count,
            reveal: details.reveal,
        });
    }
    if lower.starts_with("look at the top ") {
        let count = parse_number(&text[16..]).map(|(n, _)| n).unwrap_or(1);
        return Some(SearchCreationImperativeAst::Dig { count });
    }
    if lower.starts_with("create ") {
        return match try_parse_token(lower, text) {
            Some(Effect::CopySpell { target }) => {
                Some(SearchCreationImperativeAst::CopyTokenOf { target })
            }
            Some(Effect::Token {
                name,
                power,
                toughness,
                types,
                colors,
                keywords,
                tapped,
                count,
            }) => Some(SearchCreationImperativeAst::Token {
                token: TokenDescription {
                    name,
                    power: Some(power),
                    toughness: Some(toughness),
                    types,
                    colors,
                    keywords,
                    tapped,
                    count,
                },
            }),
            _ => None,
        };
    }
    None
}

fn lower_search_and_creation_ast(ast: SearchCreationImperativeAst) -> Effect {
    match ast {
        SearchCreationImperativeAst::SearchLibrary {
            filter,
            count,
            reveal,
        } => Effect::SearchLibrary {
            filter,
            count,
            reveal,
        },
        SearchCreationImperativeAst::Dig { count } => Effect::Dig {
            count,
            destination: None,
        },
        SearchCreationImperativeAst::CopyTokenOf { target } => Effect::CopySpell { target },
        SearchCreationImperativeAst::Token { token } => Effect::Token {
            name: token.name,
            power: token.power.unwrap_or(PtValue::Fixed(0)),
            toughness: token.toughness.unwrap_or(PtValue::Fixed(0)),
            types: token.types,
            colors: token.colors,
            keywords: token.keywords,
            tapped: token.tapped,
            count: token.count,
        },
    }
}

fn parse_hand_reveal_ast(text: &str, lower: &str) -> Option<HandRevealImperativeAst> {
    if lower.starts_with("look at ") && lower.contains("hand") {
        if contains_possessive(lower, "look at", "hand") {
            return Some(HandRevealImperativeAst::LookAtHand {
                target: TargetFilter::Any,
            });
        }

        let after_look_at = &text[8..];
        let (target, _) = parse_target(after_look_at);
        return Some(HandRevealImperativeAst::LookAtHand { target });
    }

    if !lower.starts_with("reveal ") {
        return None;
    }

    if lower.contains("hand") {
        return Some(HandRevealImperativeAst::RevealHand);
    }

    let count = if let Some(pos) = lower.find("the top ") {
        let after_top = &lower[pos + 8..];
        parse_number(after_top).map(|(n, _)| n).unwrap_or(1)
    } else {
        1
    };
    Some(HandRevealImperativeAst::RevealTop { count })
}

fn lower_hand_reveal_ast(ast: HandRevealImperativeAst) -> Effect {
    match ast {
        HandRevealImperativeAst::LookAtHand { target } => Effect::RevealHand {
            target,
            card_filter: TargetFilter::Any,
        },
        HandRevealImperativeAst::RevealHand => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter: TargetFilter::Any,
        },
        HandRevealImperativeAst::RevealTop { count } => Effect::Dig {
            count,
            destination: None,
        },
    }
}

fn parse_choose_ast(text: &str, lower: &str) -> Option<ChooseImperativeAst> {
    if let Some(rest) = lower.strip_prefix("choose ") {
        if is_choose_as_targeting(rest) {
            let stripped = &text["choose ".len()..];
            let inner = parse_effect(stripped);
            if !matches!(inner, Effect::Unimplemented { .. }) {
                return Some(ChooseImperativeAst::Reparse {
                    text: stripped.to_string(),
                });
            }
            let (target, _) = parse_target(stripped);
            return Some(ChooseImperativeAst::TargetOnly { target });
        }
    }

    if let Some(choice_type) = try_parse_named_choice(lower) {
        return Some(ChooseImperativeAst::NamedChoice { choice_type });
    }

    if lower.starts_with("choose ") && lower.contains("card from it") {
        return Some(ChooseImperativeAst::RevealHandFilter {
            card_filter: parse_choose_filter(lower),
        });
    }

    None
}

fn lower_choose_ast(ast: ChooseImperativeAst) -> Effect {
    match ast {
        ChooseImperativeAst::TargetOnly { target } => Effect::TargetOnly { target },
        ChooseImperativeAst::Reparse { text } => parse_effect(&text),
        ChooseImperativeAst::NamedChoice { choice_type } => Effect::Choose {
            choice_type,
            persist: false,
        },
        ChooseImperativeAst::RevealHandFilter { card_filter } => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter,
        },
    }
}

fn parse_utility_imperative_ast(text: &str, lower: &str) -> Option<UtilityImperativeAst> {
    if lower.starts_with("prevent ") {
        return Some(UtilityImperativeAst::Prevent {
            text: text.to_string(),
        });
    }
    if lower.starts_with("regenerate ") {
        return Some(UtilityImperativeAst::Regenerate {
            text: text.to_string(),
        });
    }
    if lower.starts_with("copy ") {
        let (target, _) = parse_target(&text[5..]);
        return Some(UtilityImperativeAst::Copy { target });
    }
    if matches!(
        lower,
        "transform"
            | "transform ~"
            | "transform this"
            | "transform this creature"
            | "transform this permanent"
            | "transform this artifact"
            | "transform this land"
    ) {
        return Some(UtilityImperativeAst::Transform {
            target: TargetFilter::SelfRef,
        });
    }
    if lower.starts_with("transform ") {
        let rest = &text["transform ".len()..];
        let (target, _) = parse_target(rest);
        if !matches!(target, TargetFilter::Any) {
            return Some(UtilityImperativeAst::Transform { target });
        }
    }
    if lower.starts_with("attach ") {
        let to_pos = lower.find(" to ").map(|p| p + 4).unwrap_or(7);
        let (target, _) = parse_target(&text[to_pos..]);
        return Some(UtilityImperativeAst::Attach { target });
    }
    None
}

fn lower_utility_imperative_ast(ast: UtilityImperativeAst) -> Effect {
    match ast {
        UtilityImperativeAst::Prevent { text } => Effect::Unimplemented {
            name: "prevent".to_string(),
            description: Some(text),
        },
        UtilityImperativeAst::Regenerate { text } => Effect::Unimplemented {
            name: "regenerate".to_string(),
            description: Some(text),
        },
        UtilityImperativeAst::Copy { target } => Effect::CopySpell { target },
        UtilityImperativeAst::Transform { target } => Effect::Transform { target },
        UtilityImperativeAst::Attach { target } => Effect::Attach { target },
    }
}

fn lower_imperative_ast(ast: ImperativeAst) -> Effect {
    match ast {
        ImperativeAst::Numeric(ast) => lower_numeric_imperative_ast(ast),
        ImperativeAst::Targeted(ast) => lower_targeted_action_ast(ast),
        ImperativeAst::SearchCreation(ast) => lower_search_and_creation_ast(ast),
        ImperativeAst::HandReveal(ast) => lower_hand_reveal_ast(ast),
        ImperativeAst::Choose(ast) => lower_choose_ast(ast),
        ImperativeAst::Utility(ast) => lower_utility_imperative_ast(ast),
    }
}

fn parse_put_ast(text: &str, lower: &str) -> Option<PutImperativeAst> {
    if !lower.starts_with("put ") {
        return None;
    }

    if lower.starts_with("put the top ") && lower.contains("graveyard") {
        let after = &lower[12..];
        let count = parse_number(after).map(|(n, _)| n).unwrap_or(1);
        return Some(PutImperativeAst::Mill { count });
    }

    if let Some(Effect::ChangeZone {
        origin,
        destination,
        target,
    }) = try_parse_put_zone_change(lower, text)
    {
        return Some(PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
        });
    }

    if lower.starts_with("put ") && lower.contains("on top of") && lower.contains("library") {
        return Some(PutImperativeAst::TopOfLibrary);
    }

    None
}

fn lower_put_ast(ast: PutImperativeAst) -> Effect {
    match ast {
        PutImperativeAst::Mill { count } => Effect::Mill {
            count,
            target: TargetFilter::Any,
        },
        PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
        } => Effect::ChangeZone {
            origin,
            destination,
            target,
        },
        PutImperativeAst::TopOfLibrary => Effect::ChangeZone {
            origin: None,
            destination: Zone::Library,
            target: TargetFilter::Any,
        },
    }
}

fn parse_shuffle_ast(text: &str, lower: &str) -> Option<ShuffleImperativeAst> {
    if matches!(lower, "shuffle" | "then shuffle") {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Controller,
        });
    }
    if matches!(lower, "that player shuffles" | "target player shuffles") {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Player,
        });
    }
    if !lower.starts_with("shuffle") || !lower.contains("library") {
        return None;
    }

    // "shuffle {possessive} library" — extract the possessive word to determine the target.
    // Only matches the exact form "shuffle your library" / "shuffle their library" etc.;
    // compound forms like "shuffle your graveyard into your library" fall through.
    if let Some(possessive) = lower
        .strip_prefix("shuffle ")
        .and_then(|s| s.strip_suffix(" library"))
    {
        let target = match possessive {
            "your" => Some(TargetFilter::Controller),
            "their" | "its owner's" | "that player's" => Some(TargetFilter::Player),
            _ => None,
        };
        if let Some(target) = target {
            return Some(ShuffleImperativeAst::ShuffleLibrary { target });
        }
    }
    if contains_object_pronoun(lower, "shuffle", "into")
        || contains_object_pronoun(lower, "shuffles", "into")
    {
        return Some(ShuffleImperativeAst::ChangeZoneToLibrary);
    }
    if contains_possessive(lower, "shuffle", "graveyard") {
        return Some(ShuffleImperativeAst::ChangeZoneAllToLibrary {
            origin: Zone::Graveyard,
        });
    }
    if contains_possessive(lower, "shuffle", "hand") {
        return Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origin: Zone::Hand });
    }

    Some(ShuffleImperativeAst::Unimplemented {
        text: text.to_string(),
    })
}

fn lower_shuffle_ast(ast: ShuffleImperativeAst) -> Effect {
    match ast {
        ShuffleImperativeAst::ShuffleLibrary { target } => Effect::Shuffle { target },
        ShuffleImperativeAst::ChangeZoneToLibrary => Effect::ChangeZone {
            origin: None,
            destination: Zone::Library,
            target: TargetFilter::Any,
        },
        ShuffleImperativeAst::ChangeZoneAllToLibrary { origin } => Effect::ChangeZoneAll {
            origin: Some(origin),
            destination: Zone::Library,
            target: TargetFilter::Controller,
        },
        ShuffleImperativeAst::Unimplemented { text } => Effect::Unimplemented {
            name: "shuffle".to_string(),
            description: Some(text),
        },
    }
}

fn apply_clause_continuation(
    defs: &mut Vec<AbilityDefinition>,
    continuation: ContinuationAst,
    kind: AbilityKind,
) {
    match continuation {
        ContinuationAst::SearchDestination { destination } => {
            defs.push(AbilityDefinition::new(
                kind,
                Effect::ChangeZone {
                    origin: Some(Zone::Library),
                    destination,
                    target: TargetFilter::Any,
                },
            ));
        }
        ContinuationAst::RevealHandFilter { card_filter } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::RevealHand {
                card_filter: existing,
                ..
            } = &mut previous.effect
            {
                *existing = card_filter;
            }
        }
        ContinuationAst::ManaRestriction { restriction } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Mana { restrictions, .. } = &mut previous.effect {
                restrictions.push(restriction);
            }
        }
        ContinuationAst::CounterSourceStatic { source_static } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Counter {
                source_static: existing,
                ..
            } = &mut previous.effect
            {
                *existing = Some(source_static);
            }
        }
    }
}

fn continuation_absorbs_current(continuation: &ContinuationAst, current_effect: &Effect) -> bool {
    match continuation {
        ContinuationAst::RevealHandFilter { .. } => {
            matches!(current_effect, Effect::RevealHand { .. })
        }
        ContinuationAst::ManaRestriction { .. } | ContinuationAst::CounterSourceStatic { .. } => {
            true
        }
        ContinuationAst::SearchDestination { .. } => false,
    }
}

fn split_clause_sequence(text: &str) -> Vec<ClauseChunk> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    let mut paren_depth = 0usize;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

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
                    in_single_quote = !in_single_quote;
                    current.push(ch);
                }
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                current.push(ch);
            }
            ',' if paren_depth == 0 && !in_single_quote && !in_double_quote => {
                let remainder = chars.clone().collect::<String>();
                if let Some((boundary, chars_to_skip)) =
                    split_comma_clause_boundary(&current, &remainder)
                {
                    push_clause_chunk(&mut chunks, &current, Some(boundary));
                    current.clear();
                    for _ in 0..chars_to_skip {
                        chars.next();
                    }
                } else {
                    current.push(ch);
                }
            }
            '.' if paren_depth == 0 && !in_single_quote && !in_double_quote => {
                push_clause_chunk(&mut chunks, &current, Some(ClauseBoundary::Sentence));
                current.clear();
                while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
                    chars.next();
                }
            }
            _ => current.push(ch),
        }
    }

    push_clause_chunk(&mut chunks, &current, None);
    chunks
}

fn split_comma_clause_boundary(current: &str, remainder: &str) -> Option<(ClauseBoundary, usize)> {
    let current_lower = current.trim().to_ascii_lowercase();
    let trimmed = remainder.trim_start();
    let whitespace_len = remainder.len() - trimmed.len();
    let trimmed_lower = trimmed.to_ascii_lowercase();

    if starts_prefix_clause(&current_lower) {
        return None;
    }

    if current_lower.starts_with("search ")
        && (trimmed_lower.starts_with("reveal ") || trimmed_lower.starts_with("put "))
    {
        return None;
    }

    if let Some(after_then) = trimmed.strip_prefix("then ") {
        if starts_clause_text(after_then) {
            return Some((ClauseBoundary::Then, whitespace_len + "then ".len()));
        }
    }

    if starts_clause_text(trimmed) {
        return Some((ClauseBoundary::Comma, whitespace_len));
    }

    None
}

fn starts_prefix_clause(current_lower: &str) -> bool {
    current_lower.starts_with("until ") || current_lower.starts_with("if ")
}

fn starts_clause_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let prefixes = [
        "add ",
        "all ",
        "attach ",
        "counter ",
        "create ",
        "deal ",
        "destroy ",
        "discard ",
        "draw ",
        "each ",
        "each player ",
        "each opponent ",
        "exile ",
        "explore",
        "fight ",
        "gain control ",
        "gain ",
        "look at ",
        "lose ",
        "mill ",
        "proliferate",
        "put ",
        "return ",
        "reveal ",
        "sacrifice ",
        "scry ",
        "search ",
        "shuffle ",
        "surveil ",
        "tap ",
        "that ",
        "this ",
        "those ",
        "they ",
        "target ",
        "untap ",
        "you may ",
        "you ",
        "it ",
    ];

    prefixes.iter().any(|prefix| lower.starts_with(prefix))
}

fn is_possessive_apostrophe(current: &str, next: Option<char>) -> bool {
    let prev = current.chars().last();
    matches!(
        (prev, next),
        (Some(prev), Some(next)) if prev.is_alphanumeric() && next.is_alphanumeric()
    )
}

fn push_clause_chunk(
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

/// Parse a "Spend this mana only to cast..." clause into a `ManaSpendRestriction`.
///
/// Handles patterns like:
/// - "spend this mana only to cast creature spells" → SpellType("Creature")
/// - "spend this mana only to cast a creature spell of the chosen type" → ChosenCreatureType
/// - "spend this mana only to cast a creature spell of the chosen type, and that spell can't be countered" → ChosenCreatureType
fn parse_mana_spend_restriction(lower: &str) -> Option<ManaSpendRestriction> {
    let rest = lower
        .strip_prefix("spend this mana only to cast ")?
        .trim_end_matches(['.', '"']);

    // Strip trailing ", and that spell can't be countered" or similar trailing clauses
    let rest = rest.split(", and ").next().unwrap_or(rest).trim();

    if rest.contains("of the chosen type") {
        return Some(ManaSpendRestriction::ChosenCreatureType);
    }

    // "creature spells" / "a creature spell" / "artifact spells" etc.
    let rest = rest
        .strip_prefix("a ")
        .or_else(|| rest.strip_prefix("an "))
        .unwrap_or(rest);
    let type_word = rest.split_whitespace().next()?;
    let type_name = capitalize(type_word);
    Some(ManaSpendRestriction::SpellType(type_name))
}

// --- Search library parser ---

fn parse_search_library_details(lower: &str) -> SearchLibraryDetails {
    let filter = if let Some(for_idx) = lower.find("for a ") {
        let after_for = &lower[for_idx + 6..];
        parse_search_filter(after_for)
    } else if let Some(for_idx) = lower.find("for an ") {
        let after_for = &lower[for_idx + 7..];
        parse_search_filter(after_for)
    } else {
        TargetFilter::Any
    };

    let reveal = lower.contains("reveal");
    let count = if lower.contains("up to two") {
        2
    } else if lower.contains("up to three") {
        3
    } else {
        1
    };

    SearchLibraryDetails {
        filter,
        count,
        reveal,
    }
}

/// Parse the card type filter from search text like "basic land card, ..."
/// or "creature card with ..." into a TargetFilter.
fn parse_search_filter(text: &str) -> TargetFilter {
    // Find the end of the type description (before comma, period, or "and put")
    let type_end = text
        .find(',')
        .or_else(|| text.find('.'))
        .or_else(|| text.find(" and put"))
        .or_else(|| text.find(" and shuffle"))
        .unwrap_or(text.len());
    let type_text = text[..type_end].trim();

    // Strip trailing "card" or "cards"
    let type_text = type_text
        .strip_suffix(" cards")
        .or_else(|| type_text.strip_suffix(" card"))
        .unwrap_or(type_text)
        .trim();

    // Check for "a card" / "card" alone (Demonic Tutor pattern)
    if type_text == "card" || type_text.is_empty() {
        return TargetFilter::Any;
    }

    // Check for "basic land" pattern
    let is_basic = type_text.contains("basic");
    let clean = type_text.replace("basic ", "");

    // Map type name to TypeFilter
    let card_type = match clean.trim() {
        "land" => Some(TypeFilter::Land),
        "creature" => Some(TypeFilter::Creature),
        "artifact" => Some(TypeFilter::Artifact),
        "enchantment" => Some(TypeFilter::Enchantment),
        "instant" => Some(TypeFilter::Instant),
        "sorcery" => Some(TypeFilter::Sorcery),
        "planeswalker" => Some(TypeFilter::Planeswalker),
        "instant or sorcery" => {
            let mut properties = vec![];
            if is_basic {
                properties.push(FilterProp::HasSupertype {
                    value: "Basic".to_string(),
                });
            }
            return TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Instant).properties(properties.clone()),
                    ),
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Sorcery).properties(properties),
                    ),
                ],
            };
        }
        other => {
            // Could be a subtype search: "forest card", "plains card", "equipment card"
            let land_subtypes = ["plains", "island", "swamp", "mountain", "forest"];
            if land_subtypes.contains(&other) {
                let mut properties = vec![];
                if is_basic {
                    properties.push(FilterProp::HasSupertype {
                        value: "Basic".to_string(),
                    });
                }
                return TargetFilter::Typed(
                    TypedFilter::land()
                        .subtype(capitalize(other))
                        .properties(properties),
                );
            }
            if other == "equipment" {
                return TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact).subtype("Equipment".to_string()),
                );
            }
            if other == "aura" {
                return TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Enchantment).subtype("Aura".to_string()),
                );
            }
            // Fallback: treat as Any
            return TargetFilter::Any;
        }
    };

    let mut properties = vec![];
    if is_basic {
        properties.push(FilterProp::HasSupertype {
            value: "Basic".to_string(),
        });
    }

    TargetFilter::Typed(TypedFilter {
        card_type,
        subtype: None,
        controller: None,
        properties,
    })
}

/// Parse the destination zone from search Oracle text.
/// Looks for "put it into your hand", "put it onto the battlefield", etc.
fn parse_search_destination(lower: &str) -> Zone {
    if lower.contains("onto the battlefield") {
        Zone::Battlefield
    } else if contains_possessive(lower, "into", "hand") {
        Zone::Hand
    } else if contains_possessive(lower, "on top of", "library") {
        Zone::Library
    } else if contains_possessive(lower, "into", "graveyard") {
        Zone::Graveyard
    } else {
        // Default destination for tutors is hand
        Zone::Hand
    }
}

/// Capitalize the first letter of a string (for subtype names).
fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

// --- Helper parsers ---

fn parsed_clause(effect: Effect) -> ParsedEffectClause {
    ParsedEffectClause {
        effect,
        duration: None,
        sub_ability: None,
    }
}

fn with_clause_duration(mut clause: ParsedEffectClause, duration: Duration) -> ParsedEffectClause {
    if clause.duration.is_none() {
        clause.duration = Some(duration.clone());
    }
    if let Effect::GenericEffect {
        duration: effect_duration,
        ..
    } = &mut clause.effect
    {
        if effect_duration.is_none() {
            *effect_duration = Some(duration);
        }
    }
    clause
}

fn try_parse_subject_predicate_ast(text: &str) -> Option<ClauseAst> {
    if try_parse_targeted_controller_gain_life(text).is_some() {
        return None;
    }

    if let Some(clause) = try_parse_subject_continuous_clause(text) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, sub_ability| PredicateAst::Continuous {
                effect,
                duration,
                sub_ability,
            },
        ));
    }

    if let Some(clause) = try_parse_subject_become_clause(text) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, _sub_ability| PredicateAst::Become { effect, duration },
        ));
    }

    if let Some(clause) = try_parse_subject_restriction_clause(text) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, _sub_ability| PredicateAst::Restriction { effect, duration },
        ));
    }

    if let Some(stripped) = strip_subject_clause(text) {
        let subject_text = extract_subject_text(text)?;
        let application = parse_subject_application(&subject_text).unwrap_or(SubjectApplication {
            affected: TargetFilter::Any,
            target: None,
        });
        return Some(ClauseAst::SubjectPredicate {
            subject: SubjectPhraseAst {
                affected: application.affected,
                target: application.target,
            },
            predicate: Box::new(PredicateAst::ImperativeFallback { text: stripped }),
        });
    }

    None
}

fn subject_predicate_ast_from_clause<F>(
    text: &str,
    clause: ParsedEffectClause,
    build_predicate: F,
) -> ClauseAst
where
    F: FnOnce(Effect, Option<Duration>, Option<Box<AbilityDefinition>>) -> PredicateAst,
{
    let subject_text = extract_subject_text(text).unwrap_or_default();
    let application = parse_subject_application(&subject_text).unwrap_or(SubjectApplication {
        affected: TargetFilter::Any,
        target: None,
    });

    ClauseAst::SubjectPredicate {
        subject: SubjectPhraseAst {
            affected: application.affected,
            target: application.target,
        },
        predicate: Box::new(build_predicate(clause.effect, clause.duration, clause.sub_ability)),
    }
}

fn extract_subject_text(text: &str) -> Option<String> {
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    if subject.is_empty() {
        None
    } else {
        Some(subject.to_string())
    }
}

fn split_leading_conditional(text: &str) -> Option<(String, String)> {
    let lower = text.to_lowercase();
    if !lower.starts_with("if ") {
        return None;
    }

    let mut paren_depth = 0u32;
    let mut in_quotes = false;

    for (idx, ch) in text.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => paren_depth += 1,
            ')' if !in_quotes => paren_depth = paren_depth.saturating_sub(1),
            ',' if !in_quotes && paren_depth == 0 => {
                let condition_text = text[..idx].trim().to_string();
                let rest = text[idx + 1..].trim();
                if !rest.is_empty() {
                    return Some((condition_text, rest.to_string()));
                }
            }
            _ => {}
        }
    }

    None
}

/// Detect "if this spell's additional cost was paid, {effect}" and return
/// the condition + remaining effect text. Called at the sentence level in
/// parse_effect_chain BEFORE parse_effect_clause, so the condition is preserved
/// rather than being discarded by strip_leading_conditional.
fn strip_additional_cost_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    if let Some(rest) = lower.strip_prefix("if this spell's additional cost was paid, ") {
        let offset = text.len() - rest.len();
        (
            Some(AbilityCondition::AdditionalCostPaid),
            text[offset..].to_string(),
        )
    } else {
        (None, text.to_string())
    }
}

fn strip_leading_duration(text: &str) -> Option<(Duration, &str)> {
    let lower = text.to_lowercase();
    for (prefix, duration) in [
        ("until end of turn, ", Duration::UntilEndOfTurn),
        ("until your next turn, ", Duration::UntilYourNextTurn),
    ] {
        if lower.starts_with(prefix) {
            return Some((duration, text[prefix.len()..].trim()));
        }
    }
    None
}

fn strip_trailing_duration(text: &str) -> (&str, Option<Duration>) {
    let lower = text.to_lowercase();
    for (suffix, duration) in [
        (" this turn", Duration::UntilEndOfTurn),
        (" until end of turn", Duration::UntilEndOfTurn),
        (" until your next turn", Duration::UntilYourNextTurn),
        (
            " until ~ leaves the battlefield",
            Duration::UntilHostLeavesPlay,
        ),
        (
            " until this creature leaves the battlefield",
            Duration::UntilHostLeavesPlay,
        ),
    ] {
        if lower.ends_with(suffix) {
            let end = text.len() - suffix.len();
            return (text[..end].trim_end_matches(',').trim(), Some(duration));
        }
    }
    (text, None)
}

fn try_parse_subject_continuous_clause(text: &str) -> Option<ParsedEffectClause> {
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    let predicate = text[verb_start..].trim();
    let application = parse_subject_application(subject)?;
    build_continuous_clause(application, predicate)
}

fn try_parse_subject_become_clause(text: &str) -> Option<ParsedEffectClause> {
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    let predicate = deconjugate_verb(text[verb_start..].trim());
    if !predicate.to_lowercase().starts_with("become ") {
        return None;
    }
    let application = parse_subject_application(subject)?;
    build_become_clause(application, &predicate)
}

fn try_parse_subject_restriction_clause(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    let (subject, predicate) = if let Some(pos) = lower.find(" can't ") {
        (text[..pos].trim(), text[pos + 1..].trim())
    } else if let Some(pos) = lower.find(" cannot ") {
        (text[..pos].trim(), text[pos + 1..].trim())
    } else {
        return None;
    };
    let application = parse_subject_application(subject)?;
    build_restriction_clause(application, predicate)
}

fn parse_subject_application(subject: &str) -> Option<SubjectApplication> {
    if subject.trim().is_empty() {
        return None;
    }

    let lower = subject.to_lowercase();

    if lower.starts_with("target ") {
        let (filter, _) = parse_target(subject);
        return subject_filter_application(filter, true);
    }
    if lower.starts_with("all ") || lower.starts_with("each ") {
        let (filter, _) = parse_target(subject);
        return subject_filter_application(filter, false);
    }
    if lower.starts_with("enchanted creature")
        || lower.starts_with("enchanted permanent")
        || lower.starts_with("equipped creature")
    {
        let (filter, _) = parse_target(subject);
        return Some(SubjectApplication {
            affected: filter,
            target: None,
        });
    }
    // Bare plural noun phrase subjects ("creatures you control", "other creatures you control")
    // are implicit "all X" forms — strip any "other " prefix and route through parse_target.
    let noun_subject = lower.strip_prefix("other ").unwrap_or(&lower);
    if !noun_subject.starts_with("target ")
        && !noun_subject.starts_with("all ")
        && !noun_subject.starts_with("each ")
    {
        let normalized = format!("all {noun_subject}");
        let (filter, rest) = parse_target(&normalized);
        if rest.trim().is_empty() {
            return subject_filter_application(filter, false);
        }
    }
    if lower == "that player" {
        return Some(SubjectApplication {
            affected: TargetFilter::Player,
            target: None,
        });
    }
    if lower == "that controller" {
        return Some(SubjectApplication {
            affected: TargetFilter::Controller,
            target: None,
        });
    }
    if matches!(
        lower.as_str(),
        "~" | "this"
            | "it"
            | "this card"
            | "this creature"
            | "this permanent"
            | "this artifact"
            | "this land"
    ) {
        return Some(SubjectApplication {
            affected: TargetFilter::SelfRef,
            target: None,
        });
    }

    let (filter, rest) = parse_type_phrase(subject);
    if rest.trim().is_empty() {
        return subject_filter_application(filter, false);
    }

    None
}

fn subject_filter_application(filter: TargetFilter, targeted: bool) -> Option<SubjectApplication> {
    Some(SubjectApplication {
        target: targeted.then_some(filter.clone()),
        affected: filter,
    })
}

/// Build a Pump or PumpAll effect from a subject application and P/T values.
fn build_pump_effect(
    application: &SubjectApplication,
    power: PtValue,
    toughness: PtValue,
) -> Effect {
    if let Some(target) = application.target.clone() {
        Effect::Pump {
            power,
            toughness,
            target,
        }
    } else if application.affected == TargetFilter::SelfRef {
        Effect::Pump {
            power,
            toughness,
            target: TargetFilter::SelfRef,
        }
    } else {
        Effect::PumpAll {
            power,
            toughness,
            target: application.affected.clone(),
        }
    }
}

/// Split compound predicates like "get +1/+1 until end of turn and you gain 1 life"
/// into a pump clause with the remainder chained as a sub_ability.
fn try_split_pump_compound(
    normalized: &str,
    application: &SubjectApplication,
) -> Option<ParsedEffectClause> {
    let lower = normalized.to_lowercase();
    // Find " and " that separates two independent clauses after a pump+duration.
    let and_pos = lower.find(" and ")?;
    let pump_part = &normalized[..and_pos];
    let remainder = normalized[and_pos + " and ".len()..].trim();
    let (remainder_without_duration, _) = strip_trailing_duration(remainder);

    if !parse_continuous_modifications(remainder_without_duration).is_empty() {
        return None;
    }

    let (power, toughness, duration) = parse_pump_clause(pump_part)?;
    let effect = build_pump_effect(application, power, toughness);

    // Parse the remainder as an independent effect chain (sub_ability).
    let sub_ability = if remainder.is_empty() {
        None
    } else {
        Some(Box::new(parse_effect_chain(remainder, AbilityKind::Spell)))
    };
    Some(ParsedEffectClause {
        effect,
        duration,
        sub_ability,
    })
}

fn build_continuous_clause(
    application: SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let normalized = deconjugate_verb(predicate);

    // Try the full predicate first (simple pump with no compound).
    if let Some((power, toughness, duration)) = parse_pump_clause(&normalized) {
        let effect = build_pump_effect(&application, power, toughness);
        return Some(ParsedEffectClause {
            effect,
            duration,
            sub_ability: None,
        });
    }

    // Compound: "get +1/+1 until end of turn and you gain 1 life"
    // Split on " and " that follows a duration marker, producing a pump
    // with a chained sub_ability for the remainder.
    if let Some(clause) = try_split_pump_compound(&normalized, &application) {
        return Some(clause);
    }

    let (predicate, duration) = strip_trailing_duration(&normalized);
    let modifications = parse_continuous_modifications(predicate);
    if modifications.is_empty() {
        return None;
    }

    if let Some((power, toughness)) = extract_pump_modifiers(&modifications) {
        let effect = build_pump_effect(&application, power, toughness);
        return Some(ParsedEffectClause {
            effect,
            duration,
            sub_ability: None,
        });
    }

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(application.affected)
                .modifications(modifications)
                .description(predicate.to_string())],
            duration: duration.clone(),
            target: application.target,
        },
        duration,
        sub_ability: None,
    })
}

fn build_become_clause(
    application: SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let normalized = deconjugate_verb(predicate);
    let (predicate, duration) = strip_trailing_duration(&normalized);
    let become_text = predicate.strip_prefix("become ")?.trim();
    let animation = parse_animation_spec(become_text)?;
    let modifications = animation_modifications(&animation);
    if modifications.is_empty() {
        return None;
    }

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(application.affected)
                .modifications(modifications)
                .description(predicate.to_string())],
            duration: duration.clone(),
            target: application.target,
        },
        duration,
        sub_ability: None,
    })
}

fn build_restriction_clause(
    application: SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let normalized = deconjugate_verb(predicate);
    let (predicate, duration) = strip_trailing_duration(&normalized);
    let lower = predicate.to_lowercase();

    let mode = if matches!(lower.as_str(), "can't block" | "cannot block") {
        StaticMode::CantBlock
    } else if matches!(lower.as_str(), "can't be blocked" | "cannot be blocked") {
        StaticMode::Other("CantBeBlocked".to_string())
    } else {
        return None;
    };

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(mode)
                .affected(application.affected)
                .description(predicate.to_string())],
            duration: duration.clone(),
            target: application.target,
        },
        duration,
        sub_ability: None,
    })
}

fn extract_pump_modifiers(
    modifications: &[crate::types::ability::ContinuousModification],
) -> Option<(PtValue, PtValue)> {
    let mut power = None;
    let mut toughness = None;

    for modification in modifications {
        match modification {
            crate::types::ability::ContinuousModification::AddPower { value } => {
                power = Some(PtValue::Fixed(*value));
            }
            crate::types::ability::ContinuousModification::AddToughness { value } => {
                toughness = Some(PtValue::Fixed(*value));
            }
            _ => return None,
        }
    }

    Some((power?, toughness?))
}

/// Detect "its controller gains life equal to its power" and similar patterns where
/// the targeted permanent's controller gains life based on the permanent's stats.
fn try_parse_targeted_controller_gain_life(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    if !lower.starts_with("its controller ") {
        return None;
    }
    if !lower.contains("gain") || !lower.contains("life") {
        return None;
    }
    let amount = if lower.contains("equal to its power") || lower.contains("its power") {
        LifeAmount::TargetPower
    } else {
        // Try to parse a fixed amount: "its controller gains 3 life"
        let after = &lower["its controller ".len()..];
        let after = after
            .strip_prefix("gains ")
            .or_else(|| after.strip_prefix("gain "))
            .unwrap_or(after);
        LifeAmount::Fixed(parse_number(after).map(|(n, _)| n as i32).unwrap_or(1))
    };
    Some(parsed_clause(Effect::GainLife {
        amount,
        player: GainLifePlayer::TargetedController,
    }))
}

fn strip_subject_clause(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    if !starts_with_subject_prefix(&lower) {
        return None;
    }

    let verb_start = find_predicate_start(text)?;
    let predicate = text[verb_start..].trim();
    if predicate.is_empty() {
        return None;
    }

    Some(deconjugate_verb(predicate))
}

fn try_parse_damage(lower: &str, _text: &str) -> Option<Effect> {
    // Match: "~ deals N damage to {target}" / "deal N damage to {target}"
    // and variable forms like "deal that much damage" or
    // "deal damage equal to its power".
    let pos = lower.find("deals ").or_else(|| lower.find("deal "))?;
    let verb_len = if lower[pos..].starts_with("deals ") {
        6
    } else {
        5
    };
    let after = &_text[pos + verb_len..];
    let after_lower = &lower[pos + verb_len..];

    let (amount, after_target) = if let Some((n, rest)) = parse_number(after_lower) {
        if rest.starts_with("damage") {
            (
                DamageAmount::Fixed(n as i32),
                &after[after.len() - rest.len() + "damage".len()..],
            )
        } else {
            return None;
        }
    } else if after_lower.starts_with("that much damage") {
        (
            DamageAmount::Variable("that much".to_string()),
            &after["that much damage".len()..],
        )
    } else if after_lower.starts_with("damage equal to ") {
        let amount_text = &after["damage equal to ".len()..];
        let to_pos = amount_text.to_lowercase().find(" to ")?;
        (
            DamageAmount::Variable(amount_text[..to_pos].trim().to_string()),
            &amount_text[to_pos + 4..],
        )
    } else {
        return None;
    };

    let after_to = after_target
        .trim()
        .strip_prefix("to ")
        .unwrap_or(after_target)
        .trim();
    if after_to.starts_with("each ") {
        let (target, _) = parse_target(after_to);
        return Some(Effect::DamageAll { amount, target });
    }

    let (target, _) = parse_target(after_to);
    Some(Effect::DealDamage { amount, target })
}

fn try_parse_pump(lower: &str, text: &str) -> Option<Effect> {
    // Match "+N/+M", "+X/+0", "-X/-X", etc.
    let re_pos = lower.find("gets ").or_else(|| lower.find("get "))?;
    let offset = if lower[re_pos..].starts_with("gets ") {
        5
    } else {
        4
    };
    let after = text[re_pos + offset..].trim();
    let token_end = after
        .find(|c: char| c.is_whitespace() || c == ',' || c == '.')
        .unwrap_or(after.len());
    let token = &after[..token_end];
    parse_pt_modifier(token).map(|(power, toughness)| Effect::Pump {
        power,
        toughness,
        target: TargetFilter::Any,
    })
}

fn parse_pump_clause(predicate: &str) -> Option<(PtValue, PtValue, Option<Duration>)> {
    let (without_where, where_x_expression) = strip_trailing_where_x(predicate);
    let (without_duration, duration) = strip_trailing_duration(without_where);
    let lower = without_duration.to_lowercase();

    let after = if lower.starts_with("gets ") {
        &without_duration[5..]
    } else if lower.starts_with("get ") {
        &without_duration[4..]
    } else {
        return None;
    }
    .trim_start();

    let token_end = after
        .find(|c: char| c.is_whitespace() || c == ',' || c == '.')
        .unwrap_or(after.len());
    let token = &after[..token_end];
    let trailing = after[token_end..]
        .trim_start_matches(|c: char| c == ',' || c.is_whitespace())
        .trim();
    if !trailing.is_empty() {
        return None;
    }

    let (power, toughness) = parse_pt_modifier(token)?;
    let power = apply_where_x_expression(power, where_x_expression.as_deref());
    let toughness = apply_where_x_expression(toughness, where_x_expression.as_deref());

    Some((power, toughness, duration))
}

fn strip_trailing_where_x(text: &str) -> (&str, Option<String>) {
    let lower = text.to_lowercase();
    for needle in [", where x is ", " where x is "] {
        if let Some(pos) = lower.find(needle) {
            let expression = text[pos + needle.len()..]
                .trim()
                .trim_end_matches('.')
                .trim()
                .to_string();
            if expression.is_empty() {
                return (text, None);
            }
            return (
                text[..pos].trim_end_matches(',').trim_end(),
                Some(expression),
            );
        }
    }
    (text, None)
}

fn strip_leading_sequence_connector(text: &str) -> &str {
    let trimmed = text.trim_start();

    if trimmed.eq_ignore_ascii_case("then") {
        return "";
    }

    trimmed
        .strip_prefix("Then, ")
        .or_else(|| trimmed.strip_prefix("Then "))
        .or_else(|| trimmed.strip_prefix("then, "))
        .or_else(|| trimmed.strip_prefix("then "))
        .unwrap_or(trimmed)
}

fn apply_where_x_expression(value: PtValue, where_x_expression: Option<&str>) -> PtValue {
    match (value, where_x_expression) {
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("X") => {
            PtValue::Variable(expression.to_string())
        }
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("-X") => {
            PtValue::Variable(format!("-({expression})"))
        }
        (value, _) => value,
    }
}

fn parse_pt_modifier(text: &str) -> Option<(PtValue, PtValue)> {
    let token = text.trim();
    let slash = token.find('/')?;
    let power = parse_signed_pt_component(token[..slash].trim())?;
    let toughness = parse_signed_pt_component(token[slash + 1..].trim())?;
    Some((power, toughness))
}

fn parse_signed_pt_component(text: &str) -> Option<PtValue> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    let (sign, body) = if let Some(rest) = text.strip_prefix('+') {
        (1, rest.trim())
    } else if let Some(rest) = text.strip_prefix('-') {
        (-1, rest.trim())
    } else {
        (1, text)
    };

    if body.eq_ignore_ascii_case("x") {
        return Some(if sign < 0 {
            PtValue::Variable("-X".to_string())
        } else {
            PtValue::Variable("X".to_string())
        });
    }

    let value = body.parse::<i32>().ok()?;
    Some(PtValue::Fixed(sign * value))
}

fn try_parse_put_counter(lower: &str, _text: &str) -> Option<Effect> {
    // "put N {type} counter(s) on {target}"
    let after_put = lower[4..].trim();
    let (count, rest) = parse_number(after_put)?;
    // Next word is counter type (e.g. "+1/+1", "loyalty", "charge")
    let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let raw_type = &rest[..type_end];
    let counter_type = normalize_counter_type(raw_type);

    // Skip "counter" or "counters" keyword, then parse target after "on"
    let after_type = rest[type_end..].trim_start();
    let after_counter_word = after_type
        .strip_prefix("counters")
        .or_else(|| after_type.strip_prefix("counter"))
        .map(|s| s.trim_start())
        .unwrap_or(after_type);

    let target = if let Some(target_text) = after_counter_word.strip_prefix("on ") {
        if target_text.starts_with("this ")
            || target_text.starts_with("~")
            || target_text == "it"
            || target_text.starts_with("it ")
            || target_text.starts_with("itself")
        {
            TargetFilter::SelfRef
        } else {
            parse_target(target_text).0
        }
    } else {
        TargetFilter::SelfRef
    };

    Some(Effect::PutCounter {
        counter_type,
        count: count as i32,
        target,
    })
}

fn try_parse_remove_counter(lower: &str) -> Option<Effect> {
    // "remove N {type} counter(s) from {target}"
    let after_remove = lower[7..].trim();
    let (count, rest) = parse_number(after_remove)?;
    let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let raw_type = &rest[..type_end];
    let counter_type = normalize_counter_type(raw_type);

    let after_type = rest[type_end..].trim_start();
    let after_counter_word = after_type
        .strip_prefix("counters")
        .or_else(|| after_type.strip_prefix("counter"))
        .map(|s| s.trim_start())?;

    let target_text = after_counter_word.strip_prefix("from ")?.trim();
    let target = if target_text.starts_with("this ")
        || target_text.starts_with("~")
        || target_text == "it"
        || target_text.starts_with("it ")
        || target_text.starts_with("itself")
    {
        TargetFilter::SelfRef
    } else {
        parse_target(target_text).0
    };

    Some(Effect::RemoveCounter {
        counter_type,
        count: count as i32,
        target,
    })
}

/// Normalize oracle-text counter type strings to canonical engine names.
fn normalize_counter_type(raw: &str) -> String {
    match raw {
        "+1/+1" => "P1P1".to_string(),
        "-1/-1" => "M1M1".to_string(),
        other => other.to_string(),
    }
}

fn try_parse_token(_lower: &str, text: &str) -> Option<Effect> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();

    // "create a token that's a copy of {target}"
    if lower.contains("token that's a copy of") || lower.contains("token thats a copy of") {
        let copy_pos = lower.find("copy of ").map(|p| p + 8).unwrap_or(lower.len());
        let (target, _) = parse_target(&text[copy_pos..]);
        return Some(Effect::CopySpell { target });
    }

    let after = text[7..].trim();
    let token = parse_token_description(after)?;
    Some(Effect::Token {
        name: token.name,
        power: token.power.unwrap_or(PtValue::Fixed(0)),
        toughness: token.toughness.unwrap_or(PtValue::Fixed(0)),
        types: token.types,
        colors: token.colors,
        keywords: token.keywords,
        tapped: token.tapped,
        count: token.count,
    })
}

fn try_parse_put_zone_change(lower: &str, text: &str) -> Option<Effect> {
    let after_put = &text[4..];
    let after_put_lower = &lower[4..];

    for (needle, destination) in [
        (" onto the battlefield", Zone::Battlefield),
        (" into your hand", Zone::Hand),
        (" into its owner's hand", Zone::Hand),
        (" into their owner's hand", Zone::Hand),
        (" into your graveyard", Zone::Graveyard),
        (" into its owner's graveyard", Zone::Graveyard),
        (" into their owner's graveyard", Zone::Graveyard),
        (" on the bottom of", Zone::Library),
        (" on top of", Zone::Library),
    ] {
        if let Some(pos) = after_put_lower.find(needle) {
            let target_text = after_put[..pos].trim();
            if target_text.is_empty() {
                return None;
            }
            let (target, _) = parse_target(target_text);
            return Some(Effect::ChangeZone {
                origin: infer_origin_zone(after_put_lower),
                destination,
                target,
            });
        }
    }

    None
}

fn infer_origin_zone(lower: &str) -> Option<Zone> {
    if contains_possessive(lower, "from", "graveyard") || lower.contains("from a graveyard") {
        Some(Zone::Graveyard)
    } else if lower.contains("from exile") {
        Some(Zone::Exile)
    } else if contains_possessive(lower, "from", "hand") {
        Some(Zone::Hand)
    } else if contains_possessive(lower, "from", "library") {
        Some(Zone::Library)
    } else {
        None
    }
}

fn parse_token_description(text: &str) -> Option<TokenDescription> {
    let text = text.trim().trim_end_matches('.');
    let lower = text.to_lowercase();
    if lower.contains(" attached to ") {
        return None;
    }

    let (mut count, leading_name, mut rest) =
        if let Some((count, rest)) = parse_token_count_prefix(text) {
            (count, None, rest)
        } else if let Some((name, rest)) = parse_named_token_preamble(text) {
            (CountValue::Fixed(1), Some(name), rest)
        } else {
            return None;
        };
    let mut tapped = false;

    loop {
        let trimmed = rest.trim_start();
        if let Some(stripped) = trimmed.strip_prefix("tapped ") {
            tapped = true;
            rest = stripped;
            continue;
        }
        if let Some(stripped) = trimmed.strip_prefix("untapped ") {
            rest = stripped;
            continue;
        }
        break;
    }

    rest = strip_token_supertypes(rest);

    let (mut power, mut toughness, rest) =
        if let Some((power, toughness, rest)) = parse_token_pt_prefix(rest) {
            (Some(power), Some(toughness), rest)
        } else {
            (None, None, rest)
        };

    let (colors, rest) = parse_token_color_prefix(rest);
    let (descriptor, suffix) = split_token_head(rest)?;
    let (name_override, suffix) = parse_token_name_clause(suffix);
    let keywords = parse_token_keyword_clause(suffix);
    let (mut name, types) = parse_token_identity(descriptor)?;

    if suffix.to_lowercase().contains(" attached to ") {
        return None;
    }

    if let Some(name_override) = leading_name.or(name_override) {
        name = name_override;
    }

    if let Some(where_expression) = extract_token_where_x_expression(suffix) {
        if matches!(&count, CountValue::Variable(alias) if alias == "X") {
            count = CountValue::Variable(where_expression.clone());
        }
        if matches!(&power, Some(PtValue::Variable(alias)) if alias == "X") {
            power = Some(PtValue::Variable(where_expression.clone()));
        }
        if matches!(&toughness, Some(PtValue::Variable(alias)) if alias == "X") {
            toughness = Some(PtValue::Variable(where_expression));
        }
    }

    if let Some(count_expression) = extract_token_count_expression(suffix) {
        if matches!(&count, CountValue::Variable(alias) if alias == "count") {
            count = CountValue::Variable(count_expression);
        }
    }

    if power.is_none() || toughness.is_none() {
        if let Some(pt_expression) = extract_token_pt_expression(suffix) {
            power = Some(PtValue::Variable(pt_expression.clone()));
            toughness = Some(PtValue::Variable(pt_expression));
        }
    }

    let is_creature = types.iter().any(|token_type| token_type == "Creature");
    if is_creature && (power.is_none() || toughness.is_none()) {
        return None;
    }

    Some(TokenDescription {
        name,
        power,
        toughness,
        types,
        colors,
        keywords,
        tapped,
        count,
    })
}

fn parse_token_count_prefix(text: &str) -> Option<(CountValue, &str)> {
    let trimmed = text.trim_start();
    let lower = trimmed.to_lowercase();
    if let Some(rest) = trimmed.strip_prefix("X ") {
        return Some((CountValue::Variable("X".to_string()), rest));
    }
    if let Some(rest) = trimmed.strip_prefix("x ") {
        return Some((CountValue::Variable("X".to_string()), rest));
    }
    if let Some(rest) = trimmed.strip_prefix("that many ") {
        return Some((CountValue::Variable("that many".to_string()), rest));
    }
    if let Some(rest) = trimmed.strip_prefix("a number of ") {
        return Some((CountValue::Variable("count".to_string()), rest));
    }
    let (count, rest) = parse_number(trimmed)?;
    if count == 0 && lower.starts_with('x') {
        return None;
    }
    Some((CountValue::Fixed(count), rest))
}

fn parse_named_token_preamble(text: &str) -> Option<(String, &str)> {
    let comma = text.find(',')?;
    let name = text[..comma].trim().trim_matches('"');
    if name.is_empty() {
        return None;
    }

    let after_comma = text[comma + 1..].trim_start();
    let rest = after_comma
        .strip_prefix("a ")
        .or_else(|| after_comma.strip_prefix("an "))?;
    Some((name.to_string(), rest))
}

fn parse_token_pt_prefix(text: &str) -> Option<(PtValue, PtValue, &str)> {
    let text = text.trim_start();
    let word_end = text.find(char::is_whitespace).unwrap_or(text.len());
    let token = &text[..word_end];
    let slash = token.find('/')?;
    let power = token[..slash].trim();
    let toughness = token[slash + 1..].trim();
    let power = parse_token_pt_component(power)?;
    let toughness = parse_token_pt_component(toughness)?;
    Some((power, toughness, text[word_end..].trim_start()))
}

fn parse_token_pt_component(text: &str) -> Option<PtValue> {
    if text.eq_ignore_ascii_case("x") {
        return Some(PtValue::Variable("X".to_string()));
    }
    text.parse::<i32>().ok().map(PtValue::Fixed)
}

fn strip_token_supertypes(mut text: &str) -> &str {
    loop {
        let trimmed = text.trim_start();
        let Some(stripped) = ["legendary ", "snow ", "basic "]
            .iter()
            .find_map(|prefix| trimmed.strip_prefix(prefix))
        else {
            return trimmed;
        };
        text = stripped;
    }
}

fn parse_token_color_prefix(mut text: &str) -> (Vec<ManaColor>, &str) {
    let mut colors = Vec::new();

    loop {
        let trimmed = text.trim_start();
        let Some((color, rest)) = strip_color_word(trimmed) else {
            break;
        };
        if let Some(color) = color {
            colors.push(color);
        }
        text = rest;

        let trimmed = text.trim_start();
        if let Some(rest) = trimmed.strip_prefix("and ") {
            text = rest;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix(", ") {
            text = rest;
            continue;
        }
        break;
    }

    (colors, text.trim_start())
}

fn strip_color_word(text: &str) -> Option<(Option<ManaColor>, &str)> {
    for (word, color) in [
        ("white", Some(ManaColor::White)),
        ("blue", Some(ManaColor::Blue)),
        ("black", Some(ManaColor::Black)),
        ("red", Some(ManaColor::Red)),
        ("green", Some(ManaColor::Green)),
        ("colorless", None),
    ] {
        if let Some(rest) = text.strip_prefix(word) {
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                return Some((color, rest.trim_start()));
            }
        }
    }
    None
}

fn split_token_head(text: &str) -> Option<(&str, &str)> {
    let lower = text.to_lowercase();
    let pos = lower.find(" token")?;
    let head = text[..pos].trim();
    let mut suffix = &text[pos + 6..];
    if let Some(stripped) = suffix.strip_prefix('s') {
        suffix = stripped;
    }
    if head.is_empty() {
        return None;
    }
    Some((head, suffix.trim()))
}

fn parse_token_name_clause(text: &str) -> (Option<String>, &str) {
    let trimmed = text.trim_start();
    let Some(after_named) = trimmed.strip_prefix("named ") else {
        return (None, trimmed);
    };

    let lower = after_named.to_lowercase();
    let mut end = after_named.len();
    for needle in [" with ", " attached ", ",", "."] {
        if let Some(pos) = lower.find(needle) {
            end = end.min(pos);
        }
    }

    let name = after_named[..end].trim().trim_matches('"');
    let rest = after_named[end..].trim_start();
    if name.is_empty() {
        (None, rest)
    } else {
        (Some(name.to_string()), rest)
    }
}

fn extract_token_where_x_expression(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let pos = lower.find("where x is ")?;
    Some(
        text[pos + "where x is ".len()..]
            .trim()
            .trim_end_matches('.')
            .to_string(),
    )
}

fn extract_token_count_expression(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let pos = lower.find("equal to ")?;
    Some(
        text[pos + "equal to ".len()..]
            .trim()
            .trim_end_matches('.')
            .to_string(),
    )
}

fn extract_token_pt_expression(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for needle in [
        "power and toughness are each equal to ",
        "power and toughness is each equal to ",
    ] {
        if let Some(pos) = lower.find(needle) {
            return Some(
                text[pos + needle.len()..]
                    .trim()
                    .trim_matches('"')
                    .trim_end_matches('.')
                    .to_string(),
            );
        }
    }
    None
}

fn parse_token_identity(descriptor: &str) -> Option<(String, Vec<String>)> {
    let mut core_types = Vec::new();
    let mut subtypes = Vec::new();

    for word in descriptor.split_whitespace() {
        match word.to_lowercase().as_str() {
            "artifact" => push_unique_string(&mut core_types, "Artifact"),
            "creature" => push_unique_string(&mut core_types, "Creature"),
            "enchantment" => push_unique_string(&mut core_types, "Enchantment"),
            "land" => push_unique_string(&mut core_types, "Land"),
            "snow" | "legendary" | "basic" => {}
            _ => subtypes.push(title_case_word(word)),
        }
    }

    if core_types.is_empty() {
        return known_named_token_identity(descriptor);
    }

    let name = if subtypes.is_empty() {
        "Token".to_string()
    } else {
        subtypes.join(" ")
    };

    let mut types = core_types;
    for subtype in subtypes {
        push_unique_owned(&mut types, subtype);
    }

    Some((name, types))
}

fn known_named_token_identity(descriptor: &str) -> Option<(String, Vec<String>)> {
    let name = match descriptor.trim().to_lowercase().as_str() {
        "treasure" => "Treasure",
        "food" => "Food",
        "clue" => "Clue",
        "blood" => "Blood",
        "map" => "Map",
        "powerstone" => "Powerstone",
        "junk" => "Junk",
        "shard" => "Shard",
        _ => return None,
    };

    Some((
        name.to_string(),
        vec!["Artifact".to_string(), name.to_string()],
    ))
}

fn parse_token_keyword_clause(text: &str) -> Vec<Keyword> {
    let trimmed = text.trim_start();
    let Some(after_with) = trimmed.strip_prefix("with ") else {
        return Vec::new();
    };

    let raw_clause = after_with
        .split('"')
        .next()
        .unwrap_or(after_with)
        .split(" where ")
        .next()
        .unwrap_or(after_with)
        .split(" attached ")
        .next()
        .unwrap_or(after_with)
        .trim()
        .trim_end_matches('.')
        .trim_end_matches(" and")
        .trim();

    split_token_keyword_list(raw_clause)
        .into_iter()
        .filter_map(map_token_keyword)
        .collect()
}

fn split_token_keyword_list(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    for chunk in text.split(", and ") {
        for sub in chunk.split(" and ") {
            for item in sub.split(", ") {
                let trimmed = item.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed);
                }
            }
        }
    }
    parts
}

fn map_token_keyword(text: &str) -> Option<Keyword> {
    let trimmed = text.trim();
    if trimmed.eq_ignore_ascii_case("all creature types") {
        return Some(Keyword::Changeling);
    }
    match Keyword::from_str(trimmed) {
        Ok(Keyword::Unknown(_)) => None,
        Ok(keyword) => Some(keyword),
        Err(_) => None,
    }
}

fn title_case_word(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn push_unique_string(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

fn push_unique_owned(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn parse_animation_spec(text: &str) -> Option<AnimationSpec> {
    let lower = text.to_lowercase();
    if lower.contains(" copy of ")
        || lower.contains(" of your choice")
        || lower.contains(" all activated abilities ")
        || lower.contains(" loses all other card types ")
        || lower.contains(" all colors")
    {
        return None;
    }

    let mut spec = AnimationSpec::default();
    let mut rest = text.trim().trim_end_matches('.');

    for suffix in [
        " and loses all other abilities",
        " and it loses all other abilities",
        " and loses all abilities",
    ] {
        if rest.to_lowercase().ends_with(suffix) {
            let end = rest.len() - suffix.len();
            rest = rest[..end].trim_end_matches(',').trim();
            spec.remove_all_abilities = true;
            break;
        }
    }

    if let Some(stripped) = rest.strip_prefix("a ") {
        rest = stripped;
    } else if let Some(stripped) = rest.strip_prefix("an ") {
        rest = stripped;
    }

    if let Some((power, toughness, after_pt)) = parse_fixed_become_pt_prefix(rest) {
        spec.power = Some(power);
        spec.toughness = Some(toughness);
        rest = after_pt;
    }

    if let Some((descriptor, power, toughness)) = split_animation_base_pt_clause(rest) {
        spec.power = Some(power);
        spec.toughness = Some(toughness);
        rest = descriptor;
    }

    let (descriptor, keywords) = split_animation_keyword_clause(rest);
    spec.keywords = keywords;
    rest = descriptor;

    if let Some((colors, after_colors)) = parse_animation_color_prefix(rest) {
        spec.colors = Some(colors);
        rest = after_colors;
    }

    spec.types = parse_animation_types(rest, spec.power.is_some() || spec.toughness.is_some());

    if spec.power.is_none()
        && spec.toughness.is_none()
        && spec.colors.is_none()
        && spec.keywords.is_empty()
        && spec.types.is_empty()
        && !spec.remove_all_abilities
    {
        None
    } else {
        Some(spec)
    }
}

fn animation_modifications(
    spec: &AnimationSpec,
) -> Vec<crate::types::ability::ContinuousModification> {
    use crate::types::ability::ContinuousModification;
    use crate::types::card_type::CoreType;

    let mut modifications = Vec::new();

    if let Some(power) = spec.power {
        modifications.push(ContinuousModification::SetPower { value: power });
    }
    if let Some(toughness) = spec.toughness {
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }
    if let Some(colors) = &spec.colors {
        modifications.push(ContinuousModification::SetColor {
            colors: colors.clone(),
        });
    }
    if spec.remove_all_abilities {
        modifications.push(ContinuousModification::RemoveAllAbilities);
    }
    for keyword in &spec.keywords {
        modifications.push(ContinuousModification::AddKeyword {
            keyword: keyword.clone(),
        });
    }
    for type_name in &spec.types {
        if let Ok(core_type) = CoreType::from_str(type_name) {
            modifications.push(ContinuousModification::AddType { core_type });
        } else {
            modifications.push(ContinuousModification::AddSubtype {
                subtype: type_name.clone(),
            });
        }
    }

    modifications
}

fn parse_animation_color_prefix(text: &str) -> Option<(Vec<ManaColor>, &str)> {
    let mut rest = text.trim_start();
    let mut saw_color = false;
    let mut colors = Vec::new();

    loop {
        if let Some(stripped) = strip_prefix_word(rest, "colorless") {
            saw_color = true;
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "white") {
            saw_color = true;
            colors.push(ManaColor::White);
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "blue") {
            saw_color = true;
            colors.push(ManaColor::Blue);
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "black") {
            saw_color = true;
            colors.push(ManaColor::Black);
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "red") {
            saw_color = true;
            colors.push(ManaColor::Red);
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "green") {
            saw_color = true;
            colors.push(ManaColor::Green);
            rest = stripped;
        } else {
            break;
        }

        if let Some(stripped) = rest.strip_prefix("and ") {
            rest = stripped;
            continue;
        }
        break;
    }

    saw_color.then_some((colors, rest.trim_start()))
}

fn strip_prefix_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(word)?;
    if rest.is_empty() {
        Some(rest)
    } else if rest.starts_with(' ') {
        Some(rest.trim_start())
    } else {
        None
    }
}

fn parse_fixed_become_pt_prefix(text: &str) -> Option<(i32, i32, &str)> {
    let (power, toughness, rest) = parse_token_pt_prefix(text)?;
    match (power, toughness) {
        (PtValue::Fixed(power), PtValue::Fixed(toughness)) => Some((power, toughness, rest)),
        _ => None,
    }
}

fn split_animation_base_pt_clause(text: &str) -> Option<(&str, i32, i32)> {
    let lower = text.to_lowercase();
    let pos = lower.find(" with base power and toughness ")?;
    let descriptor = text[..pos].trim_end_matches(',').trim();
    let pt_text = text[pos + " with base power and toughness ".len()..].trim();
    let (power, toughness, _) = parse_fixed_become_pt_prefix(pt_text)?;
    Some((descriptor, power, toughness))
}

fn parse_animation_types(text: &str, infer_creature: bool) -> Vec<String> {
    let descriptor = text
        .trim()
        .trim_end_matches(',')
        .trim_end_matches(" in addition to its other types")
        .trim();
    if descriptor.is_empty() {
        return Vec::new();
    }

    let mut core_types = Vec::new();
    let mut subtypes = Vec::new();

    for word in descriptor.split_whitespace() {
        match word.to_lowercase().as_str() {
            "artifact" => push_unique_string(&mut core_types, "Artifact"),
            "creature" => push_unique_string(&mut core_types, "Creature"),
            "enchantment" => push_unique_string(&mut core_types, "Enchantment"),
            "land" => push_unique_string(&mut core_types, "Land"),
            "planeswalker" => push_unique_string(&mut core_types, "Planeswalker"),
            "legendary" | "basic" | "snow" | "" => {}
            other => subtypes.push(title_case_word(other)),
        }
    }

    if core_types.is_empty() && subtypes.is_empty() {
        return Vec::new();
    }
    if core_types.is_empty() && infer_creature {
        push_unique_string(&mut core_types, "Creature");
    }

    let mut types = core_types;
    for subtype in subtypes {
        push_unique_owned(&mut types, subtype);
    }

    types
}

fn split_animation_keyword_clause(text: &str) -> (&str, Vec<Keyword>) {
    let lower = text.to_lowercase();
    let Some(pos) = lower.find(" with ") else {
        return (text, Vec::new());
    };

    let prefix = text[..pos].trim_end_matches(',').trim();
    let keyword_text = text[pos + 6..]
        .split('"')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches('.');
    let keywords = split_token_keyword_list(keyword_text)
        .into_iter()
        .filter_map(map_token_keyword)
        .collect();
    (prefix, keywords)
}

/// Strip third-person 's' from the first word: "discards a card" → "discard a card".
fn deconjugate_verb(text: &str) -> String {
    let text = text.trim();
    let first_space = text.find(' ').unwrap_or(text.len());
    let verb = &text[..first_space];
    let rest = &text[first_space..];
    let base = normalize_verb_token(verb);
    format!("{}{}", base, rest)
}

fn starts_with_subject_prefix(lower: &str) -> bool {
    [
        "all ",
        "each opponent ",
        "each player ",
        "enchanted ",
        "equipped ",
        "it ",
        "its controller ",
        "target ",
        "that ",
        "the chosen ",
        "they ",
        "this ",
        "those ",
        "you ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn find_predicate_start(text: &str) -> Option<usize> {
    const VERBS: &[&str] = &[
        "add",
        "attack",
        "become",
        "can",
        "cast",
        "choose",
        "copy",
        "counter",
        "create",
        "deal",
        "discard",
        "draw",
        "exile",
        "explore",
        "fight",
        "gain",
        "get",
        "have",
        "look",
        "lose",
        "mill",
        "pay",
        "put",
        "regenerate",
        "reveal",
        "return",
        "sacrifice",
        "scry",
        "search",
        "shuffle",
        "surveil",
        "tap",
        "transform",
        "untap",
    ];

    let lower = text.to_lowercase();
    let mut word_start = None;

    for (idx, ch) in lower.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = word_start.take() {
                let token = &lower[start..idx];
                if VERBS.contains(&normalize_verb_token(token).as_str()) {
                    return Some(start);
                }
            }
            continue;
        }

        if word_start.is_none() {
            word_start = Some(idx);
        }
    }

    if let Some(start) = word_start {
        let token = &lower[start..];
        if VERBS.contains(&normalize_verb_token(token).as_str()) {
            return Some(start);
        }
    }

    None
}

fn normalize_verb_token(token: &str) -> String {
    let token = token.trim_matches(|c: char| !c.is_alphabetic());
    match token {
        "does" => "do".to_string(),
        "has" => "have".to_string(),
        "is" => "be".to_string(),
        "copies" => "copy".to_string(),
        _ if token.ends_with('s') && !token.ends_with("ss") => token[..token.len() - 1].to_string(),
        _ => token.to_string(),
    }
}

fn extract_number_before(text: &str, before_word: &str) -> Option<u32> {
    let pos = text.find(before_word)?;
    let prefix = text[..pos].trim();
    let last_word = prefix.split_whitespace().last()?;
    last_word.parse::<u32>().ok()
}

fn constrain_filter_to_stack(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(TypedFilter {
            card_type,
            subtype,
            controller,
            mut properties,
        }) => {
            if !properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
            {
                properties.push(FilterProp::InZone { zone: Zone::Stack });
            }
            TargetFilter::Typed(TypedFilter {
                card_type,
                subtype,
                controller,
                properties,
            })
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.into_iter().map(constrain_filter_to_stack).collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters.into_iter().map(constrain_filter_to_stack).collect(),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        ContinuousModification, ControllerRef, ManaProduction, TypeFilter,
    };
    use crate::types::mana::ManaColor;

    #[test]
    fn effect_lightning_bolt() {
        let e = parse_effect("Lightning Bolt deals 3 damage to any target");
        assert!(matches!(
            e,
            Effect::DealDamage {
                amount: DamageAmount::Fixed(3),
                target: TargetFilter::Any
            }
        ));
    }

    #[test]
    fn effect_murder() {
        let e = parse_effect("Destroy target creature");
        assert!(matches!(
            e,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    ..
                })
            }
        ));
    }

    #[test]
    fn effect_giant_growth() {
        let e = parse_effect("Target creature gets +3/+3 until end of turn");
        assert!(matches!(
            e,
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                ..
            }
        ));
    }

    #[test]
    fn effect_counterspell() {
        let e = parse_effect("Counter target spell");
        assert!(matches!(
            e,
            Effect::Counter {
                target: TargetFilter::Typed(TypedFilter { properties, .. }),
                ..
            } if properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
        ));
    }

    #[test]
    fn effect_annul_has_stack_restricted_targets() {
        let e = parse_effect("Counter target artifact or enchantment spell");
        assert!(matches!(
            e,
            Effect::Counter {
                target: TargetFilter::Or { filters },
                ..
            } if filters.iter().all(|f| {
                matches!(
                    f,
                    TargetFilter::Typed(TypedFilter { properties, .. })
                        if properties.iter().any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
                )
            })
        ));
    }

    #[test]
    fn effect_disdainful_stroke_has_cmc_and_stack_restriction() {
        let e = parse_effect("Counter target spell with mana value 4 or greater");
        assert!(matches!(
            e,
            Effect::Counter {
                target: TargetFilter::Typed(TypedFilter { properties, .. }),
                ..
            } if properties.iter().any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
                && properties.iter().any(|p| matches!(p, FilterProp::CmcGE { value: 4 }))
        ));
    }

    #[test]
    fn effect_counter_ability_with_source_static_absorption() {
        use crate::types::ability::ContinuousModification;
        use crate::types::statics::StaticMode;

        let ability = parse_effect_chain(
            "counter up to one target activated or triggered ability. If an ability of an artifact, creature, or planeswalker is countered this way, that permanent loses all abilities for as long as ~ remains on the battlefield",
            AbilityKind::Spell,
        );
        // The "if...countered this way...loses all abilities" sentence should be absorbed
        // into the Counter effect's source_static, not left as a sub_ability.
        assert!(
            ability.sub_ability.is_none(),
            "sub_ability should be absorbed"
        );
        if let Effect::Counter { source_static, .. } = &ability.effect {
            let static_def = source_static.as_ref().expect("should have source_static");
            assert_eq!(static_def.mode, StaticMode::Continuous);
            assert_eq!(
                static_def.modifications,
                vec![ContinuousModification::RemoveAllAbilities]
            );
        } else {
            panic!("expected Counter effect");
        }
    }

    #[test]
    fn effect_mana_production() {
        let e = parse_effect("Add {W}");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::Fixed { ref colors }, ..
            } if colors == &vec![ManaColor::White]
        ));
    }

    #[test]
    fn effect_gain_life() {
        let e = parse_effect("You gain 3 life");
        assert!(matches!(
            e,
            Effect::GainLife {
                amount: LifeAmount::Fixed(3),
                ..
            }
        ));
    }

    #[test]
    fn effect_bounce() {
        let e = parse_effect("Return target creature to its owner's hand");
        assert!(matches!(e, Effect::Bounce { .. }));
    }

    #[test]
    fn effect_draw() {
        let e = parse_effect("Draw two cards");
        assert!(matches!(e, Effect::Draw { count: 2 }));
    }

    #[test]
    fn effect_scry() {
        let e = parse_effect("Scry 2");
        assert!(matches!(e, Effect::Scry { count: 2 }));
    }

    #[test]
    fn effect_disenchant() {
        let e = parse_effect("Destroy target artifact or enchantment");
        assert!(matches!(
            e,
            Effect::Destroy {
                target: TargetFilter::Or { .. }
            }
        ));
    }

    #[test]
    fn effect_explore() {
        let e = parse_effect("Explore");
        assert!(matches!(e, Effect::Explore));
    }

    #[test]
    fn effect_unimplemented_fallback() {
        let e = parse_effect("Fateseal 2");
        assert!(matches!(e, Effect::Unimplemented { .. }));
    }

    #[test]
    fn effect_chain_revitalize() {
        let def = parse_effect_chain("You gain 3 life. Draw a card.", AbilityKind::Spell);
        assert!(matches!(
            def.effect,
            Effect::GainLife {
                amount: LifeAmount::Fixed(3),
                ..
            }
        ));
        assert!(def.sub_ability.is_some());
        assert!(matches!(
            def.sub_ability.unwrap().effect,
            Effect::Draw { count: 1 }
        ));
    }

    #[test]
    fn effect_its_controller_gains_life_equal_to_power() {
        // Swords to Plowshares: "Its controller gains life equal to its power."
        let e = parse_effect("Its controller gains life equal to its power");
        assert!(
            matches!(
                e,
                Effect::GainLife {
                    amount: LifeAmount::TargetPower,
                    player: GainLifePlayer::TargetedController
                }
            ),
            "Expected TargetPower + TargetedController, got {e:?}"
        );
    }

    #[test]
    fn effect_chain_with_em_dash() {
        // Regression: em dash (U+2014, 3 bytes) must not cause a byte-boundary panic
        let def = parse_effect_chain(
            "Spell mastery — Draw two cards. You gain 2 life.",
            AbilityKind::Spell,
        );
        // First sentence contains the em dash, should parse (possibly as unimplemented)
        assert!(def.sub_ability.is_some());
    }

    #[test]
    fn effect_shuffle_library() {
        let e = parse_effect("Shuffle your library");
        assert!(matches!(
            e,
            Effect::Shuffle {
                target: TargetFilter::Controller,
            }
        ));
    }

    #[test]
    fn effect_shuffle_their_library() {
        let e = parse_effect("Shuffle their library");
        assert!(matches!(
            e,
            Effect::Shuffle {
                target: TargetFilter::Player,
            }
        ));
    }

    #[test]
    fn compound_shuffle_it_into_library() {
        let e = parse_effect("Shuffle it into its owner's library");
        assert!(matches!(
            e,
            Effect::ChangeZone {
                destination: Zone::Library,
                ..
            }
        ));
    }

    #[test]
    fn compound_shuffle_graveyard_into_library() {
        let e = parse_effect("Shuffle your graveyard into your library");
        assert!(matches!(
            e,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Library,
                ..
            }
        ));
    }

    #[test]
    fn compound_shuffle_hand_into_library() {
        let e = parse_effect("Shuffle your hand into your library");
        assert!(matches!(
            e,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Hand),
                destination: Zone::Library,
                ..
            }
        ));
    }

    #[test]
    fn parse_search_basic_land_to_hand() {
        let e = parse_effect(
            "Search your library for a basic land card, reveal it, put it into your hand",
        );
        match e {
            Effect::SearchLibrary {
                filter,
                count,
                reveal,
            } => {
                assert_eq!(count, 1);
                assert!(reveal);
                match filter {
                    TargetFilter::Typed(TypedFilter {
                        card_type,
                        properties,
                        ..
                    }) => {
                        assert_eq!(card_type, Some(TypeFilter::Land));
                        assert!(properties.iter().any(|p| matches!(
                            p,
                            FilterProp::HasSupertype { value } if value == "Basic"
                        )));
                    }
                    other => panic!("Expected Typed filter, got {:?}", other),
                }
            }
            other => panic!("Expected SearchLibrary, got {:?}", other),
        }
    }

    #[test]
    fn parse_search_creature_to_hand() {
        let e = parse_effect(
            "Search your library for a creature card, reveal it, put it into your hand",
        );
        match e {
            Effect::SearchLibrary {
                filter,
                count,
                reveal,
            } => {
                assert_eq!(count, 1);
                assert!(reveal);
                assert!(matches!(
                    filter,
                    TargetFilter::Typed(TypedFilter {
                        card_type: Some(TypeFilter::Creature),
                        ..
                    })
                ));
            }
            other => panic!("Expected SearchLibrary, got {:?}", other),
        }
    }

    #[test]
    fn parse_search_any_card_to_hand() {
        let e = parse_effect("Search your library for a card, put it into your hand");
        match e {
            Effect::SearchLibrary { filter, count, .. } => {
                assert_eq!(count, 1);
                assert!(matches!(filter, TargetFilter::Any));
            }
            other => panic!("Expected SearchLibrary, got {:?}", other),
        }
    }

    #[test]
    fn parse_search_land_to_battlefield() {
        let e = parse_effect(
            "Search your library for a basic land card, put it onto the battlefield tapped",
        );
        assert!(matches!(e, Effect::SearchLibrary { .. }));
    }

    #[test]
    fn search_ability_chain_has_changezone_and_shuffle() {
        // Full Oracle text for a tutor like Worldly Tutor / Rampant Growth
        let def = parse_effect_chain(
            "Search your library for a creature card, reveal it, put it into your hand, then shuffle your library",
            AbilityKind::Spell,
        );
        // First effect: SearchLibrary
        assert!(
            matches!(def.effect, Effect::SearchLibrary { .. }),
            "Expected SearchLibrary, got {:?}",
            def.effect
        );

        // Second in chain: ChangeZone to Hand
        let sub1 = def
            .sub_ability
            .as_ref()
            .expect("Should have sub_ability (ChangeZone)");
        assert!(
            matches!(
                sub1.effect,
                Effect::ChangeZone {
                    destination: Zone::Hand,
                    ..
                }
            ),
            "Expected ChangeZone to Hand, got {:?}",
            sub1.effect
        );

        // Third in chain: Shuffle
        let sub2 = sub1
            .sub_ability
            .as_ref()
            .expect("Should have sub_ability (Shuffle)");
        assert!(
            matches!(sub2.effect, Effect::Shuffle { .. }),
            "Expected Shuffle, got {:?}",
            sub2.effect
        );
    }

    #[test]
    fn search_ability_chain_battlefield_destination() {
        let def = parse_effect_chain(
            "Search your library for a basic land card, put it onto the battlefield tapped, then shuffle your library",
            AbilityKind::Spell,
        );
        assert!(matches!(def.effect, Effect::SearchLibrary { .. }));

        let sub1 = def.sub_ability.as_ref().expect("ChangeZone sub");
        assert!(
            matches!(
                sub1.effect,
                Effect::ChangeZone {
                    destination: Zone::Battlefield,
                    ..
                }
            ),
            "Expected ChangeZone to Battlefield, got {:?}",
            sub1.effect
        );
    }

    #[test]
    fn search_chain_with_intermediate_comma_clause() {
        let def = parse_effect_chain(
            "Search your library for a card, put that card into your hand, discard a card at random, then shuffle your library",
            AbilityKind::Spell,
        );
        assert!(matches!(def.effect, Effect::SearchLibrary { .. }));

        let sub1 = def
            .sub_ability
            .as_ref()
            .expect("search should chain into change-zone");
        assert!(matches!(
            sub1.effect,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                ..
            }
        ));

        let sub2 = sub1
            .sub_ability
            .as_ref()
            .expect("change-zone should chain into discard");
        assert!(matches!(sub2.effect, Effect::Discard { count: 1, .. }));

        let sub3 = sub2
            .sub_ability
            .as_ref()
            .expect("discard should chain into shuffle");
        assert!(matches!(sub3.effect, Effect::Shuffle { .. }));
    }

    #[test]
    fn effect_reveal_top_cards() {
        let e = parse_effect("Reveal the top 3 cards of your library");
        assert!(matches!(e, Effect::Dig { count: 3, .. }));
    }

    #[test]
    fn effect_prevent_damage() {
        let e = parse_effect("Prevent the next 3 damage");
        assert!(matches!(
            e,
            Effect::Unimplemented { ref name, .. } if name == "prevent"
        ));
    }

    #[test]
    fn effect_regenerate() {
        let e = parse_effect("Regenerate target creature");
        assert!(matches!(
            e,
            Effect::Unimplemented { ref name, .. } if name == "regenerate"
        ));
    }

    #[test]
    fn effect_copy_spell() {
        let e = parse_effect("Copy target spell");
        assert!(matches!(e, Effect::CopySpell { .. }));
    }

    #[test]
    fn effect_transform() {
        let e = parse_effect("Transform this creature");
        assert!(matches!(
            e,
            Effect::Transform {
                target: TargetFilter::SelfRef
            }
        ));
    }

    #[test]
    fn effect_transform_target_creature() {
        let e = parse_effect("Transform target creature");
        assert!(matches!(e, Effect::Transform { .. }));
    }

    #[test]
    fn effect_bare_shuffle_defaults_to_controller_library() {
        let e = parse_effect("Shuffle");
        assert!(matches!(
            e,
            Effect::Shuffle {
                target: TargetFilter::Controller
            }
        ));
    }

    #[test]
    fn effect_that_player_shuffles_targets_player() {
        let e = parse_effect("That player shuffles");
        assert!(matches!(
            e,
            Effect::Shuffle {
                target: TargetFilter::Player
            }
        ));
    }

    #[test]
    fn effect_attach() {
        let e = parse_effect("Attach this to target creature");
        assert!(matches!(e, Effect::Attach { .. }));
    }

    #[test]
    fn effect_each_opponent_discards() {
        let e = parse_effect("Each opponent discards a card");
        assert!(matches!(e, Effect::Discard { count: 1, .. }));
    }

    #[test]
    fn effect_you_may_draw() {
        let e = parse_effect("You may draw a card");
        assert!(matches!(e, Effect::Draw { count: 1 }));
    }

    #[test]
    fn effect_it_gets_pump() {
        let e = parse_effect("It gets +2/+2 until end of turn");
        assert!(matches!(
            e,
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                ..
            }
        ));
    }

    #[test]
    fn effect_they_draw() {
        let e = parse_effect("They draw two cards");
        assert!(matches!(e, Effect::Draw { count: 2 }));
    }

    #[test]
    fn effect_add_mana_any_color() {
        let e = parse_effect("Add one mana of any color");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: CountValue::Fixed(1),
                    ref color_options,
                }, ..
            } if color_options == &vec![
                ManaColor::White,
                ManaColor::Blue,
                ManaColor::Black,
                ManaColor::Red,
                ManaColor::Green,
            ]
        ));
    }

    #[test]
    fn effect_add_three_mana_any_one_color() {
        let e = parse_effect("Add three mana of any one color");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: CountValue::Fixed(3),
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn effect_add_two_mana_in_any_combination_of_colors() {
        let e = parse_effect("Add two mana in any combination of colors");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count: CountValue::Fixed(2),
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn effect_add_one_mana_of_the_chosen_color() {
        let e = parse_effect("Add one mana of the chosen color");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::ChosenColor {
                    count: CountValue::Fixed(1)
                },
                ..
            }
        ));
    }

    #[test]
    fn effect_add_x_mana_any_one_color_with_where_clause() {
        let e =
            parse_effect("Add X mana of any one color, where X is the number of lands you control");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: CountValue::Variable(ref value),
                    ..
                }, ..
            } if value == "the number of lands you control"
        ));
    }

    #[test]
    fn effect_add_x_mana_in_any_combination_of_constrained_colors() {
        let e = parse_effect("Add X mana in any combination of {U} and/or {R}");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count: CountValue::Variable(ref value),
                    ref color_options,
                }, ..
            } if value == "X"
                && color_options == &vec![ManaColor::Blue, ManaColor::Red]
        ));
    }

    #[test]
    fn effect_add_colorless_mana_symbol() {
        let e = parse_effect("Add {C}");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: CountValue::Fixed(1)
                },
                ..
            }
        ));
    }

    #[test]
    fn effect_add_color_choice_symbols() {
        let e = parse_effect("add {r} or {g}");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: CountValue::Fixed(1),
                    ref color_options,
                }, ..
            } if color_options == &vec![ManaColor::Red, ManaColor::Green]
        ));
    }

    #[test]
    fn effect_put_top_cards_into_graveyard() {
        let e = parse_effect("Put the top 3 cards of your library into your graveyard");
        assert!(matches!(e, Effect::Mill { count: 3, .. }));
    }

    #[test]
    fn effect_create_colored_token() {
        let e = parse_effect("Create a 1/1 white Soldier creature token");
        assert!(matches!(
            e,
            Effect::Token {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                count: CountValue::Fixed(1),
                ..
            }
        ));
    }

    #[test]
    fn effect_create_treasure_token() {
        let e = parse_effect("Create a Treasure token");
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                ref types,
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                count: CountValue::Fixed(1),
                ..
            } if name == "Treasure" && types == &vec!["Artifact".to_string(), "Treasure".to_string()]
        ));
    }

    #[test]
    fn effect_create_tapped_powerstone_token() {
        let e = parse_effect("Create a tapped Powerstone token");
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                tapped: true,
                ..
            } if name == "Powerstone"
        ));
    }

    #[test]
    fn effect_create_multicolor_artifact_creature_token() {
        let e = parse_effect("Create a 2/1 white and black Inkling creature token with flying");
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(1),
                ref colors,
                ref keywords,
                ..
            } if name == "Inkling"
                && colors == &vec![ManaColor::White, ManaColor::Black]
                && keywords == &vec![Keyword::Flying]
        ));
    }

    #[test]
    fn effect_create_named_artifact_token() {
        let e = parse_effect(
            "Create a colorless artifact token named Etherium Cell with \"{T}, Sacrifice this token: Add one mana of any color.\"",
        );
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                ref types,
                ..
            } if name == "Etherium Cell" && types == &vec!["Artifact".to_string()]
        ));
    }

    #[test]
    fn effect_create_attached_role_stays_unimplemented() {
        let e = parse_effect("Create a Monster Role token attached to target creature you control");
        assert!(matches!(
            e,
            Effect::Unimplemented { ref name, .. } if name == "create"
        ));
    }

    #[test]
    fn effect_create_named_legendary_creature_token() {
        let e = parse_effect("Create Voja, a legendary 2/2 green and white Wolf creature token");
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                ref colors,
                ref types,
                ..
            } if name == "Voja"
                && colors == &vec![ManaColor::Green, ManaColor::White]
                && types == &vec!["Creature".to_string(), "Wolf".to_string()]
        ));
    }

    #[test]
    fn effect_create_named_legendary_artifact_token() {
        let e = parse_effect(
            "Create Tamiyo's Notebook, a legendary colorless artifact token with \"{T}: Draw a card.\"",
        );
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                ref types,
                ..
            } if name == "Tamiyo's Notebook" && types == &vec!["Artifact".to_string()]
        ));
    }

    #[test]
    fn effect_create_variable_count_fixed_pt_token() {
        let e = parse_effect("Create X 1/1 white Soldier creature tokens");
        assert!(matches!(
            e,
            Effect::Token {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                count: CountValue::Variable(ref value),
                ..
            } if value == "X"
        ));
    }

    #[test]
    fn effect_create_variable_pt_token_from_where_clause() {
        let e = parse_effect(
            "Create an X/X green Ooze creature token, where X is the greatest power among creatures you control",
        );
        assert!(matches!(
            e,
            Effect::Token {
                power: PtValue::Variable(ref value),
                toughness: PtValue::Variable(ref value2),
                count: CountValue::Fixed(1),
                ..
            } if value == "the greatest power among creatures you control"
                && value2 == "the greatest power among creatures you control"
        ));
    }

    #[test]
    fn effect_create_variable_named_artifact_tokens() {
        let e = parse_effect(
            "Create X Map tokens, where X is one plus the number of opponents who control an artifact",
        );
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                count: CountValue::Variable(ref value),
                ref types,
                ..
            } if name == "Map"
                && value == "one plus the number of opponents who control an artifact"
                && types == &vec!["Artifact".to_string(), "Map".to_string()]
        ));
    }

    #[test]
    fn effect_create_number_of_tokens_equal_to_expression() {
        let e = parse_effect(
            "Create a number of 1/1 red Goblin creature tokens equal to two plus the number of cards named Goblin Gathering in your graveyard",
        );
        assert!(matches!(
            e,
            Effect::Token {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                count: CountValue::Variable(ref value),
                ..
            } if value == "two plus the number of cards named Goblin Gathering in your graveyard"
        ));
    }

    #[test]
    fn effect_create_token_with_quoted_variable_pt() {
        let e = parse_effect(
            "Create a red Elemental creature token with trample and \"This token's power and toughness are each equal to the number of instant and sorcery cards in your graveyard\"",
        );
        assert!(matches!(
            e,
            Effect::Token {
                power: PtValue::Variable(ref value),
                toughness: PtValue::Variable(ref value2),
                ref keywords,
                ..
            } if value == "the number of instant and sorcery cards in your graveyard"
                && value2 == "the number of instant and sorcery cards in your graveyard"
                && keywords == &vec![Keyword::Trample]
        ));
    }

    #[test]
    fn effect_that_creature_gets() {
        let e = parse_effect("That creature gets +1/+1 until end of turn");
        assert!(matches!(
            e,
            Effect::Pump {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                ..
            }
        ));
    }

    #[test]
    fn effect_target_player_draws() {
        let e = parse_effect("Target player draws a card");
        assert!(matches!(e, Effect::Draw { count: 1 }));
    }

    #[test]
    fn effect_this_creature_gets() {
        let e = parse_effect("This creature gets +2/+2 until end of turn");
        assert!(matches!(
            e,
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                ..
            }
        ));
    }

    #[test]
    fn effect_target_creature_gets_variable_pump() {
        let e = parse_effect("Target creature gets +X/+0 until end of turn");
        assert!(matches!(
            e,
            Effect::Pump {
                power: PtValue::Variable(ref value),
                toughness: PtValue::Fixed(0),
                target: TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    ..
                }),
            } if value == "X"
        ));
    }

    #[test]
    fn effect_target_creature_gets_negative_variable_pump_with_where_clause() {
        let e = parse_effect(
            "Target creature gets -X/-X until end of turn, where X is the number of Elves you control",
        );
        assert!(matches!(
            e,
            Effect::Pump {
                power: PtValue::Variable(ref value),
                toughness: PtValue::Variable(ref value2),
                target: TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    ..
                }),
            } if value == "-(the number of Elves you control)"
                && value2 == "-(the number of Elves you control)"
        ));
    }

    #[test]
    fn effect_creatures_you_control_get_variable_pump() {
        let e = parse_effect(
            "Creatures you control get +X/+X until end of turn, where X is the number of cards in your hand",
        );
        assert!(matches!(
            e,
            Effect::PumpAll {
                power: PtValue::Variable(ref value),
                toughness: PtValue::Variable(ref value2),
                target: TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    controller: Some(ControllerRef::You),
                    ..
                }),
            } if value == "the number of cards in your hand"
                && value2 == "the number of cards in your hand"
        ));
    }

    #[test]
    fn effect_attacking_creatures_you_control_get_pump() {
        let e = parse_effect("Attacking creatures you control get +1/+1 until end of turn");
        assert!(matches!(
            e,
            Effect::PumpAll {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                target: TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    controller: Some(ControllerRef::You),
                    properties,
                    ..
                }),
            } if properties == vec![FilterProp::Attacking]
        ));
    }

    #[test]
    fn effect_chain_variable_pump_preserves_duration() {
        let def = parse_effect_chain(
            "Target creature gets +X/+0 until end of turn, where X is the number of creatures you control",
            AbilityKind::Spell,
        );
        assert_eq!(def.duration, Some(Duration::UntilEndOfTurn));
        assert!(matches!(
            def.effect,
            Effect::Pump {
                power: PtValue::Variable(ref value),
                toughness: PtValue::Fixed(0),
                ..
            } if value == "the number of creatures you control"
        ));
    }

    #[test]
    fn effect_if_kicked_destroys() {
        let e = parse_effect("if it was kicked, destroy target enchantment");
        assert!(matches!(
            e,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter { .. })
            }
        ));
    }

    #[test]
    fn effect_target_creature_gains_keyword_uses_continuous_effect() {
        let e = parse_effect("Target creature gains flying until end of turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    ..
                })),
                ..
            }
        ));
    }

    #[test]
    fn effect_all_creatures_gain_keywords_uses_continuous_effect() {
        let e = parse_effect("All creatures gain trample and haste until end of turn");
        assert!(matches!(e, Effect::GenericEffect { target: None, .. }));
    }

    #[test]
    fn effect_target_creature_gets_and_gains_keyword_uses_continuous_effect() {
        let e = parse_effect("Target creature gets +1/+1 and gains trample until end of turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    ..
                })),
                ..
            }
        ));
    }

    #[test]
    fn effect_chain_ignores_leading_then_clause_connector() {
        let ability = parse_effect_chain(
            "Return up to one target creature card from your graveyard to your hand. Then, draw a card.",
            AbilityKind::Spell,
        );
        assert!(matches!(
            ability.effect,
            Effect::Bounce {
                destination: None,
                ..
            }
        ));
        let sub = ability.sub_ability.expect("expected follow-up draw");
        assert!(matches!(sub.effect, Effect::Draw { count: 1 }));
    }

    #[test]
    fn effect_target_creature_becomes_blue_uses_continuous_effect() {
        let e = parse_effect("Target creature becomes blue until end of turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    ..
                })),
                static_abilities,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].modifications.contains(&ContinuousModification::SetColor {
                    colors: vec![ManaColor::Blue],
                })
        ));
    }

    #[test]
    fn effect_self_becomes_colored_creature_with_keyword() {
        let e = parse_effect(
            "Until end of turn, this land becomes a 4/4 blue and black Shark creature with deathtouch",
        );
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: None,
                static_abilities,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].affected == Some(TargetFilter::SelfRef)
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::SetPower { value: 4 })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::SetToughness { value: 4 })
                && static_abilities[0].modifications.contains(&ContinuousModification::SetColor {
                    colors: vec![ManaColor::Blue, ManaColor::Black],
                })
                && static_abilities[0].modifications.contains(
                    &ContinuousModification::AddKeyword {
                        keyword: Keyword::Deathtouch,
                    }
                )
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::AddType {
                        core_type: crate::types::card_type::CoreType::Creature,
                    })
                && static_abilities[0].modifications.contains(
                    &ContinuousModification::AddSubtype {
                        subtype: "Shark".to_string(),
                    }
                )
        ));
    }

    #[test]
    fn effect_land_becomes_creature_with_all_creature_types() {
        let e =
            parse_effect("This land becomes a 3/3 creature with vigilance and all creature types");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: None,
                static_abilities,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].affected == Some(TargetFilter::SelfRef)
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::SetPower { value: 3 })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::SetToughness { value: 3 })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::AddType {
                        core_type: crate::types::card_type::CoreType::Creature,
                    })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::AddKeyword {
                        keyword: Keyword::Vigilance,
                    })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::AddKeyword {
                        keyword: Keyword::Changeling,
                    })
        ));
    }

    #[test]
    fn effect_self_becomes_artifact_creature_with_base_pt() {
        let e =
            parse_effect("This artifact becomes a 2/2 Beast artifact creature until end of turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: None,
                static_abilities,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].affected == Some(TargetFilter::SelfRef)
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::SetPower { value: 2 })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::SetToughness { value: 2 })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::AddType {
                        core_type: crate::types::card_type::CoreType::Artifact,
                    })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::AddType {
                        core_type: crate::types::card_type::CoreType::Creature,
                    })
                && static_abilities[0].modifications.contains(
                    &ContinuousModification::AddSubtype {
                        subtype: "Beast".to_string(),
                    }
                )
        ));
    }

    #[test]
    fn effect_target_creature_cant_block_uses_rule_static() {
        let e = parse_effect("Target creature can't block this turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    ..
                })),
                static_abilities,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].mode == StaticMode::CantBlock
                && static_abilities[0].affected == Some(TargetFilter::Typed(TypedFilter::creature()))
        ));
    }

    #[test]
    fn effect_target_creature_cant_be_blocked_uses_rule_static() {
        let e = parse_effect("Target creature can't be blocked this turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    ..
                })),
                static_abilities,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].mode
                    == StaticMode::Other("CantBeBlocked".to_string())
        ));
    }

    #[test]
    fn effect_chain_preserves_leading_duration_prefix() {
        let def = parse_effect_chain(
            "Until end of turn, target creature gains flying",
            AbilityKind::Spell,
        );
        assert_eq!(
            def.duration,
            Some(crate::types::ability::Duration::UntilEndOfTurn)
        );
        assert!(matches!(def.effect, Effect::GenericEffect { .. }));
    }

    #[test]
    fn effect_deal_damage_all_imperative() {
        let e = parse_effect("Deal 1 damage to each opponent");
        assert!(matches!(
            e,
            Effect::DamageAll {
                amount: DamageAmount::Fixed(1),
                ..
            }
        ));
    }

    #[test]
    fn effect_deal_that_much_damage() {
        let e = parse_effect("Deal that much damage to any target");
        assert!(matches!(
            e,
            Effect::DealDamage {
                amount: DamageAmount::Variable(ref value),
                ..
            } if value == "that much"
        ));
    }

    #[test]
    fn effect_deal_damage_equal_to_expression() {
        let e = parse_effect("Deal damage equal to its power to any target");
        assert!(matches!(
            e,
            Effect::DealDamage {
                amount: DamageAmount::Variable(ref value),
                ..
            } if value == "its power"
        ));
    }

    #[test]
    fn effect_put_target_on_bottom_of_library() {
        let e = parse_effect("Put target creature on the bottom of its owner's library");
        assert!(matches!(
            e,
            Effect::ChangeZone {
                destination: Zone::Library,
                ..
            }
        ));
    }

    #[test]
    fn effect_put_card_onto_battlefield() {
        let e = parse_effect("Put a land card from your hand onto the battlefield");
        assert!(matches!(
            e,
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                ..
            }
        ));
    }

    #[test]
    fn effect_put_target_into_hand() {
        let e = parse_effect("Put target nonland permanent into your hand");
        assert!(matches!(
            e,
            Effect::ChangeZone {
                destination: Zone::Hand,
                ..
            }
        ));
    }

    #[test]
    fn put_counter_this_creature_is_self_ref() {
        // "Whenever you gain life, put a +1/+1 counter on this creature." (Ajani's Pridemate)
        let e = parse_effect("put a +1/+1 counter on this creature");
        assert!(matches!(
            e,
            Effect::PutCounter {
                counter_type: ref ct,
                count: 1,
                target: TargetFilter::SelfRef,
            } if ct == "P1P1"
        ));
    }

    #[test]
    fn put_counter_on_it_is_self_ref() {
        let e = parse_effect("put a +1/+1 counter on it");
        assert!(matches!(
            e,
            Effect::PutCounter {
                target: TargetFilter::SelfRef,
                ..
            }
        ));
    }

    #[test]
    fn put_counter_on_itself_is_self_ref() {
        let e = parse_effect("put a +1/+1 counter on itself");
        assert!(matches!(
            e,
            Effect::PutCounter {
                target: TargetFilter::SelfRef,
                ..
            }
        ));
    }

    #[test]
    fn put_counter_on_target_creature_is_typed() {
        let e = parse_effect("put a +1/+1 counter on target creature");
        assert!(matches!(
            e,
            Effect::PutCounter {
                target: TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    ..
                }),
                ..
            }
        ));
    }

    #[test]
    fn put_counter_normalizes_plus1plus1_type() {
        let e = parse_effect("put a +1/+1 counter on this creature");
        let Effect::PutCounter { counter_type, .. } = e else {
            panic!("Expected PutCounter");
        };
        assert_eq!(counter_type, "P1P1");
    }

    #[test]
    fn put_counter_normalizes_minus1minus1_type() {
        let e = parse_effect("put a -1/-1 counter on this creature");
        let Effect::PutCounter { counter_type, .. } = e else {
            panic!("Expected PutCounter");
        };
        assert_eq!(counter_type, "M1M1");
    }

    #[test]
    fn put_counter_generic_type_passthrough() {
        let e = parse_effect("put a charge counter on this permanent");
        let Effect::PutCounter { counter_type, .. } = e else {
            panic!("Expected PutCounter");
        };
        assert_eq!(counter_type, "charge");
    }

    #[test]
    fn remove_counter_from_it_is_self_ref() {
        let e = parse_effect("remove a time counter from it");
        assert!(matches!(
            e,
            Effect::RemoveCounter {
                counter_type,
                count: 1,
                target: TargetFilter::SelfRef,
            } if counter_type == "time"
        ));
    }

    #[test]
    fn remove_counter_from_target_creature_is_typed() {
        let e = parse_effect("remove a -1/-1 counter from target creature");
        assert!(matches!(
            e,
            Effect::RemoveCounter {
                counter_type,
                target: TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    ..
                }),
                ..
            } if counter_type == "M1M1"
        ));
    }

    #[test]
    fn remove_all_counters_falls_back() {
        let e = parse_effect("remove all tide counters from it");
        assert!(matches!(
            e,
            Effect::Unimplemented {
                name,
                description: Some(description),
            } if name == "remove" && description == "remove all tide counters from it"
        ));
    }

    #[test]
    fn parse_activate_only_condition_with_two_land_subtypes() {
        let e = parse_effect("Activate only if you control an Island or a Swamp.");
        assert!(matches!(
            e,
            Effect::Unimplemented {
                name,
                description: Some(description),
            } if name == "activate_only_if_controls_land_subtype_any" && description == "Island|Swamp"
        ));
    }

    #[test]
    fn parse_activate_only_condition_non_land_clause_falls_back() {
        let e = parse_effect("Activate only if you control a creature with power 4 or greater.");
        assert!(matches!(
            e,
            Effect::Unimplemented {
                name,
                description: Some(_),
            } if name == "activate"
        ));
    }

    // --- RevealHand / "look at" tests ---

    #[test]
    fn parse_look_at_target_opponent_hand() {
        let e = parse_effect("look at target opponent's hand");
        match e {
            Effect::RevealHand {
                target,
                card_filter,
            } => {
                assert!(matches!(
                    target,
                    TargetFilter::Typed(TypedFilter {
                        card_type: None,
                        controller: Some(ControllerRef::Opponent),
                        ..
                    })
                ));
                assert_eq!(card_filter, TargetFilter::Any);
            }
            other => panic!("Expected RevealHand, got {:?}", other),
        }
    }

    #[test]
    fn parse_look_at_possessive_hand() {
        let e = parse_effect("look at your hand");
        assert!(matches!(
            e,
            Effect::RevealHand {
                target: TargetFilter::Any,
                card_filter: TargetFilter::Any,
            }
        ));
    }

    #[test]
    fn parse_deep_cavern_bat_chain() {
        let def = parse_effect_chain(
            "look at target opponent's hand. You may exile a nonland card from it until this creature leaves the battlefield",
            AbilityKind::Spell,
        );
        // First effect: RevealHand with opponent target and nonland card_filter
        match &def.effect {
            Effect::RevealHand {
                target,
                card_filter,
            } => {
                assert!(matches!(
                    target,
                    TargetFilter::Typed(TypedFilter {
                        card_type: None,
                        controller: Some(ControllerRef::Opponent),
                        ..
                    })
                ));
                assert!(matches!(
                    card_filter,
                    TargetFilter::Typed(TypedFilter {
                        properties,
                        ..
                    }) if !properties.is_empty()
                ));
            }
            other => panic!("Expected RevealHand, got {:?}", other),
        }
        // Sub-ability: ChangeZone to Exile with duration
        let sub = def.sub_ability.as_ref().expect("should have sub_ability");
        assert!(matches!(
            sub.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ));
        assert_eq!(sub.duration, Some(Duration::UntilHostLeavesPlay));
    }

    #[test]
    fn parse_choose_filter_nonland() {
        let filter = parse_choose_filter_from_sentence("exile a nonland card from it");
        assert!(matches!(
            filter,
            TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Card),
                properties,
                ..
            }) if properties.iter().any(|p| matches!(p, FilterProp::NonType { value } if value == "Land"))
        ));
    }

    #[test]
    fn parse_choose_filter_creature() {
        let filter = parse_choose_filter_from_sentence("exile a creature card from it");
        assert!(matches!(
            filter,
            TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Creature),
                ..
            })
        ));
    }

    #[test]
    fn trailing_duration_until_leaves() {
        let (rest, duration) =
            strip_trailing_duration("exile a card until ~ leaves the battlefield");
        assert_eq!(duration, Some(Duration::UntilHostLeavesPlay));
        assert_eq!(rest, "exile a card");
    }

    // --- "Choose" as targeting synonym ---

    #[test]
    fn choose_target_creature_parses_as_targeting() {
        // "Choose target creature" (Grave Strength first sentence)
        let e = parse_effect("Choose target creature");
        assert!(
            !matches!(e, Effect::Unimplemented { .. }),
            "expected parsed effect, got Unimplemented: {:?}",
            e
        );
    }

    #[test]
    fn choose_two_target_creatures() {
        // "Choose two target creatures controlled by the same player" (Incriminate)
        let e = parse_effect("Choose two target creatures controlled by the same player");
        assert!(
            !matches!(e, Effect::Unimplemented { .. }),
            "expected parsed effect, got Unimplemented: {:?}",
            e
        );
    }

    #[test]
    fn choose_up_to_two_target_permanent_cards() {
        // "Choose up to two target permanent cards in your graveyard" (Brought Back)
        let e =
            parse_effect("Choose up to two target permanent cards in your graveyard that were put there from the battlefield this turn");
        assert!(
            !matches!(e, Effect::Unimplemented { .. }),
            "expected parsed effect, got Unimplemented: {:?}",
            e
        );
    }

    #[test]
    fn choose_a_type_you_control() {
        // "Choose a Giant creature you control" (Crush Underfoot)
        let e = parse_effect("Choose a Giant creature you control");
        assert!(
            !matches!(e, Effect::Unimplemented { .. }),
            "expected parsed effect, got Unimplemented: {:?}",
            e
        );
    }

    #[test]
    fn choose_does_not_match_targeting_as_color() {
        // "Choose a color" should NOT be treated as targeting — it's a named choice
        let e = parse_effect("Choose a color");
        assert!(!matches!(e, Effect::TargetOnly { .. }));
        assert!(matches!(e, Effect::Choose { .. }));
    }

    #[test]
    fn choose_does_not_match_targeting_as_creature_type() {
        // "Choose a creature type" should NOT be treated as targeting — it's a named choice
        let e = parse_effect("Choose a creature type");
        assert!(!matches!(e, Effect::TargetOnly { .. }));
        assert!(matches!(e, Effect::Choose { .. }));
    }

    #[test]
    fn choose_card_from_it_still_works() {
        // "Choose a creature card from it" should still produce RevealHand
        let e = parse_effect("Choose a creature card from it");
        assert!(matches!(e, Effect::RevealHand { .. }));
    }

    #[test]
    fn is_choose_as_targeting_helper() {
        assert!(is_choose_as_targeting("target creature"));
        assert!(is_choose_as_targeting("target creature you control"));
        assert!(is_choose_as_targeting("up to two target creatures"));
        assert!(is_choose_as_targeting(
            "two target creatures controlled by the same player"
        ));
        assert!(is_choose_as_targeting("a giant creature you control"));
        assert!(!is_choose_as_targeting("a color"));
        assert!(!is_choose_as_targeting("a creature type"));
        assert!(!is_choose_as_targeting("a card type"));
        assert!(!is_choose_as_targeting("one —"));
        assert!(!is_choose_as_targeting("a creature card from it"));
    }

    #[test]
    fn choose_a_creature_type() {
        let e = parse_effect("Choose a creature type");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::CreatureType,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_a_color() {
        let e = parse_effect("Choose a color");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::Color,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_odd_or_even() {
        let e = parse_effect("Choose odd or even");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::OddOrEven,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_a_basic_land_type() {
        let e = parse_effect("Choose a basic land type");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::BasicLandType,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_a_card_type() {
        let e = parse_effect("Choose a card type");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::CardType,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_a_card_name() {
        let e = parse_effect("Choose a card name");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::CardName,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_a_nonland_card_name() {
        let e = parse_effect("Choose a nonland card name");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::CardName,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_a_creature_card_name() {
        let e = parse_effect("Choose a creature card name");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::CardName,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_a_number_between_0_and_13() {
        let e = parse_effect("Choose a number between 0 and 13");
        assert!(matches!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::NumberRange { min: 0, max: 13 },
                ..
            }
        ));
    }

    #[test]
    fn choose_a_number_greater_than_0() {
        let e = parse_effect("Choose a number greater than 0");
        assert!(matches!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::NumberRange { min: 1, .. },
                ..
            }
        ));
    }

    #[test]
    fn choose_a_number_generic() {
        let e = parse_effect("Choose a number");
        assert!(matches!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::NumberRange { min: 0, max: 20 },
                ..
            }
        ));
    }

    #[test]
    fn choose_left_or_right() {
        let e = parse_effect("Choose left or right");
        match e {
            Effect::Choose {
                choice_type: ChoiceType::Labeled { options },
                ..
            } => {
                assert_eq!(options, vec!["Left", "Right"]);
            }
            other => panic!("Expected Choose Labeled, got {:?}", other),
        }
    }

    #[test]
    fn choose_fame_or_fortune() {
        let e = parse_effect("choose fame or fortune");
        match e {
            Effect::Choose {
                choice_type: ChoiceType::Labeled { options },
                ..
            } => {
                assert_eq!(options, vec!["Fame", "Fortune"]);
            }
            other => panic!("Expected Choose Labeled, got {:?}", other),
        }
    }

    #[test]
    fn choose_hexproof_or_indestructible() {
        let e = parse_effect("Choose hexproof or indestructible");
        match e {
            Effect::Choose {
                choice_type: ChoiceType::Labeled { options },
                ..
            } => {
                assert_eq!(options, vec!["Hexproof", "Indestructible"]);
            }
            other => panic!("Expected Choose Labeled, got {:?}", other),
        }
    }

    #[test]
    fn choose_a_land_type() {
        let e = parse_effect("Choose a land type");
        assert!(matches!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::LandType,
                ..
            }
        ));
    }

    #[test]
    fn choose_a_nonbasic_land_type() {
        let e = parse_effect("Choose a nonbasic land type");
        assert!(matches!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::LandType,
                ..
            }
        ));
    }

    #[test]
    fn mana_spend_restriction_chosen_creature_type() {
        let def = parse_effect_chain(
            "Add one mana of any color. Spend this mana only to cast a creature spell of the chosen type, and that spell can't be countered.",
            AbilityKind::Activated,
        );
        assert!(matches!(
            def.effect,
            Effect::Mana {
                ref restrictions, ..
            } if restrictions == &[ManaSpendRestriction::ChosenCreatureType]
        ));
    }

    #[test]
    fn mana_spend_restriction_spell_type() {
        let def = parse_effect_chain(
            "Add one mana of any color. Spend this mana only to cast creature spells.",
            AbilityKind::Activated,
        );
        assert!(matches!(
            def.effect,
            Effect::Mana {
                ref restrictions, ..
            } if restrictions == &[ManaSpendRestriction::SpellType("Creature".to_string())]
        ));
    }
}
