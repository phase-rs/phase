use crate::parser::oracle_nom::bridge::nom_on_lower;
use crate::parser::oracle_nom::error::{oracle_err, OracleError, OracleResult};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::{opt, value, verify};
use nom::sequence::{preceded, terminated};
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

/// CR 702.34a + CR 601.2f: Split a compound flashback line that also carries a
/// self-spell cost reduction (Visions of Ruin: "Flashback {8}{R}{R}. This spell
/// costs {X} less to cast this way, …").
pub(crate) fn split_flashback_trailing_self_spell_cost_reduction<'a>(
    line: &'a str,
    lower: &'a str,
) -> Option<(&'a str, &'a str)> {
    const SPELL_MARKER: &str = ". this spell costs ";
    const CARD_MARKER: &str = ". this card costs ";

    if let Some(((), reduction_text)) = nom_on_lower(line, lower, |input| {
        preceded(
            tag("flashback"),
            value((), (take_until(SPELL_MARKER), tag(". "))),
        )
        .parse(input)
    }) {
        let flashback_len = line.len() - ". ".len() - reduction_text.len();
        return Some((line[..flashback_len].trim(), reduction_text.trim()));
    }

    if let Some(((), reduction_text)) = nom_on_lower(line, lower, |input| {
        preceded(
            tag("flashback"),
            value((), (take_until(CARD_MARKER), tag(". "))),
        )
        .parse(input)
    }) {
        let flashback_len = line.len() - ". ".len() - reduction_text.len();
        return Some((line[..flashback_len].trim(), reduction_text.trim()));
    }

    None
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
    if is_self_spell_cost_modification_body(lower) {
        return true;
    }
    // CR 207.2c: an ability-word prefix ("Void — This spell costs {2} less to
    // cast if …", Temporal Intervention) has no rules meaning — strip it so the
    // self-cost-modification guard recognizes the body. Without this, the
    // "this turn" inside the gating condition makes `should_defer_spell_to_effect`
    // route the line to the effect parser, dropping the cost reduction.
    super::oracle_modal::strip_ability_word(lower)
        .as_deref()
        .is_some_and(is_self_spell_cost_modification_body)
}

fn is_self_spell_cost_modification_body(body: &str) -> bool {
    let Ok((after_subject, _)) = alt((
        tag::<_, _, OracleError<'_>>("this spell costs "),
        tag("this card costs "),
        tag("~ costs "),
    ))
    .parse(body) else {
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
    // CR 506.5 + CR 508.1c: Master of Cruelties — "~ can only attack alone"
    // must route to the static parser (CombatAlone MustBeSole), not the effect
    // pipeline where it previously lowered to Unimplemented.
    "can only attack alone",
    "can't block",
    "can't be countered",
    "can't be copied",
    "can't be the target",
    "can't be sacrificed",
    // CR 116.2b + CR 708.7: "Permanents your opponents control can't be turned
    // face up during your turn" (Karlov Watchdog) — prohibition static. Routes
    // to parse_static_line so it lowers to StaticMode::CantBeTurnedFaceUp.
    "can't be turned face up",
    "doesn't untap",
    "don't untap",
    "attacks or blocks each combat if able",
    "attacks each combat if able",
    "blocks each combat if able",
    "can block only creatures with flying",
    "no maximum hand size",
    "may choose not to untap",
    "play with the top card",
    // CR 400.2 + CR 701.20a: Telepathy/Revelation class. Keep this narrower
    // than generic hand-reveal effects ("reveal a card from your hand") by
    // matching the continuous "hand(s) revealed" wording.
    "hands revealed",
    "hand revealed",
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
    "play any number of lands",
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
    // CR 502.3: Smoke / Damping Field / Winter Orb class — "Players can't untap
    // more than one <type> during their untap steps." Routes to the static
    // parser so it lowers to StaticMode::MaxUntapPerType instead of an effect.
    "can't untap more than",
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
    // CR 509.1b + CR 702.28b: shadow block permission (Heartwood Dryad, Wall of
    // Diffusion) — "can block creatures with shadow as though [they didn't|it] had
    // shadow". Anchored on the full subject so it never false-matches a plain
    // shadow grant or attacker-side restriction.
    "block creatures with shadow as though",
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
    // CR 708.5: "You may look at face-down creatures [you don't control | your
    // opponents control] any time." (Found Footage) — top-level look-permission
    // static. Routed to `parse_static_line` so it lowers to MayLookAtFaceDown.
    "you may look at face-down creatures",
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

    if super::oracle_static::is_tiered_enters_with_additional_counters_static(lower) {
        return true;
    }

    if super::oracle_static::is_extra_blockers_static_candidate(lower) {
        return true;
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
    // CR 604.2 + CR 601.2a: head-anchor the "you may play"/"you may cast"
    // permission lead, allowing an optional leading once-per-turn frequency
    // phrase ("Once during each of your turns, " / "Once each turn, ") to be
    // stripped first. This classifies the disjunctive once-per-turn play/cast-
    // from-zone permission (The Eighth Doctor, Serra Paragon) as static so it
    // routes ahead of the Priority 8 "would" replacement fallback — the granted
    // rider's "would leave the battlefield" text would otherwise misclassify the
    // whole line as a replacement. Class-level anchor, not a per-card branch.
    if preceded(
        opt(alt((
            tag::<_, _, OracleError<'_>>("once during each of your turns, "),
            tag("once each turn, "),
        ))),
        alt((tag("you may play"), tag("you may cast"))),
    )
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
    // CR 117.1c + CR 113.6b: Evendo-class compact persistent exile-play
    // permission. Like the Matrix form above, this may be preceded by timing
    // and condition qualifiers.
    if scan_contains(lower, "you may play cards exiled with")
        || scan_contains(lower, "you may play the cards exiled with")
    {
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
    // CR 701.55c: "If an opponent would face a villainous choice, they face that
    // choice an additional time." (The Valeyard) leads with "if …" and contains
    // "would ", so it is otherwise classified as a replacement and never reaches
    // the static parser. It is in fact an extra-instance rule-modifying static
    // (`StaticMode::GrantsExtraVillainousChoice`, the CR 701.55c twin of
    // `GrantsExtraVote`). Route it to Priority 7 static dispatch — which runs
    // before the Priority 8 replacement gate — so it lowers to the static.
    if scan_contains(lower, "face a villainous choice") && scan_contains(lower, "additional time") {
        return true;
    }
    // CR 121.1 / CR 613.11: "[subject] draw(s) cards from the bottom of [your|
    // their] library rather than/instead of the top." — River Song's draw-source
    // redirection static (Meet in Reverse). The body verb is "draw", so none of
    // the generic static keywords (get/have/can't) anchor it; without this gate
    // the (ability-word-prefixed) line never reaches Priority 7 and falls to the
    // spell catch-all as Unimplemented. The "from the bottom of" + "library" +
    // top-reference combination is the diagnostic; extraction is delegated to
    // `parse_draw_from_bottom`, which lowers it to `StaticMode::DrawFromBottom`.
    if scan_contains(lower, "from the bottom of")
        && scan_contains(lower, "library")
        && (scan_contains(lower, "rather than the top")
            || scan_contains(lower, "instead of the top"))
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
    // CR 110.2a + CR 614.1d: "[self] enters under the control of an opponent of
    // your choice" (Xantcha, Sleeper Agent; Pendant of Prosperity; Abby,
    // Merciless Soldier). A self-ETB controller-override replacement — route the
    // line to `parse_replacement_line`/`parse_self_enters_under_opponent`, whose
    // self-subject gate rejects external-subject false positives. Without this,
    // the line falls through to the effect parser and emits Unimplemented.
    "enters under the control of",
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

    // CR 614.1e + CR 708.11: "As ~ is turned face up, [effect]"
    // is a replacement effect. The "When ~ is turned face up" form is a trigger
    // and stays out of this path, so the lead is required to be "As".
    if lower_starts_with(lower, "as ") && scan_contains(lower, "is turned face up") {
        return true;
    }

    is_replacement_compound_pattern(lower)
}

fn is_replacement_compound_pattern(lower: &str) -> bool {
    if is_as_enters_choose_pattern(lower) {
        return true;
    }
    // CR 614.1c + CR 614.12: "As a [filter] enters, it becomes a [P/T] [type]
    // creature in addition to its other types" — a replacement from another
    // source affecting a subset of entrants (Displaced Dinosaurs). Routes to
    // `parse_replacement_line`. The line does not match `is_static_pattern`
    // (no "becomes"/"in addition" static-contains entry; the "as " prefix is
    // not a static-prefix entry), so no Priority-7 reroute is required.
    if is_as_enters_becomes_in_addition_pattern(lower) {
        return true;
    }
    // CR 614.1c: "enters with [counters]" replacement effects. The plural-subject
    // forms ("Other creatures you control enter with …", "… creatures escape
    // with …") use the bare-verb "enter"/"escape" rather than "enters"/"escapes",
    // so accept both at word boundaries. Gated on "counter" so the bare verb
    // alone never reclassifies a non-counter line.
    if (scan_contains(lower, "enters")
        || scan_contains(lower, "escapes")
        || scan_contains(lower, "enter with")
        || scan_contains(lower, "escape with"))
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
    if scan_contains(lower, "an effect causes you to discard a card")
        && scan_contains(lower, "instead of into your graveyard")
    {
        return true;
    }
    false
}

/// CR 614.1c + CR 614.12: Recognizer for the *dynamically scaled* distributive
/// "[Other/each] [type] you control enter(s) with [an additional] [counter] …
/// for each …" replacement lines (Gev, Scaled Scorch). Used by the Priority 7
/// (static-pattern) dispatcher to route these counter replacements to the
/// replacement parser before the static parser claims them — their
/// "[type] you control …" subject also satisfies `is_static_pattern`.
///
/// The " for each " gate is load-bearing: the fixed-count and conditional-tier
/// distributive forms ("Each other Vehicle … enters with an additional +1/+1
/// counter on it if its mana value is 4 or less. Otherwise …" — Thunderous
/// Velocipede) are owned by `StaticMode::EntersWithAdditionalCounters` (which
/// carries a fixed `count`), so this recognizer must NOT intercept them. Only
/// the per-each *scaled* count, which the static mode cannot represent, routes
/// to the dynamic-capable replacement (`PutCounter { count: QuantityExpr }`).
pub(crate) fn is_enters_with_counter_replacement_line(lower: &str) -> bool {
    (scan_contains(lower, "enters")
        || scan_contains(lower, "escapes")
        || scan_contains(lower, "enter with")
        || scan_contains(lower, "escape with"))
        && scan_contains(lower, "counter")
        && scan_contains(lower, "for each")
}

/// CR 614.1c + CR 614.12: nom recognizer for the non-self "As a [filter] enters,
/// it becomes a [P/T] [type] creature in addition to its other types" replacement
/// template (Displaced Dinosaurs). The subject is a non-empty external permanent
/// filter (never the bare self anaphor), and the additive "in addition to its
/// other types" tail (CR 205.1b) is required so this never claims a set-replacing
/// "becomes" line. Self / copy "enter as a copy" lines are claimed by earlier
/// handlers and additionally fail the handler's `Typed`-subject guard.
fn parse_as_enters_becomes_in_addition(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = tag("as ").parse(input)?;
    let (input, subject) = take_until(" enters").parse(input)?;
    if subject.trim().is_empty() || subject.trim() == "~" {
        return Err(oracle_err(input));
    }
    let (input, _) = alt((
        tag(" enters, it becomes a "),
        tag(" enters, it becomes an "),
        tag(" enters the battlefield, it becomes a "),
        tag(" enters the battlefield, it becomes an "),
    ))
    .parse(input)?;
    // CR 205.1b + CR 105.3: require the full additive marker via the shared
    // animation combinator so this recognizer covers the entire marker class
    // (possessive variants, "creature types", "colors and types") rather than
    // the single hardcoded Displaced Dinosaurs literal.
    let (input, _) =
        crate::parser::oracle_effect::animation::locate_in_addition_other_types_marker(input)?;
    Ok((input, ()))
}

pub(crate) fn is_as_enters_becomes_in_addition_pattern(lower: &str) -> bool {
    parse_as_enters_becomes_in_addition(lower).is_ok()
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
    use super::nom_primitives::strip_double_quoted_spans;
    use super::*;

    #[test]
    fn masked_white_suns_twilight_is_not_static() {
        // The only static-shaped marker ("can't block") lives INSIDE the token's
        // quoted ability text; masking it must yield a non-static spell line.
        let line = "you gain x life. create x 1/1 colorless phyrexian mite artifact \
            creature tokens with toxic 1 and \"this token can't block.\" if x is 5 or more, \
            destroy all other creatures.";
        assert!(!is_static_pattern(&strip_double_quoted_spans(line)));
    }

    #[test]
    fn masked_brood_birthing_stays_static() {
        // Brood Birthing invariant: the "have " grant marker is OUTSIDE the quote,
        // so masking the quoted span must NOT flip the line off static.
        let line = "they have \"sacrifice this token: add {c}.\"";
        assert!(is_static_pattern(&strip_double_quoted_spans(line)));
    }

    #[test]
    fn unquoted_cant_block_static_unchanged() {
        // No quotes → fast path → classification unchanged.
        assert!(is_static_pattern("creatures you control can't block"));
    }

    #[test]
    fn split_flashback_trailing_self_spell_cost_reduction_splits_visions_line() {
        let line = "Flashback {8}{R}{R}. This spell costs {X} less to cast this way, where X is the greatest mana value of a commander you own on the battlefield or in the command zone.";
        let lower = line.to_lowercase();
        let (flashback, reduction) =
            split_flashback_trailing_self_spell_cost_reduction(line, &lower).unwrap();
        assert_eq!(flashback, "Flashback {8}{R}{R}");
        assert_eq!(
            reduction,
            "This spell costs {X} less to cast this way, where X is the greatest mana value of a commander you own on the battlefield or in the command zone."
        );
    }

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

    #[test]
    fn classifies_tiered_enters_with_additional_counters_static() {
        let lower = "each other vehicle and creature you control enters with an additional +1/+1 counter on it if its mana value is 4 or less. otherwise, it enters with three additional +1/+1 counters on it.";
        assert!(is_static_pattern(lower));
        assert!(is_replacement_pattern(lower));
    }
}
