//! Duration combinators for Oracle text parsing.
//!
//! **Single authority for the phrase→`Duration` grammar** (oracle-parser
//! SKILL §7). Parses: "until end of turn", "until end of combat", "until the
//! end of your/their next turn", "until your/their next turn", the
//! `until [the beginning of] <possessor> next <step>` step-deadline family
//! ("until your next end step", "until the next end step", "until your next
//! upkeep", "until its controller's next untap step"), "until ~/this creature
//! leaves the battlefield", "until you exile another card with ~/this
//! ability", "for the rest of the game", "for as long as [condition]", "this
//! turn", "this/that combat", and the "during target opponent's/player's next
//! turn" WINDOW (Gideon Jura).
//!
//! A phrase added here is taken away from every clause-level grammar that owned
//! it, because the positional wrappers below run first — see
//! `parse_next_turn_window_possessor` for why the "during …" arm accepts only
//! the targeted possessives.
//!
//! Positional wrappers (`strip_trailing_duration` / `strip_leading_duration`
//! in `oracle_effect/lower.rs`, the clause shell, and the combat-grant
//! parsers in `oracle_effect/subject.rs`) decide WHERE a duration clause
//! sits; the phrase→variant mapping lives only here. Adding a new duration
//! phrase means editing only this file.

use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::multispace0;
use nom::combinator::{eof, map, opt, rest, value, verify};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::condition::{parse_inner_condition, parse_recipient_has_counters};
use super::error::{oracle_err, OracleError, OracleResult};
use super::primitives::scan_contains;
use crate::types::ability::{
    ControllerRef, Duration, ObjectScope, PlayerScope, StaticCondition, TargetFilter,
};
use crate::types::phase::Phase;

/// Parse a duration phrase from Oracle text.
///
/// Nested by prefix dispatch: the shared "until " and "for " heads are
/// factored once, then the body sub-combinators dispatch on the remainder.
///
/// Note: the "for as long as [condition]" branch is clause-final — it
/// consumes the rest of its input (see `parse_for_as_long_as_condition`).
pub fn parse_duration(input: &str) -> OracleResult<'_, Duration> {
    alt((
        preceded(tag("until "), parse_until_body),
        preceded(tag("for "), parse_for_body),
        preceded(tag("during "), parse_during_body),
        parse_current_phase_duration,
    ))
    .parse(input)
}

/// Alternatives after the shared "during " prefix.
///
/// CR 514.2 + CR 508.1d: "during <possessor> next turn" names a WINDOW — the
/// whole of that player's next turn — which is exactly the span
/// [`Duration::UntilEndOfNextTurnOf`] already models (armed at that player's
/// untap step, pruned at that turn's cleanup). CR 508.1d's closing sentence
/// makes the whole-turn reading load-bearing rather than incidental: "If a
/// requirement that says a creature attacks if able during a certain turn refers
/// to a turn with multiple combat phases, the creature attacks if able during
/// each declare attackers step in that turn." Gideon Jura's official ruling says
/// the same in card terms — the "+2" "applies during each combat phase of the
/// affected player's next turn (as opposed to applying during the affected
/// player's next combat phase)".
///
/// The possessor is its own axis (`parse_next_turn_window_possessor`), so
/// "during your next turn" and "during target opponent's next turn" are one
/// production rather than enumerated full-string arms.
fn parse_during_body(input: &str) -> OracleResult<'_, Duration> {
    let (rest, possessor) = parse_next_turn_window_possessor(input)?;
    let (rest, _) = tag(" next turn").parse(rest)?;
    Ok((
        rest,
        Duration::UntilEndOfNextTurnOf {
            player: possessor.scope(),
        },
    ))
}

/// The possessor of a "during <possessor> next turn" window, **as written**.
///
/// Deliberately distinct from the emitted [`PlayerScope`], for the same reason
/// [`StepDeadlinePossessor`] is: two spellings that produce the SAME runtime
/// `PlayerScope` can still differ in what the rest of the parser must do about
/// them. Here, "target player's" and "target opponent's" both emit
/// `PlayerScope::Target` (CR 109.4 — the duration reads the first player target
/// either way), but they declare different companion target SLOTS, and the
/// clause body's "that player" anaphor must inherit the matching
/// [`ControllerRef`] so the slot's legal-target set is right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NextTurnWindowPossessor {
    /// CR 109.4: "target player's".
    TargetPlayer,
    /// CR 109.4 + CR 102.2: "target opponent's" (Gideon Jura). Runtime-read
    /// identical to `TargetPlayer`; the slot excludes the controller.
    TargetOpponent,
}

impl NextTurnWindowPossessor {
    /// The duration's runtime scope. Both spellings collapse here — the legality
    /// difference lives in [`Self::controller_ref`], not in the duration.
    fn scope(self) -> PlayerScope {
        match self {
            Self::TargetPlayer | Self::TargetOpponent => PlayerScope::Target,
        }
    }

    /// CR 608.2c: the `ControllerRef` a "that player" anaphor in the clause body
    /// must bind to.
    pub(crate) fn controller_ref(self) -> ControllerRef {
        match self {
            Self::TargetPlayer => ControllerRef::TargetPlayer,
            Self::TargetOpponent => ControllerRef::TargetOpponent,
        }
    }
}

/// CR 109.4: the possessor axis of a "during <possessor> next turn" window.
///
/// **Deliberately TARGETED possessives only** — not
/// [`parse_controller_possessive_pronoun`]'s "your"/"their". This grammar is
/// reached through the POSITIONAL wrappers (`strip_leading_duration` /
/// `strip_trailing_duration`), which peel a duration phrase off ANY clause, so a
/// phrase added here is taken away from every other clause-level grammar that
/// owns it. "during their next turn" is owned by the CR 723.1 control-next-turn
/// grammar (`try_parse_control_next_turn_suffix` — Mindslaver, Construct a
/// Cosmic Cube's "you control target opponent during their next turn"), where
/// the window is part of the effect rather than a separable duration. Accepting
/// the pronoun forms here silently stripped that window and left the
/// control-opponent rider unparsed.
///
/// "during target opponent's next turn" (Gideon Jura) is owned by no other
/// grammar, so it is safe — and necessary — here.
///
/// The possessive marker is a shared trailing `alt()` over both apostrophe
/// glyphs and the apostrophe-less spelling, matching the factoring in
/// [`parse_object_controller_possessive`] — the noun and the marker are separate
/// axes, not enumerated pairs.
fn parse_next_turn_window_possessor(input: &str) -> OracleResult<'_, NextTurnWindowPossessor> {
    terminated(
        preceded(
            tag("target "),
            alt((
                value(NextTurnWindowPossessor::TargetOpponent, tag("opponent")),
                value(NextTurnWindowPossessor::TargetPlayer, tag("player")),
            )),
        ),
        alt((tag("\u{2019}s"), tag("'s"), tag("s"))),
    )
    .parse(input)
}

/// CR 608.2c + CR 109.4: The possessor of a LEADING "during <possessor> next
/// turn, …" window, for callers that must publish the window's targeted player
/// as the clause body's relative-player scope.
///
/// Shares the single possessor combinator with [`parse_during_body`], so the
/// duration value and the anaphor scope can never disagree about which spelling
/// was written. Returns `None` when `input` does not open with such a window.
pub(crate) fn leading_next_turn_window_possessor(input: &str) -> Option<NextTurnWindowPossessor> {
    let (rest, possessor) = preceded(
        tag::<_, _, OracleError<'_>>("during "),
        parse_next_turn_window_possessor,
    )
    .parse(input)
    .ok()?;
    let (_, _) = (tag::<_, _, OracleError<'_>>(" next turn"), tag(", "))
        .parse(rest)
        .ok()?;
    Some(possessor)
}

/// Alternatives after the shared "until " prefix.
fn parse_until_body(input: &str) -> OracleResult<'_, Duration> {
    alt((
        value(Duration::UntilEndOfTurn, tag("end of turn")),
        // CR 511.2: effects that last "until end of combat" expire at the end
        // of the combat phase.
        value(Duration::UntilEndOfCombat, tag("end of combat")),
        parse_until_end_of_next_turn,
        parse_until_next_turn,
        // CR 611.2a + CR 500.4: the `<possessor> next <step>` deadline family —
        // one production over a possessor axis and a step axis, replacing the
        // former pair of hardcoded "your next end step" / "the next end step"
        // arms.
        parse_until_next_step,
        // Host-lifetime expiry: "until ~ leaves the battlefield" /
        // "until this creature leaves the battlefield". A card whose name
        // normalizes to a plural subject (e.g. "Cloak and Dagger, Entwined")
        // takes plural verb agreement in its own oracle text — "until Cloak
        // and Dagger leave the battlefield" — so both "leaves"/"leave" must
        // be accepted here, mirroring the same singular/plural alternation
        // already handled by the LTB *trigger* parsers (the `leaves_tail`
        // alt in `oracle_trigger.rs` and `oracle_effect/mod.rs`'s
        // "leaves the battlefield"/"leave the battlefield" alt). CR 611.2a:
        // the parsed value is the continuous effect's stated duration — here
        // the host permanent's own battlefield lifetime.
        value(
            Duration::UntilHostLeavesPlay,
            (
                alt((tag("~"), tag("this creature"))),
                tag(" "),
                alt((tag("leaves the battlefield"), tag("leave the battlefield"))),
            ),
        ),
        // CR 607.2a + CR 611.2a: source-linked impulse grants such as
        // Furious Rise last until the same source exiles another card.
        value(
            Duration::UntilSourceExilesAnotherCard,
            parse_until_source_exiles_another_card_body,
        ),
    ))
    .parse(input)
}

pub(crate) fn parse_until_source_exiles_another_card_body(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = tag("you exile another card with ").parse(input)?;
    let (input, _) = alt((
        tag::<_, _, OracleError<'_>>("~"),
        tag("this ability"),
        tag("this enchantment"),
        tag("this artifact"),
        tag("this creature"),
        tag("this permanent"),
    ))
    .parse(input)?;
    Ok((input, ()))
}

/// Alternatives after the shared "for " prefix.
fn parse_for_body(input: &str) -> OracleResult<'_, Duration> {
    alt((
        // CR 611.2a: "A continuous effect generated by the resolution of a
        // spell or ability lasts as long as stated by the spell or ability
        // creating it ... If no duration is stated, it lasts until the end of
        // the game." A continuous restriction worded "... for the rest of the
        // game" (Screaming Nemesis: "can't gain life for the rest of the
        // game") therefore has no expiry — modeled as `Duration::Permanent`.
        // CR 119.7 governs the restriction's semantics for the "can't gain
        // life" case specifically.
        value(Duration::Permanent, tag("the rest of the game")),
        // CR 611.2b: "for as long as" durations embed a condition that is
        // continuously checked — effect expires when the condition becomes
        // false.
        preceded(tag("as long as "), parse_for_as_long_as_condition),
    ))
    .parse(input)
}

/// Current-phase demonstratives: "this turn", "this combat", "that combat".
fn parse_current_phase_duration(input: &str) -> OracleResult<'_, Duration> {
    alt((
        preceded(
            tag("this "),
            alt((
                value(Duration::UntilEndOfTurn, tag("turn")),
                // CR 511.2: "this combat" scopes a grant or restriction to the
                // current combat — end-of-combat expiry.
                value(Duration::UntilEndOfCombat, tag("combat")),
            )),
        ),
        // CR 511.2: demonstrative "that combat" (grants referencing an
        // additional or identified combat phase) shares end-of-combat expiry.
        value(Duration::UntilEndOfCombat, tag("that combat")),
    ))
    .parse(input)
}

fn parse_controller_possessive_pronoun(input: &str) -> OracleResult<'_, PlayerScope> {
    // CR 109.5 + CR 608.2c: in this shared duration parser, "your" and
    // third-person "their" are both resolved by the caller's controller/grantee
    // binding; runtime pruning currently arms Controller-scoped durations.
    alt((
        value(PlayerScope::Controller, tag("your")),
        value(PlayerScope::Controller, tag("their")),
    ))
    .parse(input)
}

/// CR 611.2a + CR 500.4: `until [the beginning of ]<possessor> next <step>` —
/// the step-deadline duration production.
///
/// Two axes, composed rather than enumerated (CLAUDE.md "Compose nom
/// combinators, don't enumerate permutations"): `parse_next_step_possessor`
/// reads the possessor and `parse_next_step_name` reads the step. Both feed
/// `step_deadline_scope`, which is the single place that decides whether the
/// resulting pair has a runtime expiry authority whose scoping actually matches
/// the phrase — see its doc comment for why the pairing is not free. Every
/// emitted pair uses the existing [`Duration::UntilNextStepOf`] variant, so no
/// new engine surface is added.
fn parse_until_next_step(input: &str) -> OracleResult<'_, Duration> {
    // CR 503.1 + CR 513.1: "the beginning of your next upkeep" (Elkin Bottle,
    // Grinning Totem) names the same instant as the bare "your next upkeep"
    // form (Xenic Poltergeist) — `UntilNextStepOf` already expires when the
    // named step begins, so the prefix is a pure phrasing axis carrying no
    // additional semantics.
    let (rest, _) = opt(tag("the beginning of ")).parse(input)?;
    let (rest, possessor) = parse_next_step_possessor(rest)?;
    let (rest, _) = tag(" next ").parse(rest)?;
    let (rest, step) = parse_next_step_name(rest)?;
    let Some(player) = step_deadline_scope(possessor, step) else {
        return Err(oracle_err(input));
    };
    Ok((rest, Duration::UntilNextStepOf { step, player }))
}

/// The possessor of a step deadline, **as written**.
///
/// Deliberately distinct from the emitted [`PlayerScope`]: two different
/// phrasings both lower to `PlayerScope::Controller`, and the runtime resolves
/// that scope against *different* players depending on which prune owns the
/// step. Keeping the written form typed is what lets `step_deadline_scope`
/// reject a pairing whose authority would resolve it against the wrong player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepDeadlinePossessor {
    /// "your" / "their" — the controller of the ability that created the effect
    /// (CR 109.5).
    AbilityController,
    /// "its controller's" / "their controller's" — the controller of the object
    /// the effect is applied to (CR 109.4).
    ObjectController,
    /// "the" — no possessor. CR 611.2a: the effect lasts as long as *stated*,
    /// and this phrasing states a step without naming whose it is, so the
    /// deadline is the first occurrence of that step, whoever's turn it is.
    AnyTurn,
}

/// CR 611.2a + CR 500.4: map a written `(possessor, step)` pair onto the
/// [`PlayerScope`] the step's expiry authority actually implements, or `None`
/// when no authority implements that pairing.
///
/// CR 611.2a fixes *what* the duration is — the effect lasts as long as the
/// spell or ability states — and CR 500.4 fixes *when* it ends: as the named
/// step begins. The per-step rules below (CR 502.3 / CR 503.1 / CR 513.1) are
/// descriptive context identifying which step each prune owns; they are not the
/// authority for the expiry itself.
///
/// The pairing is **not** free, because `PlayerScope::Controller` is resolved
/// against two different players by the three prunes:
///
/// - `layers::prune_until_next_end_step_effects` (CR 513.1) and
///   `layers::prune_until_next_upkeep_effects` (CR 503.1) gate it on the
///   EFFECT's `controller` — which is what "your next end step / upkeep" means.
/// - `layers::prune_controller_untap_step_effects` (CR 502.3) gates it on the
///   AFFECTED OBJECT's controller (it inspects `TargetFilter::SpecificObject`)
///   — which is what "its controller's next untap step" means.
///
/// So "until your next untap step" and "until its controller's next end step"
/// would each be enforced against the wrong player. Rather than install an
/// effect that expires on someone else's step, decline: the clause then stays
/// visible and lowers to `Effect::unimplemented`, keeping coverage honest. No
/// card in the corpus prints either form today.
fn step_deadline_scope(possessor: StepDeadlinePossessor, step: Phase) -> Option<PlayerScope> {
    match (possessor, step) {
        (StepDeadlinePossessor::AbilityController, Phase::End | Phase::Upkeep) => {
            Some(PlayerScope::Controller)
        }
        (StepDeadlinePossessor::ObjectController, Phase::Untap) => Some(PlayerScope::Controller),
        // CR 611.2a: the stated duration names no player, so it is keyed on
        // none — all three prunes drop it at the first occurrence of the step
        // (CR 500.4).
        (StepDeadlinePossessor::AnyTurn, _) => Some(PlayerScope::AnyTurn),
        _ => None,
    }
}

/// CR 109.4 + CR 109.5 + CR 611.2a: the possessor axis of a step deadline —
/// object controller, ability controller, or (per the stated-duration reading
/// of CR 611.2a) none at all.
///
/// Ordered longest-discriminant-first: "their controller's" must be tried
/// before the bare "their" pronoun, or the pronoun arm shadows it and leaves an
/// unconsumable " controller's next …" remainder.
fn parse_next_step_possessor(input: &str) -> OracleResult<'_, StepDeadlinePossessor> {
    alt((
        value(
            StepDeadlinePossessor::ObjectController,
            parse_object_controller_possessive,
        ),
        value(
            StepDeadlinePossessor::AbilityController,
            parse_controller_possessive_pronoun,
        ),
        value(StepDeadlinePossessor::AnyTurn, tag("the")),
    ))
    .parse(input)
}

/// "its controller's" / "their controller's". The trailing possessive marker is
/// its own `alt()` axis so the curly-apostrophe and apostrophe-less spellings
/// present in the Oracle corpus are covered without enumerating pronoun ×
/// apostrophe pairs — the same factoring `oracle_casting::parse_opponent_possessive`
/// uses for "an opponent's".
fn parse_object_controller_possessive(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            alt((tag("its"), tag("their"))),
            tag(" controller"),
            alt((tag("\u{2019}s"), tag("'s"), tag("s"))),
        ),
    )
    .parse(input)
}

/// CR 502.1 + CR 503.1 + CR 513.1: the step axis of a step deadline.
///
/// Within a step, the longest spelling comes first so an optional " step"
/// suffix is consumed rather than left as a residual that would fail the
/// positional wrapper's whole-consumption check (`strip_trailing_duration`
/// requires the duration phrase to reach `eof`).
fn parse_next_step_name(input: &str) -> OracleResult<'_, Phase> {
    alt((
        // CR 513.1: the end step.
        value(Phase::End, tag("end step")),
        // CR 503.1: the upkeep step. Oracle text spells this both with and
        // without the explicit noun " step" ("until your next upkeep").
        value(Phase::Upkeep, alt((tag("upkeep step"), tag("upkeep")))),
        // CR 502.1: the untap step.
        value(Phase::Untap, tag("untap step")),
    ))
    .parse(input)
}

fn parse_until_end_of_next_turn(input: &str) -> OracleResult<'_, Duration> {
    // CR 514.2: "until the end of [your/their] next turn" persists through the
    // whole next turn (cleanup), distinct from "until [your/their] next turn"
    // (beginning of next turn). CR 108.3: third-person "their" appears in
    // grants whose grantee is not the ability's controller (Suspend
    // Aggression, Expedited Inheritance); the grantee binding resolves the
    // Controller scope at prune time.
    let (rest, _) = tag("the end of ").parse(input)?;
    let (rest, player) = parse_controller_possessive_pronoun(rest)?;
    let (rest, _) = tag(" next turn").parse(rest)?;
    Ok((rest, Duration::UntilEndOfNextTurnOf { player }))
}

fn parse_until_next_turn(input: &str) -> OracleResult<'_, Duration> {
    let (rest, player) = parse_controller_possessive_pronoun(input)?;
    let (rest, _) = tag(" next turn").parse(rest)?;
    Ok((rest, Duration::UntilNextTurnOf { player }))
}

/// CR 611.2b: map the condition text after "for as long as " to a `Duration`.
///
/// Mapping (ported verbatim from the legacy `strip_trailing_duration` table):
/// - compound "[a] and [b]" → `ForAsLongAs(And[..])`
/// - "[subject] remains tapped" → `ForAsLongAs(SourceIsTapped)` for source
///   subjects, `ForAsLongAs(IsTapped { scope: Target })` for demonstrative
///   subjects (see `parse_remains_tapped`)
/// - "you control [subject]" → `WhileControllingHost`
/// - "[subject] remains on the battlefield" → `WhileHostOnBattlefield`
/// - "[subject] has [N] [type] counter(s) on it" → `ForAsLongAs(HasCounters)`
/// - any whole-clause condition `parse_inner_condition` recognizes →
///   `ForAsLongAs(condition)`
/// - otherwise → `ForAsLongAs(Unrecognized)` (coverage parity with the legacy
///   strip table; the swallow detectors flag the unrecognized text)
///
/// Clause-final: every arm consumes the remainder of its input — the phrase
/// sits at the trailing edge of an effect clause in Oracle text.
pub fn parse_for_as_long_as_condition(input: &str) -> OracleResult<'_, Duration> {
    alt((
        parse_compound_for_as_long_as,
        // "[subject] remains tapped" — the grammatical subject selects the
        // tracked object's scope. Demonstrative subjects ("that creature") bind
        // the duration to the copy/control TARGET; source subjects ("~", "this
        // creature", bare card names) bind to the source. See
        // `parse_remains_tapped`.
        parse_remains_tapped,
        // CR 611.2b + CR 301.5: an Equipment's "for as long as ~ remains
        // attached to it" duration follows the creature that received the
        // effect. `AttachedTo` is evaluated source-relatively each layer pass,
        // so the duration ends as soon as the Equipment leaves that creature.
        parse_remains_attached_to_it,
        // CR 311.2 + CR 901.7 + CR 611.2b: "[this plane] remains face up" — the
        // plane-face-up-gated continuous-effect duration. Kept adjacent to
        // `parse_remains_tapped` (the sibling source-status "remains X" family).
        parse_remains_face_up,
        // CR 611.2b: "you control [subject]" → the CONTROL-bound host lifetime.
        // Kept distinct from the presence-bound reading below because the two
        // end at different moments and both are printed: a control change with
        // the permanent still on the battlefield ends this one (CR 611.2b's own
        // Master Thief example is this duration CLASS — it illustrates the
        // duration failing to START, not this end, which is read off the
        // wording) and leaves the other running.
        value(
            Duration::WhileControllingHost,
            preceded(tag("you control "), rest),
        ),
        // CR 611.2b + CR 702.26f: "[subject] remains on the battlefield" → the
        // PRESENCE-bound host lifetime, a stated "for as long as . . ."
        // duration. Intet, the Dreamer and The Day of the Doctor print this
        // wording on a play permission, Sower of Temptation on a control
        // effect. A control change does not end it; a phase-out of the host
        // does, which is why it is a separate variant from the
        // `UntilHostLeavesPlay` event deadline parsed in `parse_duration`
        // (CR 702.26d: a phase-out is not the host leaving the battlefield).
        value(
            Duration::WhileHostOnBattlefield,
            verify(rest, |tail: &str| {
                scan_contains(tail, "remains on the battlefield")
            }),
        ),
        // CR 122.1 + CR 611.2b: "[subject] has [N] [type] counter[s] on it" —
        // delegate to the recipient-aware counter-condition combinator so the
        // bound pronoun "it" in "for as long as it has a counter" binds to the
        // affected object (the controlled/granted creature), evaluated by the
        // layer system. A source subject ("~"/"this creature") stays
        // `HasCounters`. The typed/bare/quantity grammar lives in one authority.
        map(
            terminated(parse_recipient_has_counters, (multispace0, eof)),
            |condition| Duration::ForAsLongAs { condition },
        ),
        // Any whole-clause condition the shared condition grammar recognizes.
        map(
            terminated(parse_inner_condition, (multispace0, eof)),
            |condition| Duration::ForAsLongAs { condition },
        ),
        // Fallback: unrecognized condition text.
        map(rest, |text: &str| Duration::ForAsLongAs {
            condition: StaticCondition::Unrecognized {
                text: text.trim().trim_end_matches('.').to_string(),
            },
        }),
    ))
    .parse(input)
}

fn parse_remains_attached_to_it(input: &str) -> OracleResult<'_, Duration> {
    value(
        Duration::ForAsLongAs {
            condition: StaticCondition::RecipientMatchesFilter {
                filter: TargetFilter::AttachedTo,
            },
        },
        (
            parse_self_reference_subject,
            tag(" remains attached to it"),
            rest,
        ),
    )
    .parse(input)
}

/// CR 110.5b + CR 611.2b: subject-aware "[subject] remains tapped" duration.
///
/// The grammatical subject of "remains tapped" selects which object's tap state
/// the `ForAsLongAs` duration tracks:
///
/// - **Demonstrative subjects** ("that creature/permanent/artifact") → the
///   copy/control TARGET. Emits `IsTapped { scope: Target }` so the resolver
///   (`become_copy.rs`) binds the duration to the resolved target object (Zygon
///   Infiltrator: "as long as THAT creature remains tapped" tracks the copied
///   creature, not Zygon). This tier is tried FIRST.
///
/// The anaphoric pronoun "it" is deliberately NOT in the demonstrative set: its
/// referent is clause-context-dependent ("you control ~ and it remains tapped"
/// → "it" is the source; "tap another target permanent. Its abilities can't be
/// activated for as long as it remains tapped" → "it" is the target). The
/// duration combinator has no clause subject to disambiguate, so "it" falls to
/// the source fallback (the pre-existing behavior), preserving every "it
/// remains tapped" card's current parse. The only cluster card needing the
/// target binding (Zygon) uses the unambiguous "that creature".
/// - **Source subjects** ("~", "this creature", a `SELF_REF_TYPE_PHRASES`
///   self-reference, or a bare card name like "The Blackstaff of Waterdeep") →
///   the source. Emits `SourceIsTapped`. The explicit self-reference `alt()`
///   plus the retained `scan_contains` word-boundary fallback covers every
///   source phrasing, including proper names beginning with "The".
///
/// Demonstrative dispatch MUST precede the source fallback so a proper-name card
/// is never misread as a target subject and "that creature" is never swallowed
/// by the source `scan_contains` scan. The closed demonstrative `alt()`
/// deliberately excludes any "the " arm — a leading "The" belongs to a card name
/// (source), not a demonstrative. Compound cards (Hivis, Rubinia) are split by
/// `parse_compound_for_as_long_as` before this combinator runs, so each side
/// re-enters here and lands on the source fallback.
fn parse_remains_tapped(input: &str) -> OracleResult<'_, Duration> {
    // Each tier is clause-final: a trailing `rest` consumes any remainder after
    // "[subject] remains tapped" so the arm behaves like the legacy `verify(rest,
    // ..)` arm (the phrase sits at the trailing edge of the effect clause).
    //
    // Tier 1: demonstrative subject → target-relative tap state.
    let demonstrative = value(
        Duration::ForAsLongAs {
            condition: StaticCondition::IsTapped {
                scope: ObjectScope::Target,
            },
        },
        (
            alt((
                tag("that creature"),
                tag("that permanent"),
                tag("that artifact"),
            )),
            tag(" remains tapped"),
            rest,
        ),
    );

    // Tier 2a: explicit source self-reference → source-relative tap state.
    let source_self_ref = value(
        Duration::ForAsLongAs {
            condition: StaticCondition::SourceIsTapped,
        },
        (parse_self_reference_subject, tag(" remains tapped"), rest),
    );

    // Tier 2b: any other source phrasing (proper card names like "The Blackstaff
    // of Waterdeep", compound-clause remnants) → source. Word-boundary scan, not
    // a dispatch primitive, and only reached after demonstrative dispatch fails.
    let source_fallback = value(
        Duration::ForAsLongAs {
            condition: StaticCondition::SourceIsTapped,
        },
        verify(rest, |tail: &str| scan_contains(tail, "remains tapped")),
    );

    alt((demonstrative, source_self_ref, source_fallback)).parse(input)
}

/// CR 311.2 + CR 901.7 + CR 611.2b: subject-aware "[this plane] remains face up"
/// duration, mirroring `parse_remains_tapped`. Plane/phenomenon cards are always
/// the source subject of their own face-up duration, so the referent normalizes
/// to `~` (or a `SELF_REF_TYPE_PHRASES` self-reference). Emits
/// `ForAsLongAs(SourceIsFaceUp)`; the layer system evaluates it against the
/// command-zone active plane (`planechase::active_plane`), so the effect ends the
/// instant the plane is planeswalked away and turned face down (CR 701.31b).
///
/// The Doctor's Childhood Barn ("They can't phase in for as long as ~ remains
/// face up") is the source-subject witness; the `scan_contains` fallback catches
/// any residual proper-name phrasing the same way the tapped fallback does.
fn parse_remains_face_up(input: &str) -> OracleResult<'_, Duration> {
    // Tier 1: explicit source self-reference ("~", "this creature", …).
    let source_self_ref = value(
        Duration::ForAsLongAs {
            condition: StaticCondition::SourceIsFaceUp,
        },
        (parse_self_reference_subject, tag(" remains face up"), rest),
    );

    // Tier 2: any other source phrasing (proper card names, compound remnants).
    // Word-boundary scan, not a dispatch primitive, reached only after the
    // self-reference tier fails.
    let source_fallback = value(
        Duration::ForAsLongAs {
            condition: StaticCondition::SourceIsFaceUp,
        },
        verify(rest, |tail: &str| scan_contains(tail, "remains face up")),
    );

    alt((source_self_ref, source_fallback)).parse(input)
}

/// Source self-reference subject combinator: "~" or any `SELF_REF_TYPE_PHRASES`
/// phrase ("this creature", "this permanent", …). Iterates the shared
/// self-reference constant — the single authority for source self-references —
/// so no card-specific phrasing leaks in. (`SELF_REF_TYPE_PHRASES` is a runtime
/// slice, so the closed set is folded by iteration rather than a fixed `alt()`
/// tuple; each candidate is still matched with the nom `tag()` combinator.)
fn parse_self_reference_subject(input: &str) -> OracleResult<'_, ()> {
    for phrase in std::iter::once(&"~").chain(crate::parser::oracle_util::SELF_REF_TYPE_PHRASES) {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(*phrase).parse(input) {
            return Ok((rest, ()));
        }
    }
    // No self-reference subject matched — surface a recoverable nom error so the
    // outer `alt()` falls through to the `scan_contains` source fallback.
    Err(oracle_err(input))
}

/// Compound "for as long as [a] and [b]" → `ForAsLongAs(And[..])`.
fn parse_compound_for_as_long_as(input: &str) -> OracleResult<'_, Duration> {
    let (right, left) = terminated(take_until(" and "), tag(" and ")).parse(input)?;
    let (_, left_dur) = parse_for_as_long_as_condition(left.trim())?;
    let (_, right_dur) = parse_for_as_long_as_condition(right.trim())?;
    Ok((
        "",
        Duration::ForAsLongAs {
            condition: StaticCondition::And {
                conditions: vec![
                    duration_to_condition(left_dur),
                    duration_to_condition(right_dur),
                ],
            },
        },
    ))
}

/// Convert a `Duration` back into a `StaticCondition` for compound "and"
/// clauses. The host-lifetime readings map to `IsPresent { filter: None }` —
/// which is NOT a runtime presence test (it evaluates to `true`; the arm's
/// comment below says exactly what it is and is not).
fn duration_to_condition(dur: Duration) -> StaticCondition {
    match dur {
        Duration::ForAsLongAs { condition } => condition,
        // CR 611.2a + CR 611.2b: all three host-lifetime readings map to the
        // value this conversion produced for the "you control ~" wording
        // BEFORE the readings were split. The explicit arm exists for parity:
        // the compound "and" form must not change because of either split.
        //
        // What it is NOT: a presence test. `IsPresent { filter: None }`
        // evaluates to `true` (`layers::evaluate_condition_with_context`), the
        // same answer the `_` arm's `StaticCondition::None` gives; the two
        // differ only in which characteristic changes re-invalidate the layer
        // cache (`CONTROLLER` vs. nothing). Neither carries the host leg, and
        // the control leg cannot be carried at all here —
        // `StaticCondition::SourceControllerEquals` stores a concrete
        // `PlayerId` and the parser has no player.
        //
        // Printed compounds reaching here — Helm of Possession, Hivis of the
        // Scale, Rubinia Soulsinger, Seasinger, Willow Satyr — all pair the
        // wording with "… and ~ remains tapped", and THAT leg is carried
        // exactly, so each of them stays gated on its tapped condition.
        Duration::UntilHostLeavesPlay
        | Duration::WhileControllingHost
        | Duration::WhileHostOnBattlefield => StaticCondition::IsPresent { filter: None },
        _ => StaticCondition::None,
    }
}

/// Parse an optional trailing duration: returns `Some(Duration)` if present,
/// `None` if no duration phrase follows. Does NOT consume leading whitespace.
pub fn parse_optional_duration(input: &str) -> OracleResult<'_, Option<Duration>> {
    match parse_duration(input) {
        Ok((rest, d)) => Ok((rest, Some(d))),
        Err(_) => Ok((input, None)),
    }
}

/// CR 608.2h + CR 608.2i: the cast/activation-time value-snapshot suffix.
/// CR 608.2h fixes a computed value once when the effect is applied; CR 608.2i
/// is the past-tense ("you controlled") look-back exception sharing this
/// grammar. The suffix is a pure timing marker — it does not change the object
/// filter — so callers strip it before the empty-remainder filter check and let
/// the resolver perform the snapshot.
pub fn parse_cast_snapshot_suffix(input: &str) -> OracleResult<'_, ()> {
    preceded(
        opt(tag(" ")),
        value(
            (),
            alt((
                tag("as you cast this spell"),
                tag("as you cast it"),
                tag("as you activate this ability"),
            )),
        ),
    )
    .parse(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::StaticCondition;

    #[test]
    fn test_parse_duration_end_of_turn() {
        let (rest, d) = parse_duration("until end of turn.").unwrap();
        assert_eq!(d, Duration::UntilEndOfTurn);
        assert_eq!(rest, ".");
    }

    #[test]
    fn test_parse_duration_end_of_combat() {
        let (rest, d) = parse_duration("until end of combat").unwrap();
        assert_eq!(d, Duration::UntilEndOfCombat);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_duration_next_turn() {
        let (rest, d) = parse_duration("until your next turn and").unwrap();
        assert_eq!(
            d,
            Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            }
        );
        assert_eq!(rest, " and");
    }

    #[test]
    fn test_parse_duration_until_end_of_next_turn() {
        let (rest, d) = parse_duration("until the end of their next turn.").unwrap();
        assert_eq!(
            d,
            Duration::UntilEndOfNextTurnOf {
                player: PlayerScope::Controller,
            }
        );
        assert_eq!(rest, ".");
    }

    #[test]
    fn test_parse_duration_their_next_turn() {
        let (rest, d) = parse_duration("until their next turn.").unwrap();
        assert_eq!(
            d,
            Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            }
        );
        assert_eq!(rest, ".");
    }

    #[test]
    fn test_parse_duration_this_turn() {
        let (rest, d) = parse_duration("this turn.").unwrap();
        assert_eq!(d, Duration::UntilEndOfTurn);
        assert_eq!(rest, ".");
    }

    #[test]
    fn test_parse_duration_for_as_long_as() {
        let (rest, d) = parse_duration("for as long as ~ is tapped").unwrap();
        assert_eq!(rest, "");
        match d {
            Duration::ForAsLongAs { condition } => {
                assert!(matches!(condition, StaticCondition::SourceIsTapped));
            }
            _ => panic!("expected ForAsLongAs"),
        }
    }

    #[test]
    fn test_parse_duration_for_the_rest_of_the_game() {
        let (rest, d) = parse_duration("for the rest of the game.").unwrap();
        assert_eq!(d, Duration::Permanent);
        assert_eq!(rest, ".");
    }

    #[test]
    fn test_parse_duration_until_host_leaves_battlefield() {
        for text in [
            "until ~ leaves the battlefield",
            "until this creature leaves the battlefield",
        ] {
            let (rest, d) = parse_duration(text).unwrap();
            assert_eq!(d, Duration::UntilHostLeavesPlay, "failed for {text:?}");
            assert_eq!(rest, "");
        }
    }

    #[test]
    fn test_parse_duration_until_source_exiles_another_card() {
        for text in [
            "until you exile another card with ~",
            "until you exile another card with this ability",
            "until you exile another card with this enchantment",
        ] {
            let (rest, d) = parse_duration(text).unwrap();
            assert_eq!(
                d,
                Duration::UntilSourceExilesAnotherCard,
                "failed for {text:?}"
            );
            assert_eq!(rest, "");
        }
    }

    #[test]
    fn test_parse_duration_this_and_that_combat() {
        for text in ["this combat", "that combat"] {
            let (rest, d) = parse_duration(text).unwrap();
            assert_eq!(d, Duration::UntilEndOfCombat, "failed for {text:?}");
            assert_eq!(rest, "");
        }
    }

    #[test]
    fn test_parse_duration_this_combat_if_able_leaves_remainder() {
        let (rest, d) = parse_duration("this combat if able").unwrap();
        assert_eq!(d, Duration::UntilEndOfCombat);
        assert_eq!(rest, " if able");
    }

    #[test]
    fn test_parse_duration_until_your_next_end_step() {
        let (rest, d) = parse_duration("until your next end step, ").unwrap();
        assert_eq!(
            d,
            Duration::UntilNextStepOf {
                step: Phase::End,
                player: PlayerScope::Controller,
            }
        );
        assert_eq!(rest, ", ");
    }

    // ---- "until [the beginning of] <possessor> next <step>" (CR 500.4) ----

    /// CR 503.1: the bare-noun upkeep spelling. Verbatim clause tails from
    /// Xenic Poltergeist / Erhnam Djinn / Gabriel Angelfire / Cycle of Life /
    /// Spatial Binding (7 cards share this exact phrase), plus the explicit
    /// " step" noun the grammar also accepts.
    #[test]
    fn test_parse_duration_until_your_next_upkeep() {
        for text in [
            "until your next upkeep",
            "until their next upkeep",
            "until your next upkeep step",
        ] {
            let (rest, d) = parse_duration(text).unwrap();
            assert_eq!(rest, "", "failed for {text:?}");
            assert_eq!(
                d,
                Duration::UntilNextStepOf {
                    step: Phase::Upkeep,
                    player: PlayerScope::Controller,
                },
                "failed for {text:?}",
            );
        }
    }

    /// CR 503.1: "the beginning of" is a phrasing axis only — Elkin Bottle and
    /// Grinning Totem name the same instant as the bare form above, because
    /// `UntilNextStepOf` already expires as the named step begins (CR 500.4).
    #[test]
    fn test_parse_duration_until_the_beginning_of_your_next_upkeep() {
        let (rest, d) = parse_duration("until the beginning of your next upkeep, ").unwrap();
        assert_eq!(rest, ", ");
        assert_eq!(
            d,
            Duration::UntilNextStepOf {
                step: Phase::Upkeep,
                player: PlayerScope::Controller,
            }
        );
    }

    /// CR 502.3: Orcish Farmer — "until its controller's next untap step".
    /// The possessor names the AFFECTED object's controller, which
    /// `layers::prune_controller_untap_step_effects` already reads off
    /// `PlayerScope::Controller` for this `UntilNextStepOf` shape. Both the
    /// "its"/"their" pronouns and the curly-apostrophe / apostrophe-less
    /// spellings resolve to the same scope.
    #[test]
    fn test_parse_duration_until_object_controllers_next_untap_step() {
        for text in [
            "until its controller's next untap step",
            "until their controller's next untap step",
            "until its controller\u{2019}s next untap step",
            "until its controllers next untap step",
        ] {
            let (rest, d) = parse_duration(text).unwrap();
            assert_eq!(rest, "", "failed for {text:?}");
            assert_eq!(
                d,
                Duration::UntilNextStepOf {
                    step: Phase::Untap,
                    player: PlayerScope::Controller,
                },
                "failed for {text:?}",
            );
        }
    }

    /// Regression: factoring the possessor × step axes must leave the two
    /// phrasings that previously had their own hardcoded arms bit-identical —
    /// possessive "your next end step" (Rocco, Street Chef) stays
    /// `Controller`, definite-article "the next end step" (Niko, Light of
    /// Hope) stays turn-agnostic `AnyTurn` (CR 611.2a: the stated duration
    /// names a step but no player).
    #[test]
    fn test_parse_duration_end_step_possessor_axis_unchanged() {
        for (text, player) in [
            ("until your next end step", PlayerScope::Controller),
            ("until their next end step", PlayerScope::Controller),
            ("until the next end step", PlayerScope::AnyTurn),
        ] {
            let (rest, d) = parse_duration(text).unwrap();
            assert_eq!(rest, "", "failed for {text:?}");
            assert_eq!(
                d,
                Duration::UntilNextStepOf {
                    step: Phase::End,
                    player,
                },
                "failed for {text:?}",
            );
        }
    }

    /// The step axis is a CLOSED set — only the steps with a runtime expiry
    /// authority (CR 502.3 untap, CR 503.1 upkeep, CR 513.1 end) are accepted.
    /// A draw/main/combat-damage deadline has no prune, so accepting it would
    /// silently install a never-expiring effect; it must stay unparsed so the
    /// clause reaches the honest `Effect::unimplemented` fallback instead.
    #[test]
    fn test_parse_duration_rejects_steps_without_a_runtime_deadline() {
        for text in [
            "until your next draw step",
            "until your next main phase",
            "until your next combat damage step",
        ] {
            assert!(
                parse_duration(text).is_err(),
                "{text:?} must not parse — no runtime expiry authority exists for it"
            );
        }
    }

    /// The possessor axis must not swallow a TURN deadline it cannot model:
    /// "until its controller's next turn" needs a target-relative
    /// `UntilNextTurnOf`, which has no runtime authority, so the step
    /// production must fail rather than mis-bind it to a step.
    #[test]
    fn test_parse_duration_rejects_object_controllers_next_turn() {
        assert!(parse_duration("until its controller's next turn").is_err());
        assert!(parse_duration("until that player's next end step").is_err());
    }

    /// CR 500.4: the possessor × step pairing is NOT free — `step_deadline_scope`
    /// declines a pair whose expiry authority resolves `PlayerScope::Controller`
    /// against a different player than the phrase names. Both directions must
    /// fail so the clause stays honestly `Effect::unimplemented` rather than
    /// expiring on the wrong player's step. (Neither form is printed on any card
    /// today; this pins the decision so a future step/possessor addition cannot
    /// silently open a mis-scoped pairing.)
    #[test]
    fn test_parse_duration_rejects_mismatched_possessor_step_pairings() {
        // The untap prune keys on the AFFECTED OBJECT's controller, so an
        // ability-controller possessive cannot use it.
        assert!(parse_duration("until your next untap step").is_err());
        // The end-step / upkeep prunes key on the EFFECT's controller, so an
        // object-controller possessive cannot use them.
        assert!(parse_duration("until its controller's next end step").is_err());
        assert!(parse_duration("until its controller's next upkeep").is_err());
    }

    /// The turn-agnostic possessor pairs with every step in the closed set,
    /// because no prune keys `PlayerScope::AnyTurn` on a player — CR 611.2a's
    /// stated duration names the step without naming whose it is.
    #[test]
    fn test_parse_duration_turn_agnostic_possessor_pairs_with_every_step() {
        for (text, step) in [
            ("until the next end step", Phase::End),
            ("until the next upkeep", Phase::Upkeep),
            ("until the next untap step", Phase::Untap),
        ] {
            let (rest, d) = parse_duration(text).unwrap();
            assert_eq!(rest, "", "failed for {text:?}");
            assert_eq!(
                d,
                Duration::UntilNextStepOf {
                    step,
                    player: PlayerScope::AnyTurn,
                },
                "failed for {text:?}",
            );
        }
    }

    /// CR 611.2a vs CR 611.2b: the two host-lifetime wordings are DIFFERENT
    /// durations and must not collapse onto one variant.
    ///
    /// All three are printed on play permissions or effects, so the differences
    /// are observable at runtime rather than cosmetic: Gwen Stacy and Hama, the
    /// Bloodbender print "for as long as you control ~" (ends on a control
    /// change, CR 611.2b's own Master Thief example); Intet, the Dreamer and
    /// The Day of the Doctor print "for as long as ~ remains on the
    /// battlefield" (CR 611.2b — a control change leaves it running, a
    /// phase-out ends it, CR 702.26f); the event deadline "until ~ leaves the
    /// battlefield" is ended by neither a control change nor a phase-out
    /// (CR 702.26d).
    ///
    /// Revert-to-red: mapping "you control" back onto a presence variant fails
    /// the first assertion; collapsing "remains on the battlefield" back into
    /// the event deadline fails the second and would keep Sower of
    /// Temptation's steal running across a phase-out; mapping it onto the
    /// control-bound variant would revoke Intet's permission on a control
    /// change that does not end it.
    #[test]
    fn test_the_three_host_lifetime_wordings_do_not_collapse() {
        let (rest, control_bound) = parse_duration("for as long as you control ~").unwrap();
        assert_eq!(rest, "");
        assert_eq!(control_bound, Duration::WhileControllingHost);

        let (rest, presence_bound) =
            parse_duration("for as long as ~ remains on the battlefield").unwrap();
        assert_eq!(rest, "");
        assert_eq!(presence_bound, Duration::WhileHostOnBattlefield);

        let (rest, event_bound) = parse_duration("until ~ leaves the battlefield").unwrap();
        assert_eq!(rest, "");
        assert_eq!(event_bound, Duration::UntilHostLeavesPlay);

        assert_ne!(
            control_bound, presence_bound,
            "CR 611.2b: continued control and continued presence end at different \
             moments; one variant cannot carry both"
        );
        assert_ne!(
            presence_bound, event_bound,
            "CR 702.26f vs CR 702.26d: a phase-out ends the presence reading but \
             not the event deadline; one variant cannot carry both"
        );
        // All three still end when the host leaves the battlefield — that leg
        // is shared, and every battlefield-exit consumer asks this predicate.
        assert!(control_bound.ends_when_host_leaves_play());
        assert!(presence_bound.ends_when_host_leaves_play());
        assert!(event_bound.ends_when_host_leaves_play());
    }

    #[test]
    fn test_for_as_long_as_compound_control_and_tapped() {
        let (rest, d) =
            parse_duration("for as long as you control ~ and it remains tapped").unwrap();
        assert_eq!(rest, "");
        match d {
            Duration::ForAsLongAs {
                condition: StaticCondition::And { conditions },
            } => {
                assert_eq!(conditions.len(), 2);
                assert!(matches!(
                    conditions[0],
                    StaticCondition::IsPresent { filter: None }
                ));
                assert!(matches!(conditions[1], StaticCondition::SourceIsTapped));
            }
            other => panic!("expected ForAsLongAs(And[..]), got {other:?}"),
        }
    }

    #[test]
    fn test_for_as_long_as_unrecognized_fallback() {
        let (rest, d) = parse_duration("for as long as the moon is full").unwrap();
        assert_eq!(rest, "");
        match d {
            Duration::ForAsLongAs {
                condition: StaticCondition::Unrecognized { text },
            } => assert_eq!(text, "the moon is full"),
            other => panic!("expected ForAsLongAs(Unrecognized), got {other:?}"),
        }
    }

    #[test]
    fn test_parse_optional_duration_present() {
        let (rest, d) = parse_optional_duration("until end of turn.").unwrap();
        assert_eq!(d, Some(Duration::UntilEndOfTurn));
        assert_eq!(rest, ".");
    }

    #[test]
    fn test_parse_optional_duration_absent() {
        let (rest, d) = parse_optional_duration("and draws a card").unwrap();
        assert_eq!(d, None);
        assert_eq!(rest, "and draws a card");
    }

    #[test]
    fn test_parse_duration_failure() {
        assert!(parse_duration("permanently").is_err());
    }

    #[test]
    fn test_cast_snapshot_suffix_cast_this_spell_leading_space() {
        assert_eq!(
            parse_cast_snapshot_suffix(" as you cast this spell"),
            Ok(("", ()))
        );
    }

    #[test]
    fn test_cast_snapshot_suffix_cast_it_leading_space() {
        assert_eq!(parse_cast_snapshot_suffix(" as you cast it"), Ok(("", ())));
    }

    #[test]
    fn test_cast_snapshot_suffix_activate_ability_leading_space() {
        assert_eq!(
            parse_cast_snapshot_suffix(" as you activate this ability"),
            Ok(("", ()))
        );
    }

    #[test]
    fn test_cast_snapshot_suffix_no_leading_space() {
        assert_eq!(
            parse_cast_snapshot_suffix("as you cast this spell"),
            Ok(("", ()))
        );
    }

    #[test]
    fn test_cast_snapshot_suffix_rejects_duration() {
        assert!(parse_cast_snapshot_suffix(" until end of turn").is_err());
    }

    #[test]
    fn test_cast_snapshot_suffix_rejects_empty() {
        assert!(parse_cast_snapshot_suffix("").is_err());
    }

    #[test]
    fn test_cast_snapshot_suffix_trailing_period() {
        assert_eq!(
            parse_cast_snapshot_suffix(" as you cast this spell."),
            Ok((".", ()))
        );
    }

    // ---- "remains tapped" subject-aware duration (CR 110.5b + CR 611.2b) ----

    /// Demonstrative subject ("that creature") → target-relative tap state. This
    /// is the Zygon Infiltrator copy-duration class: the duration must track the
    /// copied creature (the target), not the source.
    #[test]
    fn test_remains_tapped_demonstrative_binds_target() {
        for subject in ["that creature", "that permanent", "that artifact"] {
            let text = format!("for as long as {subject} remains tapped");
            let (rest, d) = parse_duration(&text).unwrap();
            assert_eq!(rest, "", "failed for {subject:?}");
            assert_eq!(
                d,
                Duration::ForAsLongAs {
                    condition: StaticCondition::IsTapped {
                        scope: ObjectScope::Target,
                    },
                },
                "demonstrative subject {subject:?} must bind the target",
            );
        }
    }

    /// Source self-reference ("this creature"/"~") → source-relative tap state
    /// (`SourceIsTapped`) — the 41-card source-subject majority of the class.
    #[test]
    fn test_remains_tapped_self_reference_binds_source() {
        for subject in ["~", "this creature", "this artifact", "this permanent"] {
            let text = format!("for as long as {subject} remains tapped");
            let (rest, d) = parse_duration(&text).unwrap();
            assert_eq!(rest, "", "failed for {subject:?}");
            assert_eq!(
                d,
                Duration::ForAsLongAs {
                    condition: StaticCondition::SourceIsTapped,
                },
                "self-reference subject {subject:?} must bind the source",
            );
        }
    }

    #[test]
    fn test_remains_attached_to_it_binds_recipient() {
        for subject in ["~", "this equipment", "this artifact", "this permanent"] {
            let text = format!("for as long as {subject} remains attached to it");
            let (rest, duration) = parse_duration(&text).unwrap();
            assert_eq!(rest, "", "failed for {subject:?}");
            assert_eq!(
                duration,
                Duration::ForAsLongAs {
                    condition: StaticCondition::RecipientMatchesFilter {
                        filter: TargetFilter::AttachedTo,
                    },
                },
                "attachment duration for {subject:?} must follow the recipient",
            );
        }
    }

    /// CR 311.2 + CR 901.7: "for as long as [this plane] remains face up" → the
    /// source plane's face-up status (`SourceIsFaceUp`). The normalized `~` self-
    /// reference (The Doctor's Childhood Barn) and any self-ref phrasing bind the
    /// source; a leading-`The` proper name hits the `scan_contains` fallback.
    #[test]
    fn test_remains_face_up_binds_source() {
        for subject in ["~", "this permanent", "The Doctor's Childhood Barn"] {
            let text = format!("for as long as {subject} remains face up");
            let (rest, d) = parse_duration(&text).unwrap();
            assert_eq!(rest, "", "failed for {subject:?}");
            assert_eq!(
                d,
                Duration::ForAsLongAs {
                    condition: StaticCondition::SourceIsFaceUp,
                },
                "face-up subject {subject:?} must bind the source plane",
            );
        }
    }

    /// The bare condition text arriving at `parse_for_as_long_as_condition`
    /// (post-"for as long as " strip, normalized to `~`) resolves to
    /// `SourceIsFaceUp`, NOT the `Unrecognized` fallback that left the Barn's
    /// "can't phase in" lock permanently active.
    #[test]
    fn test_for_as_long_as_condition_face_up_not_unrecognized() {
        let (_, d) = parse_for_as_long_as_condition("~ remains face up").unwrap();
        assert_eq!(
            d,
            Duration::ForAsLongAs {
                condition: StaticCondition::SourceIsFaceUp,
            },
        );
    }

    /// Proper-name regression: a card name beginning with "The" is a SOURCE
    /// subject and must NOT be misread as a demonstrative target despite the
    /// leading "The" (Animate Walking Statue → The Blackstaff of Waterdeep; The
    /// Pandorica). Guards the closed demonstrative `alt()` from a "the " leak.
    #[test]
    fn test_remains_tapped_proper_name_binds_source() {
        for subject in ["The Blackstaff of Waterdeep", "The Pandorica"] {
            let text = format!("for as long as {subject} remains tapped");
            let (rest, d) = parse_duration(&text).unwrap();
            assert_eq!(rest, "", "failed for {subject:?}");
            assert_eq!(
                d,
                Duration::ForAsLongAs {
                    condition: StaticCondition::SourceIsTapped,
                },
                "proper-name subject {subject:?} must stay source-bound",
            );
        }
    }

    /// Compound regression (Hivis / Rubinia): "you control X and X remains
    /// tapped" splits on " and " and each side re-enters the combinator; the
    /// "X remains tapped" side hits the source fallback → `SourceIsTapped`. The
    /// new demonstrative tier must not perturb the compound path.
    #[test]
    fn test_remains_tapped_compound_card_name_binds_source() {
        for name in ["rubinia soulsinger", "hivis"] {
            let text = format!("for as long as you control {name} and {name} remains tapped");
            let (rest, d) = parse_duration(&text).unwrap();
            assert_eq!(rest, "", "failed for {name:?}");
            match d {
                Duration::ForAsLongAs {
                    condition: StaticCondition::And { conditions },
                } => {
                    assert_eq!(conditions.len(), 2, "failed for {name:?}");
                    assert!(
                        matches!(conditions[0], StaticCondition::IsPresent { filter: None }),
                        "control side for {name:?}",
                    );
                    assert!(
                        matches!(conditions[1], StaticCondition::SourceIsTapped),
                        "tapped side for {name:?} must be source-bound",
                    );
                }
                other => panic!("expected ForAsLongAs(And[..]) for {name:?}, got {other:?}"),
            }
        }
    }

    /// Anaphoric "it" stays source-bound (the pre-existing behavior): "it" is
    /// excluded from the demonstrative set because its referent is clause-context
    /// dependent. This pins the decision so a future edit cannot silently move
    /// "it" into the target-binding tier and regress the compound-control class.
    #[test]
    fn test_remains_tapped_it_stays_source() {
        let (rest, d) = parse_duration("for as long as it remains tapped").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            d,
            Duration::ForAsLongAs {
                condition: StaticCondition::SourceIsTapped,
            },
        );
    }
}
