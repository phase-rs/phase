use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::verify;
use nom::sequence::terminated;
use nom::Parser;

use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::primitives::scan_contains;
use super::oracle_util::parse_mana_symbols;
use crate::parser::oracle_effect::{split_leading_conditional, try_parse_named_choice};

pub(crate) fn is_cant_win_lose_compound(lower: &str) -> bool {
    scan_contains(lower, "can't win the game") && scan_contains(lower, "can't lose the game")
}

pub(crate) fn has_roll_die_pattern(lower: &str) -> bool {
    // CR 706: Detect both "roll a dN" and word-form "roll a six-sided die" patterns.
    scan_contains(lower, "roll a d")
        || scan_contains(lower, "rolls a d")
        || scan_contains(lower, "-sided die")
}

pub(crate) fn is_instead_replacement_line(text: &str) -> bool {
    split_leading_conditional(text).is_some_and(|(_, body)| {
        let body_lower = body.to_lowercase();
        body_lower.starts_with("instead ")
    })
}

pub(crate) fn has_trigger_prefix(lower: &str) -> bool {
    alt((
        tag::<_, _, OracleError<'_>>("when "),
        tag("whenever "),
        tag("at "),
    ))
    .parse(lower)
    .is_ok()
}

pub(crate) fn lower_starts_with(lower: &str, prefix: &str) -> bool {
    tag::<_, _, OracleError<'_>>(prefix).parse(lower).is_ok()
}

pub(crate) fn is_flashback_equal_mana_cost(lower: &str) -> bool {
    scan_contains(lower, "flashback cost")
        && scan_contains(lower, "equal to")
        && scan_contains(lower, "mana cost")
}

pub(crate) fn is_defiler_cost_pattern(lower: &str) -> bool {
    lower_starts_with(lower, "as an additional cost to cast ")
        && !scan_contains(lower, "this spell")
        && scan_contains(lower, "you may pay")
        && scan_contains(lower, "life")
}

/// CR 118.9: Mana-cost-alternative-grant static — "You may [pay X] rather than
/// pay [the/its/this <object>'s] mana cost for [filter] spells you cast."
/// Rooftop Storm / Fist of Suns / Jodah class. `scan_contains` is a cheap
/// structural pre-filter; the lowering (`parse_spells_alternative_cost`)
/// re-parses with combinators and strict-fails on non-mana / unparsed filters.
pub(crate) fn is_spells_alternative_cost_pattern(lower: &str) -> bool {
    lower_starts_with(lower, "you may pay ")
        && scan_contains(lower, "rather than pay")
        && scan_contains(lower, "mana cost for")
        && scan_contains(lower, "spells you cast")
}

/// CR 118.9 + CR 701.59a: Collect-evidence alternative-cost grant static —
/// "You may collect evidence N rather than pay the mana cost for [filter]
/// spells you cast." Conspiracy Unraveler class. Separate from
/// `is_spells_alternative_cost_pattern` because the verb is "collect evidence",
/// not "pay". Verified: CR 118.9 (docs/MagicCompRules.txt:1014).
pub(crate) fn is_collect_evidence_alt_cost_pattern(lower: &str) -> bool {
    lower_starts_with(lower, "you may collect evidence ")
        && scan_contains(lower, "rather than pay")
        && scan_contains(lower, "mana cost for")
        && scan_contains(lower, "spells you cast")
}

/// CR 107.4f: K'rrik-class payment substitution — "For each {C} in a cost,
/// you may pay 2 life rather than pay that mana." Routes to
/// `parse_pay_life_as_colored_mana`.
/// Verified: CR 107.4f (docs/MagicCompRules.txt:507).
pub(crate) fn is_pay_life_as_colored_mana_pattern(lower: &str) -> bool {
    lower_starts_with(lower, "for each {")
        && scan_contains(lower, "in a cost")
        && scan_contains(lower, "you may pay")
        && scan_contains(lower, "rather than pay that mana")
}

/// CR 118.9 + CR 702.29a + CR 702.122a: Alternative keyword-cost grant static —
/// "[As long as <cond>, ]You may [cost] rather than pay [card-ref's] [keyword] cost[s]."
/// New Perspectives (cycling) / Heart of Kiran (crew) / Gavi class. Accepts an
/// optional leading "as long as " gate (New Perspectives); the lowering
/// (`parse_alternative_keyword_cost`) splits and types the condition, strict-failing
/// when the gate is unrecognized.
/// Verified: CR 702.29a (docs/MagicCompRules.txt:4202), CR 702.122a (docs/MagicCompRules.txt:4870).
pub(crate) fn is_alternative_keyword_cost_pattern(lower: &str) -> bool {
    (lower_starts_with(lower, "you may ")
        || (lower_starts_with(lower, "as long as ") && scan_contains(lower, "you may ")))
        && scan_contains(lower, "rather than pay")
        && (scan_contains(lower, "cycling cost") || scan_contains(lower, "crew cost"))
}

/// CR 118.9: Alternative-cost grant — "You may cast [filter] by paying {cost}
/// rather than paying their mana costs." Primal Prayers class. Structural
/// pre-filter; lowering is `parse_cast_spells_alternative_cost_multi`.
pub(crate) fn is_cast_spells_alternative_cost_pattern(lower: &str) -> bool {
    lower_starts_with(lower, "you may cast ")
        && scan_contains(lower, "by paying ")
        && scan_contains(lower, "rather than paying")
        && (scan_contains(lower, "their mana costs") || scan_contains(lower, "its mana cost"))
}

pub(crate) fn is_enters_tapped_cant_untap_compound(lower: &str) -> bool {
    let has_enters_tapped = scan_contains(lower, "enters tapped")
        || scan_contains(lower, "enters the battlefield tapped");
    let has_cant_untap = scan_contains(lower, "doesn't untap during")
        || scan_contains(lower, "doesn’t untap during");

    has_enters_tapped && has_cant_untap
}

pub(crate) fn is_compound_turn_limit(lower: &str) -> bool {
    scan_contains(lower, "only during your turn")
        && scan_contains(lower, "and ")
        && scan_contains(lower, "each turn")
}

pub(crate) fn is_opening_hand_begin_game(lower: &str) -> bool {
    scan_contains(lower, "opening hand") && scan_contains(lower, "begin the game")
}

pub(crate) fn is_ability_activate_cost_static(lower: &str) -> bool {
    scan_contains(lower, "abilities you activate")
        && scan_contains(lower, "cost")
        && scan_contains(lower, "less to activate")
}

pub(crate) fn is_damage_prevention_pattern(lower: &str) -> bool {
    scan_contains(lower, "damage") && scan_contains(lower, "can't be prevented")
}

pub(crate) fn should_defer_spell_to_effect(lower: &str) -> bool {
    if is_self_spell_cost_modification(lower) {
        return false;
    }

    if is_spell_resolution_cast_from_hand_free(lower) {
        return true;
    }

    if is_spell_resolution_next_untap_restriction(lower) {
        return true;
    }

    ((scan_contains(lower, "deals ") || scan_contains(lower, "deal "))
        && scan_contains(lower, "damage"))
        || scan_contains(lower, "until end of turn")
        || scan_contains(lower, "until your next turn")
        || scan_contains(lower, "this turn")
}

fn is_spell_resolution_next_untap_restriction(lower: &str) -> bool {
    let has_next_untap_restriction = (scan_contains(lower, "doesn't untap during")
        || scan_contains(lower, "doesn’t untap during"))
        && scan_contains(lower, "next untap step");
    if !has_next_untap_restriction {
        return false;
    }

    alt((
        tag::<_, _, OracleError<'_>>("put "),
        tag("tap "),
        tag("untap "),
        tag("target "),
        tag("that "),
        tag("it "),
        tag("those "),
    ))
    .parse(lower)
    .is_ok()
}

fn is_spell_resolution_cast_from_hand_free(lower: &str) -> bool {
    alt((
        tag::<_, _, OracleError<'_>>("you may cast "),
        tag("you may play "),
    ))
    .parse(lower)
    .is_ok()
        && scan_contains(lower, "from your hand")
        && (scan_contains(lower, "without paying its mana cost")
            || scan_contains(lower, "without paying their mana cost")
            || scan_contains(lower, "without paying their mana costs"))
}

fn is_self_spell_cost_modification(lower: &str) -> bool {
    let Ok((after_subject, _)) = alt((
        tag::<_, _, OracleError<'_>>("this spell costs "),
        tag("this card costs "),
        tag("~ costs "),
    ))
    .parse(lower) else {
        return false;
    };
    let Some((_, after_cost)) = parse_mana_symbols(after_subject) else {
        return false;
    };
    let after_cost = after_cost.trim_start();
    alt((
        tag::<_, _, OracleError<'_>>("less to cast"),
        tag("more to cast"),
    ))
    .parse(after_cost)
    .is_ok()
}

const STATIC_CONTAINS_PATTERNS: &[&str] = &[
    "gets +",
    "gets -",
    "get +",
    "get -",
    "have ",
    "has ",
    "can't be blocked",
    // CR 301.5 + CR 303.4 + CR 701.3a: positive attachment restriction on an
    // Aura/Equipment ("~ can be attached only to {filter}") — Strata Scythe,
    // Brass Knuckles, Konda's Banner. Routes to parse_static_line so it lowers
    // to StaticMode::AttachmentRestriction instead of an effect.
    "can be attached only to",
    "can't attack",
    "can't block",
    "can't be countered",
    "can't be copied",
    "can't be the target",
    "can't be sacrificed",
    "doesn't untap",
    "don't untap",
    "attacks or blocks each combat if able",
    "attacks each combat if able",
    "blocks each combat if able",
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
    "can't pay life",
    "can't win the game",
    "can't lose the game",
    "don't lose the game",
    // CR 704.5j: Mirror Gallery / Sakashima of a Thousand Faces class —
    // "the \"legend rule\" doesn't apply [to <scope> you control]". The leading
    // quote is required: scan_contains only matches at word starts, and "legend"
    // is glued to its opening quote ("legend) in the Oracle text.
    "\"legend rule\" doesn't apply",
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
    "can't draw cards",
    "can cast spells only during",
    // CR 602.5 + CR 117.1b: City of Solitude class — combined cast+activate
    // prohibition. The conjunction "and activate abilities" is the
    // discriminator; we route through the static parser so
    // `parse_cast_and_activate_only_during` emits the paired statics.
    "and activate abilities only during",
    "activated abilities can't be activated",
    "to cast spells or activate abilities",
    // CR 602.5 + CR 603.2a: Clarion/Karn-class global filter-scoped activation prohibition.
    // The "of ..." infix between "abilities" and "can't be activated" blocks the contiguous
    // scan above; recognize the dispatched prefix separately so parse_static_line is reached.
    "activated abilities of ",
    // CR 701.23 + CR 609.3: Ashiok-class search prohibition.
    "can't cause their controller to search their library",
    // CR 603.2 + CR 609.3: The Master, Multiplied-class sacrifice/exile prohibition.
    "triggered abilities ",
    "can't cause you to sacrifice or exile",
    // CR 701.23 + CR 609.3: Mindlock Orb-class search prohibition.
    "can't search libraries",
    "cannot search libraries",
    "may not search libraries",
    // CR 603.2g + CR 603.6a + CR 700.4: Torpor Orb / Hushbringer trigger suppression.
    "don't cause abilities to trigger",
    "skip your ",
    "maximum hand size",
    "life total can't change",
    "assigns combat damage equal to its toughness",
    "as though it weren't blocked",
    "attacking doesn't cause",
    "as though they had flash",
    "as though those creatures had haste",
    "as though that creature had haste",
    // CR 205.3 + CR 700.8: "<source> is also a[n] <subtype>(, <subtype>)*" —
    // self continuous type-grant (Burakos, Veteran Adventurer, and any future
    // printing whose first subtype opens with a vowel: "is also an Elf, …").
    // The phrase appears
    // only in CR 205.3 additive subtype statics, so the contains-scan cannot
    // false-positive into other pattern classes. Both articles must be
    // listed because the trailing space anchors the match to the article
    // boundary — "is also a " does not subsume "is also an X".
    "is also a ",
    "is also an ",
    // CR 702.73a + CR 205.3: "[subject] {is|are} every creature type" —
    // Changeling-class type grant (Mistform Ultimus / Dr. Julius Jumblemorph
    // self-ref CDA, Maskwood Nexus / Omo filter-subject grant, and the
    // Aura/Equipment conjunctive form on Arachnoform / Runed Stalactite /
    // Amorphous Axe). Both articles are listed because subject number
    // ("creature" vs "creatures") drives copula choice — neither subsumes the
    // other. The phrase is unique to creature-type grants (no other CR 205.3
    // construction uses "every creature type"), so the contains-scan cannot
    // false-positive into other pattern classes.
    "is every creature type",
    "are every creature type",
    // CR 502.3 + CR 113.6: Seedborn-class untap permission — "untap <subject>
    // during each other player's untap step" is always a continuous static, so
    // route it to `parse_static_line` regardless of subject (covers the self-ref
    // form "Untap this artifact …" on Bender's Waterskin, not just the "untap
    // all <type> you control" subject that already matched other patterns).
    // Lines that merely *trigger* at an untap step lead with "at the beginning
    // of …" and are caught by the trigger-prefix check before this point, so
    // this contains-scan stays specific to the static body. Both apostrophe
    // forms are listed because the source text is not apostrophe-normalized.
    "during each other player's untap step",
    "during each other player\u{2019}s untap step",
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
    // CR 601.3e: shorter sibling of "once during each of your turns, you may
    // cast" — Maralen, Fae Ascendant prints "Once each turn, you may cast a
    // creature spell from exile …". CR 601.3e governs static abilities that
    // allow casting spells from non-hand zones (Garruk's Horde / Melek
    // family). Routes the line into the static classifier so the cast-from-
    // exile-permission handler (follow-up PR) can pick it up. With no
    // handler implemented yet, `parse_static_line_multi` returns an empty
    // Vec and dispatch falls through to the next priority, matching pre-
    // change behavior — no regression today, correct preparatory routing
    // for the follow-up.
    "once each turn, you may cast",
    // CR 110.4 + CR 305.1 + CR 601.2a: Muldrotha — combined "play a land or
    // cast a permanent spell of each permanent type from your graveyard"
    // prefix. Routed to `parse_static_line` so the
    // `try_parse_graveyard_cast_permission` Muldrotha-class branch fires.
    "during each of your turns, you may play a land",
    "a deck can have",
    "nonland ",
    "noncreature ",
    "each noncreature ",
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
        .any(|pattern| scan_contains(lower, pattern))
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
    if scan_contains(lower, "as though it had flash") && !lower_starts_with(lower, "you may cast") {
        return true;
    }
    if scan_contains(lower, "enters with ") && !scan_contains(lower, "counter") {
        return true;
    }
    if lower_starts_with(lower, "creatures your opponents control ")
        && !lower.trim_end_matches('.').ends_with("enter tapped")
    {
        return true;
    }
    // CR 608.2g + CR 601.2: The one-shot free-cast window class —
    // "you may cast up to N [filter] spells ... from your graveyard and/or hand
    // without paying their mana costs" — is a SPELL-RESOLUTION effect, not a
    // continuous static permission. The diagnostic combination "up to" +
    // "without paying" never appears on the standing graveyard/exile permission
    // statics (Muldrotha, Gisa+Geralf, etc.), so route this form to effect
    // parsing (`try_parse_free_cast_from_zones`) instead of the static classifier.
    if scan_contains(lower, "you may cast up to")
        && scan_contains(lower, "from your")
        && scan_contains(lower, "without paying")
    {
        return false;
    }
    if alt((
        tag::<_, _, OracleError<'_>>("you may play"),
        tag("you may cast"),
    ))
    .parse(lower)
    .is_ok()
        && (scan_contains(lower, "from your graveyard")
            || (scan_contains(lower, "from your hand") && scan_contains(lower, "without paying"))
            // CR 401.5 + CR 118.9 + CR 601.2a: "you may [play|cast] X from the
            // top of your library" — top-of-library cast permission class
            // (Realmwalker, Future Sight, Bolas's Citadel, Magus of the Future,
            // Vivien on the Hunt static). Routes the line to `parse_static_line`
            // so it lowers to `StaticMode::TopOfLibraryCastPermission` instead
            // of falling through to `try_parse_cast_effect`'s impulse-draw flow.
            || scan_contains(lower, "from the top of your library")
            // CR 113.6b + CR 406.6: "you may play lands and cast spells from
            // among cards exiled with ~" — persistent, name-anchored exile-play
            // permission (The Matrix of Time). Routes to `parse_static_line` so
            // it lowers to `StaticMode::ExileCastPermission { pool: Persistent }`
            // instead of falling through to the imperative impulse-draw flow.
            || scan_contains(lower, "from among cards exiled with"))
    {
        return true;
    }
    // CR 117.1c + CR 113.6b: The Matrix-of-Time form leads with the timing
    // qualifier ("During your turn, you may play lands and cast spells from
    // among cards exiled with ~."), so the "you may [play|cast]" prefix is not
    // at the head of the line. The "play lands and cast spells from among cards
    // exiled with" anchor is the diagnostic substring; route it to the static
    // parser regardless of leading text.
    if scan_contains(
        lower,
        "play lands and cast spells from among cards exiled with",
    ) {
        return true;
    }
    // CR 601.3f + CR 406.6: The "look-at" variant leads with "you may look at
    // cards exiled with ~, and you may play lands and cast spells from among
    // those cards." — the play/cast clause uses "those cards" (a back-reference
    // to the exiled-with set) rather than repeating "cards exiled with". Require
    // both the source-anchored exile anchor and the play/cast clause so this
    // stays specific to the persistent exile-play permission.
    if scan_contains(lower, "cards exiled with")
        && scan_contains(lower, "play lands and cast spells from among those cards")
    {
        return true;
    }
    if scan_contains(lower, "can't cast") && scan_contains(lower, "spells") {
        return true;
    }
    // Passive voice: "Creature spells can't be cast."
    if scan_contains(lower, "spells can't be cast") {
        return true;
    }
    if scan_contains(lower, "no more than")
        && scan_contains(lower, "spells")
        && scan_contains(lower, "each turn")
    {
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

const GRANTED_STATIC_VERBS: &[&str] = &["has \"", "have \"", "gains \"", "gain \""];

pub(crate) fn is_granted_static_line(lower: &str) -> bool {
    GRANTED_STATIC_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        && GRANTED_STATIC_VERBS
            .iter()
            .any(|verb| scan_contains(lower, verb))
}

pub(crate) fn is_vehicle_tier_line(lower: &str) -> bool {
    if let Ok((_, (before, _))) = nom_primitives::split_once_on(lower, " | ") {
        let prefix = before.trim();
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
    "enters untapped",
    "enters prepared",
    "enter as a copy of",
    "enter tapped as a copy of",
    // CR 614.1c: "As ~ enters, you may have it become a copy of …" (Cursed Mirror
    // class). Shares parser/runtime with the "enter as a copy of" class but uses
    // a different verb; classify as replacement so the line routes through
    // `parse_replacement_line` even when its suffix carries a static keyword
    // pattern like "has haste" that would otherwise classify it as static.
    "become a copy of",
];

pub(crate) fn is_replacement_pattern(lower: &str) -> bool {
    if is_counter_prohibition_replacement_pattern(lower) {
        return true;
    }

    if REPLACEMENT_CONTAINS_PATTERNS
        .iter()
        .any(|pattern| scan_contains(lower, pattern))
    {
        return true;
    }

    if lower.trim_end_matches('.').ends_with(" enter tapped") {
        return true;
    }

    if lower.trim_end_matches('.').ends_with(" enter untapped") {
        return true;
    }

    is_replacement_compound_pattern(lower)
}

fn is_replacement_compound_pattern(lower: &str) -> bool {
    if is_as_enters_choose_pattern(lower) {
        return true;
    }
    if (scan_contains(lower, "enters") || scan_contains(lower, "escapes"))
        && scan_contains(lower, "counter")
    {
        return true;
    }
    if scan_contains(lower, "tapped for mana") && scan_contains(lower, "instead") {
        return true;
    }
    if scan_contains(lower, "you tap")
        && scan_contains(lower, "for mana")
        && scan_contains(lower, "instead")
    {
        return true;
    }
    if scan_contains(lower, "causes you to discard this card")
        && scan_contains(lower, "instead of putting it into your graveyard")
    {
        return true;
    }
    false
}

fn is_counter_prohibition_replacement_pattern(lower: &str) -> bool {
    // CR 614.17 + CR 122.1: Counter-prohibition effects lack "would" or
    // "instead" but still route through the replacement pipeline.
    nom_primitives::scan_at_word_boundaries(lower, |input| {
        alt((
            tag::<_, _, OracleError>("can't have counters put on"),
            tag("players can't get counters"),
            tag("counters can't be put on"),
        ))
        .parse(input)
    })
    .is_some()
}

fn is_as_enters_choose_pattern(lower: &str) -> bool {
    let has_as = nom_primitives::scan_at_word_boundaries(lower, |i| {
        tag::<_, _, OracleError<'_>>("as ").parse(i)
    })
    .is_some();
    let has_enters = nom_primitives::scan_at_word_boundaries(lower, |i| {
        tag::<_, _, OracleError<'_>>("enters").parse(i)
    })
    .is_some();
    let has_choose = nom_primitives::scan_at_word_boundaries(lower, |i| {
        verify(tag::<_, _, OracleError<'_>>("choose "), |_: &&str| {
            try_parse_named_choice(i).is_some()
        })
        .parse(i)
    })
    .is_some();
    has_as && has_enters && has_choose
}

/// CR 603.2 vs CR 614.1c: "Whenever <subject> enters with a counter on it, <consequence>"
/// is an ETB-with-counter triggered ability (it watches for ANY counter, hence the
/// untyped "a counter"), NOT a CR 614.1c self/granted enters-with replacement (which
/// always specifies a typed/counted counter: "a +1/+1 counter", "X +1/+1 counters",
/// "an additional loyalty counter", ...). Recognizing the untyped form lets the
/// Priority 5-pre replacement interceptor exclude Murderous Redcap Avatar and cousins
/// while still capturing the typed/counted replacements.
pub(crate) fn is_enters_with_counter_trigger(lower: &str) -> bool {
    nom_primitives::scan_at_word_boundaries(lower, |i| {
        terminated(
            tag::<_, _, OracleError<'_>>("enters with a counter on it"),
            tag(","),
        )
        .parse(i)
    })
    .is_some()
}

const EFFECT_IMPERATIVE_PREFIXES: &[&str] = &[
    "add ",
    "attach ",
    "counter ",
    "create ",
    "open ",
    "opens ",
    "roll to visit ",
    "deal ",
    "destroy ",
    "detain ",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_enters_with_counter_trigger() {
        // CR 603.2: untyped "enters with a counter on it," — ETB trigger.
        assert!(is_enters_with_counter_trigger(
            "whenever a creature you control enters with a counter on it, you may have it deal damage"
        ));
        assert!(is_enters_with_counter_trigger(
            "when a permanent you control enters with a counter on it, draw a card"
        ));
        // CR 614.1c: typed/counted forms are replacements, NOT triggers.
        assert!(!is_enters_with_counter_trigger(
            "this creature enters with x +1/+1 counters on it"
        ));
        assert!(!is_enters_with_counter_trigger(
            "that creature enters with a +1/+1 counter on it."
        ));
        assert!(!is_enters_with_counter_trigger(
            "that planeswalker enters with an additional loyalty counter on it."
        ));
        assert!(!is_enters_with_counter_trigger(
            "the token enters with x +1/+1 counters on it"
        ));
        assert!(!is_enters_with_counter_trigger(
            "it enters with twice that many +1/+1 counters on it"
        ));
    }

    /// CR 118.9: the mana-cost-alternative-grant classifier must recognize the
    /// Rooftop Storm / Fist of Suns shape and reject flash-permission text.
    #[test]
    fn classifies_spells_alternative_cost_pattern() {
        assert!(is_spells_alternative_cost_pattern(
            "you may pay {0} rather than pay the mana cost for zombie creature spells you cast."
        ));
        assert!(is_spells_alternative_cost_pattern(
            "you may pay {w}{u}{b}{r}{g} rather than pay the mana cost for spells you cast."
        ));
        assert!(!is_spells_alternative_cost_pattern(
            "you may cast this spell as though it had flash."
        ));
    }

    /// CR 118.9 + CR 107.14: Primal Prayers "you may cast ... by paying {E}"
    /// shape must route to the cast-by-paying alt-cost parser.
    #[test]
    fn classifies_cast_spells_alternative_cost_pattern() {
        assert!(is_cast_spells_alternative_cost_pattern(
            "you may cast creature spells with mana value 3 or less by paying {e} \
             rather than paying their mana costs."
        ));
        assert!(!is_cast_spells_alternative_cost_pattern(
            "you may pay {0} rather than pay the mana cost for zombie creature spells you cast."
        ));
    }
}
