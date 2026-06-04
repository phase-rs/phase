//! Recursive structural-slot peeling for clause text.
//!
//! Phase 2 of the no-text-swallowing refactor (see
//! `data/parser-swallow-progress.md`). The parser's body parsers were
//! historically responsible for recognizing AND consuming structural slots
//! (optional, condition, duration, etc.) inline. When a body parser matched
//! its core verb but ignored a surrounding structural slot, the slot was
//! silently dropped — the swallowing problem.
//!
//! The shell inverts that responsibility. `peel_clause` strips structural
//! slots off the head of a clause, accumulating them into a `ClauseContext`
//! (synthesized attributes). The peeled bare imperative is then handed to
//! the existing body parsers, and the accumulated context is applied onto
//! the resulting `AbilityDefinition`/`ParsedEffectClause` before return.
//!
//! ## Migrated slots
//!
//! - **Optional** (`you may [verb]` → `optional: true`)
//! - **Duration** (`... this turn` / `... until end of turn` /
//!   `... until your next turn` / `... for as long as ~ remains tapped` etc.
//!   → `Duration` variant)
//!
//! Every other slot continues to flow through its existing per-callsite
//! handling until a follow-up commit migrates it here.
//!
//! As slots migrate, each becomes:
//! 1. A new field on `ClauseContext`.
//! 2. A new branch in `peel_inner`.
//! 3. A new `apply_*` method on `ClauseContext`.
//! 4. Removal of the corresponding linear `strip_*` helper at its call
//!    sites.
//!
//! ## Specialized-phrase blocklist (CR 608.2d)
//!
//! "you may " in Oracle text isn't always a generic optional-effect modal.
//! Several specialized constructions share the same prefix. The disambiguation
//! is on the BODY SHAPE after the verb, not the verb itself — a bare
//! imperative like "you may reveal that card and put it into your hand" must
//! peel as a generic optional effect, while "you may reveal a creature card
//! from your hand" must NOT peel because `RevealFromHand` needs the full
//! surface form to dispatch.
//!
//! Head-only specialized phrases (always block on these verb heads):
//! - `you may pay {X} rather than ...` — alternative cost
//! - `you may cast ... as though ...` — static permission grant
//! - `you may play that card ...` — impulse draw permission
//! - `you may have it [verb] ...` — causative construction
//! - `you may choose new targets for ...` — retarget effect
//! - `you may instead ...` — Dig alternative selection
//! - `you may repeat this process` — directive (no effect)
//! - `you may look at ...` — peek-with-may
//!
//! Shape-gated specialized phrases (block only when the body matches):
//! - `you may reveal {your hand | ... from your hand | ... from outside the
//!   game | ... from among ... | ... opening hand}` — specialized reveal
//!   handlers; anaphoric `reveal that card`/`reveal it` peels through.
//! - `you may put ... {from among | of them | of those cards}` — Dig
//!   keep-from-among grammar; generic `put it onto the battlefield` peels
//!   through.
//!
//! `search` and bare anaphoric `reveal`/`put` are intentionally NOT blocked
//! — they peel cleanly and reach the generic optional-effect path.

use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::value;
use nom::Parser;

use super::oracle_effect::conditions::{
    strip_leading_general_conditional, strip_unrecognized_conditional_head_when_body_optional,
};
use super::oracle_effect::strip_trailing_duration;
use super::oracle_ir::context::ParseContext;
use super::oracle_nom::bridge::nom_on_lower;
use crate::types::ability::{AbilityCondition, Duration};

/// Synthesized attributes accumulated by `peel_clause` as it strips
/// structural slots off the head of a clause.
///
/// The shell guarantees that any slot recognized here is *removed* from the
/// peeled text — body parsers see only the bare imperative remainder. The
/// caller is responsible for applying each populated slot back onto the
/// parsed clause via the corresponding `apply_*` method (or by reading the
/// slot value directly via accessors like `duration()`) before returning.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ClauseContext {
    /// CR 608.2d: "you may [verb]" — controller-choice optional effect made
    /// during the spell/ability's resolution. Set when `peel_clause` strips
    /// a "you may " prefix.
    pub(crate) optional: bool,
    /// CR 611.2 + CR 514.2: "... this turn" / "... until end of turn" /
    /// "... until your next turn" / "... for as long as ~ remains tapped"
    /// etc. — temporal-bound duration suffix. Set when `peel_clause` strips
    /// a recognized duration suffix via the existing `strip_trailing_duration`
    /// building block.
    pub(crate) duration: Option<Duration>,
    /// CR 608.2c: "if [condition], [effect]" — leading conditional guard.
    /// Set when `peel_clause` strips a recognized "if" prefix and the
    /// condition fragment parses through the `parse_inner_condition`
    /// pipeline (via the existing `strip_leading_general_conditional`
    /// building block).
    pub(crate) condition: Option<AbilityCondition>,
}

impl ClauseContext {
    /// Apply the accumulated optional context onto a target boolean.
    ///
    /// Idempotent: if the target is already `true`, this is a no-op.
    pub(crate) fn apply_optional(&self, target: &mut bool) {
        if self.optional {
            *target = true;
        }
    }

    /// Read the accumulated duration slot.
    ///
    /// Returned by reference so callers can pass the value through the
    /// existing `with_clause_duration` building block without coupling
    /// `clause_shell` to the `pub(super)` `ParsedEffectClause` type.
    pub(crate) fn duration(&self) -> Option<&Duration> {
        self.duration.as_ref()
    }

    /// Read the accumulated leading-condition slot.
    ///
    /// Returned by reference; callers assign onto `clause.condition` when
    /// the inner parse didn't already produce one.
    pub(crate) fn condition(&self) -> Option<&AbilityCondition> {
        self.condition.as_ref()
    }

    /// True when no slot has been populated. Callers can short-circuit
    /// `apply_*` when nothing was peeled.
    pub(crate) fn is_empty(&self) -> bool {
        !self.optional && self.duration.is_none() && self.condition.is_none()
    }
}

/// Recursive peel: strip structural slots off the head of `text` until no
/// stripper matches. Returns the bare imperative remainder and the
/// accumulated `ClauseContext`.
///
/// The remainder is owned (`String`) because peelers may need to perform
/// case-insensitive head matching that produces a remainder slice with a
/// shorter lifetime than the input. Allocation is one-per-peel and only
/// when at least one slot fires.
pub(crate) fn peel_clause(text: &str) -> (String, ClauseContext) {
    peel_inner(text.to_string(), ClauseContext::default())
}

fn peel_inner(text: String, mut ctx: ClauseContext) -> (String, ClauseContext) {
    // Optional: "you may [bare imperative verb]" — leading prefix.
    if !ctx.optional {
        if let Some(rest) = strip_optional_prefix(&text) {
            ctx.optional = true;
            return peel_inner(rest, ctx);
        }
    }

    // Duration: "... this turn" / "... until end of turn" / etc. — trailing
    // suffix. Delegates to the existing `strip_trailing_duration` building
    // block so the suffix table stays the single source of truth across
    // both the peel shell and the legacy linear callsites.
    if ctx.duration.is_none() {
        let lower = text.to_lowercase();
        if !is_specialized_duration_carrier(&lower) {
            let (rest, dur) = strip_trailing_duration(&text);
            if let Some(dur) = dur {
                ctx.duration = Some(dur);
                return peel_inner(rest.to_string(), ctx);
            }
        }
    }

    // Condition: "if [cond], [effect]" — leading conditional. Delegates to
    // the existing `strip_leading_general_conditional` building block so
    // the same `parse_inner_condition` pipeline that the chunk loop uses
    // is the single authority for condition recognition.
    if ctx.condition.is_none() {
        let (cond, rest) = strip_leading_general_conditional(&text, &mut ParseContext::default());
        if let Some(cond) = cond {
            ctx.condition = Some(cond);
            return peel_inner(rest, ctx);
        }
    }

    // CR 608.2c + CR 608.2d: Unrepresentable `If <X>, ` head fallback — strips
    // ONLY when the body begins with `"you may "` so the recursive call's
    // step-1 optional-prefix peel captures the may-choice. Runs after the
    // typed strip so any recognized condition wins; the condition is
    // intentionally dropped (the Condition_If swallow detector still flags
    // it). Issue #2277.
    if ctx.condition.is_none() && !ctx.optional {
        let stripped = strip_unrecognized_conditional_head_when_body_optional(&text);
        if stripped.len() < text.len() {
            return peel_inner(stripped, ctx);
        }
    }

    // Termination: no stripper matched.
    (text, ctx)
}

/// CR 608.2d: Strip a leading bare "you may " when it precedes a fresh
/// imperative. Returns the remainder text on a match, `None` when the
/// "you may " phrase is part of a specialized construction handled by a
/// dedicated body parser (see `is_specialized_you_may_phrase`).
fn strip_optional_prefix(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let (_, rest) = nom_on_lower(text, &lower, |i| {
        value((), tag::<_, _, OracleError<'_>>("you may ")).parse(i)
    })?;
    let rest_lower = rest.to_lowercase();
    if is_specialized_you_may_phrase(&rest_lower) {
        return None;
    }
    Some(rest.to_string())
}

/// CR 400.7i: Detect duration-carrying constructions whose specialized
/// parsers REQUIRE the duration suffix to be present in their input as a
/// disambiguation signal. The impulse-draw parser at
/// `try_parse_play_from_exile` distinguishes "bare play that card"
/// (`Effect::CastFromZone`) from "play that card this turn" (impulse-draw
/// `GrantCastingPermission`) on the presence of "this turn" / "until ".
/// If we peel the duration here, the disambiguation fails. Defer to the
/// specialized parser by leaving the duration on for these phrases.
fn is_specialized_duration_carrier(text_lower: &str) -> bool {
    use nom::branch::alt;
    use nom::bytes::complete::tag;
    use nom::combinator::value;
    let head: nom::IResult<&str, (), OracleError<'_>> = alt((
        // CR 400.7i — impulse-draw bare form (post strip_optional_effect_prefix
        // in the chunk loop). `try_parse_play_from_exile` requires the
        // duration suffix to disambiguate vs. `Effect::CastFromZone`.
        value((), tag("play that ")),
        value((), tag("play those ")),
        value((), tag("play it")),
        value((), tag("play one of ")),
        value((), tag("cast that ")),
        value((), tag("cast those ")),
        value((), tag("cast it")),
        value((), tag("cast one of ")),
        // Full impulse-draw form (when the optional strip didn't fire
        // because we're a recursive sub-clause). Mirrors the alternatives
        // at `oracle_effect/mod.rs:2701`.
        value((), tag("you may play ")),
        value((), tag("you may cast ")),
        // CR 601.2f — "the next [type] spell you cast this turn ..."
        // next-spell limiter (cost reduction, keyword grant). The
        // specialized parser at `oracle_effect/mod.rs:571` requires
        // "this turn" to be present in the input.
        value((), tag("the next ")),
    ))
    .parse(text_lower);
    head.is_ok()
}

/// CR 608.2d: Suffixes after "you may " that have specialized parsing elsewhere
/// in the parser. Stripping the prefix in front of these would prevent the
/// dedicated parser from matching its full pattern.
///
/// The disambiguation is on the BODY SHAPE after the verb, not the verb itself.
/// A bare imperative like "you may reveal that card and put it into your hand"
/// must peel — the inner anaphoric reveal is a generic optional effect, NOT
/// the specialized `RevealFromHand`/`RevealUntil`/etc. constructions that
/// require the full surface form. Generic `you may search your library for X`
/// likewise peels cleanly; the from-among continuation is handled by the
/// prior sub_ability path (sequence.rs), not parse_effect_clause.
fn is_specialized_you_may_phrase(rest_lower: &str) -> bool {
    // Head-only blocklist: phrases whose dedicated body parsers need the full
    // `you may <verb>` surface present in their input to dispatch correctly.
    let head: nom::IResult<&str, (), OracleError<'_>> = alt((
        // "you may have target creature get ..." — causative
        value((), tag("have ")),
        // "you may cast ... as though ..." — static permission grant
        value((), tag("cast ")),
        // "you may play that card ..." — impulse draw permission
        value((), tag("play ")),
        // "you may choose new targets for ..." — retarget effect
        value((), tag("choose new targets ")),
        value((), tag("choose new target ")),
        // "you may instead ..." — Dig alternative selection
        value((), tag("instead ")),
        // "you may repeat this process" — repetition directive
        value((), tag("repeat ")),
        // "you may pay {X} rather than pay this spell's mana cost" — alt cost
        value((), tag("pay ")),
        // "you may look at ..." — peek-with-may
        value((), tag("look ")),
    ))
    .parse(rest_lower);
    if head.is_ok() {
        return true;
    }
    // Body-shape sub-checks: only block when the body after the verb matches
    // a specialized shape; anaphoric/conjunctive forms peel through as generic
    // optional effects (CR 608.2d).
    is_specialized_reveal_body(rest_lower) || is_specialized_put_body(rest_lower)
}

/// CR 701.20 (Reveal): `reveal `-headed phrases are specialized ONLY when
/// the body matches a hand/outside/from-among/opening-hand shape. Anaphoric
/// reveals (`reveal that card`, `reveal it`, `reveal those cards`, etc.) are
/// generic optional effects — peel the `you may ` prefix and let the generic
/// reveal handler take over.
fn is_specialized_reveal_body(rest_lower: &str) -> bool {
    let Ok((after_reveal, _)) = tag::<_, _, OracleError<'_>>("reveal ").parse(rest_lower) else {
        return false;
    };
    // Specialized shapes:
    //   "your hand"                  — RevealHand
    //   "... from your hand"         — RevealFromHand
    //   "... from outside the game"  — SearchOutsideGame / reveal-outside
    //   "... from among ..."         — Dig keep-from-among
    //   "... opening hand"           — opening-hand reveal
    let starts_with_your_hand: nom::IResult<&str, (), OracleError<'_>> =
        value((), tag("your hand")).parse(after_reveal);
    if starts_with_your_hand.is_ok() {
        return true;
    }
    take_until::<_, _, OracleError<'_>>("from your hand")
        .parse(after_reveal)
        .is_ok()
        || take_until::<_, _, OracleError<'_>>("from outside the game")
            .parse(after_reveal)
            .is_ok()
        || take_until::<_, _, OracleError<'_>>("from among")
            .parse(after_reveal)
            .is_ok()
        || take_until::<_, _, OracleError<'_>>("opening hand")
            .parse(after_reveal)
            .is_ok()
}

/// CR 608.2d: `put `-headed phrases are specialized ONLY when
/// the body contains a `from among …` / ` of them` / ` of those cards`
/// continuation — keep-from-among grammar that the dedicated handler
/// needs the full surface for. All other `you may put …` forms (onto the
/// battlefield from hand, onto the battlefield with anaphor, etc.) peel as
/// generic optional effects.
fn is_specialized_put_body(rest_lower: &str) -> bool {
    let Ok((after_put, _)) = tag::<_, _, OracleError<'_>>("put ").parse(rest_lower) else {
        return false;
    };
    take_until::<_, _, OracleError<'_>>("from among")
        .parse(after_put)
        .is_ok()
        || take_until::<_, _, OracleError<'_>>(" of them")
            .parse(after_put)
            .is_ok()
        || take_until::<_, _, OracleError<'_>>(" of those cards")
            .parse(after_put)
            .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peel_optional_prefix_strips_you_may() {
        let (peeled, ctx) = peel_clause("You may draw a card");
        assert_eq!(peeled, "draw a card");
        assert!(ctx.optional);
    }

    #[test]
    fn peel_optional_prefix_case_insensitive() {
        let (peeled, ctx) = peel_clause("you may gain 3 life");
        assert_eq!(peeled, "gain 3 life");
        assert!(ctx.optional);
    }

    #[test]
    fn peel_skips_specialized_you_may_have() {
        let (peeled, ctx) = peel_clause("you may have target creature get +3/+3");
        assert_eq!(peeled, "you may have target creature get +3/+3");
        assert!(!ctx.optional);
    }

    #[test]
    fn peel_skips_specialized_you_may_pay() {
        let input = "you may pay {2} rather than pay this spell's mana cost";
        let (peeled, ctx) = peel_clause(input);
        assert_eq!(peeled, input);
        assert!(!ctx.optional);
    }

    #[test]
    fn peel_skips_specialized_you_may_cast() {
        let (peeled, ctx) = peel_clause("you may cast that card");
        assert_eq!(peeled, "you may cast that card");
        assert!(!ctx.optional);
    }

    #[test]
    fn peel_optional_prefix_strips_generic_you_may_put() {
        let input =
            "you may put a construct, robot, or vehicle card from your hand onto the battlefield";
        let (peeled, ctx) = peel_clause(input);
        assert_eq!(
            peeled,
            "put a construct, robot, or vehicle card from your hand onto the battlefield"
        );
        assert!(ctx.optional);
    }

    #[test]
    fn peel_skips_specialized_you_may_put_from_among() {
        let input = "you may put a creature card from among them onto the battlefield";
        let (peeled, ctx) = peel_clause(input);
        assert_eq!(peeled, input);
        assert!(!ctx.optional);
    }

    /// CR 608.2d: An anaphoric reveal (`reveal that card …`) is a generic
    /// optional effect, NOT a specialized `RevealFromHand`/`RevealUntil`
    /// construction. The `you may ` prefix must peel and `ctx.optional`
    /// must be set so the controller chooses during resolution.
    /// Regression for issue #2277 (Amareth-pattern reveal-and-put).
    #[test]
    fn peel_optional_prefix_strips_anaphoric_reveal() {
        let input = "you may reveal that card and put it into your hand";
        let (peeled, ctx) = peel_clause(input);
        assert_eq!(peeled, "reveal that card and put it into your hand");
        assert!(ctx.optional);
    }

    /// CR 608.2d: Generic `you may search your library for X` is an optional
    /// effect — the from-among continuation is on the prior sub_ability path
    /// (sequence.rs), not parse_effect_clause. The prefix must peel.
    /// Regression for issue #2277 (Tithe-pattern optional search).
    #[test]
    fn peel_optional_prefix_strips_search_library() {
        let input = "you may search your library for an additional plains card";
        let (peeled, ctx) = peel_clause(input);
        assert_eq!(peeled, "search your library for an additional plains card");
        assert!(ctx.optional);
    }

    /// CR 608.2d: Generic `you may put it onto the battlefield` (no
    /// `from among`/`of them`/`of those cards` continuation) is an optional
    /// effect — the prefix must peel.
    #[test]
    fn peel_optional_prefix_strips_anaphoric_put_onto_battlefield() {
        let input = "you may put it onto the battlefield";
        let (peeled, ctx) = peel_clause(input);
        assert_eq!(peeled, "put it onto the battlefield");
        assert!(ctx.optional);
    }

    /// CR 701.20 (Reveal): `reveal a [type] card from your hand` is the
    /// specialized `RevealFromHand` shape — the dedicated body parser needs the
    /// full `you may reveal …` surface to dispatch. Must NOT peel.
    #[test]
    fn peel_skips_specialized_reveal_from_hand() {
        let input = "you may reveal an instant card from your hand";
        let (peeled, ctx) = peel_clause(input);
        assert_eq!(peeled, input);
        assert!(!ctx.optional);
    }

    /// CR 701.20: `reveal a card from among them` is the specialized Dig
    /// keep-from-among shape — must NOT peel.
    #[test]
    fn peel_skips_specialized_reveal_from_among() {
        let input = "you may reveal a creature card from among them";
        let (peeled, ctx) = peel_clause(input);
        assert_eq!(peeled, input);
        assert!(!ctx.optional);
    }

    #[test]
    fn peel_no_match_passes_through() {
        let (peeled, ctx) = peel_clause("draw a card");
        assert_eq!(peeled, "draw a card");
        assert!(!ctx.optional);
        assert!(ctx.is_empty());
    }

    #[test]
    fn apply_optional_idempotent() {
        let ctx = ClauseContext {
            optional: true,
            ..ClauseContext::default()
        };
        let mut already_true = true;
        ctx.apply_optional(&mut already_true);
        assert!(already_true);

        let mut starts_false = false;
        ctx.apply_optional(&mut starts_false);
        assert!(starts_false);
    }

    #[test]
    fn apply_optional_when_unset_is_noop() {
        let ctx = ClauseContext::default();
        let mut target = false;
        ctx.apply_optional(&mut target);
        assert!(!target);
    }

    #[test]
    fn peel_duration_this_turn() {
        let (peeled, ctx) = peel_clause("target creature gets +2/+2 this turn");
        assert_eq!(peeled, "target creature gets +2/+2");
        assert_eq!(ctx.duration(), Some(&Duration::UntilEndOfTurn));
    }

    #[test]
    fn peel_duration_until_end_of_turn() {
        let (peeled, ctx) = peel_clause("target creature gains flying until end of turn");
        assert_eq!(peeled, "target creature gains flying");
        assert_eq!(ctx.duration(), Some(&Duration::UntilEndOfTurn));
    }

    #[test]
    fn peel_duration_until_your_next_turn() {
        let (peeled, ctx) = peel_clause("you don't lose unspent mana until your next turn");
        assert_eq!(peeled, "you don't lose unspent mana");
        assert_eq!(
            ctx.duration(),
            Some(&Duration::UntilNextTurnOf {
                player: crate::types::ability::PlayerScope::Controller,
            })
        );
    }

    #[test]
    fn peel_combines_optional_and_duration() {
        let (peeled, ctx) = peel_clause("you may draw a card this turn");
        assert_eq!(peeled, "draw a card");
        assert!(ctx.optional);
        assert_eq!(ctx.duration(), Some(&Duration::UntilEndOfTurn));
    }

    #[test]
    fn peel_duration_no_suffix_passes_through() {
        let (peeled, ctx) = peel_clause("draw a card");
        assert_eq!(peeled, "draw a card");
        assert!(ctx.duration().is_none());
    }

    #[test]
    fn peel_leading_condition_if_strips_and_captures() {
        let (peeled, ctx) = peel_clause("if you control a Forest, draw a card");
        assert_eq!(peeled, "draw a card");
        assert!(ctx.condition().is_some());
    }

    #[test]
    fn peel_no_condition_passes_through() {
        let (peeled, ctx) = peel_clause("draw a card");
        assert_eq!(peeled, "draw a card");
        assert!(ctx.condition().is_none());
    }

    #[test]
    fn peel_combines_condition_and_duration() {
        // Condition stripper runs after duration stripper; both peel.
        let (peeled, ctx) =
            peel_clause("if you control a Forest, target creature gets +2/+2 until end of turn");
        assert_eq!(peeled, "target creature gets +2/+2");
        assert!(ctx.condition().is_some());
        assert_eq!(ctx.duration(), Some(&Duration::UntilEndOfTurn));
    }
}
