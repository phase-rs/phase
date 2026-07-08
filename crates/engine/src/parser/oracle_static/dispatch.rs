// CR 604 — `parse_static_line_inner` category dispatch.
use super::super::oracle_nom::error::oracle_err;
#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;
use super::{
    anthem::*, cda::*, cost_mod::*, evasion::*, keyword_grant::*, loyalty::*, mana_transform::*,
    restriction::*, type_change::*,
};
use crate::types::statics::ProhibitionScope;

/// CR 201.5: Consume a self-reference subject — `~` (produced by
/// `normalize_card_name_refs` for "text that refers to the object it's on by
/// name") or a typed self-reference phrase ("this creature", "this permanent",
/// …). Used by dynamic referent parsing where the subject is the static's own
/// source ("where X is ~'s power").
fn parse_self_reference_subject(input: &str) -> OracleResult<'_, ()> {
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("~").parse(input) {
        return Ok((rest, ()));
    }
    for phrase in SELF_REF_TYPE_PHRASES {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(*phrase).parse(input) {
            return Ok((rest, ()));
        }
    }
    Err(oracle_err(input))
}

/// CR 208.1 + CR 113.7: Parse the dynamic referent of a "{X} … less to activate,
/// where X is [source]'s {power|toughness|mana value}" activated-ability cost
/// reduction (Agatha of the Vile Cauldron — "where X is Agatha's power", which
/// `normalize_card_name_refs` rewrites to "where X is ~'s power"). Returns the
/// typed `QuantityRef` scoped to the static's source object (`ObjectScope::Source`),
/// so the reduction reads the source's post-layer characteristic at
/// cost-determination time (CR 113.7).
fn parse_where_x_is_self_stat(input: &str) -> OracleResult<'_, QuantityRef> {
    let (input, _) = tag(", where x is ").parse(input)?;
    let (input, _) = parse_self_reference_subject(input)?;
    alt((
        value(
            QuantityRef::Power {
                scope: ObjectScope::Source,
            },
            tag("'s power"),
        ),
        value(
            QuantityRef::Toughness {
                scope: ObjectScope::Source,
            },
            tag("'s toughness"),
        ),
        value(
            QuantityRef::ObjectManaValue {
                scope: ObjectScope::Source,
            },
            tag("'s mana value"),
        ),
    ))
    .parse(input)
}

/// Whether the inverted `"As long as <cond>, <effect>"` detector may fire.
///
/// Used as a one-way recursion gate: the outer call runs with `Allow`; when the
/// detector rewrites the line into canonical form `"<effect> as long as <cond>"`
/// and re-invokes `parse_static_line_inner`, it passes `Skip` so the detector
/// cannot re-enter. Any call path that does not originate from the inverted-form
/// rewrite uses `Allow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InvertedAsLongAs {
    Allow,
    Skip,
}

/// Single authority for recognizing the speed-cap-lift sentence
/// "Your speed can increase beyond 4" (with or without the trailing period).
///
/// CR 702.179d–e: a player's speed normally tops out at 4 (the inherent
/// speed trigger stops at "less than 4", and "max speed" means exactly 4);
/// this static lifts that cap. The routing call sites in `oracle.rs` and the
/// semantic parse in [`parse_static_line_inner`] all delegate here — never
/// re-encode the phrase as a string literal elsewhere.
pub(crate) fn is_speed_unlock_sentence(lower: &str) -> bool {
    all_consuming(terminated(
        tag::<_, _, OracleError<'_>>("your speed can increase beyond 4"),
        opt(tag(".")),
    ))
    .parse(lower)
    .is_ok()
}

/// CR 305.2: static land-play permissions with an explicit additional-drop
/// count greater than the ordinary +1 grant. `u8::MAX` represents "any
/// number"; runtime summing saturates so it stays effectively unbounded when
/// combined with ordinary extra drops.
fn parse_static_additional_land_drop_count(input: &str) -> OracleResult<'_, u8> {
    all_consuming(terminated(
        preceded(
            (opt(tag("you may ")), tag("play ")),
            alt((
                value(u8::MAX, tag("any number of lands")),
                value(2, tag("two additional lands")),
            )),
        ),
        (
            opt((
                space1,
                alt((
                    tag("on each of your turns"),
                    tag("on each of their turns"),
                    tag("during each of your turns"),
                    tag("during each of their turns"),
                )),
            )),
            opt(tag(".")),
            space0,
        ),
    ))
    .parse(input)
}

/// CR 502.3: Trailing "during their untap step(s)" clause of the
/// max-untap restriction (Smoke / Damping Field / Winter Orb class). The
/// canonical printing uses the plural possessive "their untap steps", but the
/// apostrophe and the singular form are admitted so the combinator covers
/// reprints and the "during each player's untap step" wording without a flat
/// alt of full sentences.
fn parse_during_their_untap_step_suffix(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        all_consuming((
            space1,
            tag("during "),
            alt((
                tag("their"),
                tag("each player's"),
                tag("each player\u{2019}s"),
            )),
            space1,
            tag("untap step"),
            opt(tag("s")),
            opt(tag(".")),
            space0,
        )),
    )
    .parse(input)
}

fn parse_each_other_players_untap_step_suffix(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        all_consuming((
            space1,
            alt((
                tag("during each other player's untap step"),
                tag("during each other player\u{2019}s untap step"),
            )),
            opt(tag(".")),
            space0,
        )),
    )
    .parse(input)
}

#[derive(Clone, Copy)]
enum AllPlayerStepSkipSubject {
    Players,
    EachPlayer,
}

fn parse_all_player_step_skip_subject(input: &str) -> OracleResult<'_, AllPlayerStepSkipSubject> {
    alt((
        value(AllPlayerStepSkipSubject::Players, tag("players")),
        value(AllPlayerStepSkipSubject::EachPlayer, tag("each player")),
    ))
    .parse(input)
}

fn parse_all_player_step_skip_verb(
    subject: AllPlayerStepSkipSubject,
    input: &str,
) -> OracleResult<'_, ()> {
    match subject {
        AllPlayerStepSkipSubject::Players => value((), tag("skip")).parse(input),
        AllPlayerStepSkipSubject::EachPlayer => value((), tag("skips")).parse(input),
    }
}

fn parse_all_player_skip_step(input: &str) -> OracleResult<'_, Phase> {
    let (input, subject) = parse_all_player_step_skip_subject(input)?;
    let (input, _) = space1.parse(input)?;
    let (input, _) = parse_all_player_step_skip_verb(subject, input)?;
    let (input, _) = space1.parse(input)?;
    let (input, _) = tag("their").parse(input)?;
    let (input, _) = space1.parse(input)?;
    let (input, step) = parse_step_name_nom(input)?;
    let (input, _) = opt(tag("s")).parse(input)?;
    Ok((input, step))
}

fn parse_skip_step_static(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    // CR 614.1b + CR 614.10: continuous static replacement effects that
    // replace a named step with nothing. Keep the subject axis explicit so
    // controller-scoped "your" and all-player "players/their" text share the
    // same StaticMode without over-broadening either one.
    let (_, (affected, step)) = all_consuming(terminated(
        alt((
            map(
                preceded(
                    tag::<_, _, OracleError<'_>>("skip your "),
                    parse_step_name_nom,
                ),
                |step| (TargetFilter::Controller, step),
            ),
            map(parse_all_player_skip_step, |step| {
                (TargetFilter::Player, step)
            }),
        )),
        opt(tag(".")),
    ))
    .parse(tp.lower)
    .ok()?;

    Some(
        StaticDefinition::new(StaticMode::SkipStep { step })
            .affected(affected)
            .description(text.to_string()),
    )
}

#[derive(Clone, Copy)]
enum RevealHandSubject {
    Opponents,
    Players,
    AllPlayers,
    EachPlayer,
    Controller,
}

fn parse_reveal_hand_subject(input: &str) -> OracleResult<'_, RevealHandSubject> {
    alt((
        value(RevealHandSubject::Opponents, tag("your opponents")),
        value(RevealHandSubject::AllPlayers, tag("all players")),
        value(RevealHandSubject::Players, tag("players")),
        value(RevealHandSubject::EachPlayer, tag("each player")),
        map(opt(tag("you")), |_| RevealHandSubject::Controller),
    ))
    .parse(input)
}

fn parse_reveal_hand_verb(subject: RevealHandSubject, input: &str) -> OracleResult<'_, ()> {
    match subject {
        RevealHandSubject::Opponents
        | RevealHandSubject::Players
        | RevealHandSubject::AllPlayers
        | RevealHandSubject::Controller => value((), tag("play with")).parse(input),
        RevealHandSubject::EachPlayer => value((), tag("plays with")).parse(input),
    }
}

fn parse_reveal_hand_possessive(subject: RevealHandSubject, input: &str) -> OracleResult<'_, ()> {
    match subject {
        RevealHandSubject::Controller => value((), tag("your")).parse(input),
        RevealHandSubject::Opponents
        | RevealHandSubject::Players
        | RevealHandSubject::AllPlayers
        | RevealHandSubject::EachPlayer => value((), tag("their")).parse(input),
    }
}

fn reveal_hand_scope(subject: RevealHandSubject) -> ProhibitionScope {
    match subject {
        RevealHandSubject::Opponents => ProhibitionScope::Opponents,
        RevealHandSubject::Players
        | RevealHandSubject::AllPlayers
        | RevealHandSubject::EachPlayer => ProhibitionScope::AllPlayers,
        RevealHandSubject::Controller => ProhibitionScope::Controller,
    }
}

fn parse_reveal_hand_scope(input: &str) -> OracleResult<'_, ProhibitionScope> {
    let (input, subject) = parse_reveal_hand_subject(input)?;
    let (input, _) = space0(input)?;
    let (input, _) = parse_reveal_hand_verb(subject, input)?;
    let (input, _) = space1(input)?;
    let (input, _) = parse_reveal_hand_possessive(subject, input)?;
    let (input, _) = space1(input)?;
    let (input, _) = alt((tag("hands"), tag("hand"))).parse(input)?;
    let (input, _) = space1(input)?;
    let (input, _) = tag("revealed").parse(input)?;
    Ok((input, reveal_hand_scope(subject)))
}

fn parse_reveal_hand_static(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    let (_, who) = all_consuming(terminated(parse_reveal_hand_scope, (opt(tag(".")), space0)))
        .parse(tp.lower)
        .ok()?;

    Some(
        StaticDefinition::new(StaticMode::RevealHand { who })
            .affected(TargetFilter::SelfRef)
            .description(text.to_string()),
    )
}

/// CR 708.5: Single authority for the "[you may ]look at face-down [permanents]
/// [you don't control | your opponents control] any time" permission phrase.
/// Strips an optional `"you may "` prefix (the static-line surface form carries
/// it; the activated-ability effect form arrives already peeled of "you may " by
/// the clause shell) and the required `"look at "` verb, parses the subject via
/// the shared `parse_target` (so both controller-scope wordings route through one
/// handler), and returns the affected filter only when it carries
/// `FilterProp::FaceDown` and the trailing clause is exactly `"any time"`. Shared
/// by the static-line builder (Found Footage's continuous permission) and the
/// activated-ability effect intercept (Lumbering Laundry's `Until end of turn`
/// grant), so the two surface forms never re-encode the grammar independently.
pub(crate) fn parse_may_look_at_face_down_filter(
    original: &str,
    lower: &str,
) -> Option<TargetFilter> {
    let tp = TextPair::new(original, lower);
    // "you may " is optional: the static line keeps it ("You may look at …"),
    // while the activated-ability clause shell already peeled it.
    let tp = nom_tag_tp(&tp, "you may ").unwrap_or(tp);
    let rest = nom_tag_tp(&tp, "look at ")?;
    // `parse_target` consumes the original-cased subject phrase that the
    // lowercase tag matched past.
    let (filter, remainder) = parse_target(rest.original);
    // The subject must carry the face-down property; "any time" is the only
    // permitted trailing clause for this permission.
    let has_face_down = matches!(
        &filter,
        TargetFilter::Typed(t) if t.properties.iter().any(|p| matches!(p, FilterProp::FaceDown))
    );
    if !has_face_down {
        return None;
    }
    let tail = remainder.trim().trim_end_matches('.').trim();
    if !tail.eq_ignore_ascii_case("any time") {
        return None;
    }
    Some(filter)
}

/// CR 708.5: "You may look at face-down creatures [you don't control | your
/// opponents control] any time." (Found Footage). Builds a
/// `StaticMode::MayLookAtFaceDown` whose `affected` filter is the subject phrase
/// (carrying `FilterProp::FaceDown` plus the controller scope), parsed via the
/// shared [`parse_may_look_at_face_down_filter`] so both scope wordings — and
/// the activated-ability duration-bound form — route through one handler.
fn parse_may_look_at_face_down_static(tp: &TextPair<'_>) -> Option<StaticDefinition> {
    let filter = parse_may_look_at_face_down_filter(tp.original, tp.lower)?;
    Some(
        StaticDefinition::new(StaticMode::MayLookAtFaceDown)
            .affected(filter)
            .description(tp.original.to_string()),
    )
}

/// CR 116.2b + CR 708.7: "[subject] can't be turned face up [during your turn]."
/// (Karlov Watchdog). Builds a `StaticMode::CantBeTurnedFaceUp` whose `affected`
/// filter is the subject phrase and whose optional timing rides on `condition`.
/// The subject is parsed with `parse_target` so any permanent-scope wording
/// ("permanents your opponents control", "creatures you control", `~`) is
/// covered, not just one card.
fn parse_cant_be_turned_face_up_static(tp: &TextPair<'_>) -> Option<StaticDefinition> {
    // Split the line on the prohibition predicate via a nom combinator. The
    // combinator consumes "<subject> can't be turned face up" and yields the
    // subject's byte length; the remainder (original case) is the timing window.
    let (subject_len, tail_original) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, subject) =
            take_until::<_, _, OracleError<'_>>("can't be turned face up").parse(i)?;
        let subject_len = subject.len();
        let (i, _) = tag("can't be turned face up").parse(i)?;
        Ok((i, subject_len))
    })?;
    let subject = tp.original.get(..subject_len)?.trim();
    let affected = if subject.is_empty() {
        TargetFilter::SelfRef
    } else {
        let (filter, remainder) = parse_target(subject);
        if matches!(filter, TargetFilter::None) || !remainder.trim().is_empty() {
            return None;
        }
        filter
    };

    // Trailing timing window after the predicate. Only "during your turn" is
    // modeled today; a bare prohibition (no timing) leaves `condition` None.
    let tail = tail_original
        .trim()
        .trim_end_matches('.')
        .trim()
        .to_ascii_lowercase();
    let mut def = StaticDefinition::new(StaticMode::CantBeTurnedFaceUp)
        .affected(affected)
        .description(tp.original.to_string());
    if tail.is_empty() {
        // Unconditional prohibition (no timing window).
    } else if tail == "during your turn" {
        def = def.condition(StaticCondition::DuringYourTurn);
    } else {
        // An unmodeled timing window — fail rather than silently drop it.
        return None;
    }
    Some(def)
}

/// CR 514.2: "Damage isn't removed from [subject] during cleanup steps."
/// Builds a `StaticMode::DamageNotRemovedDuringCleanup` static whose `affected`
/// filter is the subject — `~`/`this creature`/`this permanent` map to SelfRef
/// (Ancient Adamantoise, Uthgardt Fury), and any other subject ("creatures",
/// "creatures your opponents control") is parsed as a typed filter (Patient
/// Zero, Case of the Market Melee). The cleanup turn-based action skips removing
/// damage from permanents matching an active such static.
pub(crate) fn parse_damage_not_removed_during_cleanup(
    tp: &TextPair,
    text: &str,
) -> Option<StaticDefinition> {
    // Composed grammar (CR 514.2):
    //   "damage isn't removed from " <subject> " during cleanup steps" [.] EOF
    // where <subject> is a self-reference or a type phrase. The cleanup-step
    // suffix is anchored at end-of-sentence (nothing but an optional period may
    // follow), so a sentence that merely mentions "cleanup" later is rejected
    // and the subject must parse to completion.
    let body = nom_tag_lower(tp.lower, tp.lower, "damage isn't removed from ")?;

    let (affected, after_subject) = if let Some(rest) = nom_tag_lower(body, body, "~")
        .or_else(|| nom_tag_lower(body, body, "this creature"))
        .or_else(|| nom_tag_lower(body, body, "this permanent"))
    {
        (TargetFilter::SelfRef, rest)
    } else {
        let (filter, rest) = parse_type_phrase(body);
        if matches!(&filter, TargetFilter::Any) {
            return None;
        }
        (filter, rest)
    };

    let tail = nom_tag_lower(after_subject, after_subject, " during cleanup steps")?;
    if !tail.trim_end_matches('.').trim().is_empty() {
        return None;
    }

    Some(
        StaticDefinition::new(StaticMode::DamageNotRemovedDuringCleanup)
            .affected(affected)
            .description(text.to_string()),
    )
}

/// CR 509.1b: "Creatures with power <comparison> <quantity> can't
/// block this creature." — a can't-be-blocked-by restriction whose blocker
/// filter gates on a power threshold that may be DYNAMIC (Kraken of the Straits:
/// "Creatures with power less than the number of Islands you control can't block
/// this creature."). Sibling of `parse_source_power_block_restriction` (which
/// fixes the threshold to `~'s power` and targets `creatures you control`); this
/// arm accepts any `parse_target` power-comparison filter — including a dynamic
/// `ObjectCount` threshold — and targets the source itself. Without it the
/// subject-first "creatures with power … can't block this creature" wording
/// mis-dispatches to a bare `CantBlock { SelfRef }` (source can't block), which
/// is the inverse of the intended restriction.
fn parse_power_threshold_block_restriction(text: &str) -> Option<StaticDefinition> {
    // allow-noncombinator: split on the fixed clause anchor, not parsing dispatch.
    let (filter_text, after) = text.split_once(" can't block ")?;
    // CR 509.1b: the restriction is on blocking the SOURCE ("this creature"/"~").
    let after = after.trim().trim_end_matches('.').trim().to_lowercase();
    if after != "this creature" && after != "~" {
        return None;
    }
    // Reuse the shared filter grammar so the power comparison + dynamic threshold
    // ("less than the number of Islands you control") lower through one authority.
    let (filter, remainder) = parse_target(filter_text.trim());
    if !remainder.trim().is_empty()
        || matches!(filter, TargetFilter::Any)
        || !target_filter_has_power_comparison(&filter)
    {
        return None;
    }
    Some(
        StaticDefinition::new(StaticMode::CantBeBlockedBy { filter })
            .affected(TargetFilter::SelfRef)
            .description(text.to_string()),
    )
}

fn target_filter_has_power_comparison(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed
            .properties
            .iter()
            .any(filter_prop_has_power_comparison),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(target_filter_has_power_comparison)
        }
        TargetFilter::Not { filter } => target_filter_has_power_comparison(filter),
        _ => false,
    }
}

fn filter_prop_has_power_comparison(prop: &FilterProp) -> bool {
    match prop {
        FilterProp::PtComparison {
            stat: PtStat::Power,
            ..
        } => true,
        FilterProp::AnyOf { props } => props.iter().any(filter_prop_has_power_comparison),
        _ => false,
    }
}

pub(crate) fn parse_static_line_inner(
    text: &str,
    inverted: InvertedAsLongAs,
) -> Option<StaticDefinition> {
    let raw_lower = text.to_lowercase();
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();
    let tp = TextPair::new(&text, &lower);

    if let Some(def) = parse_arcane_adaptation_chosen_type_static(&tp, &text) {
        return Some(def);
    }
    // CR 305.6 + CR 607.2d: land-axis counterpart — "Lands you control are the
    // chosen type in addition to their other types" (Realmwright).
    if let Some(def) = parse_chosen_land_type_static(&tp, &text) {
        return Some(def);
    }
    // CR 514.2: "Damage isn't removed from [subject] during cleanup steps."
    if let Some(def) = parse_damage_not_removed_during_cleanup(&tp, &text) {
        return Some(def);
    }
    // CR 101.2 + CR 109.5: "Each opponent who [did X] this turn can't [Y]" —
    // per-affected-player conditional prohibition (Angelic Arbiter). Must run
    // BEFORE the generic "can't attack" arm and the `parse_cant_cast_type_spells`
    // dispatch so the per-player predicate is preserved and the attack clause is
    // not misparsed as a SelfRef restriction.
    if let Some(def) = parse_per_player_conditional_prohibition(&tp, &text) {
        return Some(def);
    }
    if let Some(def) = parse_every_creature_type_static(&tp, &text) {
        return Some(def);
    }
    if let Some(def) = parse_collection_counter_play_permission_static(&tp, &text) {
        return Some(def);
    }

    // CR 601.2f + CR 118.8: Static-imposed additional non-mana costs must dispatch
    // before generic cost-mod and restriction arms that share "cost"/"spells" tokens.
    // Use word-boundary scans only on phrases that start a token; numeric life amounts
    // sit immediately before "life" without a leading space ("3 life to cast").
    if nom_primitives::scan_contains(tp.lower, "cost an additional")
        && nom_primitives::scan_contains(tp.lower, "life to cast")
    {
        if let Some(def) = try_parse_impose_additional_cost(&text, &lower) {
            return Some(def);
        }
    }

    if let Some(mode) = parse_max_combat_creatures_static(&lower) {
        return Some(StaticDefinition::new(mode).description(text.to_string()));
    }

    // CR 508.1c: The directional attack restriction (Pramikon, Sky Rampart;
    // Mystic Barrier; Teyo, Geometric Tactician).
    if let Some(mode) = parse_attack_only_neighbor_static(&lower) {
        return Some(StaticDefinition::new(mode).description(text.to_string()));
    }

    if let Some(defs) = parse_cost_payment_prohibition_statics(&tp, &text) {
        return defs.into_iter().next();
    }

    if let Some(def) = parse_loyalty_activation_timing_permission(&tp, &text) {
        return Some(def);
    }

    // CR 510.1c: Attached-object conditional variants must precede the generic
    // inverted "As long as ..." rewrite so the condition binds to the
    // enchanted/equipped creature rather than becoming an unrecognized SelfRef
    // condition.
    if let Some(def) = parse_attached_assigns_damage_from_toughness(&tp, &text) {
        return Some(def);
    }

    if let Some(def) = parse_soulbond_paired_static(&tp, &text) {
        return Some(def);
    }

    // CR 509.1b + CR 609.4 + CR 702.14c + CR 702.14d: "Creatures with <X>walk can
    // be blocked as though they didn't have <X>walk." Global landwalk-restriction
    // canceller (Ur-Drago class). Must run before the inverted "As long as" rewrite
    // so the full literal sentence is detected before any structural rewriting.
    if let Some(def) = try_parse_ignore_landwalk_for_blocking(&tp, &text) {
        return Some(def);
    }

    // CR 509.1b + CR 609.4 + CR 702.28b: "<subject> can block creatures with shadow
    // as though they didn't have shadow" / "... as though it had shadow" — per-source
    // permission to block shadow attackers (Heartwood Dryad, Wall of Diffusion).
    if let Some(def) = parse_block_shadow_as_though(&tp, &text) {
        return Some(def);
    }

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
            // CR 400.2 + CR 701.20a: "As long as <condition>, all players
            // play with their hands revealed." The generic continuous fallback
            // can otherwise accept the canonical rewrite before this data-
            // carrying static sees the isolated effect clause.
            {
                let effect_lower = split.effect_text.to_lowercase();
                let tp_effect = TextPair::new(&split.effect_text, &effect_lower);
                if let Some(mut def) = parse_reveal_hand_static(&tp_effect, &split.effect_text) {
                    let condition = parse_static_condition(&split.condition_text).unwrap_or(
                        StaticCondition::Unrecognized {
                            text: split.condition_text.clone(),
                        },
                    );
                    def.condition = Some(condition);
                    def.description = Some(text.to_string());
                    return Some(def);
                }
            }
            if let Some(mut def) = parse_static_line_inner(&split.canonical, InvertedAsLongAs::Skip)
            {
                // CR 611.3a: the split stripped the "as long as <condition>" gate
                // from the canonical rewrite, so the recursed effect parser (e.g.
                // DoubleTriggers, which carries no condition of its own) never sees
                // it. Re-attach the split condition whenever the recursed def
                // didn't derive one itself — this restores the gate for the whole
                // class of split inverted-as-long-as statics, not just Cloud.
                if def.condition.is_none() {
                    if let Some(condition) = parse_static_condition(&split.condition_text) {
                        def.condition = Some(condition);
                    }
                }
                return Some(def.description(text.to_string()));
            }
            // CR 601.3b + CR 702.8a: Inverted flash-grant conditional:
            // "As long as X, you may cast [type] spells as though they had flash."
            // The recursed call above fails because `parse_cast_as_though_flash_static`
            // uses `eof` and the canonical form carries a trailing condition clause.
            // Try the flash parser against the isolated effect slice; if it succeeds,
            // attach the condition from the split.
            {
                let effect_lower = split.effect_text.to_lowercase();
                let tp_effect = TextPair::new(&split.effect_text, &effect_lower);
                if let Some(mut def) =
                    parse_cast_as_though_flash_static(&tp_effect, &split.effect_text)
                {
                    let condition = parse_static_condition(&split.condition_text).unwrap_or(
                        StaticCondition::Unrecognized {
                            text: split.condition_text.clone(),
                        },
                    );
                    def.condition = Some(condition);
                    def.description = Some(text.to_string());
                    return Some(def);
                }
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

    // --- "[Type] spells you cast [from zone] have [keyword]" (CR 702.51a) ---
    // Dispatch before generic "has/have" continuous parsing; spell keyword
    // grants function during casting, not as battlefield continuous grants.
    if let Some(def) = parse_spells_have_keyword(&tp, &text) {
        return Some(def);
    }

    if is_speed_unlock_sentence(tp.lower) {
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
        // The optional comma after "while voting" is a single `opt` axis rather
        // than two flat full-sentence permutations (CLAUDE.md: compose
        // combinators, don't enumerate permutations).
        let res: nom::IResult<&str, (), OracleError<'_>> = nom::combinator::value(
            (),
            (
                nom::bytes::complete::tag("while voting"),
                nom::combinator::opt(nom::bytes::complete::tag(",")),
                nom::bytes::complete::tag(" you may vote an additional time"),
            ),
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

    // CR 701.55c: "If an opponent would face a villainous choice, they face that
    // choice an additional time." (The Valeyard) — an extra-instance replacement
    // static, the structural twin of `GrantsExtraVote` (CR 701.38d). `affected`
    // is `Player` (mirroring `GrantsExtraVote`); the opponent-of-the-facing-
    // player scoping is owned by the resolver
    // (`choose_one_of::villainous_extra_instances_for`), which counts only
    // sources controlled by an opponent of the facing player — `affected` here is
    // a coverage/semantic marker, not the scope authority. Reminder text "(They
    // can make the same or different choices.)" is already stripped above by
    // `strip_reminder_text`. Comma/no-comma variants are alt arms, not flat
    // The optional comma after "choice" is a single `opt` axis rather than two
    // flat full-sentence permutations (CLAUDE.md: compose combinators, don't
    // enumerate permutations).
    {
        let lower_trim = tp.lower.trim_end_matches('.').trim();
        let res: nom::IResult<&str, (), OracleError<'_>> = nom::combinator::value(
            (),
            (
                nom::bytes::complete::tag("if an opponent would face a villainous choice"),
                nom::combinator::opt(nom::bytes::complete::tag(",")),
                nom::bytes::complete::tag(" they face that choice an additional time"),
            ),
        )
        .parse(lower_trim);
        if res.is_ok() {
            return Some(
                StaticDefinition::new(StaticMode::GrantsExtraVillainousChoice)
                    .affected(TargetFilter::Player)
                    .description(text.to_string()),
            );
        }
    }

    // CR 702.170f: "You may plot [filter] cards from the top of your library."
    // Plot-from-library permission (Fblthp, Lost on the Range). Dispatched
    // BEFORE the cast-permission arm so plot lines are claimed by the plot
    // parser. The cast arm anchors on "you may play"/"you may cast" while this
    // anchors on "you may plot", so there is no real collision — ordering
    // documents intent and guards against future drift. Plot is a CR 702.170
    // special action (Library → Exile, later Exile → Stack), categorically
    // distinct from the cast permission's CR 601.2a Library → Stack cast.
    if let Some(result) = try_parse_top_of_library_plot_permission(&text, &lower) {
        return Some(result);
    }

    // CR 702.170f + CR 702.170a: "The top card of your library has plot[. The
    // plot cost is equal to its mana cost]." Mechanic-establishing plot grant
    // for the top library card (Fblthp). Also claimed ahead of the cast arm.
    if let Some(result) = try_parse_top_of_library_has_plot(&text, &lower) {
        return Some(result);
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

    // CR 122.2 + CR 113.6b: "Counters remain on ~ as it moves to any zone other
    // than [zone list]." Counter-persistence override (Me, the Immortal;
    // Skullbriar, the Walking Grave).
    if let Some(result) = try_parse_counters_persist_across_zones(&text, &lower) {
        return Some(result);
    }

    // CR 601.2a + CR 113.6b + CR 118.9: "Once each turn, you may cast [filter]
    // from among cards exiled with ~ this turn [without paying its mana cost]."
    // Maralen, Fae Ascendant is the type specimen; the handler accepts the
    // wider class (any frequency, any mana-value comparator) so future
    // printings slot in without parser changes.
    if let Some(result) = try_parse_exile_cast_permission(&text, &lower) {
        return Some(result);
    }

    // CR 113.6b + CR 305.1 + CR 406.6 + CR 117.1c: Persistent, name-anchored
    // exile-play permission — "[During your turn, ]you may play lands and cast
    // spells from among cards exiled with ~." (The Matrix of Time) and the
    // "you may look at cards exiled with ~, and you may play lands and cast
    // spells from among those cards." variant (Prosper/Tibalt impulse-commander
    // class). Lowers to `ExileCastPermission { pool: Persistent, play_mode:
    // Play, frequency: Unlimited }` reading the lifetime `exile_links` set,
    // distinct from the Maralen "this turn" rolling-pool handler above.
    if let Some(result) = try_parse_persistent_exile_play_permission(&text, &lower) {
        return Some(result);
    }

    // CR 601.2b + CR 118.9a + CR 601.2: Omniscience-class restricted free-cast
    // static. Optional " from your hand" zone qualifier — Dracogenesis's
    // "you may cast Dragon spells without paying their mana costs" relies on
    // CR 601.2's implicit hand zone.
    if let Some(result) = try_parse_cast_free_permission(&text, &lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_retain_unspent_mana_static(&text, &lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_transform_unspent_mana_static(&text, &lower) {
        return Some(result);
    }

    // CR 609.4b: "You may spend mana as though it were mana of any color to
    // activate abilities of <subject>." (Agatha's Soul Cauldron / Joiner Adept).
    if let Some(def) = try_parse_spend_any_color_to_activate_abilities(&text, &tp) {
        return Some(def);
    }

    // CR 609.4b: "You may spend mana as though it were mana of any color."
    if tp.lower.trim_end_matches('.') == "you may spend mana as though it were mana of any color" {
        return Some(
            StaticDefinition::new(StaticMode::SpendManaAsAnyColor {
                spell_filter: None,
                activation_source_filter: None,
            })
            .affected(TargetFilter::Player)
            .description(text.to_string()),
        );
    }

    // CR 609.4b: Spell-class-filtered any-type-mana spend —
    // "You may/can spend mana of any type to cast <spell-filter> spells."
    // (Vizier of the Menagerie: "creature spells"). Scoped to the matching
    // spell class via `spell_filter`, so off-color mana never helps a spell
    // outside the class.
    if let Some(def) = try_parse_filtered_spend_any_type_to_cast(&text, tp.lower) {
        return Some(def);
    }

    // CR 107.4f: K'rrik-class life-for-color payment substitution —
    // "For each {C} in a cost, you may pay 2 life rather than pay that mana."
    // Combinator parses `{C}` directly from the original text (mana symbols are
    // case-preserved in Oracle text); lowercase tail matching on the rest of
    // the sentence is fine because Oracle text outside the braces is normalized.
    if let Some(def) = parse_pay_life_as_colored_mana(&text) {
        return Some(def);
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

    // --- "Players can't untap more than one <type> during their untap steps." ---
    // CR 502.3: Smoke / Damping Field / Imi Statue / Stoic Angel / Winter Orb
    // class — a global cap on the untap turn-based action. The count and the
    // permanent-type filter are parsed compositionally (number combinator +
    // `parse_type_phrase`), so one branch covers every cap N and every
    // permanent type (creature, artifact, nonbasic land, …). The cap rides
    // inline on `StaticMode::MaxUntapPerType` because the restriction applies
    // to whoever is the active player, not the source's controller.
    if let Some(rest) = nom_tag_tp(&tp, "players can't untap more than ") {
        // The cap count and the type filter are both case-insensitive
        // (`parse_type_phrase` lowercases internally and produces a typed,
        // name-free filter), so the lowercase remainder is the canonical input
        // for the whole tail — no original-case offset arithmetic needed.
        if let Ok((after_count, count)) = nom_primitives::parse_number(rest.lower) {
            let (filter, remainder) = parse_type_phrase(after_count.trim_start());
            // `remainder` is already lowercase, so the suffix combinator runs
            // directly without the `nom_on_lower` case bridge.
            let suffix_ok = parse_during_their_untap_step_suffix(remainder).is_ok();
            // Require a real permanent-type filter (parse_type_phrase returns a
            // typed filter for "creature"/"artifact"/"nonbasic land"); a bare
            // generic permanent with no narrowing would over-broaden.
            let has_type = matches!(&filter, TargetFilter::Typed(_));
            if suffix_ok && has_type {
                return Some(
                    StaticDefinition::new(StaticMode::MaxUntapPerType { filter, max: count })
                        .description(text.to_string()),
                );
            }
        }
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
        let during_ok = nom_on_lower(
            remainder,
            &remainder_lower,
            parse_each_other_players_untap_step_suffix,
        )
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

    // --- "Untap this <permanent> during each other player's untap step." ---
    // CR 502.3 + CR 113.6: the self-referential Seedborn-class variant (Bender's
    // Waterskin: "Untap this artifact during each other player's untap step").
    // Shares the runtime of the "untap all" form
    // (`StaticMode::UntapsDuringEachOtherPlayersUntapStep`), but the affected
    // filter is the source itself (`SelfRef`) so its controller untaps only it
    // during every other player's untap step. Ordered after the "untap all" arm
    // — the typed "you control" subject and these self-reference subjects are
    // disjoint, so neither shadows the other.
    if let Some(rest) = nom_tag_tp(&tp, "untap ") {
        let self_subject =
            nom_on_lower(rest.original, rest.lower, nom_target::parse_self_reference);
        if let Some((TargetFilter::SelfRef, remainder)) = self_subject {
            let remainder_lower = remainder.to_lowercase();
            let during_ok = nom_on_lower(
                remainder,
                &remainder_lower,
                parse_each_other_players_untap_step_suffix,
            )
            .is_some();
            if during_ok {
                return Some(
                    StaticDefinition::new(StaticMode::UntapsDuringEachOtherPlayersUntapStep)
                        .affected(TargetFilter::SelfRef)
                        .description(text.to_string()),
                );
            }
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

    // --- "Your opponents/Players play with their hands revealed" ---
    // CR 400.2 + CR 701.20a: continuous effect making hand cards public.
    if let Some(def) = parse_reveal_hand_static(&tp, &text) {
        return Some(def);
    }

    // --- "Skip your [step] step" / "Players skip their [step] steps" ---
    if let Some(def) = parse_skip_step_static(&tp, &text) {
        return Some(def);
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

    // CR 205.1a + CR 613.1f: Imprisoned-in-the-Moon — "Enchanted <subject> is a
    // colorless [<subtype>...] <type> with "<ability>" and loses all other card
    // types and abilities." Must precede parse_enchanted_is_type, whose base-P/T
    // split does not model the with-"<ability>" clause (issue #4770).
    if let Some(def) = parse_enchanted_becomes_type_with_ability(&tp, &text) {
        return Some(def);
    }
    // CR 205.1a + CR 702.6: "Each <subject> is an Equipment with equip {N} and
    // "<ability>"" — the become-Equipment anthem (Bram, Bludgeon Brawl). Grants
    // the Equipment subtype + Equip keyword + the quoted static ability.
    if let Some(def) = parse_becomes_equipment_with_ability(&tp, &text) {
        return Some(def);
    }
    // CR 613.1d + CR 205.1a: "Enchanted [permanent-type] is a [type] [with base P/T N/N]
    // [in addition to its other types]" — type-changing aura effects.
    // Must come before the basic-land-type handler which is a subset of this pattern.
    if let Some(def) = parse_enchanted_is_type(&tp, &text) {
        return Some(def);
    }

    // CR 613.1d (Layer 4) + CR 205.1b: "[Enchanted|Equipped] <subject> isn't a
    // <type> and is a <type> in addition to its other types" — attached-permanent
    // type SWAP (Luxior: equipped planeswalker loses Planeswalker, gains
    // Creature). Placed after `parse_enchanted_is_type` (whose "is a" copula
    // parse rejects the "isn't a ..." lead) and before the generic
    // enchanted/equipped predicate arms so the type-removal clause is preserved.
    if let Some(def) = parse_attached_isnt_and_is_type(&tp, &text) {
        return Some(def);
    }

    // --- "Enchanted creature gets +N/+M" or "has {keyword}" ---
    if let Some(rest) = nom_tag_tp(&tp, "enchanted creature ") {
        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]));
        if let Some(def) = parse_enchanted_equipped_predicate(rest.original, filter, &text)
            .into_iter()
            .next()
        {
            return Some(def);
        }
    }

    // --- "Enchanted permanent gets/has ..." ---
    if let Some(rest) = nom_tag_tp(&tp, "enchanted permanent ") {
        let filter =
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy]));
        if let Some(def) = parse_enchanted_equipped_predicate(rest.original, filter, &text)
            .into_iter()
            .next()
        {
            return Some(def);
        }
    }

    // CR 305.7 + CR 305.6: "Enchanted land is the chosen type" — Aura sets the
    // enchanted land's subtype to the basic land type chosen as the Aura entered.
    if let Some(def) = parse_enchanted_land_chosen_type_static(&tp, &text) {
        return Some(def);
    }

    // CR 305.7: "Enchanted land is a [type]" — must be before general "enchanted land" handler.
    if let Some(rest) = nom_tag_tp(&tp, "enchanted land is a ") {
        let rest = rest.trim_end_matches('.');
        // "in addition to its other types" → AddSubtype (not replacement)
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        if let Some(land_name) = rest.strip_suffix(" in addition to its other types") {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
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
        if let Some(def) = parse_enchanted_equipped_predicate(rest.original, filter, &text)
            .into_iter()
            .next()
        {
            return Some(def);
        }
    }

    // --- "Equipped creature gets +N/+M" ---
    if let Some(rest) = nom_tag_tp(&tp, "equipped creature ") {
        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy]));
        if let Some(def) = parse_enchanted_equipped_predicate(rest.original, filter, &text)
            .into_iter()
            .next()
        {
            return Some(def);
        }
    }

    // CR 508.1b: "All creatures attacking you <predicate>" — filter scoped to attackers
    // whose defending player is the source's controller. Must precede the generic
    // "all creatures " branch below since that would otherwise consume the prefix
    // and leave "attacking you <predicate>" as input to `parse_continuous_gets_has`,
    // which expects a verb ("gets"/"has"/"is"), not a subject continuation.
    if let Some(rest) = nom_tag_tp(&tp, "all creatures attacking you ") {
        let filter =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Attacking {
                    defender: Some(ControllerRef::You),
                }]),
            );
        if let Some(def) = parse_continuous_gets_has(rest.original, filter, &text) {
            return Some(def);
        }
    }

    // CR 508.1b: "Creatures attacking you <predicate>" — same defender scope as
    // the "all creatures" form above (Boarded Window, Watchdog-class statics
    // without the quantifier).
    if let Some(rest) = nom_tag_tp(&tp, "creatures attacking you ") {
        let filter =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Attacking {
                    defender: Some(ControllerRef::You),
                }]),
            );
        if let Some(def) = parse_continuous_gets_has(rest.original, filter, &text) {
            return Some(def);
        }
    }

    // CR 508.1b: "Creatures attacking your opponents [and/or planeswalkers they
    // control] have/get ..." — attackers whose defending player is an opponent
    // of the source's controller (Blast-Furnace Hellkite, Neyali).
    if let Some(rest) = nom_tag_tp(
        &tp,
        "creatures attacking your opponents and/or planeswalkers they control ",
    )
    .or_else(|| nom_tag_tp(&tp, "creatures attacking your opponents "))
    {
        let filter =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Attacking {
                    defender: Some(ControllerRef::Opponent),
                }]),
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

    // CR 205.4b + CR 613.1d (Layer 4): "[subject] is/are [no longer] [supertype]"
    // — supertype sibling of the color path (Leyline of Singularity "All nonland
    // permanents are legendary", Melting "All lands are no longer snow"). The
    // supertype predicate (legendary/basic/snow) is disjoint from color and
    // land-type words, so this is order-safe here; it precedes `parse_land_type_change`
    // so "All lands are basic" (supertype) is not probed as a land-type line.
    if let Some(def) = parse_subject_is_supertype(&tp, &text) {
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

    // CR 602.5a: "[You may ]activate abilities of <subject> as though those
    // creatures had haste" — lifts the summoning-sickness gate on {T}/{Q}
    // activated abilities for a subject class (Tyvar, Jubilant Brawler).
    // Returns None when the phrase is absent or the subject is unresolved.
    if let Some(def) = parse_activate_abilities_as_though_haste(&tp, &text) {
        return Some(def);
    }

    // --- "Each creature you control [with condition] assigns combat damage equal to its toughness" ---
    // CR 510.1c: Doran-class effects that cause creatures to use toughness for combat damage.
    if let Some(rest_tp) = nom_tag_tp(&tp, "during your turn, ") {
        if let Some(def) = parse_assigns_damage_from_toughness(rest_tp.lower, rest_tp.original) {
            return Some(
                def.condition(StaticCondition::DuringYourTurn)
                    .description(text.to_string()),
            );
        }
    }
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

    if let Some(def) = parse_contextual_continuous_subject_static(&tp, &text) {
        return Some(def);
    }

    // CR 614.1c + CR 122.1: "[scope] creatures you control enter with an
    // additional +1/+1 counter on them." Continuous "enters with" replacement
    // static (Kalain, Bard Class, Gorma the Gullet, Master Chef). The verb here
    // is "enter", not "get"/"has", so it must dispatch BEFORE the anthem
    // "creatures you control ..." branches below (which route to
    // parse_continuous_gets_has and only recognize get/has verbs).
    if let Some(def) = parse_enters_with_additional_counters(&tp, &text) {
        return Some(def);
    }

    // --- "Creatures you control [with counter condition] get/have ..." ---
    // Must come BEFORE parse_typed_you_control to prevent core type words like
    // "Creatures" from falling through to the subtype path (A1 fix: 162+ cards).
    if let Some(rest_tp) = nom_tag_tp(&tp, "creatures you control ") {
        let after_prefix = rest_tp.original;
        let (filter, predicate_text) = if let Some((owned_prop, rest)) =
            strip_negated_ownership_qualifier(after_prefix)
        {
            // CR 108.3 + CR 109.4: "Creatures you control but don't own …"
            // (Laughing Jasper Flint). Preserve the negated-ownership axis the
            // controller-prefix arm would otherwise drop.
            (
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![owned_prop]),
                ),
                rest,
            )
        } else if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
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
            if let Some((filter, predicate_text)) =
                parse_creatures_you_control_that_clause(after_prefix, rest_tp.lower, false)
            {
                (filter, predicate_text)
            } else if let Some((prop, prop_rest_original)) = nom_on_lower(
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
        } else if let Some((filter, predicate_text)) = parse_qualified_creatures_you_control_suffix(
            "Creatures you control",
            after_prefix,
            rest_tp.lower,
        ) {
            (filter, predicate_text)
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
            if let Some((filter, predicate_text)) =
                parse_creatures_you_control_that_clause(after_prefix, rest_tp.lower, true)
            {
                (filter, predicate_text)
            } else if let Some((prop, prop_rest_original)) = nom_on_lower(
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
        } else if let Some((filter, predicate_text)) = parse_qualified_creatures_you_control_suffix(
            "Other creatures you control",
            after_prefix,
            rest_tp.lower,
        ) {
            (filter, predicate_text)
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

    // CR 611.3 + CR 613.1 + CR 613.4b: "All <X> and all <Y> are <predicate>" —
    // a compound-subject animation where one predicate applies to every object
    // matching either subject (Life and Limb). Must precede parse_land_animation
    // (which splits on "are" and would claim only the first subject with an
    // incomplete predicate); the " and all " conjunction + Or-subject guard keep
    // single-subject animation lines falling through to parse_land_animation.
    if let Some(def) = parse_compound_all_subjects_type_change(&tp, &text) {
        return Some(def);
    }

    // CR 611.3 + CR 205.1a + CR 613.4b: non-additive compound-subject animation
    // ("All Elves and all Goblins are 2/2 Zombie creatures") — replacement
    // subtype semantics via animation_modifications_with_replacement. Must follow
    // the additive compound handler so the CR 205.1b gate stays authoritative.
    if let Some(def) = parse_compound_all_subjects_type_replacement(&tp, &text) {
        return Some(def);
    }

    // CR 611.3 + CR 305.7: "All <X> and all <Y> are <basic land type>" — compound-
    // subject land type replacement/addition. Must follow the animation compound
    // handlers (creature-gated) and precede parse_land_animation /
    // parse_land_type_change, which only resolve single-subject land filters.
    if let Some(def) = parse_compound_all_subjects_land_type_change(&tp, &text) {
        return Some(def);
    }

    // CR 613.1d + CR 613.4b: "[Subject] lands are [P/T] creatures that are still
    // lands" — continuous land animation (Living Plane, Nature's Revolt). Must
    // come before parse_land_type_change: both split on "are", but the land
    // animation form carries a creature descriptor the type-change parser can't
    // claim. The "creature" guard lets land *type* lines fall through.
    if let Some(def) = parse_land_animation(&tp, &text) {
        return Some(def);
    }

    // CR 305.7: "[Subject] lands are [type]" — land type-changing statics.
    // Must come before parse_subject_continuous_static (which splits on "gets/has/gains"
    // verbs and would not match "are" predicates).
    if let Some(def) = parse_land_type_change(&tp, &text) {
        return Some(def);
    }

    // CR 702.73a + CR 205.3 + CR 604.3: "[Subject] {is|are} every creature
    // type" — sibling of the land type-change dispatcher for the
    // Changeling-class type grant. Self-reference subjects (`~`) lower to a
    // CDA that functions in all zones (Mistform Ultimus, Dr. Julius
    // Jumblemorph). Filter subjects ("Creatures you control are every
    // creature type" — Maskwood Nexus) are mostly handled upstream by the
    // `parse_continuous_gets_has` path via `parse_continuous_modifications`;
    // this is the residual dispatcher that catches the shapes those code
    // paths don't strip — primarily self-references.
    if let Some(def) = parse_all_creature_types_grant(&tp, &text) {
        return Some(def);
    }

    // CR 702.16k: Player-subject protection ("You have protection from <X>")
    // must be claimed before the permanent-subject continuous path, which would
    // otherwise grant the protection keyword to permanents you control instead
    // of to you (the player). Distinguishes the player subject from the
    // permanent subject — the permanent path is left untouched.
    if let Some(def) = parse_player_protection_static(&text, &lower) {
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
    // Distinguish creature type (Metallic Mimic / Roaming Throne) vs land-type forms.
    if let Ok((_, kind)) = parse_self_chosen_type_static(tp.lower) {
        let modification = ContinuousModification::AddChosenSubtype { kind };
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![modification])
                .description(text.to_string()),
        );
    }

    // CR 205.3 + CR 700.8: "~ is also a <subtype>(, <subtype>)*[, [and|or] <subtype>]"
    // Continuous self-static that adds creature subtypes to the source. Used by
    // party-tribal cards so the source counts itself toward the controller's
    // party (CR 700.8a) regardless of its printed subtypes.
    // Anchored on `~` so it cannot collide with attached-object grants
    // ("Enchanted land is a Mountain") which retain their dedicated path.
    if let Some(modifications) = try_parse_self_is_also_subtypes(&tp) {
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(modifications)
                .description(text.to_string()),
        );
    }

    // CR 604.3 + CR 604.3a + CR 105.2c + CR 613.1e: Self-scoped
    // characteristic-defining color line ("~ is colorless.",
    // "~ is white and blue."). CDAs function in all zones and define the
    // source object's own color characteristic.
    if let Some(def) = parse_self_subject_is_color_cda(&tp, &text) {
        return Some(def);
    }

    // CR 613.1e + CR 105.2 / CR 105.3: "[subject] is/are [color expression]" for
    // an ARBITRARY filter subject — Leyline of the Guildpact ("Each nonland
    // permanent you control is all colors"), Shimmerwilds Growth ("Enchanted land
    // is the chosen color"). Generalizes `parse_all_subject_are_color` (the "All
    // ..." quantifier) and adds the "the chosen color" reading. Dispatched AFTER
    // the specialized color branches so they keep ownership of their cases: the
    // "All ..." fast path (`parse_all_subject_are_color`, which routes
    // artifact/land subtypes through `typed_filter_for_subtype`) and the
    // self-referential color CDA (`parse_self_subject_is_color_cda`, which
    // marks `~ is colorless` characteristic-defining and declines raw card
    // names). This branch only claims the residual general-filter subjects those
    // two leave unparsed.
    if let Some(def) = parse_subject_is_color(&tp, &text) {
        return Some(def);
    }

    // --- CDA: "~'s power is equal to the number of card types among cards in all graveyards
    //     and its toughness is equal to that number plus 1" (Tarmogoyf) ---
    if let Some(def) = parse_cda_pt_equality(tp.lower, tp.original) {
        return Some(def);
    }

    if let Some(def) = parse_conditional_static(&text) {
        return Some(def);
    }

    if let Some(def) = parse_contextual_continuous_subject_static(&tp, &text) {
        return Some(def);
    }

    // --- "~ has [keyword] as long as ..." (must be before generic self-ref "has") ---
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    if let Some(has_pos) = tp.find(" has ") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        if let Some(cond_pos) = tp.find(" as long as ") {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
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

    // --- "<self> has <kw> if <cond>" single-pair conditional keyword grant ---
    // CR 613.1f + CR 611.3a + CR 702.11b: A self-referential keyword grant gated
    // on a typed source-state condition (Palladia-Mors, the Ruiner; Karakyk
    // Guardian: "has hexproof if it hasn't dealt damage yet"). The MULTI-pair list
    // (Multiclass Baldric) is owned by the attached-grant path
    // (`parse_conditional_keyword_list`'s `len() > 1` gate); the SELF single-pair
    // case has no other handler, so it is parsed here. `parse_self_reference`
    // consumes every self-subject form ("~", "this creature", "this permanent",
    // "it", ...) uniformly — including the "this creature" forms the generic
    // `has`/`gets` arm below excludes via its `scan_contains(subject, "creature")`
    // guard. Full consumption + the mandatory " if " in
    // `parse_conditional_keyword_list` mean it cannot steal "has flying as long
    // as", "has base power and toughness X", or "has all activated abilities".
    // Placed BEFORE the generic `has`/`gets` arm so it owns the `if`-gated form
    // first. Calls `parse_conditional_keyword_list` DIRECTLY so the multi-pair
    // gate there is untouched.
    if let Some((TargetFilter::SelfRef, after_subject)) =
        nom_on_lower(&text, &lower, nom_target::parse_self_reference)
    {
        if let Some(after_has) = nom_tag_lower(after_subject, after_subject, " has ") {
            let after_has_lower = after_has.to_lowercase();
            if let Ok((rest, pairs)) = parse_conditional_keyword_list(&after_has_lower) {
                if rest.trim().trim_end_matches('.').is_empty() && pairs.len() == 1 {
                    if let Some((keyword, condition)) = pairs.into_iter().next() {
                        return Some(
                            StaticDefinition::continuous()
                                .affected(TargetFilter::SelfRef)
                                .modifications(vec![ContinuousModification::AddKeyword { keyword }])
                                .condition(condition)
                                .description(text.to_string()),
                        );
                    }
                }
            }
        }
    }

    // --- "~ has/gets ..." (self-referential) ---
    // Match lines like "CARDNAME has deathtouch" or "CARDNAME gets +1/+1"
    if let Some(pos) = tp
        .find(" has ") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        .or_else(|| tp.find(" gets ")) // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        .or_else(|| tp.find(" get "))
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
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
    // a <type>." into canonical "~ isn't a <type> as long as <cond>"; both
    // orientations must produce non-empty modifications plus an attached
    // condition (CR 611.3a).
    //
    // The "isn't a <type>" type-removal modification must come from the
    // EFFECT clause. In the canonical inverted form "<effect> as long as
    // <condition>", an "isn't a" inside the condition (Animate Artifact's
    // "as long as enchanted artifact isn't a creature") is NOT the
    // modification — that card removes nothing and instead animates. Scope the
    // scan to the pre-condition slice so the condition body cannot drive it.
    let (effect_slice_tp, trailing_condition_tp) = match tp.split_around(" as long as ") {
        Some((before, after)) => (before, Some(after)),
        None => (tp, None),
    };
    if let Ok((_, (_, type_rest))) =
        nom_primitives::split_once_on(effect_slice_tp.lower, "isn't a ")
    {
        // type_rest is a suffix of effect_slice_tp.lower; original/lower have
        // equal byte lengths, so the original-case slice is recovered by
        // offsetting from effect_slice_tp.original (NOT tp.original — after
        // scoping the scan the suffix no longer belongs to tp.lower).
        let type_rest_original =
            &effect_slice_tp.original[effect_slice_tp.original.len() - type_rest.len()..];
        let type_text_tp = TextPair::new(type_rest_original, type_rest);
        // The condition is already isolated as `trailing_condition_tp`; no
        // inner " as long as " strip is needed.
        let condition_tp = trailing_condition_tp;
        let type_name = type_text_tp.lower.trim().trim_end_matches('.');
        // Pre-anchored slice — `split_once_on("isn't a ")` over the
        // condition-free effect slice consumed everything up to and including
        // "isn't a ". What remains is the type word plus an optional trailing
        // period, so a literal `match` on the five core types is idiomatic
        // enum-conversion (not parsing dispatch).
        let core_type = match type_name {
            "creature" => Some(CoreType::Creature), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            "artifact" => Some(CoreType::Artifact), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            "enchantment" => Some(CoreType::Enchantment), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            "land" => Some(CoreType::Land), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            "planeswalker" => Some(CoreType::Planeswalker), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
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

    // --- "[pronoun]'s a/an <types> with <P/T clause> [as long as <cond>]" ---
    // CR 613.1d + CR 613.1g: self-referential conditional animation static
    // (Animate Artifact). Dispatched after the `isn't a` type-removal block so
    // the condition-is-`isn't a creature` case (this card) reaches it.
    if let Some(def) = parse_pronoun_becomes_type_static(&tp, &text) {
        return Some(def);
    }

    // CR 205.2 + CR 613.1d + CR 613.4b: class-wide animation static for
    // "Each noncreature <T> ..." subjects (March of the Machines, Karn).
    // Opalescence ("Each other non-Aura enchantment ...") starts with
    // "Each other" and is handled by a different arm. The affirmative-type
    // token is artifact or enchantment; the dynamic-P/T tail is delegated
    // to the existing helper.
    if let Some(def) = parse_each_noncreature_subject_is_creature_with_pt_mv(&tp, &text) {
        return Some(def);
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

            // CR 509.1b: "can't be blocked by more than N creature(s)" — a
            // per-creature blocker MAXIMUM (Stalking Tiger). Must be tried before
            // the generic "by <filter>" branch below, which would otherwise read
            // "more than one creature" as a blocker quality filter.
            if let Ok((rest, _)) =
                tag::<_, _, OracleError<'_>>("by more than ").parse(after_blocked)
            {
                if let Ok((rest, max)) = nom_primitives::parse_number(rest) {
                    if let Ok((rest, _)) =
                        alt((tag::<_, _, OracleError<'_>>(" creatures"), tag(" creature")))
                            .parse(rest)
                    {
                        if rest.trim().is_empty() {
                            return Some(
                                StaticDefinition::new(StaticMode::CantBeBlockedByMoreThan { max })
                                    .affected(TargetFilter::SelfRef)
                                    .description(text.to_string()),
                            );
                        }
                    }
                }
            }

            // CR 509.1b: "can't be blocked by <filter>" — extract blocker restriction filter.
            if let Ok((by_rest, _)) = tag::<_, _, OracleError<'_>>("by ").parse(after_blocked) {
                // CR 105.4 + CR 608.2c (issue #327): Try the chosen-qualifier
                // parser first so "creatures of that color" / "creatures of
                // the chosen color" produces a filter with
                // `FilterProp::IsChosenColor`. Falls back to `parse_type_phrase`
                // for non-anaphor filter shapes.
                let by_rest_tp = TextPair::new(by_rest, by_rest);
                let (filter, remainder) =
                    if let Some(chosen) = parse_chosen_qualifier_subject(&by_rest_tp) {
                        (chosen, "")
                    } else {
                        parse_type_phrase(by_rest)
                    };
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

    // CR 702.122a / 702.171a / 702.184c: crew/saddle/station power-contribution
    // modifier (Reckoner Bankbuster, Giant Ox, Stoic Star-Captain).
    if let Some(def) = parse_crew_contribution_static(&text) {
        return Some(def);
    }

    if let Some(def) = parse_source_power_block_restriction(&text) {
        return Some(def);
    }

    if let Some(def) = parse_power_threshold_block_restriction(&text) {
        return Some(def);
    }

    // CR 506.5 + CR 508.1c: "~ can only attack alone" — CombatAlone(Attack, MustBeSole).
    // The creature may attack only if it is the sole attacker (Master of Cruelties).
    // Must precede the generic "can't attack" arm to avoid mis-dispatch.
    if let Some((_, _, rest)) = nom_primitives::scan_preceded(tp.lower, |i: &str| {
        let (i, _) = tag::<_, _, OracleError<'_>>("can only attack alone").parse(i)?;
        let (i, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(i)?;
        Ok((i, ()))
    }) {
        if rest.trim().is_empty() {
            return Some(
                StaticDefinition::new(StaticMode::CombatAlone {
                    action: CombatAloneAction::Attack,
                    requirement: CombatAloneRequirement::MustBeSole,
                })
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
            );
        }
    }

    // CR 506.5 + CR 508.1a + CR 509.1b: "~ can't attack alone" / "~ can't
    // block alone" / "~ can't attack or block alone".
    // Must precede the generic "can't block" / "can't attack" arms below, which
    // would otherwise swallow these as a blanket CantBlock / CantAttack. The
    // compound "attack or block alone" emits the attack half here so the
    // single-return path is non-None; `parse_static_line_multi` emits both halves.
    if let Some((_, restriction, rest)) =
        nom_primitives::scan_preceded(tp.lower, parse_alone_combat_restriction)
    {
        if rest.trim().is_empty() {
            let action = match restriction {
                AloneCombatRestriction::Attack | AloneCombatRestriction::AttackOrBlock => {
                    CombatAloneAction::Attack
                }
                AloneCombatRestriction::Block => CombatAloneAction::Block,
            };
            return Some(
                StaticDefinition::new(StaticMode::CombatAlone {
                    action,
                    requirement: CombatAloneRequirement::NeedsCompanion,
                })
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
            );
        }
    }

    // --- "~ can't block" ---
    if nom_primitives::scan_contains(tp.lower, "can't block")
        && !nom_primitives::scan_contains(tp.lower, "can't be blocked")
    {
        let mut def = StaticDefinition::new(StaticMode::CantBlock)
            .affected(TargetFilter::SelfRef)
            .description(text.to_string());
        // CR 509.1b + CR 611.3a: a trailing "unless [cost]", "as long as
        // [board-state]", or "if [board-state]" clause scopes the restriction;
        // attach whichever is present. "as long as" is tried before "if" to match
        // `split_trailing_gate_condition`'s precedence. (CR 509.1b is the block
        // *restriction* rule — "a creature can't block" — not 509.1c, which is
        // block *requirements*.)
        if let Some(condition) = parse_unless_static_condition(&tp)
            .or_else(|| parse_as_long_as_static_condition(&tp))
            .or_else(|| parse_if_static_condition(&tp))
        {
            def.condition = Some(condition);
        }
        return Some(def);
    }

    // --- "~ can't attack" ---
    if nom_primitives::scan_contains(tp.lower, "can't attack") {
        // CR 508.1d: Subject-led lines ("Each creature ... can't attack you") must not
        // collapse to SelfRef — `parse_subject_combat_rule_static` handles them above.
        if let Some((subject_lower, _, rest)) =
            nom_primitives::scan_preceded(tp.lower, parse_cant_attack_rule_static_predicate_nom)
        {
            let rest = match opt(tag::<_, _, OracleError<'_>>(".")).parse(rest) {
                Ok((r, _)) => r,
                Err(_) => rest,
            };
            // Only defer when the line is a fully consumed scoped cant-attack
            // (Eriette). Trailing "unless"/"if" clauses must still use SelfRef.
            if rest.trim().is_empty() {
                let subject = tp.original[..subject_lower.len()].trim();
                let subject_lower = subject.to_lowercase();
                if !subject.is_empty()
                    && subject_lower != "~"
                    && subject_lower != "it"
                    && subject_lower != "this"
                    && !SELF_REF_PARSE_ONLY_PHRASES.contains(&subject_lower.as_str())
                {
                    return None;
                }
            }
        }
        let mode = if nom_primitives::scan_contains(tp.lower, "can't attack or block") {
            StaticMode::CantAttackOrBlock
        } else {
            StaticMode::CantAttack
        };
        let mut def = StaticDefinition::new(mode)
            .affected(TargetFilter::SelfRef)
            .description(text.to_string());
        // CR 508.1 + CR 611.3a: a trailing "unless [cost]", "as long as
        // [board-state]", or "if [board-state]" clause scopes the restriction;
        // attach whichever is present. "as long as" is tried before "if" to match
        // `split_trailing_gate_condition`'s precedence (Seer of the Bright Side:
        // "... can't attack or block as long as it has a stun counter on it.").
        if let Some(condition) = parse_unless_static_condition(&tp)
            .or_else(|| parse_as_long_as_static_condition(&tp))
            .or_else(|| parse_if_static_condition(&tp))
        {
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

    // --- "~ can be attached only to {filter}" ---
    // CR 301.5 + CR 303.4 + CR 701.3a: Positive attachment restriction on an
    // Aura/Equipment — the source can only attach to a host matching the parsed
    // `TargetFilter` (Strata Scythe, Brass Knuckles, Konda's Banner). Enforced in
    // game/effects/attach.rs::attachment_illegality.
    if let Some(def) = parse_attach_only_restriction(&tp, &text) {
        return Some(def);
    }

    // --- "Spells and abilities <scope> can't cause their controller to search their library" ---
    // CR 701.23 + CR 609.3: Ashiok, Dream Render's first static. Subject-scoped
    // prohibition where `cause` identifies whose spells/abilities are muzzled.
    if let Some(def) = parse_cant_search_library(&tp, &text) {
        return Some(def);
    }

    // --- "If an opponent/a player would search a library, that player searches the top N cards ... instead" ---
    // CR 701.23f + CR 614.1a: Aven Mindcensor class. Replaces a SEARCHER-scoped
    // library search with a top-N search. `who` scopes which searcher is
    // restricted; `count` is the visible portion. Runtime enforcement is in
    // game/effects/search_library.rs::library_search_top_limit.
    if let Some(def) = parse_restrict_search_to_top(&tp, &text) {
        return Some(def);
    }

    // --- "Triggered abilities <scope> can't cause you to sacrifice or exile <affected>" ---
    // CR 603.2 + CR 609.3: The Master, Multiplied class. Subject-scoped prohibition
    // where `cause` identifies whose triggered abilities are muzzled and `affected`
    // identifies the protected objects.
    if let Some(def) = parse_cant_cause_sacrifice_or_exile(&tp, &text) {
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
    if super::shared::contains_activated_abilities_cant_be_activated(tp.lower) {
        let exemption = parse_cant_be_activated_exemption_in_text(tp.lower);
        let mut def = StaticDefinition::new(StaticMode::CantBeActivated {
            who: ProhibitionScope::AllPlayers,
            source_filter: TargetFilter::SelfRef,
            exemption,
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
    // CR 702.18a / 702.11a: these descriptive phrasings ARE Shroud / Hexproof.
    if let Some(scope) = crate::parser::oracle_keyword::classify_cant_be_targeted(tp.lower) {
        return Some(match scope {
            // CR 702.11a: "... your opponents control" — grant Hexproof so the
            // permanent's own controller can still target it.
            crate::parser::oracle_keyword::CantBeTargetedScope::OpponentsOnly => {
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Hexproof,
                    }])
                    .description(text.to_string())
            }
            // CR 702.18a: blanket — can't be targeted by any player. Enforced in
            // `targeting.rs::can_target` via the object's active static definitions.
            crate::parser::oracle_keyword::CantBeTargetedScope::AnyPlayer => {
                StaticDefinition::new(StaticMode::CantBeTargeted)
                    .affected(TargetFilter::SelfRef)
                    .description(text.to_string())
            }
        });
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

    // --- CR 117.1a + CR 604.1: "[subject] can cast spells only during {your | their own} turn(s)" ---
    // E.g., Fires of Invention: "You can cast spells only during your turn." → SourceRelative
    // E.g., Dosan, the Falling Leaf: "Players can cast spells only during their own turns." → PerAffected
    //
    // Must be checked AFTER PerTurnCastLimit (which handles "no more than N" in compound
    // clauses) and BEFORE the generic CantCastDuring block (which matches "can't cast
    // spells during"). Guard: exclude compound lines containing "each turn" — those are
    // split at the oracle.rs level so CantCastDuring and PerTurnCastLimit emit independently.
    if nom_primitives::scan_contains(tp.lower, "can cast spells only during")
        && !nom_primitives::scan_contains(tp.lower, "each turn")
    {
        // Subject → scope, via the shared building block.
        let (who, after_subject) = strip_casting_prohibition_subject(tp.lower)
            .unwrap_or((ProhibitionScope::Controller, tp.lower));
        // Predicate must be exactly "can cast spells " + parse_when_clause.
        fn parse_predicate(i: &str) -> OracleResult<'_, WhenKind> {
            let (i, _) = tag::<_, _, OracleError<'_>>("can cast spells ").parse(i)?;
            let (i, kind) = parse_when_clause(i)?;
            Ok((i, kind))
        }
        if let Ok((rest, kind)) = parse_predicate(after_subject) {
            if rest.trim().is_empty() {
                return Some(
                    StaticDefinition::new(StaticMode::CantCastDuring {
                        who,
                        when: when_kind_to_condition(kind),
                    })
                    .description(text.to_string()),
                );
            }
        }
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

    // --- CR 601.3 + CR 101.2 + CR 109.5: "[subject] can't cast spells from [zones]" ---
    // Two phrasings collapse here, discriminated by the zone clause:
    // - Explicit list (Grafdigger's Cage): "Players can't cast spells from
    //   graveyards or libraries." → prohibited = the listed zones.
    // - Inverse "anywhere other than" (Drannith Magistrate): "Your opponents
    //   can't cast spells from anywhere other than their hands." → prohibited =
    //   every cast-capable zone except the named allowed zone.
    // The subject prefix rides the `who` scope axis via the shared building block.
    if nom_primitives::scan_contains(tp.lower, "can't cast spells from") {
        let who = strip_casting_prohibition_subject(tp.lower)
            .map(|(scope, _)| scope)
            .unwrap_or(ProhibitionScope::AllPlayers);
        // CR 601.2a: Prefer the "anywhere other than" complement; fall back to the
        // explicit zone list. An empty list (no recognized zone) yields no static —
        // returning `TargetFilter::Any` here would over-block every zone.
        let zones = parse_cast_from_anywhere_other_than_tp(&tp)
            .unwrap_or_else(|| parse_zone_names_from_tp(&tp));
        if !zones.is_empty() {
            let affected = TargetFilter::Typed(TypedFilter {
                properties: vec![FilterProp::InAnyZone { zones }],
                ..TypedFilter::default()
            });
            return Some(
                StaticDefinition::new(StaticMode::CantCastFrom { who })
                    .affected(affected)
                    .description(text.to_string()),
            );
        }
    }

    // --- CR 101.2: Blanket casting prohibition ("can't cast [type] spells") ---
    // e.g., Steel Golem: "You can't cast creature spells."
    // e.g., Hymn of the Wilds: "You can't cast instant or sorcery spells."
    // Excludes lines handled by PerTurnCastLimit ("can't cast more than"),
    // CantCastDuring ("can't cast spells during"), and CantCastFrom ("can't cast spells from").
    if let Some(def) = parse_cant_cast_type_spells(tp.lower, &text, &raw_lower) {
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

    // --- CR 121.1 / CR 613.11: Draw-source redirection ("draw cards from the
    // bottom of your library rather than the top") — River Song, "Meet in
    // Reverse". ---
    if let Some(def) = parse_draw_from_bottom(tp.lower, &text) {
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

    // CR 708.5: "You may look at face-down creatures [you don't control | your
    // opponents control] any time." (Found Footage). The default rule lets you
    // look only at face-down permanents you control; this static lifts that for
    // the permanents named by the subject phrase. The affected filter (carrying
    // FilterProp::FaceDown plus the controller scope) is parsed via
    // `parse_target` so the same handler covers both scope wordings.
    if let Some(def) = parse_may_look_at_face_down_static(&tp) {
        return Some(def);
    }

    // CR 116.2b + CR 708.7: "Permanents your opponents control can't be turned
    // face up during your turn." (Karlov Watchdog) — turn-face-up prohibition.
    if let Some(def) = parse_cant_be_turned_face_up_static(&tp) {
        return Some(def);
    }

    // NOTE: "enters with N counters" patterns are now handled by oracle_replacement.rs
    // as proper Moved replacement effects (paralleling the "enters tapped" pattern).

    // --- CR 702.142b: "[Filter] can boast N times ... rather than once" ---
    // Birgi, God of Storytelling: modifies per-turn activation limit for boast abilities.
    if let Some((new_limit, _)) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, _) = take_until("can boast ").parse(i)?;
        let (i, _) = tag("can boast ").parse(i)?;
        // "twice" / "thrice" are multiplicative adverbs; "[N] times" is cardinal.
        let (i, n) = alt((
            value(2u32, tag("twice")),
            value(3u32, tag("thrice")),
            terminated(nom_primitives::parse_number, tag(" times")),
        ))
        .parse(i)?;
        let (i, _) = take_until("rather than once").parse(i)?;
        let (i, _) = tag("rather than once").parse(i)?;
        Ok((i, n as u8))
    }) {
        // Parse the affected filter from the beginning of the text (before "can boast")
        let (affected, _) = parse_type_phrase(tp.original);
        return Some(
            StaticDefinition::new(StaticMode::ModifyActivationLimit {
                keyword: "boast".to_string(),
                new_limit,
            })
            .affected(affected)
            .description(text.to_string()),
        );
    }

    // --- "{Ability} abilities you activate cost {N} less to activate" ---
    // CR 601.2f: Ability-type-specific cost reduction (e.g., Silver-Fur Master, Fluctuator).
    if nom_primitives::scan_contains(tp.lower, "abilities you activate")
        && nom_primitives::scan_contains(tp.lower, "less to activate")
    {
        // Extract keyword name and amount via nom combinators
        if let Some(((keyword, amount), remainder)) = nom_on_lower(tp.original, tp.lower, |i| {
            let (i, kw) = terminated(
                nom::bytes::complete::take_until(" abilities you activate"),
                tag(" abilities you activate"),
            )
            .parse(i)?;
            let (i, _) = take_until(" cost ").parse(i)?;
            let (i, _) = tag(" cost ").parse(i)?;
            let (i, amt) =
                nom::sequence::delimited(tag("{"), nom_primitives::parse_number, tag("}"))
                    .parse(i)?;
            let (i, _) = tag(" less to activate").parse(i)?;
            Ok((i, (kw.to_string(), amt)))
        })
        .filter(|((keyword, _), _)| !keyword.trim().is_empty())
        {
            // CR 601.2f: Extract optional "for each [X]" dynamic count clause from remainder.
            let remainder_lower = remainder.to_lowercase();
            let dynamic_count: Option<QuantityRef> = tag::<_, _, OracleError<'_>>(" for each ")
                .parse(remainder_lower.as_str())
                .ok()
                .and_then(|(for_each_rest, _)| {
                    crate::parser::oracle_quantity::parse_for_each_clause_expr(for_each_rest)
                })
                .map(|expr| match expr {
                    QuantityExpr::Ref { qty } => qty,
                    _ => QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(TypedFilter::card()),
                    },
                });
            // CR 602.2: "abilities you activate" is ACTIVATOR-scoped — the discount
            // keys off who activates the ability (the static's controller, "you"),
            // not who controls the ability's source. Emit the activator axis rather
            // than a `controller(You)` source filter, which mis-scoped abilities on
            // permanents another player controls but this player may activate.
            return Some(
                StaticDefinition::new(StaticMode::ReduceAbilityCost {
                    mode: CostModifyMode::Reduce,
                    keyword: keyword.trim().to_string(),
                    amount,
                    minimum_mana: parse_activated_cost_reduction_minimum_mana(tp.lower),
                    dynamic_count,
                    exemption: ActivationExemption::None,
                    activator: Some(PlayerFilter::Controller),
                })
                .description(text.to_string()),
            );
        }
    }

    // --- "<Keyword> abilities of [subject] cost {N} less to activate" ---
    // CR 601.2f: Class-scoped keyword-ability activation cost reduction keyed on
    // a tagged activated keyword (CR 602.1). The keyword is the ability tag
    // ("power-up", "exhaust", "boast", "outlast"); the static runtime gate
    // (`apply_static_activated_ability_cost_reduction`) matches the activating
    // ability's `AbilityTag::keyword_str()`. The `<subject>` filter is routed
    // through `parse_type_phrase`, which handles the "other" self-exclusion.
    //   - Hulk / Gamma Goliath: "Power-up abilities of other creatures you control…"
    //   - Boom Scholar: "Exhaust abilities of other permanents you control…"
    if let Some(((keyword, subject, amount), _)) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, keyword) = parse_taggable_ability_keyword(i)?;
        let (i, _) = tag(" abilities of ").parse(i)?;
        let (i, subject) = take_until(" cost ").parse(i)?;
        let (i, _) = tag(" cost ").parse(i)?;
        let (i, amount) =
            nom::sequence::delimited(tag("{"), nom_primitives::parse_number, tag("}")).parse(i)?;
        let (i, _) = tag(" less to activate").parse(i)?;
        Ok((i, (keyword, subject.to_string(), amount)))
    }) {
        let (affected, _rest) = parse_type_phrase(&subject);
        return Some(
            StaticDefinition::new(StaticMode::ReduceAbilityCost {
                mode: CostModifyMode::Reduce,
                keyword: keyword.to_string(),
                amount,
                minimum_mana: parse_activated_cost_reduction_minimum_mana(tp.lower),
                dynamic_count: None,
                exemption: ActivationExemption::None,
                // Source-scoped ("abilities of <subject>"): scope is the `affected`
                // filter below; no activator gate.
                activator: None,
            })
            .affected(affected)
            .description(text.to_string()),
        );
    }

    // --- "[Subject]'s <keyword> abilities cost {N} less/more to activate" ---
    // CR 602.1 + CR 601.2f + CR 118.7 + CR 702.6a: Possessive keyword-ability cost
    // modifier keyed on a tagged activated keyword. Firion, Wild Rose Warrior's
    // granted ability "This Equipment's equip abilities cost {2} less to activate"
    // (keyword = "equip", subject = SelfRef). Distinct from the "<keyword>
    // abilities of [subject]" form above by its possessive ("'s") grammar.
    if let Some(((subject, keyword, amount, mode), _)) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, subject) = take_until("'s ").parse(i)?;
        let (i, _) = tag("'s ").parse(i)?;
        let (i, keyword) = parse_taggable_ability_keyword(i)?;
        let (i, _) = tag(" abilities cost ").parse(i)?;
        let (i, amount) =
            nom::sequence::delimited(tag("{"), nom_primitives::parse_number, tag("}")).parse(i)?;
        let (i, _) = tag(" ").parse(i)?;
        let (i, mode) = alt((
            value(CostModifyMode::Reduce, tag("less to activate")),
            value(CostModifyMode::Raise, tag("more to activate")),
        ))
        .parse(i)?;
        Ok((i, (subject.to_string(), keyword, amount, mode)))
    }) {
        // CR 109.5: "This Equipment's …" (and other "this <type>" possessives)
        // self-reference the source permanent → SelfRef, so the static affects
        // only this object's equip ability — not every Equipment. Mirrors the
        // self-reference dispatch in `evasion.rs`.
        let subject_lower = subject.to_lowercase();
        let affected = if subject == "~" || SELF_REF_TYPE_PHRASES.contains(&subject_lower.as_str())
        {
            TargetFilter::SelfRef
        } else {
            parse_type_phrase(&subject).0
        };
        let minimum_mana = matches!(mode, CostModifyMode::Reduce)
            .then(|| parse_activated_cost_reduction_minimum_mana(tp.lower))
            .flatten();
        return Some(
            StaticDefinition::new(StaticMode::ReduceAbilityCost {
                mode,
                keyword: keyword.to_string(),
                amount,
                minimum_mana,
                dynamic_count: None,
                exemption: ActivationExemption::None,
                // Source-scoped ("<subject>'s <keyword> abilities"): scope is the
                // `affected` filter below; no activator gate.
                activator: None,
            })
            .affected(affected)
            .description(text.to_string()),
        );
    }

    // --- "Each power-up ability of [subject] can be activated an additional time" ---
    // CR 602.5b: Class-scoped power-up activation-limit raise (Wonder
    // Man / Hollywood Hero). Power-up's base limit is once-per-game; "an additional
    // time" with no per-turn qualifier raises the per-game cap to 2.
    if let Some((subject, _)) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, _) = tag("each power-up ability of ").parse(i)?;
        let (i, subject) = take_until(" can be activated an additional time").parse(i)?;
        let (i, _) = tag(" can be activated an additional time").parse(i)?;
        Ok((i, subject.to_string()))
    }) {
        let (affected, _rest) = parse_type_phrase(&subject);
        return Some(
            StaticDefinition::new(StaticMode::ModifyActivationLimit {
                keyword: "power-up".to_string(),
                new_limit: 2,
            })
            .affected(affected)
            .description(text.to_string()),
        );
    }

    // --- "[Enchanted/Equipped] [type]'s activated abilities cost {N} less to activate" ---
    // CR 303.4 + CR 602.1 + CR 601.2f: Aura/Equipment-granted activated ability
    // cost reduction for the attached object (Power Artifact).
    if let Some(((prefix, filter_part, amount), _)) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, prefix) = alt((
            value("enchanted ", tag::<_, _, OracleError<'_>>("enchanted ")),
            value("equipped ", tag::<_, _, OracleError<'_>>("equipped ")),
        ))
        .parse(i)?;
        let (i, filter_part) = take_until("'s activated abilities cost ").parse(i)?;
        let (i, _) = tag("'s activated abilities cost ").parse(i)?;
        let (i, amount) =
            nom::sequence::delimited(tag("{"), nom_primitives::parse_number, tag("}")).parse(i)?;
        let (i, _) = tag(" less to activate").parse(i)?;
        Ok((i, (prefix, filter_part.to_string(), amount)))
    }) {
        let filter_text = format!("{prefix}{filter_part}");
        let (affected, _rest) = parse_type_phrase(&filter_text);
        return Some(
            StaticDefinition::new(StaticMode::ReduceAbilityCost {
                mode: CostModifyMode::Reduce,
                keyword: "activated".to_string(),
                amount,
                minimum_mana: parse_activated_cost_reduction_minimum_mana(tp.lower),
                dynamic_count: None,
                exemption: ActivationExemption::None,
                // Source-scoped ("[Enchanted/Equipped] <type>'s activated
                // abilities"): scope is the `affected` filter below; no activator gate.
                activator: None,
            })
            .affected(affected)
            .description(text.to_string()),
        );
    }

    // --- "Activated abilities of [filter] cost {N} less/more to activate" ---
    // CR 602.1 + CR 601.2f + CR 118.7: Generic activated-ability cost modifier,
    // directional. Reduce (Training Grounds: "Activated abilities of creatures you
    // control cost {2} less to activate") and Raise (Skyseer's Chariot: "Activated
    // abilities of sources with the chosen name cost {2} more to activate").
    // Combinator: prefix → subject → " cost {N} " → direction. The subject is
    // either the chosen-name source phrase (→ HasChosenName) or a type phrase.
    if let Some(((amount_n, is_x, mode, subject_filter, dynamic_count), _)) =
        nom_on_lower(tp.original, tp.lower, |i| {
            let (i, _) = tag("activated abilities of ").parse(i)?;
            let (i, subject) = take_until(" cost ").parse(i)?;
            let (i, _) = tag(" cost ").parse(i)?;
            // CR 107.3 + CR 601.2f: the amount is a fixed `{N}` (Training Grounds)
            // or the variable `{X}` (Agatha), whose value is supplied by the
            // trailing "where X is …" referent parsed below.
            let (i, (amount_n, is_x)) = nom::sequence::delimited(
                tag("{"),
                alt((
                    map(nom_primitives::parse_number, |n| (n, false)),
                    value((0u32, true), tag("x")),
                )),
                tag("}"),
            )
            .parse(i)?;
            let (i, _) = tag(" ").parse(i)?;
            let (i, mode) = alt((
                value(CostModifyMode::Reduce, tag("less to activate")),
                value(CostModifyMode::Raise, tag("more to activate")),
            ))
            .parse(i)?;
            // CR 208.1 + CR 113.7: optional dynamic referent for `{X}`
            // ("where X is ~'s power", Agatha).
            let (i, dynamic_count) = opt(parse_where_x_is_self_stat).parse(i)?;
            Ok((
                i,
                (amount_n, is_x, mode, subject.to_string(), dynamic_count),
            ))
        })
    {
        // CR 601.2f: A fixed `{N}` reduces by exactly `N`; a `{X}` reduces by
        // `1 × resolve_quantity(dynamic_count)` (Agatha: X = ~'s power). A bare
        // `{X}` with no recognized referent is unresolvable — leave it for a
        // later branch (ultimately unsupported) rather than emit a wrong amount.
        let resolved: Option<(u32, Option<QuantityRef>)> = if is_x {
            dynamic_count.map(|qty| (1u32, Some(qty)))
        } else {
            Some((amount_n, None))
        };
        if let Some((amount, dynamic_count)) = resolved {
            // CR 113.6 + CR 201.2: "sources with the chosen name" → HasChosenName,
            // shared with the CantBeActivated name-picker class. Otherwise a type phrase.
            let affected = parse_chosen_name_source_filter(&subject_filter)
                .unwrap_or_else(|| parse_type_phrase(&subject_filter).0);
            // CR 118.7: a one-mana floor only applies to reductions.
            let minimum_mana = matches!(mode, CostModifyMode::Reduce)
                .then(|| parse_activated_cost_reduction_minimum_mana(tp.lower))
                .flatten();
            return Some(
                StaticDefinition::new(StaticMode::ReduceAbilityCost {
                    mode,
                    keyword: "activated".to_string(),
                    amount,
                    minimum_mana,
                    dynamic_count,
                    exemption: ActivationExemption::None,
                    // Source-scoped ("Activated abilities of <filter>"): scope is
                    // the `affected` filter below; no activator gate.
                    activator: None,
                })
                .affected(affected)
                .description(text.to_string()),
            );
        }
    }

    // --- "Activated abilities cost {N} less/more to activate [unless they're mana abilities]" (global)
    // --- "Abilities you activate [that aren't mana abilities] cost {N} less/more to activate" (activator) ---
    // CR 601.2f + CR 118.7 + CR 605.1a: Unscoped (Suppression Field: "Activated
    // abilities cost {2} more to activate unless they're mana abilities") or
    // activator-scoped (Zirda, the Dawnwaker: "Abilities you activate that aren't
    // mana abilities cost {2} less to activate") activated-ability cost modifier.
    // The scoped "Activated abilities OF <subject>" form is owned by the branch
    // above; this handles the two subjects that carry no "of <subject>" filter.
    // `keyword == "activated"` matches every activated ability at runtime; the
    // optional mana-ability exemption (prefix "that aren't mana abilities" or
    // suffix "unless they're mana abilities") is enforced there via
    // `ActivationExemption::ManaAbilities`. CR 602.2: the global form (Suppression
    // Field) leaves both scopes open (`activator = None`, `affected = None`); the
    // "abilities you activate" form is ACTIVATOR-scoped, not source-scoped, so it
    // sets `activator = Some(PlayerFilter::Controller)` ("you" = the static's
    // controller) and leaves `affected = None` — the discount keys off who
    // activates the ability, never who controls its source.
    if let Some(((activator, exemption, amount, mode), _)) =
        nom_on_lower(tp.original, tp.lower, |i| {
            let (i, (activator, prefix_exempt)) = alt((
                map(
                    (
                        tag("abilities you activate"),
                        opt(tag(" that aren't mana abilities")),
                    ),
                    |(_, exempt): (&str, Option<&str>)| {
                        (Some(PlayerFilter::Controller), exempt.is_some())
                    },
                ),
                value((None::<PlayerFilter>, false), tag("activated abilities")),
            ))
            .parse(i)?;
            let (i, _) = tag(" cost {").parse(i)?;
            let (i, amount) = nom_primitives::parse_number(i)?;
            let (i, _) = tag("} ").parse(i)?;
            let (i, mode) = alt((
                value(CostModifyMode::Reduce, tag("less to activate")),
                value(CostModifyMode::Raise, tag("more to activate")),
            ))
            .parse(i)?;
            // CR 605.1a: dual-apostrophe exemption suffix (Suppression Field class).
            let (i, suffix_exempt) =
                opt(super::shared::parse_mana_ability_exemption_suffix).parse(i)?;
            let exemption = if prefix_exempt || suffix_exempt.is_some() {
                ActivationExemption::ManaAbilities
            } else {
                ActivationExemption::None
            };
            Ok((i, (activator, exemption, amount, mode)))
        })
    {
        // CR 118.7: a one-mana floor only applies to reductions.
        let minimum_mana = matches!(mode, CostModifyMode::Reduce)
            .then(|| parse_activated_cost_reduction_minimum_mana(tp.lower))
            .flatten();
        return Some(
            StaticDefinition::new(StaticMode::ReduceAbilityCost {
                mode,
                keyword: "activated".to_string(),
                amount,
                minimum_mana,
                dynamic_count: None,
                exemption,
                activator,
            })
            .description(text.to_string()),
        );
    }

    // --- CR 116.2 + CR 118.7a: special-action (plot/unlock) cost reduction ---
    // "Plotting cards from your hand costs {N} less" (Doc Aurlock, CR 116.2k) /
    // "Unlock costs you pay cost {N} less" (Inquisitive Glimmer, CR 116.2m). A
    // dedicated `SpecialAction` axis, NOT the generic activated-ability reducer
    // above — plot/unlock payments carry no `AbilityTag`, so routing them
    // through `ReduceAbilityCost { keyword: "activated" }` would never fire and
    // would wrongly let "activated abilities cost less" reduce plot costs.
    if let Some(def) = parse_action_cost_reduction(&text, &lower) {
        return Some(def);
    }

    // --- CR 601.2f: Cost-floor statics (Trinisphere class) ---
    // Pattern: "each spell that would cost less than {N} mana to cast costs {N} mana to cast"
    // Dispatched BEFORE the additive cost modifier branch because the floor's "less than"
    // would otherwise be misclassified as a ReduceCost shape.
    if let Some(def) = try_parse_cost_floor(&text, &lower) {
        return Some(def);
    }

    // --- CR 601.2f: Cost modification statics ---
    // Patterns: "[Type] spells [you/your opponents] cast cost {N} less/more to cast"
    // Also: "Noncreature spells cost {1} more to cast" (Thalia, no "you cast")
    if nom_primitives::scan_contains(tp.lower, "cost")
        && nom_primitives::scan_contains(tp.lower, "spell")
        && (nom_primitives::scan_contains(tp.lower, "less")
            || nom_primitives::scan_contains(tp.lower, "more"))
    {
        if let Some(def) = try_parse_cost_modification(&text, &lower, None) {
            return Some(def);
        }
    }

    // --- "must be blocked [by <quality>] if able" (CR 509.1c) ---
    if nom_primitives::scan_contains(tp.lower, "must be blocked") {
        // CR 509.1c: classify the OPTIONAL "by <quality>" conjunct so a present
        // quality is never silently weakened to the bare "any blocker" (None)
        // requirement. Mirrors the attached-grant paths (grammar.rs / shared.rs)
        // which distinguish the same three cases via the shared conjunct helper:
        //   * Recognized quality   → typed `MustBeBlocked { by: Some(filter) }`.
        //   * Unrecognized quality → leave the line Unimplemented (`return None`);
        //     emitting `by: None` here would force a block by ANY creature and
        //     drop the quality restriction. Falling through surfaces the gap to
        //     coverage instead of weakening the requirement.
        //   * No quality (bare "must be blocked if able") → `by: None`.
        match extract_must_be_blocked_by_conjunct(tp.lower) {
            Some(MustBeBlockedByConjunct::Recognized(filter)) => {
                return Some(
                    StaticDefinition::new(StaticMode::MustBeBlocked { by: Some(filter) })
                        .affected(TargetFilter::SelfRef)
                        .description(text.to_string()),
                );
            }
            Some(MustBeBlockedByConjunct::Unrecognized(_)) => return None,
            None => {
                return Some(
                    StaticDefinition::new(StaticMode::MustBeBlocked { by: None })
                        .affected(TargetFilter::SelfRef)
                        .description(text.to_string()),
                );
            }
        }
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
        let def = StaticDefinition::new(StaticMode::Other("CantPlayLand".to_string()))
            .affected(affected)
            .description(text.to_string());
        // CR 611.3a: a trailing "as long as <condition>" (Limited Resources:
        // "... as long as ten or more lands are on the battlefield") or "if
        // <condition>" (Rock Jockey: "... if this creature was cast this turn")
        // gates the restriction. If the rider is present but its condition is
        // NOT recognized, leave the whole line unsupported (return None) rather
        // than marking it a CantPlayLand enforced unconditionally.
        return match split_trailing_gate_condition(tp.lower) {
            Some(condition_text) => Some(def.condition(parse_static_condition(condition_text)?)),
            None => Some(def),
        };
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

    // --- "the \"legend rule\" doesn't apply [to <scope> you control]" (CR 704.5j) ---
    // Mirror Gallery (global), Sakashima of a Thousand Faces / Mirror Box
    // ("permanents you control"), Sliver Gravemother / Spider-Verse (subtype).
    if let Some(def) = parse_legend_rule_exemption(&tp, &text) {
        return Some(def);
    }

    // --- "You may cast [type] spells as though they had flash" (CR 601.3b / CR 702.8a) ---
    // Emits `CastWithKeyword { Flash }` with the spell-type filter — the only
    // static mode the flash-timing path (granted_spell_keywords) reads, and the
    // one that preserves the "creature spells" restriction (issue #1957).
    if let Some(def) = parse_cast_as_though_flash_static(&tp, &text) {
        return Some(def);
    }

    // --- "[Type] spells you cast [from zone] have [keyword]" (CR 702.51a) ---
    // E.g., "Creature spells you cast have convoke."
    // Also: "Creature cards you own that aren't on the battlefield have flash."
    if let Some(def) = parse_spells_have_keyword(&tp, &text) {
        return Some(def);
    }

    // --- "<type> cards in your hand [without <kw>] have <kw>. Its <kw> cost is
    // equal to its mana cost reduced by {N}." (CR 702.143d + CR 702 alt-cost
    // off-zone family) — Singing Towers of Darillium grants foretell with a
    // per-recipient derived cost.
    if let Some(def) = parse_hand_cards_have_derived_cost_keyword(&text) {
        return Some(def);
    }

    // --- "can block an additional creature [as long as|if <cond>]" / "can block any number" ---
    // CR 509.1c + CR 611.3a: an extra-blocker grant may carry a trailing "as long
    // as <cond>" / "if <cond>" gate (Entourage of Trest: "This creature can block
    // an additional creature each combat as long as you're the monarch"). The bare
    // form is parsed directly; a trailing gate is peeled via the shared gate
    // authority (`split_trailing_gate_condition_with_body`, which owns the
    // `as long as` > last-valid-`if` > `as if`-exclusion rules) so the
    // condition-free body reaches `parse_extra_blockers_static`, then the parsed
    // condition attaches. CR 611.3a: fail CLOSED — an unrecognized condition leaves
    // the whole line unsupported (`parse_static_condition` returns `None` → `?`)
    // rather than enforcing the extra-block grant unconditionally (an `Unrecognized`
    // gate evaluates as always-true in the layer system).
    if let Some(def) = parse_extra_blockers_static(&text) {
        return Some(def);
    }
    if let Some((body, condition_text)) = split_trailing_gate_condition_with_body(&tp) {
        if let Some(mut def) = parse_extra_blockers_static(body) {
            def.condition = Some(parse_static_condition(condition_text)?);
            return Some(def);
        }
    }

    // --- CR 509.1c: "All creatures able to block <self/enchanted creature> do so"
    // — printed permanent forced-block ("lure") static (Ochran Assassin, Breaker
    // of Armies, Prized Unicorn, Lure). The one-shot "… target creature this turn
    // do so" spell form is left to `try_parse_mass_forced_block` in the effect
    // parser. ---
    if let Some(def) = parse_forced_block_static(&text) {
        return Some(def);
    }

    // --- "play any number of lands" / counted additional land-drop grants ---
    // The ordinary +1 phrase ("play an additional land") is handled by the
    // rule-static subject/predicate shell so embedded subjects such as
    // "Each player who last chose green anchor ..." keep their affected filter.
    if let Ok((_, count)) = parse_static_additional_land_drop_count(tp.lower) {
        return Some(
            StaticDefinition::new(StaticMode::AdditionalLandDrop { count })
                .affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                ))
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

    // CR 309.4c: Hama Pashar — "Room abilities of dungeons you own trigger
    // an additional time." Parsed with composed nom tags (not scan_contains).
    if parse_room_ability_doubling_phrase(tp.lower) {
        return Some(
            StaticDefinition::new(StaticMode::DoubleTriggers {
                cause: TriggerCause::RoomEntered,
            })
            .description(text.to_string()),
        );
    }

    // CR 603.2d: Trigger doubling — "triggers/trigger an additional time".
    //
    // Cause classification by phrasing:
    // - "being dealt damage causes" / "dealt damage causes" — Wayta, Trainer
    //   Prodigy (ControlledCreatureDealtDamage).
    // - "attacking causes" — Isshin, Two Heavens as One (CreatureAttacking).
    // - "entering" / "enters the battlefield" / "enters" — Panharmonicon-class
    //   (EntersBattlefield). Panharmonicon itself names "artifact or creature
    //   entering", so both CoreTypes qualify; narrower wordings ("creature
    //   entering") collapse to [Creature] only.
    // - Otherwise (e.g. "If a triggered ability ... triggers, it triggers an
    //   additional time" — Roaming Throne, Strionic Resonator copies) use the
    //   unrestricted `Any` cause; the doubler's `affected` filter narrows
    //   which source's triggers qualify.
    if nom_primitives::scan_contains(tp.lower, "triggers an additional time")
        || nom_primitives::scan_contains(tp.lower, "trigger an additional time")
    {
        let cause = if nom_primitives::scan_contains(tp.lower, "being dealt damage causes")
            || nom_primitives::scan_contains(tp.lower, "dealt damage causes")
        {
            TriggerCause::ControlledCreatureDealtDamage
        } else if nom_primitives::scan_contains(tp.lower, "attacking causes") {
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
        // CR 603.2d: Narrow the doubler to triggers from a specific source when
        // the text names one ("a triggered ability of a Ninja creature you
        // control"). Without this the `affected` filter is `None` and
        // `apply_trigger_doubling` doubles every controlled permanent's
        // triggers, not just the named source's (Splinter, Roaming Throne).
        let mut def = StaticDefinition::new(StaticMode::DoubleTriggers { cause })
            .description(text.to_string());
        if let Some(filter) = parse_doubler_source_filter(tp.lower) {
            def = def.affected(filter);
        }
        return Some(def);
    }

    None
}

/// CR 309.4c: "Room abilities of dungeons you own trigger(s) an additional time."
fn parse_room_ability_doubling_phrase(lower: &str) -> bool {
    all_consuming((
        tag::<_, _, OracleError<'_>>("room abilities of "),
        tag("dungeons you own "),
        alt((tag("trigger "), tag("triggers "))),
        tag("an additional time"),
        opt(tag(".")),
    ))
    .parse(lower)
    .is_ok()
}

/// CR 614.1c + CR 122.1: Parse a continuous "enters with an additional counter"
/// replacement static.
///
/// Grammar (combinator-composed, one `alt()` per axis of variation):
/// ```text
/// <subject> " enter[s] with an additional " <counter> " counter on " <pronoun>
/// ```
/// where `<subject>` is a controller-scoped creature phrase
/// ("[Other|Legendary|Nontoken|Token ]creatures you control"), `<counter>` is a
/// recognized counter type (currently +1/+1 in shipping printings, but the
/// strict counter-type combinator admits any recognized type so the class is not
/// special-cased to one counter), and `<pronoun>` is "them"/"it".
///
/// The affected-permanent scope rides on `StaticDefinition::affected` exactly
/// like the anthem statics — reuse `parse_continuous_subject_filter` so every
/// Other/Legendary/Nontoken/Token qualifier is handled by the shared subject
/// parser rather than re-enumerated here. The filter MUST anchor to
/// `ControllerRef::You` (CR 109.5: "you control"); subjects without that anchor
/// fall through to leave the line Unimplemented.
///
/// FIXED-count form only: a dynamic count (Gev, "for each opponent who lost
/// life") produces no fixed `<counter>` token and so fails the combinator,
/// leaving the line Unimplemented until a dynamic-count axis exists.
fn parse_enters_with_additional_counters(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    // Split the subject from the predicate at the " enter[s] with an additional "
    // verb phrase, scanned at word boundaries so any controlled-creature subject
    // length is handled.
    let (subject_lower, predicate_lower) = nom_primitives::scan_split_at_phrase(tp.lower, |i| {
        alt((
            tag::<_, _, OracleError<'_>>("enter with an additional "),
            tag("enters with an additional "),
        ))
        .parse(i)
    })?;

    // Parse the predicate: verb phrase, counter type, " counter on ", pronoun.
    fn parse_predicate(i: &str) -> OracleResult<'_, crate::types::counter::CounterType> {
        let (i, _) = alt((
            tag::<_, _, OracleError<'_>>("enter with an additional "),
            tag("enters with an additional "),
        ))
        .parse(i)?;
        let (i, counter_type) = nom_primitives::parse_strict_counter_type(i)?;
        let (i, _) = tag(" counter on ").parse(i)?;
        let (i, _) = alt((tag("them"), tag("it"))).parse(i)?;
        let (i, _) = opt(tag(".")).parse(i)?;
        Ok((i, counter_type))
    }
    let (_rest, counter_type) = all_consuming(parse_predicate)
        .parse(predicate_lower.trim_end())
        .ok()?;

    // CR 109.5: the subject must be a controller-scoped ("you control") creature
    // phrase. Recover the original-case slice so the shared subject parser sees
    // the printed capitalization (subtypes/supertypes are capitalized in Oracle).
    let subject_original = tp.original[..subject_lower.len()].trim();
    let affected = parse_continuous_subject_filter(subject_original)?;
    if !filter_is_controller_you(&affected) {
        return None;
    }

    Some(
        StaticDefinition::new(StaticMode::EntersWithAdditionalCounters {
            counter_type,
            count: 1,
        })
        .affected(affected)
        .description(text.to_string()),
    )
}
