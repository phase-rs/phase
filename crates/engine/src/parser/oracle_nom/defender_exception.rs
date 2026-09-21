//! CR 702.3b (docs/MagicCompRules.txt:3915) + CR 609.4 (:2854): the ONE
//! recognizer for the defender-exception grammar
//! "<subject> can attack [<segment>] as though <pronoun> didn't have defender".
//!
//! `<segment>` — the INTERPOSED SEGMENT — names the class of defenders the
//! permission covers ("players who attacked you during their last turn",
//! CR 508.6 :2327). SIX EDITED CALL SITES share this module — productions (a),
//! (b) and (c), the conjunctive static splitter, the continuous compound, and
//! the shared recognition predicate `is_can_attack_despite_defender_predicate`
//! — so the class cannot be supported on one printed shape and misparsed on
//! another. A SEVENTH site, `sequence::combat_requirement_conjunct_prepend`, is
//! UNEDITED and reaches this module through the shared predicate. FIVE of the
//! six EMIT a `CanAttackWithDefender` and therefore call `permission_condition`;
//! the predicate emits nothing. (Counts derived by command, not listed —
//! revision 5, DERIVED SWEEP.)
//!
//! SINGLE CONDITION AUTHORITY: recognition is delegated to
//! `oracle_nom::condition::parse_inner_condition`. This module applies a purely
//! GRAMMATICAL normalization from the relative-clause surface the card prints to
//! the clausal surface the authority owns, and NAMES the class. It holds no
//! condition vocabulary, and a class->condition table here would be a second
//! authority (forbidden by CLAUDE.md).
//!
//! FAIL-CLOSED: a segment the authority declines, or one it recognizes with no
//! defending-player-anchored reading, yields `unenforceable_gate_marker` —
//! permanently inert at runtime AND red in coverage. Never a bare
//! `Unrecognized`, which `layers::evaluate_condition_inner` (layers.rs:2120)
//! reads as TRUE and which would make the PERMISSION apply to every defender.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::{all_consuming, opt, peek, value};
use nom::Parser;

use super::condition as nom_condition;
use super::error::OracleError;
use super::primitives as nom_primitives;
use crate::types::ability::StaticCondition;

/// The segment a defender-exception line interposes between `can attack` and
/// the `as though ... didn't have defender` tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DefenderExceptionSegment {
    /// Empty segment: the plain, unrestricted CR 702.3b permission.
    Unrestricted,
    /// A duration adverbial ("this turn") — NOT a player class. Carries no
    /// payload deliberately: the effect-side production derives its own
    /// `Duration` from the whole clause (see `try_parse_can_attack_with_defender`),
    /// and a payload nobody reads would misstate where that authority lives.
    DurationAdverbial,
    /// `parse_inner_condition` recognized the normalized clause AND it has a
    /// defending-player-anchored reading. CR 508.1b (:2268) is what CREATES the
    /// creature->target pairing this reading is relative to; CR 508.1c (:2270)
    /// then checks restrictions against it; CR 611.3a (:2926) keeps the
    /// STATIC-side effect unlocked, and CR 611.2c (:2913) does the same job for
    /// the RESOLUTION-side production (c) and the continuous compound, which
    /// 611.3a by its own text does not cover. (Revision 5, review finding m-E.)
    /// The class is answerable per proposed pairing.
    AnchoredClass(StaticCondition),
    /// `parse_inner_condition` recognized the normalized clause, but it has NO
    /// anchored reading, so it cannot be answered per pairing. Fail-closed.
    UnanchorableClass { text: String },
    /// `parse_inner_condition` declined the normalized clause. Fail-closed.
    UnrecognizedClass { text: String },
}

impl DefenderExceptionSegment {
    /// The condition a `StaticMode::CanAttackWithDefender` built from this
    /// segment must carry. THE SINGLE PLACE the inert-marker decision is made,
    /// so the FIVE EMITTING PRODUCTIONS cannot disagree about it — (a), (b),
    /// (c), the conjunctive static splitter and the continuous compound.
    /// (`is_can_attack_despite_defender_predicate` is the sixth call site of
    /// the recognizer but emits nothing, so it does not call this. Derived by
    /// command — revision 5, DERIVED SWEEP.)
    ///
    /// `unenforceable_gate_marker` yields `Not(Unrecognized{..})`, which
    /// `layers::evaluate_condition_inner` negates to FALSE forever — so an
    /// unparsed class leaves the PERMISSION permanently inert rather than
    /// fail-OPEN, while `StaticCondition::contains_unrecognized` still reports
    /// the gap and `coverage::check_statics` still labels the card red. A bare
    /// `Unrecognized` here would grant the permission against EVERY defender.
    pub(crate) fn permission_condition(&self) -> Option<StaticCondition> {
        match self {
            Self::Unrestricted | Self::DurationAdverbial => None,
            Self::AnchoredClass(condition) => Some(condition.clone()),
            Self::UnanchorableClass { text } | Self::UnrecognizedClass { text } => Some(
                crate::parser::oracle_static::unenforceable_gate_marker(text),
            ),
        }
    }
}

/// Parse `"can attack [<segment>] as though <pronoun> didn't have defender"`
/// from the START of `lower`, returning the classified segment and the
/// UNCONSUMED remainder.
///
/// Callers keep their OWN consuming policy: productions (a) and (b) accept a
/// remainder (their base behaviour is a prefix match, and one corpus card's line
/// continues with "and it can't be blocked"), while
/// `defender_exception_predicate_all_consuming` requires an empty one.
///
/// **IT DOES NOT TRIM ITS INPUT, and that is a decision, not an omission
/// (revision 4, SYMMETRY SWEEP S-2).** Trimming here would make production (a)
/// accept a line base refuses, on a path with no test and no card: base's
/// CR 702.3b arm is `alt((tag(..), tag(..))).parse(pred_lower.as_str())` with no
/// trim at all, so a leading space makes base DECLINE. Each caller keeps base's
/// own trimming: the all-consuming entry point below is handed `lower.trim()` by
/// 8a, exactly as base's `is_can_attack_despite_defender_predicate` trims;
/// production (a) passes `pred_lower.as_str()` untrimmed, exactly as base does.
pub(crate) fn parse_defender_exception_predicate(
    lower: &str,
) -> Option<(DefenderExceptionSegment, &str)> {
    // A PURE type/shape adapter over `defender_exception_ir`, which is the single
    // place the grammar is written. ONE spelling, SIX edited call sites (DERIVED
    // SWEEP). It carries no behaviour of its own — which is why it needs no
    // DISCRIMINATION row, and why the sweep that looked for one removed the line
    // that would have needed one.
    defender_exception_ir(lower)
        .ok()
        .map(|(rest, segment)| (segment, rest))
}

/// THE ALL-CONSUMING POLICY, WRITTEN ONCE — and the only place in this phase
/// where it is written at all.
///
/// A defender-exception predicate is "all-consuming" when nothing follows the
/// tail except at most a single terminator. **Every all-consuming call site in
/// the phase routes through this function**, because at `PHASE_BASE_SHA` the
/// policy already lives in exactly one place — `is_can_attack_despite_defender_predicate`
/// — and production (c) is one of its consumers rather than carrying a copy
/// (measured, M-23). A second spelling would re-create, in the very module whose
/// purpose is to remove copies of this grammar, the duplication the module
/// exists to delete.
///
/// **THE EXACT SPELLING MATTERS.** Base is
/// `all_consuming((.., opt(tag("."))))` over a PRE-TRIMMED input, so the only
/// remainders it accepts after the tail are `""` and `"."`, plus trailing
/// whitespace that the input trim had already removed. A `rest.trim()` here
/// would additionally accept `" ."` — i.e. `"… didn't have defender ."` — a line
/// base DECLINES. Corpus exposure of that difference is zero (M-13's tail census
/// enumerates every tail), but this helper's whole job is to PRESERVE a policy,
/// so it is spelled to reproduce base rather than to approximate it. `trim_end`
/// reproduces the input trim's only surviving effect; the remainder is NOT
/// re-trimmed on the left.
///
/// DISCRIMINATION 1.9 walks the `opt(tag("."))`; 1.10 the emptiness check;
/// **8a.2, 1.24 / 8b.2 and 8c.2 walk this function's THREE call sites' choices
/// to apply it.** All FIVE are bought at CARD altitude — see their rows.
fn all_consuming_defender_tail(rest: &str) -> Option<()> {
    // 1.9
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>("."))
        .parse(rest.trim_end())
        .ok()?;
    // 1.10
    rest.is_empty().then_some(())
}

/// Parse from the START of `lower` under the all-consuming policy.
/// **TWO DIRECT CALLERS:** 8a (`is_can_attack_despite_defender_predicate`, and
/// through it the compound's GATE and the sequence splitter) and **8c, the
/// compound's per-segment LOOP, which calls this directly because it needs the
/// `class`**. Both pass a TRIMMED input — 8a passes `lower.trim()` (base's own
/// trim); 8c passes `segment.to_lowercase()` where base already trimmed
/// `segment`. See `all_consuming_defender_tail`.
///
/// **THE `all_consuming_defender_tail(rest)?` LINE BELOW IS DISCRIMINATION 8a.2.**
/// Delete it — equivalently, make 8a call `parse_defender_exception_predicate`
/// directly — and the continuous compound's per-segment gate OPENS for a
/// defender segment that carries trailing text, pushing an unconditioned
/// `CanAttackWithDefender` where base and the candidate both refuse. Measured
/// (M-24, MODE 5). Its SIBLING,
/// `split_defender_exception_predicate_all_consuming` below, carries the
/// identical line for production (c) and is walked as 1.24 / 8b.2; the compound
/// LOOP's own choice to route through THIS function is 8c.2. **None of the
/// three call sites may be edited without the other two's rows (8a.2,
/// 1.24 / 8b.2, 8c.2) being re-checked: that asymmetry — one sibling walked,
/// the others not — is what review findings MG-1 and MG-A each caught.**
pub(crate) fn defender_exception_predicate_all_consuming(
    lower: &str,
) -> Option<DefenderExceptionSegment> {
    let (segment, rest) = parse_defender_exception_predicate(lower)?;
    all_consuming_defender_tail(rest)?; // 8a.2 — sibling of 1.24 / 8b.2 and of 8c.2
    Some(segment)
}

/// SCAN for the predicate anywhere at a word boundary, under the all-consuming
/// policy, returning the subject prefix and the classification.
///
/// **Production (c)'s entry point.** (c) needs both halves at once — the subject
/// prefix, to build its `affected`, and the all-consuming policy, which is its
/// base behaviour — and this composition is what lets it have both without
/// respelling the policy. See `all_consuming_defender_tail`.
pub(crate) fn split_defender_exception_predicate_all_consuming(
    lower: &str,
) -> Option<(&str, DefenderExceptionSegment)> {
    let (subject_prefix, segment, rest) = split_defender_exception_predicate(lower)?;
    all_consuming_defender_tail(rest)?; // 1.24 / 8b.2 — sibling of 8a.2 and of 8c.2
    Some((subject_prefix, segment))
}

/// The predicate as an `IResult` parser, so it composes with the `oracle_nom`
/// scanners. **This inner form exists for a type reason, not a style reason:**
/// `nom_primitives::scan_preceded` is declared
/// `F: FnMut(&'a str) -> IResult<&'a str, O, OracleError<'a>>`, and
/// `parse_defender_exception_predicate` returns `Option<(..)>`, which does not
/// satisfy that bound. `split_defender_exception_predicate` and the conjunctive
/// static splitter (Step 6) both scan with THIS function; the `Option`-returning
/// `parse_defender_exception_predicate` above is a thin adapter over it for the
/// call sites that parse from the start of their input. **ONE spelling of the
/// grammar, SIX EDITED CALL SITES** (derived by command — revision 5, DERIVED
/// SWEEP; a seventh, 8d, is unedited and arrives through the shared predicate)
/// — which is the whole reason the module exists.
///
/// `pub(crate)` rather than module-private because the conjunctive static
/// splitter (`oracle_static::evasion::try_split_and_can_attack_despite_defender`)
/// scans with it from another module.
pub(crate) fn defender_exception_ir(
    input: &str,
) -> nom::IResult<&str, DefenderExceptionSegment, OracleError<'_>> {
    type VE<'a> = OracleError<'a>;
    let (after_verb, _) = tag::<_, _, VE>("can attack").parse(input)?;
    // Word boundary: without this, "can attackers ..." matches the verb phrase
    // and the interposed segment becomes "ers". (Row 8 arm 8.)
    peek(tag::<_, _, VE>(" ")).parse(after_verb)?;
    let (segment, _tail, rest) = nom_primitives::scan_preceded(after_verb, |i: &str| {
        (
            tag::<_, _, VE>("as though "),
            alt((tag("it"), tag("they"))),
            tag(" didn't have defender"),
        )
            .parse(i)
    })
    // The in-tree idiom for "a hand-rolled scan declined" (e.g. `oracle.rs:226`).
    .ok_or_else(|| nom::Err::Error(OracleError::new(input, nom::error::ErrorKind::Tag)))?;
    Ok((rest, classify_interposed_segment(segment.trim())))
}

/// Scan at word boundaries for the first position where the predicate parses,
/// returning `(subject_prefix, segment, rest)`. Production (b) needs the subject
/// prefix to dispatch its `affected` filter; production (c) needs the same split.
///
/// `scan_preceded` advances one WORD at a time and returns `&text[..offset]` as
/// `before`, so the scan lands on the first word boundary at which the WHOLE
/// predicate parses — not merely on the first occurrence of `"can attack"`.
///
/// **THE BYTE-LENGTH INVARIANT the returned `subject_prefix` depends on.** Every
/// caller receives a prefix of the LOWERCASED text and indexes the
/// ORIGINAL-CASE text with its length (production (b) already does exactly this,
/// `body_tp.original[..subject_prefix.len()]`). That is legal because ASCII
/// lowercasing preserves byte lengths — the invariant is stated in-tree in
/// `evasion.rs` and relied on by `try_split_and_can_attack_despite_defender`. It
/// holds for every corpus defender-exception line, all of which are ASCII in the
/// subject position. Callers that index original-case text MUST keep using the
/// LOWER string's prefix length and must not re-lowercase per call.
pub(crate) fn split_defender_exception_predicate(
    lower: &str,
) -> Option<(&str, DefenderExceptionSegment, &str)> {
    nom_primitives::scan_preceded(lower, defender_exception_ir)
}

fn classify_interposed_segment(segment: &str) -> DefenderExceptionSegment {
    if segment.is_empty() {
        return DefenderExceptionSegment::Unrestricted;
    }
    type VE<'a> = OracleError<'a>;
    if all_consuming(tag::<_, _, VE>("this turn"))
        .parse(segment)
        .is_ok()
    {
        return DefenderExceptionSegment::DurationAdverbial;
    }
    let Some(clausal) = relative_clause_to_clausal(segment) else {
        return DefenderExceptionSegment::UnrecognizedClass {
            text: segment.to_string(),
        };
    };
    match nom_condition::parse_inner_condition(&clausal) {
        // The authority's empty-remainder contract: a PREFIX parse is not an
        // acceptance (see `parse_static_condition`, which applies the same rule).
        Ok((rest, condition)) if rest.trim().is_empty() => {
            match condition.defending_player_anchored_form() {
                Some(anchored) => DefenderExceptionSegment::AnchoredClass(anchored),
                None => DefenderExceptionSegment::UnanchorableClass {
                    text: segment.to_string(),
                },
            }
        }
        _ => DefenderExceptionSegment::UnrecognizedClass {
            text: segment.to_string(),
        },
    }
}

/// CR 109.5 (:610): rewrite the RELATIVE-CLAUSE surface a defender-exception line
/// prints ("players who <clause>") to the CLAUSAL surface
/// `parse_inner_condition` owns ("a player <clause>"). Purely a
/// subject/quantifier transform: the quantifier and the relativizer are the only
/// tokens touched, and NO condition vocabulary appears here.
///
/// It does NOT conjugate. A present-tense relative clause ("players who control a
/// Mountain") reaches the authority with its plural verb and is declined — which
/// routes it to the fail-closed inert marker (permanently inert, card red), the
/// correct direction for an unparsed class.
fn relative_clause_to_clausal(segment: &str) -> Option<String> {
    type VE<'a> = OracleError<'a>;
    let (rest, subject) = alt((
        value(
            "a player ",
            alt((
                tag::<_, _, VE>("players who "),
                tag("a player who "),
                tag("any player who "),
                tag("each player who "),
            )),
        ),
        value(
            "an opponent ",
            alt((
                tag::<_, _, VE>("opponents who "),
                tag("an opponent who "),
                tag("any opponent who "),
                tag("each opponent who "),
            )),
        ),
    ))
    .parse(segment)
    .ok()?;
    Some(format!("{subject}{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::AttackedYouScope;

    const ANCHORED_CLAUSE: &str = "attacked you during their last turn";

    /// CR 508.6 (:2327): every `alt` member of the relative-clause -> clausal
    /// normalization, TABLE-DRIVEN so deleting any member reds a NAMED entry
    /// (DISCRIMINATION 1.13, 1.14 and 1.15).
    #[test]
    fn every_relative_clause_surface_normalizes_and_anchors() {
        let surfaces = [
            "players who",
            "a player who",
            "any player who",
            "each player who",
            "opponents who",
            "an opponent who",
            "any opponent who",
            "each opponent who",
        ];
        for surface in surfaces {
            let segment = format!("{surface} {ANCHORED_CLAUSE}");
            assert!(
                relative_clause_to_clausal(&segment).is_some(),
                "normalization must own the `{surface}` surface"
            );
            assert_eq!(
                classify_interposed_segment(&segment),
                DefenderExceptionSegment::AnchoredClass(
                    StaticCondition::AnyPlayerAttackedYouLastTurn {
                        scope: AttackedYouScope::AttackedPlayer,
                    }
                ),
                "the `{surface}` surface must reach the anchored class"
            );
        }
    }

    /// C3.6 + Row 6 (i): the accept set is the CONDITION AUTHORITY's, not a local
    /// table — the variant difference IS the instrument. All three classification
    /// terminals in one fixture.
    ///
    /// `"players who discarded a card this turn"` normalizes to
    /// `"a player discarded a card this turn"`, which `parse_inner_condition`
    /// OWNS (as a `QuantityComparison`) and which has NO anchored reading. A
    /// classifier with a bespoke local class table has never heard of the phrase
    /// and would answer `UnrecognizedClass`.
    #[test]
    fn classifier_accept_set_is_the_condition_authoritys_not_a_local_table() {
        assert!(
            matches!(
                classify_interposed_segment("players who discarded a card this turn"),
                DefenderExceptionSegment::UnanchorableClass { .. }
            ),
            "C3.6: the classifier must DELEGATE — a local table would say UnrecognizedClass; \
             got {:?}",
            classify_interposed_segment("players who discarded a card this turn")
        );
        assert!(matches!(
            classify_interposed_segment("players who attacked you during their last turn"),
            DefenderExceptionSegment::AnchoredClass(_)
        ));
        assert!(matches!(
            classify_interposed_segment("players who wear a hat"),
            DefenderExceptionSegment::UnrecognizedClass { .. }
        ));
    }

    /// Row 6's hostile fixture: the authority's EMPTY-REMAINDER contract
    /// (DISCRIMINATION 1.18). A segment the authority owns only as a PREFIX must
    /// NOT be accepted.
    #[test]
    fn classifier_enforces_the_authoritys_empty_remainder_contract() {
        assert!(
            matches!(
                classify_interposed_segment(
                    "players who attacked you during their last turn and also"
                ),
                DefenderExceptionSegment::UnrecognizedClass { .. }
            ),
            "a PREFIX parse is not an acceptance"
        );
        // PAIRED POSITIVE CONTROL, same fixture shape: without the trailing
        // remainder the same segment IS accepted, so the negative above is a
        // measured refusal rather than a silence.
        assert!(matches!(
            classify_interposed_segment("players who attacked you during their last turn"),
            DefenderExceptionSegment::AnchoredClass(_)
        ));
    }

    /// DISCRIMINATION 1.11 + 1.1: the empty segment is the plain, unrestricted
    /// CR 702.3b permission; DISCRIMINATION 1.2 / 1.12: `"this turn"` is a
    /// DURATION adverbial, not a player class.
    #[test]
    fn empty_and_duration_segments_are_their_own_terminals() {
        assert_eq!(
            classify_interposed_segment(""),
            DefenderExceptionSegment::Unrestricted
        );
        assert_eq!(
            classify_interposed_segment("this turn"),
            DefenderExceptionSegment::DurationAdverbial
        );
        // `all_consuming`, not a prefix: "this turn players who ..." is NOT a
        // duration adverbial (Row 4's hostile fixture).
        assert!(matches!(
            classify_interposed_segment(
                "this turn players who attacked you during their last turn"
            ),
            DefenderExceptionSegment::UnrecognizedClass { .. }
        ));
    }

    /// DISCRIMINATION 1.7: the word-boundary guard after the verb phrase.
    /// `"can attackers ..."` must NOT parse; the same sentence WITHOUT `ers` must.
    /// A MINIMAL PAIR, three characters apart (Row 8 arm 8 / arm 1 at line level).
    #[test]
    fn verb_phrase_requires_a_word_boundary() {
        assert!(parse_defender_exception_predicate(
            "can attackers as though it didn't have defender."
        )
        .is_none());
        assert!(
            parse_defender_exception_predicate("can attack as though it didn't have defender.")
                .is_some(),
            "control: the same sentence without `ers` MUST parse"
        );
    }

    /// `permission_condition`'s FOUR outcomes — the single place the
    /// inert-marker decision is made (DISCRIMINATION 1.20 / 1.21 / 1.22).
    #[test]
    fn permission_condition_maps_every_terminal() {
        assert_eq!(
            DefenderExceptionSegment::Unrestricted.permission_condition(),
            None
        );
        assert_eq!(
            DefenderExceptionSegment::DurationAdverbial.permission_condition(),
            None
        );
        let anchored = StaticCondition::AnyPlayerAttackedYouLastTurn {
            scope: AttackedYouScope::AttackedPlayer,
        };
        assert_eq!(
            DefenderExceptionSegment::AnchoredClass(anchored.clone()).permission_condition(),
            Some(anchored)
        );
        // BOTH fail-closed terminals produce the ONE inert-marker shape, so
        // coverage tooling sees a single shape (`unenforceable_gate_marker`'s
        // own doc mandates this).
        let inert = Some(StaticCondition::Not {
            condition: Box::new(StaticCondition::Unrecognized {
                text: "players who wear a hat".to_string(),
            }),
        });
        assert_eq!(
            DefenderExceptionSegment::UnrecognizedClass {
                text: "players who wear a hat".to_string(),
            }
            .permission_condition(),
            inert
        );
        assert_eq!(
            DefenderExceptionSegment::UnanchorableClass {
                text: "players who wear a hat".to_string(),
            }
            .permission_condition(),
            inert
        );
    }
}
