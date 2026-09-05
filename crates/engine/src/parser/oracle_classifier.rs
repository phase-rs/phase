use crate::parser::oracle_nom::bridge::nom_on_lower;
use crate::parser::oracle_nom::error::{oracle_err, OracleError, OracleResult};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::{opt, peek, value, verify};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::oracle_nom::condition::parse_reflexive_entry_this_way_rider;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::primitives::scan_contains;
use super::oracle_util::parse_mana_symbols;
use crate::parser::oracle_effect::{
    split_leading_conditional, try_parse_named_choice, try_parse_named_choice_conjunction,
};

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
    // CR 118.9 + CR 601.2b: an optional once-per-turn frequency prefix (As
    // Foretold: "Once each turn, ...") precedes the "you may pay ..." grant.
    // Strip it via the shared lowering combinator before the structural gate.
    let after_frequency = opt(crate::parser::oracle_static::parse_alt_cost_frequency_prefix)
        .parse(lower)
        .map_or(lower, |(rest, _)| rest);
    lower_starts_with(after_frequency, "you may pay ")
        && scan_contains(lower, "rather than pay")
        && scan_contains(lower, "mana cost for")
        // Accept singular ("a spell you cast") and plural ("spells you cast").
        && (scan_contains(lower, "spell you cast") || scan_contains(lower, "spells you cast"))
}

/// CR 118.9 + CR 701.59a: Collect-evidence alternative-cost grant static —
/// "You may collect evidence N rather than pay the mana cost for [filter]
/// spells you cast." Conspiracy Unraveler class. Separate from
/// `is_spells_alternative_cost_pattern` because the verb is "collect evidence",
/// not "pay". Verified: CR 118.9.
pub(crate) fn is_collect_evidence_alt_cost_pattern(lower: &str) -> bool {
    lower_starts_with(lower, "you may collect evidence ")
        && scan_contains(lower, "rather than pay")
        && scan_contains(lower, "mana cost for")
        && scan_contains(lower, "spells you cast")
}

/// CR 107.4f: K'rrik-class payment substitution — "For each {C} in a cost,
/// you may pay 2 life rather than pay that mana." Routes to
/// `parse_pay_life_as_colored_mana`.
/// Verified: CR 107.4f.
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
/// Verified: CR 702.29a, CR 702.122a.
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
    // CR 114.1: An emblem-granting instant/sorcery ("You get an emblem with
    // \"…\"") whose quoted body contains static text (Gideon of the Trials'
    // can't-lose/win locks) matches `is_static_pattern` on the unmasked view, so
    // the spell IR loop would otherwise consume the whole line through the static
    // classifier — splitting the quoted body mid-sentence. Defer it to the
    // effect-chain parser, whose `try_parse_emblem_creation` seam produces a
    // single `Effect::CreateEmblem`. Reuses the emblem-head prefix combinator.
    if super::oracle_effect::sequence::is_emblem_creation_head(lower) {
        return true;
    }

    // CR 702.8a + CR 601.3d: "[~|this spell] has flash[ as long as <condition>]"
    // is a self-referential Flash CASTING PERMISSION
    // (`SpellCastingOption::AsThoughHadFlash`, built by
    // `oracle_casting::parse_self_has_flash_option`), not a continuous keyword
    // static — even though it also matches the generic "has " arm of
    // `STATIC_CONTAINS_PATTERNS`. Left unclassified, Priority 7 would lower it
    // to `StaticDefinition { affected: SelfRef, modifications:
    // [AddKeyword(Flash)] }`, which can never actually apply: CR 611.3b makes
    // a static's continuous effect apply while its source is on the
    // battlefield OR in "the appropriate zone", but this engine's
    // `for_each_static_effect_source` (an implementation choice, not itself a
    // numbered rule) only indexes continuous-effect SOURCES from the
    // battlefield and command zone, so a Hand-zone spell's own self-
    // referential static is never visited. That produces a "supported but
    // inert" card — the parser reports a non-empty static while the printed
    // permission never grants instant-speed casting. Deferring here routes
    // the line past
    // Priority 7 to Priority 8e (`parse_spell_casting_option_line`), the only
    // path that actually authorizes the cast.
    if is_self_conditional_flash_grant(lower) {
        return true;
    }

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

/// CR 702.8a + CR 601.3d: "[~|this spell] has flash" prefix — see the call
/// site in `should_defer_spell_to_effect` for why this must be deferred past
/// the static classifier. `this spell` is deliberately excluded from the `~`
/// self-reference normalization (`SELF_REF_PARSE_ONLY_PHRASES` in
/// `oracle_util.rs`), so both spellings are matched directly here — the same
/// `alt((tag("~"), tag("this spell")))` idiom `oracle_casting.rs` already uses
/// for sibling self-referential casting predicates
/// (`parse_cant_spend_mana_restriction`, `parse_negative_self_casting_restriction`).
fn is_self_conditional_flash_grant(lower: &str) -> bool {
    nom_primitives::parse_self_spell_has_flash_prefix(lower).is_ok()
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
    // CR 701.23 + CR 101.2: Ashiok-class search prohibition — a "can't search"
    // effect takes precedence over any effect directing a search.
    "can't cause their controller to search their library",
    // CR 603.2 + CR 101.2: The Master, Multiplied-class sacrifice/exile prohibition —
    // the "can't" effect takes precedence over the triggered ability directing it.
    "triggered abilities ",
    "can't cause you to sacrifice or exile",
    // CR 701.9a + CR 701.21a + CR 609.3: Sigarda, Host of Herons / Tajuru
    // Preserver / Tamiyo, Collector of Tales-class player-level protection —
    // the "can't" effect takes precedence over the spell/ability directing
    // the sacrifice/discard.
    "can't cause you to sacrifice permanents",
    "can't cause you to discard cards",
    // CR 701.23 + CR 101.2: Mindlock Orb-class search prohibition — the "can't"
    // effect takes precedence over any effect directing a search.
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

    if super::oracle_static::is_control_players_during_own_library_search(lower) {
        return true;
    }

    if super::oracle_static::is_tiered_enters_with_additional_counters_static(lower) {
        return true;
    }

    if super::oracle_static::is_extra_blockers_static_candidate(lower) {
        return true;
    }

    if super::oracle_static::is_unspent_mana_loss_causes_life_loss_static(lower) {
        return true;
    }

    // CR 509.1c: A printed permanent forced-block ("lure") static, "All creatures
    // able to block <self/enchanted creature> do so" (Ochran Assassin, Breaker of
    // Armies, Lure), routes to the static parser — NOT the one-shot spell form
    // "… target creature this turn do so", which stays an effect.
    if super::oracle_static::is_forced_block_static_candidate(lower) {
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
            // CR 117.1c: "During your turn, you may [cast|play] … from <zone>"
            // — the timing qualifier gates a standing cast-from-zone permission
            // (Leonardo, Sewer Samurai; Festival of Embers). Route to the static
            // parser ahead of the Priority-8 "enters … counter" replacement gate;
            // the graveyard/exile builder honors the qualifier via a
            // `DuringYourTurn` condition. Narrowly widens only the leading
            // frequency/timing qualifier, not the zone anchors below.
            tag("during your turn, "),
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
            || scan_contains(lower, "from among cards exiled with")
            // CR 108.3 + CR 113.6b: The "cards you own exiled with ~" variant
            // (Intrepid Paleontologist; Dawnhand Dissident) carries a "you own"
            // ownership infix between "cards" and "exiled with". Tolerate it so
            // the ExileCastPermission line routes to the static parser instead
            // of the Priority-8 replacement gate. Narrowly widens the exile
            // anchor to accept the ownership infix.
            || scan_contains(lower, "from among cards you own exiled with"))
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
    // CR 701.23f + CR 614.1a: "If an opponent/a player would search a library,
    // that player searches the top N cards of that library instead." (Aven
    // Mindcensor) leads with "...would search...", which the Priority-8 "would "
    // replacement gate would otherwise swallow (there is no Search replacement
    // event). Route to Priority-7 static. Specific conjunction avoids false hits.
    if scan_contains(lower, "would search a library") && scan_contains(lower, "instead") {
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
    "enter the battlefield untapped",
    "enters the battlefield untapped",
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

/// CR 608.2c + CR 614.1c: return the HEAD instruction of `lower` — the text with
/// every reflexive battlefield-entry "… this way" rider sentence removed.
///
/// A CR 608.2c rider is a back-reference to an instruction earlier in the same
/// ability. It routinely contributes the exact tokens CR 614.1c classification
/// keys on ("enters", "counter", "enters tapped", "enters under the control of"),
/// but those tokens belong to the back-reference, never to a replacement head.
/// The whole rider sentence is dropped, consequent included: the consequent's
/// tokens are the rider's, not the head's.
///
/// CONSUMPTION: `parse_reflexive_entry_this_way_rider` is a PREFIX recognizer and
/// is deliberately NOT wrapped in `all_consuming` here. Its remainder is the
/// rider's own consequent (", it enters with two additional +1/+1 counters on
/// it."), so requiring full consumption would reject every real rider — the class
/// exists only because it has a consequent. Fail-closed behavior comes from the
/// recognizer's narrowness instead (an article-or-pronoun subject + a
/// battlefield-entry verb + `" this way"` is not a shape any CR 614.1c head can
/// take), pinned in both directions by
/// `a_rider_prefix_drops_its_whole_sentence_by_contract`.
///
/// Segmentation uses `oracle_nom::primitives::split_sentence_units`, the total
/// wrapper over `parse_period_sentence` — the SAME combinator that feeds
/// `is_replacement_pattern` at its only sentence-scoped call site (`oracle.rs` via
/// `parse_replacement_sentence_sequence_ir`). A second, `split('.')`-based sentence
/// model would diverge from it in three ways that all matter here: `split` keeps
/// the leading space, drops the terminal '.', and emits an empty tail element — and
/// the residual is then fed to PREFIX-ANCHORED arms below
/// (`lower_starts_with(lower, "as ")`) that a leading space silently kills, and to
/// SUFFIX-anchored arms (`ends_with(" enter tapped")`) that need the period.
///
/// `None` means the text unit is ONLY riders and therefore has no replacement head
/// at all. That case is reachable at sentence scope: Pharika's Spawn's second
/// sentence ("When it enters this way, each opponent sacrifices a non-Gorgon
/// creature of their choice.") is entirely a rider.
///
/// `pub(crate)` because head-scoping is a property of CR 608.2c grammar, not of one
/// predicate: `oracle.rs` scopes the spell-line static gate and the Priority 5-pre
/// enters-with interceptor with this same function, so all three classification
/// gates share one model of "what is the head instruction".
pub(crate) fn strip_entry_this_way_riders(lower: &str) -> Option<std::borrow::Cow<'_, str>> {
    let is_rider = |unit: &str| parse_reflexive_entry_this_way_rider(unit).is_ok();

    let units = nom_primitives::split_sentence_units(lower);
    let kept: Vec<&str> = units.iter().copied().filter(|u| !is_rider(u)).collect();
    if kept.len() == units.len() {
        // Hot path: no rider anywhere, hand back the original with no allocation.
        return Some(std::borrow::Cow::Borrowed(lower));
    }

    // Residual normalization: a single separating space and no leading/trailing
    // whitespace, so prefix- and suffix-anchored arms still see an anchored string.
    let joined = kept.join(" ").trim().to_string();
    if joined.is_empty() {
        None
    } else {
        Some(std::borrow::Cow::Owned(joined))
    }
}

/// CR 614.1c + CR 608.2c: classify the HEAD instruction, not the whole text unit.
///
/// A reflexive battlefield-entry "… this way" rider (CR 608.2c) contributes
/// "enters"/"counter"/"enters tapped"/"enters under the control of" tokens that
/// belong to a back-reference, never to a replacement head. Scoping here — rather
/// than at one inner predicate — is required because SIX predicates below are
/// equally rider-contaminable: the `REPLACEMENT_CONTAINS_PATTERNS` scan alone
/// carries "enters the battlefield tapped", "enters tapped", "enters untapped" and
/// "enters under the control of". A text unit that is ONLY a rider has no head and
/// is not a replacement.
///
/// This subsumes and replaces the former
/// `has_trigger_prefix(lower) && scan_contains(lower, "enters this way,")` early
/// return, which was dead at five of this function's six call sites (they all sit
/// behind a `has_trigger_prefix` gate) and, at the one live sentence-scoped site,
/// returned `false` for exactly the inputs the blank-residual rule now returns
/// `false` for. Regression context it carried: Winter Soldier, Reborn Avenger,
/// whose TRIGGER routing is decided one gate earlier by the Priority 5-pre
/// enters-with interceptor in `oracle.rs` — that gate is head-scoped by
/// `strip_entry_this_way_riders` too, so both classification gates now model the
/// rider class identically instead of one of them re-deriving it from a literal.
pub(crate) fn is_replacement_pattern(lower: &str) -> bool {
    match strip_entry_this_way_riders(lower) {
        None => false,
        Some(head) => is_replacement_pattern_head_scoped(&head),
    }
}

fn is_replacement_pattern_head_scoped(lower: &str) -> bool {
    if super::oracle_replacement::is_search_found_replacement_pattern(lower) {
        return true;
    }

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
    // CR 701.3a + CR 614.1: "As ~ becomes attached [to X], choose …" — the
    // attach-time analogue of `is_as_enters_choose_pattern` (Psychic Paper).
    if is_as_becomes_attached_choose_pattern(lower) {
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
    // CR 614.1c + CR 208.2b: modal "As ~ enters, it becomes your choice of
    // [P/T profiles]" as-enters replacement (Primal Plasma / Primal Clay /
    // Corrupted Shapeshifter / Aquamorph Entity). The self-anchored modal
    // form is claimed here so the Priority-8 modal-lowering branch fires
    // before the generic replacement/static parsers.
    if is_as_enters_becomes_choice_pattern(lower) {
        return true;
    }
    // CR 614.1c: "enters with [counters]" replacement effects.
    if has_enters_with_counter_tokens(lower) {
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
/// The line is head-scoped by `strip_entry_this_way_riders` for the same CR 608.2c
/// reason `is_replacement_pattern` is: a rider's "enters … counter … for each"
/// tokens describe the back-reference, not a replacement head.
pub(crate) fn is_enters_with_counter_replacement_line(lower: &str) -> bool {
    strip_entry_this_way_riders(lower).is_some_and(|head| {
        has_enters_with_counter_tokens(&head) && scan_contains(&head, "for each")
    })
}

/// CR 614.1c: token signature of an "enters/escapes with counters" replacement.
///
/// The plural-subject forms ("Other creatures you control enter with …",
/// "… creatures escape with …") use the bare verb "enter"/"escape" rather than
/// "enters"/"escapes", so accept both at word boundaries. Gated on "counter" so
/// the bare verb alone never reclassifies a non-counter line.
///
/// Shared by `is_replacement_pattern_head_scoped` and
/// `is_enters_with_counter_replacement_line` so the two cannot drift.
fn has_enters_with_counter_tokens(lower: &str) -> bool {
    (scan_contains(lower, "enters")
        || scan_contains(lower, "escapes")
        || scan_contains(lower, "enter with")
        || scan_contains(lower, "escape with"))
        && scan_contains(lower, "counter")
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

/// CR 614.1c + CR 208.2b: modal "As ~ enters, it becomes your choice of
/// [P/T profiles]" as-enters replacement recognizer. Mirrors
/// [`parse_as_enters_becomes_in_addition`] but inverts its self-anchor gate: the
/// modal-choice form is always self-anchored (the entering creature becomes one
/// of two-or-more profiles it chooses for itself), so the subject MUST be the
/// bare `~` anaphor — the exact opposite of the non-self "in addition" subset
/// template. The "your choice of " pivot plus a required following fixed `<n>/<n>`
/// power/toughness token distinguishes this modal-P/T class from anchor-word
/// modals (bullet blocks) and from generic "choose a color" as-enters lines.
fn parse_as_enters_becomes_choice(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = tag("as ").parse(input)?;
    let (input, subject) = take_until(" enters").parse(input)?;
    if subject.trim() != "~" {
        return Err(oracle_err(input));
    }
    let (input, _) = alt((
        tag(" enters, it becomes your choice of "),
        tag(" enters the battlefield, it becomes your choice of "),
        tag(" enters or is turned face up, it becomes your choice of "),
    ))
    .parse(input)?;
    // Strip an optional leading article so the fixed-P/T peek reaches the token
    // ("a 3/3 creature" / "5/1"). `opt` never fails, preserving the slice when
    // absent.
    let (input, _) = opt(alt((tag("a "), tag("an ")))).parse(input)?;
    // Require a following fixed `<n>/<n>` power/toughness token: the modal
    // as-enters replacement (CR 208.2b) always lists specific P/T values. This
    // excludes non-P/T "becomes your choice of" phrasings from claiming the
    // modal-P/T lowering path.
    peek(verify(
        nom_primitives::parse_pt_value,
        |(power, toughness)| {
            matches!(
                (power, toughness),
                (
                    crate::types::ability::PtValue::Fixed(_),
                    crate::types::ability::PtValue::Fixed(_)
                )
            )
        },
    ))
    .parse(input)?;
    Ok((input, ()))
}

pub(crate) fn is_as_enters_becomes_choice_pattern(lower: &str) -> bool {
    parse_as_enters_becomes_choice(lower).is_ok()
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
    // Named-attribute choices only ("choose a creature type", "choose a color").
    // Object choices ("choose a creature" — Metamorphic Alteration, Dauntless
    // Bodyguard, Scheming Fence) are NOT replacement-classified here: claiming
    // them as Moved without a proven CopyChosen consumer changes unsupported
    // card shape for the whole class. Metamorphic's ChoosePermanent is injected
    // only by `LinkedChoiceKind::CopyChosenHost` after the companion static
    // parses.
    let has_choose = nom_primitives::scan_at_word_boundaries(lower, |i| {
        verify(tag::<_, _, OracleError<'_>>("choose "), |_: &&str| {
            try_parse_named_choice(i).is_some()
        })
        .parse(i)
    })
    .is_some();
    has_as && has_enters && has_choose
}

/// CR 701.3a + CR 614.1: the attach-time analogue of `is_as_enters_choose_pattern`
/// (Psychic Paper: "As this Equipment becomes attached to a creature, choose a
/// creature card name and a creature type."). Accepts both a single choice and
/// a conjunction ("choose X and Y") sharing one "choose".
fn is_as_becomes_attached_choose_pattern(lower: &str) -> bool {
    let has_as = nom_primitives::scan_at_word_boundaries(lower, |i| {
        tag::<_, _, OracleError<'_>>("as ").parse(i)
    })
    .is_some();
    let has_becomes_attached = nom_primitives::scan_at_word_boundaries(lower, |i| {
        tag::<_, _, OracleError<'_>>("becomes attached").parse(i)
    })
    .is_some();
    let has_choose = nom_primitives::scan_at_word_boundaries(lower, |i| {
        verify(tag::<_, _, OracleError<'_>>("choose "), |_: &&str| {
            try_parse_named_choice(i).is_some() || try_parse_named_choice_conjunction(i).is_some()
        })
        .parse(i)
    })
    .is_some();
    has_as && has_becomes_attached && has_choose
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
    fn self_conditional_flash_grant_matches_tilde_and_this_spell() {
        assert!(is_self_conditional_flash_grant(
            "~ has flash as long as you've committed a crime this turn."
        ));
        assert!(is_self_conditional_flash_grant(
            "this spell has flash as long as there are five or more mana values \
             among cards in your graveyard."
        ));
        assert!(is_self_conditional_flash_grant("~ has flash."));
    }

    #[test]
    fn self_conditional_flash_grant_word_boundary_excludes_flashback() {
        // Regression: "flash" is a literal prefix of "flashback", so a naive
        // `tag(" has flash")` would wrongly claim a real keyword-grant line
        // like "~ has flashback {2}{U}" and defer it away from the static
        // classifier that actually knows how to parse it.
        assert!(!is_self_conditional_flash_grant("~ has flashback {2}{u}."));
        assert!(!is_self_conditional_flash_grant(
            "this spell has flashback {2}{u}."
        ));
    }

    #[test]
    fn should_defer_spell_to_effect_defers_self_conditional_flash_but_not_flashback() {
        assert!(should_defer_spell_to_effect(
            "this spell has flash as long as there are five or more mana values \
             among cards in your graveyard."
        ));
        assert!(!should_defer_spell_to_effect("~ has flashback {2}{u}."));
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

    // -------------------------------------------------------------------
    // CR 608.2c + CR 614.1c: reflexive battlefield-entry rider head-scoping
    // -------------------------------------------------------------------

    /// Heroic Return, printed line index 1 (verbatim, lowercased).
    const HEROIC_RETURN_REANIMATION_LINE: &str =
        "return target creature card from your graveyard to the battlefield. \
         if a hero enters this way, it enters with two additional +1/+1 counters on it.";
    /// Recommission, printed line index 0 (verbatim, lowercased).
    const RECOMMISSION_REANIMATION_LINE: &str =
        "return target artifact or creature card with mana value 3 or less from your \
         graveyard to the battlefield. if a creature enters this way, it enters with \
         an additional +1/+1 counter on it.";
    /// Pharika's Spawn, escape line — sentence 1 IS a genuine CR 614.1c head.
    const PHARIKA_SENTENCE_1: &str = "this creature escapes with two +1/+1 counters on it.";
    /// Pharika's Spawn — sentence 2 is entirely a rider (the blank-residual case,
    /// and the one input the deleted `has_trigger_prefix` guard actually fired on).
    const PHARIKA_SENTENCE_2: &str =
        "when it enters this way, each opponent sacrifices a non-gorgon creature of their choice.";
    /// Silver Surfer, Cosmic Voyager — rider sentence; TRUE today only via the
    /// `REPLACEMENT_CONTAINS_PATTERNS` "enters tapped" literal.
    const SILVER_SURFER_RIDER_SENTENCE: &str = "if a land enters this way, it enters tapped.";
    /// Winter Soldier, Reborn Avenger — rider sentence; TRUE today via enters+counter.
    const WINTER_SOLDIER_RIDER_SENTENCE: &str =
        "if a hero enters this way, it enters with an additional +1/+1 counter on it.";

    /// V1: the two misparsing spell lines stop being classified as replacements,
    /// while a genuine replacement head at the same (sentence) scope does not —
    /// so a blanket-`false` regression cannot pass this test.
    #[test]
    fn reflexive_entry_rider_does_not_make_a_line_a_replacement() {
        assert!(!is_replacement_pattern(HEROIC_RETURN_REANIMATION_LINE));
        assert!(!is_replacement_pattern(RECOMMISSION_REANIMATION_LINE));

        // Non-vacuous positive: the head IS a replacement.
        assert!(is_replacement_pattern(PHARIKA_SENTENCE_1));

        // Rider-only text units have no head at all (blank-residual rule). These
        // reproduce, at the one scope where it was live, the verdict of the
        // deleted `has_trigger_prefix && "enters this way,"` guard.
        assert!(!is_replacement_pattern(PHARIKA_SENTENCE_2));
        assert!(!is_replacement_pattern(SILVER_SURFER_RIDER_SENTENCE));
        assert!(!is_replacement_pattern(WINTER_SOLDIER_RIDER_SENTENCE));
    }

    /// V1: the trigger-prefixed full LINES the deleted guard covered keep their
    /// `false` verdict, so no line-scope routing moved for that pair.
    #[test]
    fn trigger_prefixed_entry_rider_lines_stay_non_replacement() {
        assert!(!is_replacement_pattern(
            "whenever this creature attacks, return target creature card from your \
             graveyard to the battlefield. if a hero enters this way, it enters with \
             an additional +1/+1 counter on it."
        ));
        assert!(!is_replacement_pattern(
            "when this creature enters, search your library for a land card, put it onto \
             the battlefield, then shuffle. if a land enters this way, it enters tapped."
        ));
    }

    /// V0c: residual normalization. A dropped rider must leave the surviving head
    /// anchored (no leading space), or prefix-anchored arms below silently die.
    #[test]
    fn stripping_a_rider_leaves_the_head_anchored() {
        let line = "as this creature is turned face up, draw a card. \
                    if a creature enters this way, it enters tapped.";
        let head = strip_entry_this_way_riders(line).expect("head survives");
        assert!(
            lower_starts_with(&head, "as "),
            "residual must stay prefix-anchored, got {head:?}"
        );
        // The line is still a replacement via the "as … is turned face up" arm.
        assert!(is_replacement_pattern(line));

        // Zero-allocation hot path: a rider-free line is handed back borrowed.
        assert!(matches!(
            strip_entry_this_way_riders(PHARIKA_SENTENCE_1),
            Some(std::borrow::Cow::Borrowed(_))
        ));
        // A text unit that is ONLY a rider has no head.
        assert!(strip_entry_this_way_riders(PHARIKA_SENTENCE_2).is_none());
    }

    /// V1b: Priority-7 routing keeps its class while becoming head-scoped.
    #[test]
    fn enters_with_counter_replacement_line_is_head_scoped() {
        // Gev, Scaled Scorch (verbatim): tokens come from the HEAD, so
        // over-stripping fails here.
        const GEV_DISTRIBUTIVE_LINE: &str =
            "other creatures you control enter with an additional +1/+1 counter on them \
             for each opponent who lost life this turn.";
        assert!(is_enters_with_counter_replacement_line(
            GEV_DISTRIBUTIVE_LINE
        ));

        // Every token comes from the rider sentence — fails on revert to the
        // whole-line form.
        const RIDER_ONLY_FOR_EACH_LINE: &str =
            "return target creature card from your graveyard to the battlefield. \
             if a hero enters this way, it enters with an additional +1/+1 counter on it \
             for each card in your graveyard.";
        assert!(!is_enters_with_counter_replacement_line(
            RIDER_ONLY_FOR_EACH_LINE
        ));

        // The " for each " gate is preserved on the residual: an enters+counter
        // head without it must stay out of the Priority-7 reroute.
        assert!(!is_enters_with_counter_replacement_line(PHARIKA_SENTENCE_1));
    }

    /// The head-scoper covers the WHOLE rider class the combinator recognizes,
    /// not the single present-tense/comma-terminated voice the two retired
    /// literals modelled. Both `oracle.rs` gates (spell-line static, Priority
    /// 5-pre enters-with) consume this function, so each voice below is a voice
    /// those gates now scope off too.
    #[test]
    fn head_scoping_covers_every_rider_voice_the_literal_missed() {
        // Passive voice: the retired literals scanned for "enters this way," and
        // this text does not contain it, so both let the rider's tokens through.
        const PASSIVE: &str = "return target creature card from your graveyard to the \
             battlefield. if a creature is put onto the battlefield this way, it enters \
             with an additional +1/+1 counter on it.";
        assert!(
            !scan_contains(PASSIVE, "enters this way,"),
            "premise: the retired literal does not match the passive voice"
        );
        let head = strip_entry_this_way_riders(PASSIVE).expect("head survives");
        assert!(
            !scan_contains(&head, "enters with"),
            "the passive rider must be scoped off the head, got {head:?}"
        );

        // Comma-less voice: the rider's clause ends at the period, not a comma —
        // the SUBJECT is still clause-initial, which is the position
        // `parse_entry_this_way_clause` recognizes.
        const COMMA_LESS: &str = "return target creature card from your graveyard to the \
             battlefield. a hero enters this way.";
        assert!(
            !scan_contains(COMMA_LESS, "enters this way,"),
            "premise: the retired literal does not match the comma-less voice"
        );
        let head = strip_entry_this_way_riders(COMMA_LESS).expect("head survives");
        assert!(
            !scan_contains(&head, "enters this way"),
            "the comma-less rider must be scoped off the head, got {head:?}"
        );

        // Active "you put …" voice.
        const ACTIVE: &str = "search your library for a land card and put it onto the \
             battlefield. if you put a land onto the battlefield this way, it enters with \
             a +1/+1 counter on it.";
        assert!(
            !scan_contains(ACTIVE, "enters this way,"),
            "premise: the retired literal does not match the active voice"
        );
        let head = strip_entry_this_way_riders(ACTIVE).expect("head survives");
        assert!(
            !scan_contains(&head, "enters with"),
            "the active-voice rider must be scoped off the head, got {head:?}"
        );

        // Non-vacuous: a genuine CR 614.1c head keeps its tokens in every case.
        for genuine in [
            "this creature enters with two +1/+1 counters on it.",
            "other creatures you control enter with an additional +1/+1 counter on them.",
        ] {
            let head = strip_entry_this_way_riders(genuine).expect("head survives");
            assert!(
                scan_contains(&head, "enters with") || scan_contains(&head, "enter with"),
                "a genuine head must keep its tokens, got {head:?}"
            );
        }
    }

    /// LOW-finding sibling gate: `is_static_pattern` is rider-contaminable through
    /// exactly the same `enters with ` token, one branch EARLIER on the spell path
    /// than `is_replacement_pattern`. `oracle.rs` head-scopes it with this same
    /// function; this pins the verdict flip the head-scoping produces.
    #[test]
    fn static_classification_is_rider_contaminable_without_head_scoping() {
        // A non-counter rider consequent: `is_static_compound_pattern` fires on
        // `"enters with " && !"counter"`, which the rider alone supplies.
        const NON_COUNTER_RIDER_LINE: &str = "return target creature card from your \
             graveyard to the battlefield. if a hero enters this way, it enters with \
             your choice of flying or vigilance.";
        assert!(
            is_static_pattern(NON_COUNTER_RIDER_LINE),
            "premise: the un-scoped line classifies as a static — this is the gate \
             that dropped the head instruction"
        );
        let head = strip_entry_this_way_riders(NON_COUNTER_RIDER_LINE).expect("head survives");
        assert!(
            !is_static_pattern(&head),
            "the head instruction alone is not a static, got {head:?}"
        );

        // Non-vacuous: a real static keeps its verdict through head-scoping.
        const REAL_STATIC: &str = "creatures you control can't block.";
        assert!(is_static_pattern(REAL_STATIC));
        assert!(
            strip_entry_this_way_riders(REAL_STATIC).is_some_and(|head| is_static_pattern(&head))
        );
    }

    /// POSITION BOUNDARY: `parse_entry_this_way_clause` recognizes a rider only
    /// CLAUSE-INITIALLY, so a trailing-position entry rider is deliberately left
    /// unscoped. This test states that limit rather than implying coverage the
    /// combinator does not have.
    ///
    /// The limit is safe because the trailing voice is UNPRINTED: a Scryfall regex
    /// sweep for a sentence-final battlefield-entry back-reference
    /// (`o:/(enters|enter|is put onto the battlefield|are put onto the battlefield) this way\./`)
    /// returns zero cards. The shape that DOES print sentence-finally is the
    /// second assertion below — a genuine CR 614.1c head whose trailing back-reference
    /// is a NON-entry zone change — and that one must keep its tokens.
    #[test]
    fn head_scoping_leaves_the_unprinted_trailing_rider_voice_alone() {
        // Trailing-position entry rider: synthetic, unprinted, and out of scope.
        const TRAILING_RIDER: &str = "return target creature card from your graveyard to the \
             battlefield. it enters with an additional +1/+1 counter on it if a hero \
             enters this way.";
        let head = strip_entry_this_way_riders(TRAILING_RIDER).expect("head survives");
        assert!(
            scan_contains(&head, "enters with"),
            "documented limit: a trailing-position rider is NOT scoped off the head, \
             got {head:?}"
        );

        // Arsenal Thresher (verbatim second sentence): a real CR 614.1c head with a
        // trailing NON-entry back-reference. `ThisWayVerbScope::BattlefieldEntry`
        // withholds "revealed", so this head keeps its tokens — the property a
        // position-scanning recognizer would put at risk.
        const ARSENAL_THRESHER_HEAD: &str =
            "this creature enters with a +1/+1 counter on it for each card revealed this way.";
        let head = strip_entry_this_way_riders(ARSENAL_THRESHER_HEAD).expect("head survives");
        assert!(
            scan_contains(&head, "enters with"),
            "a printed CR 614.1c head with a trailing non-entry back-reference must \
             keep its tokens, got {head:?}"
        );
    }

    /// CONSUMPTION CONTRACT: `parse_reflexive_entry_this_way_rider` is a PREFIX
    /// recognizer, and this consumer discards the WHOLE sentence on a prefix match.
    /// That is the intended contract, not an oversight: the text after the comma is
    /// the back-reference's own consequent, so its "enters"/"counter"/"tapped"
    /// tokens are the rider's and never a CR 614.1c head's. Wrapping the recognizer
    /// in `all_consuming` here would reject every real rider, since the class exists
    /// only because it HAS a consequent.
    ///
    /// Fail-closed behavior comes from the recognizer's narrowness instead, pinned
    /// in both directions below.
    #[test]
    fn a_rider_prefix_drops_its_whole_sentence_by_contract() {
        // Prefix match → whole sentence gone, consequent included.
        const RIDER_WITH_CONSEQUENT: &str = "return target creature card from your graveyard \
             to the battlefield. if a hero enters this way, it enters tapped and enters \
             under the control of an opponent.";
        let head = strip_entry_this_way_riders(RIDER_WITH_CONSEQUENT).expect("head survives");
        assert!(
            !scan_contains(&head, "enters tapped")
                && !scan_contains(&head, "enters under the control of"),
            "the consequent's tokens belong to the rider and must go with it, got {head:?}"
        );
        assert!(
            scan_contains(&head, "return target creature card"),
            "the head instruction must survive, got {head:?}"
        );

        // The other direction: a sentence that merely CONTAINS an entry verb does
        // not open with a back-reference, so nothing is dropped. The recognizer's
        // narrowness — not full consumption — is what keeps this fail-closed.
        for retained in [
            "this creature enters with two +1/+1 counters on it.",
            "when this creature enters, draw a card.",
            "if a creature card was exiled this way, you may cast it.",
        ] {
            assert!(
                matches!(
                    strip_entry_this_way_riders(retained),
                    Some(std::borrow::Cow::Borrowed(_))
                ),
                "a non-rider sentence must be handed back untouched: {retained}"
            );
        }
    }
}
