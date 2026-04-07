use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::Parser;
use nom_language::error::VerboseError;

use crate::parser::oracle_effect::split_leading_conditional;

pub(crate) fn is_cant_win_lose_compound(lower: &str) -> bool {
    lower.contains("can't win the game") && lower.contains("can't lose the game")
}

pub(crate) fn has_roll_die_pattern(lower: &str) -> bool {
    // CR 706: Detect both "roll a dN" and word-form "roll a six-sided die" patterns.
    lower.contains("roll a d") || lower.contains("rolls a d") || lower.contains("-sided die")
}

pub(crate) fn is_instead_replacement_line(text: &str) -> bool {
    split_leading_conditional(text).is_some_and(|(_, body)| {
        let body_lower = body.to_lowercase();
        body_lower.starts_with("instead ")
    })
}

pub(crate) fn has_trigger_prefix(lower: &str) -> bool {
    alt((
        tag::<_, _, VerboseError<&str>>("when "),
        tag("whenever "),
        tag("at "),
    ))
    .parse(lower)
    .is_ok()
}

pub(crate) fn lower_starts_with(lower: &str, prefix: &str) -> bool {
    tag::<_, _, VerboseError<&str>>(prefix).parse(lower).is_ok()
}

pub(crate) fn is_flashback_equal_mana_cost(lower: &str) -> bool {
    lower.contains("flashback cost") && lower.contains("equal to") && lower.contains("mana cost")
}

pub(crate) fn is_defiler_cost_pattern(lower: &str) -> bool {
    lower_starts_with(lower, "as an additional cost to cast ")
        && !lower.contains("this spell")
        && lower.contains("you may pay")
        && lower.contains(" life")
}

pub(crate) fn is_compound_turn_limit(lower: &str) -> bool {
    lower.contains("only during your turn")
        && lower.contains(" and ")
        && lower.contains("each turn")
}

pub(crate) fn is_opening_hand_begin_game(lower: &str) -> bool {
    lower.contains("opening hand") && lower.contains("begin the game")
}

pub(crate) fn is_ability_activate_cost_static(lower: &str) -> bool {
    lower.contains("abilities you activate cost") && lower.contains("less")
}

pub(crate) fn is_damage_prevention_pattern(lower: &str) -> bool {
    lower.contains("damage") && lower.contains("can't be prevented")
}

pub(crate) fn should_defer_spell_to_effect(lower: &str) -> bool {
    ((lower.contains(" deals ") || lower.contains(" deal ")) && lower.contains(" damage"))
        || lower.contains("until end of turn")
        || lower.contains("until your next turn")
        || lower.contains("this turn")
}

const STATIC_CONTAINS_PATTERNS: &[&str] = &[
    "gets +",
    "gets -",
    "get +",
    "get -",
    "have ",
    "has ",
    "can't be blocked",
    "can't attack",
    "can't block",
    "can't be countered",
    "can't be the target",
    "can't be sacrificed",
    "doesn't untap",
    "don't untap",
    "attacks each combat if able",
    "can block only creatures with flying",
    "no maximum hand size",
    "may choose not to untap",
    "play with the top card",
    "cost {",
    "costs {",
    "cost less",
    "cost more",
    "costs less",
    "costs more",
    "is the chosen type",
    "lose all abilities",
    "power is equal to",
    "power and toughness are each equal to",
    "must be blocked",
    "can't gain life",
    "can't win the game",
    "can't lose the game",
    "don't lose the game",
    "can block an additional",
    "can block any number",
    "play an additional land",
    "play two additional lands",
    "triggers an additional time",
    "can't enter the battlefield",
    "can't cast spells from",
    "can't cast spells during",
    "can't cast more than",
    "can cast no more than",
    "can't cast creature",
    "can't cast instant",
    "can't cast sorcery",
    "can't cast noncreature",
    "spells can't be cast",
    "can't cast spells with",
    "can't cast spells of the chosen",
    "can't draw more than",
    "can cast spells only during",
    "skip your ",
    "maximum hand size",
    "life total can't change",
    "assigns combat damage equal to its toughness",
    "as though it weren't blocked",
    "attacking doesn't cause",
    "as though they had flash",
];

const STATIC_PREFIX_PATTERNS: &[&str] = &[
    "as long as ",
    "enchanted ",
    "equipped ",
    "you control enchanted ",
    "all creatures ",
    "all permanents ",
    "other ",
    "each creature ",
    "cards in ",
    "creatures you control ",
    "each player ",
    "spells you cast ",
    "spells your opponents cast ",
    "you may look at the top card of your library",
    "once during each of your turns, you may cast",
    "a deck can have",
    "nonland ",
    "noncreature ",
    "nonbasic lands are ",
    "each land is a ",
    "all lands are ",
    "lands you control are ",
    "you may spend mana as though",
];

pub(crate) fn is_static_pattern(lower: &str) -> bool {
    if lower_starts_with(lower, "target") {
        return false;
    }

    if STATIC_CONTAINS_PATTERNS
        .iter()
        .any(|pattern| lower.contains(pattern))
    {
        return true;
    }

    if STATIC_PREFIX_PATTERNS
        .iter()
        .any(|pattern| lower.starts_with(pattern))
    {
        return true;
    }

    is_static_compound_pattern(lower)
}

fn is_static_compound_pattern(lower: &str) -> bool {
    if lower.contains("as though it had flash") && !lower_starts_with(lower, "you may cast") {
        return true;
    }
    if lower.contains("enters with ") && !lower.contains("counter") {
        return true;
    }
    if lower_starts_with(lower, "creatures your opponents control ")
        && !lower.trim_end_matches('.').ends_with("enter tapped")
    {
        return true;
    }
    if alt((
        tag::<_, _, VerboseError<&str>>("you may play"),
        tag("you may cast"),
    ))
    .parse(lower)
    .is_ok()
        && (lower.contains("from your graveyard")
            || (lower.contains("from your hand") && lower.contains("without paying")))
    {
        return true;
    }
    if lower.contains("can't cast") && lower.contains("spells") {
        return true;
    }
    // Passive voice: "Creature spells can't be cast."
    if lower.contains("spells can't be cast") {
        return true;
    }
    if lower.contains("no more than") && lower.contains("spells") && lower.contains("each turn") {
        return true;
    }
    false
}

const GRANTED_STATIC_PREFIXES: &[&str] = &[
    "enchanted ",
    "equipped ",
    "all ",
    "creatures ",
    "lands ",
    "other ",
    "you ",
    "players ",
    "each player ",
];

const GRANTED_STATIC_VERBS: &[&str] = &[" has \"", " have \"", " gains \"", " gain \""];

pub(crate) fn is_granted_static_line(lower: &str) -> bool {
    GRANTED_STATIC_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        && GRANTED_STATIC_VERBS.iter().any(|verb| lower.contains(verb))
}

pub(crate) fn is_vehicle_tier_line(lower: &str) -> bool {
    if let Some(pipe_pos) = lower.find(" | ") {
        let prefix = lower[..pipe_pos].trim();
        if let Some(num_part) = prefix.strip_suffix('+') {
            return !num_part.is_empty() && num_part.chars().all(|c| c.is_ascii_digit());
        }
    }
    false
}

const REPLACEMENT_CONTAINS_PATTERNS: &[&str] = &[
    "would ",
    "prevent all",
    "enters the battlefield tapped",
    "enters tapped",
    "enter as a copy of",
];

pub(crate) fn is_replacement_pattern(lower: &str) -> bool {
    if REPLACEMENT_CONTAINS_PATTERNS
        .iter()
        .any(|pattern| lower.contains(pattern))
    {
        return true;
    }

    if lower.trim_end_matches('.').ends_with(" enter tapped") {
        return true;
    }

    is_replacement_compound_pattern(lower)
}

fn is_replacement_compound_pattern(lower: &str) -> bool {
    if lower.contains("as ") && lower.contains("enters") && lower.contains("choose a") {
        return true;
    }
    if (lower.contains("enters") || lower.contains("escapes")) && lower.contains("counter") {
        return true;
    }
    if lower.contains("tapped for mana") && lower.contains("instead") {
        return true;
    }
    false
}

const EFFECT_IMPERATIVE_PREFIXES: &[&str] = &[
    "add ",
    "attach ",
    "counter ",
    "create ",
    "deal ",
    "destroy ",
    "discard ",
    "draw ",
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
    "untap ",
    "you may ",
];

const EFFECT_SUBJECT_PREFIXES: &[&str] = &[
    "all ", "if ", "it ", "target ", "that ", "they ", "this ", "those ", "you ", "~ ",
];

pub(crate) fn is_effect_sentence_candidate(lower: &str) -> bool {
    EFFECT_IMPERATIVE_PREFIXES
        .iter()
        .chain(EFFECT_SUBJECT_PREFIXES.iter())
        .any(|prefix| lower.starts_with(prefix))
}
