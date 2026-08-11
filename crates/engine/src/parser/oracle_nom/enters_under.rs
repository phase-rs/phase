//! CR 110.2a battlefield-entry control-clause grammar.
//!
//! One combinator for every printed spelling of the `"under <possessor>
//! control"` clause that trails (or leads) a battlefield destination phrase,
//! plus the CR 608.2c antecedent resolution that turns a third-person anaphor
//! into a bound [`ControllerRef`].
//!
//! CR 110.2a (docs/MagicCompRules.txt:618): "If an effect instructs a player to
//! put an object onto the battlefield, that object enters the battlefield under
//! that player's control unless the effect states otherwise." The clause parsed
//! here is precisely the "unless the effect states otherwise" escape, so the
//! grammar and the binding are the authority for who ends up controlling the
//! permanent.
//!
//! CR 110.1 (docs/MagicCompRules.txt:614) scopes this to battlefield
//! destinations only; a `"to its owner's hand"` phrase has no controller.
//!
//! Before this module the four rewired seams each recognized a *different*
//! hand-picked subset of the spellings by literal comparison, and every
//! third-person form (`"under their control"`, `"under that player's
//! control"`) was silently dropped into the existing no-override carrier. That
//! carrier cannot preserve an explicitly named third-person controller.
//! Collapsing the spellings into one grammar makes the dropped forms either
//! *bound* (when an antecedent is nameable) or *honestly unsupported* (when it
//! is not) — never silently wrong.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::{opt, value, verify};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::error::OracleResult;
use super::primitives as nom_primitives;
use crate::parser::oracle_ir::ast::EntersUnderSpec;
use crate::parser::oracle_ir::context::ParseContext;
use crate::types::ability::{ControllerRef, FilterProp, TargetFilter};

/// CR 110.2a (docs/MagicCompRules.txt:618): the possessor named by a
/// battlefield-entry control clause, AS WRITTEN. A syntax type, deliberately
/// NOT a `ControllerRef`: a combinator sees only the clause, never its
/// antecedent, so it cannot produce a bound reference without guessing.
///
/// `Copy` is load-bearing: `strip_return_destination_ext_with_remainder` reads
/// this out of a `&'static [(...)]` destination table row by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum ControlClausePossessor {
    /// CR 109.5 (docs/MagicCompRules.txt:610): `"under your control"` — the
    /// resolving player.
    You,
    /// CR 110.2 (docs/MagicCompRules.txt:616): `"under its/their/his/her
    /// owner's control"` — explicitly names the moved object's owner. The
    /// existing `None` carrier preserves that per-object owner at resolution.
    Owner,
    /// CR 608.2c (docs/MagicCompRules.txt:2793): a bare third-person plural
    /// anaphor, `"under their control"`. Needs an antecedent.
    TheirAnaphor,
    /// CR 608.2c: the demonstrative `"under that player's control"`. Needs an
    /// antecedent, and specifically a *player-valued* one.
    ThatPlayerDemonstrative,
}

impl ControlClausePossessor {
    /// The verbatim printed clause, in MTGJSON's spelling (U+2019 apostrophe).
    /// Both anaphor forms have exactly one printed spelling, so the fragment is
    /// truthful without carrying a text slice — which is what lets
    /// `lower_put_ast(ast) -> Effect`, a lowering function that receives no
    /// text at all, still emit an honest `Effect::unimplemented` fragment.
    pub(crate) fn printed_clause(self) -> &'static str {
        match self {
            ControlClausePossessor::You => "under your control",
            ControlClausePossessor::Owner => "under its owner\u{2019}s control",
            ControlClausePossessor::TheirAnaphor => "under their control",
            ControlClausePossessor::ThatPlayerDemonstrative => "under that player\u{2019}s control",
        }
    }
}

/// CR 608.2c (docs/MagicCompRules.txt:2793): the antecedent a third-person
/// control-clause anaphor resolves to, already reduced to something the engine
/// can name. Never a raw parse scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControlAnaphorAntecedent {
    /// A player reference already MAPPED through [`map_relative_player_scope`].
    ContextPlayer(ControllerRef),
    /// CR 400.1 (:1933) + CR 400.3 (:1937) + CR 404.1 (:2030) + CR 108.3
    /// (:564): the moved object's own owner, named by the clause's parsed
    /// object filter.
    MovedObjectOwner,
    /// No antecedent can be named — fail closed.
    Unnameable,
}

/// CR 110.2a: `"under <possessor> control"`. NO leading space — this is the
/// canonical form, so [`nom_primitives::scan_at_word_boundaries`] (which
/// left-trims every position after the first) can find it mid-sentence.
pub(crate) fn parse_control_clause(i: &str) -> OracleResult<'_, ControlClausePossessor> {
    preceded(
        tag("under "),
        terminated(parse_control_clause_possessor, tag(" control")),
    )
    .parse(i)
}

/// The same clause in head position after a destination phrase, i.e. with its
/// separating space still attached (`"... to the battlefield under their
/// control"`).
pub(crate) fn parse_leading_control_clause(i: &str) -> OracleResult<'_, ControlClausePossessor> {
    preceded(tag(" "), parse_control_clause).parse(i)
}

/// CR 108.3 (docs/MagicCompRules.txt:564): the shared possessive-mark axis.
/// `'s` / `’s` singular, bare `'` / `’` plural (`"owners'"`). LONGEST-FIRST, so
/// the singular mark is never short-matched by the bare apostrophe arm.
fn parse_possessive_mark(i: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((tag("'s"), tag("\u{2019}s"), tag("'"), tag("\u{2019}"))),
    )
    .parse(i)
}

fn parse_control_clause_possessor(i: &str) -> OracleResult<'_, ControlClausePossessor> {
    alt((
        // NON-MATCH BY NON-BACKTRACKING, NOT BY ABSENT ARM: on "under your
        // opponents' control", tag("your") matches here, then the OUTER
        // terminated(.., tag(" control")) fails on " opponents' control" and nom
        // does NOT re-enter this alt. The clause therefore correctly fails to
        // parse — but only as an accident of nom's commit semantics. If a
        // `your`-sub-axis is ever added, this changes silently. Pinned by a
        // module test below.
        value(ControlClausePossessor::You, tag("your")),
        // CR 110.2 (:616): the owner forms MUST precede the bare `their` arm —
        // nom's alt does not re-enter once the outer terminated(..) step
        // commits, so a leading `value(TheirAnaphor, tag("their"))` would make
        // "under their owner's control" unparseable.
        value(
            ControlClausePossessor::Owner,
            (
                alt((tag("its"), tag("their"), tag("his"), tag("her"))),
                tag(" owner"),
                opt(tag("s")),
                parse_possessive_mark,
            ),
        ),
        value(
            ControlClausePossessor::ThatPlayerDemonstrative,
            (tag("that player"), parse_possessive_mark),
        ),
        value(ControlClausePossessor::TheirAnaphor, tag("their")),
        // DEFERRED possessor forms, each a one-arm extension once an antecedent
        // source exists for it: "under an opponent's control" (10 cards),
        // "under target opponent's control" (Evil Presents), "under target
        // player's control" (Yavimaya Dryad), "under a player's control"
        // (Mojave Desert), "under your opponents' control" (Vren, the
        // Relentless). NOTE the carve-out: "under an opponent's control" is not
        // uniformly unhandled today — `oracle_trigger.rs`'s
        // `parse_enters_control_rider` already maps it to `ControllerRef::Opponent`
        // at the CR 603.2 trigger-CONDITION layer, which this module does not
        // touch. "No regression and no improvement" is scoped to the four
        // destination-phrase seams that call into here.
    ))
    .parse(i)
}

/// CR 110.2a: reduce every control clause in `span` to one possessor with an
/// EXPLICIT, ORDER-INDEPENDENT priority — `You` beats an anaphor, an anaphor
/// beats `Owner`. This is NOT a first-match latch: `You`-wins is exactly what
/// makes the fold byte-for-byte non-regressive against the
/// `scan_contains(span, "under your control")` boolean checks it replaces
/// (Rootweaver Druid's one sentence carries both `"under your control"` and
/// `"under their control"`, and today's literal check returns `You`).
///
/// ONE [`nom_primitives::scan_at_word_boundaries`] pass PER POSSESSOR, gated by
/// `verify` — never a hand-rolled scanning loop. Order-independence is
/// structural, not a property of where the clauses happen to sit in the text.
pub(crate) fn fold_control_clauses(span: &str) -> Option<ControlClausePossessor> {
    [
        ControlClausePossessor::You,
        ControlClausePossessor::TheirAnaphor,
        ControlClausePossessor::ThatPlayerDemonstrative,
        ControlClausePossessor::Owner,
    ]
    .into_iter()
    .find(|&want| {
        nom_primitives::scan_at_word_boundaries(span, move |i| {
            verify(parse_control_clause, move |p: &ControlClausePossessor| {
                *p == want
            })
            .parse(i)
        })
        .is_some()
    })
}

/// CR 608.2c (docs/MagicCompRules.txt:2793): map a relative-player parse scope
/// to a `ControllerRef` the CR 110.2a binding may legally use. EXHAUSTIVE, NO
/// WILDCARD, FAIL-CLOSED — a future `ControllerRef` variant is a compile error
/// here rather than a silently-admitted controller binding.
///
/// The traced precedent `resolve_player_anaphor_damage_recipient`
/// (`oracle_effect/lower.rs`) MAPS rather than passes through, for the same
/// reason; unlike it, this table has no `_ =>` arm.
///
/// REJECTED SOURCE, recorded so it is not re-derived: the traced function's
/// third source — the CR 608.2k (:2814) `ctx.subject` bare-player-filter
/// fallback — maps unconditionally to `TriggeringPlayer`. At these seams that
/// is WRONG for The Beamtown Bullies (an ACTIVATED ability whose "target
/// opponent" subject is a TARGET, so there is no trigger event in scope at all)
/// and for Endless Whispers (a dies-trigger whose "that player" is a CHOSEN
/// opponent, not the triggering player), and REDUNDANT for Gerrymandering
/// (already bound by the `ScopedPlayer` arm). Unsafe in two of three cases,
/// redundant in the third.
///
/// DECLARED RUNTIME CAVEAT: `ScopedPlayer` resolves through
/// `game/filter.rs::scoped_player_or_controller`, which falls back through the
/// triggering-event player to the source's controller — it does NOT fail
/// closed. That is safe here because the only seeders that place `ScopedPlayer`
/// in `relative_player_scope` at these seams are the `player_scope.is_some()`
/// rung and the trigger-scope seeder, both of which mean the clause IS fanned
/// out over an "each player" population (CR 115.10 @ :886, CR 608.2e @ :2798).
fn map_relative_player_scope(scope: &ControllerRef) -> Option<ControllerRef> {
    match scope {
        // CR 115.10 (:886): the current player of an "each player / each
        // opponent" fan-out. Production-proven end-to-end.
        ControllerRef::ScopedPlayer => Some(ControllerRef::ScopedPlayer),
        // CR 608.2c: a player chosen during resolution by an earlier
        // `Effect::Choose { choice_type: Player }` in the same chain.
        ControllerRef::ChosenPlayer { index } => {
            Some(ControllerRef::ChosenPlayer { index: *index })
        }
        // CR 603.2 (:2561): the player identified by the triggering event.
        ControllerRef::TriggeringPlayer => Some(ControllerRef::TriggeringPlayer),
        // REFUSED. Two incompatible provenances feed these: a genuine
        // per-opponent fan-out, and a trigger-event alias that the traced
        // precedent itself re-maps to `TriggeringPlayer`. Worse, `filter.rs`
        // resolves both by reading the FIRST `TargetRef::Player` in the
        // ability's targets, so on a card whose player target is unrelated to
        // the moved object (or absent entirely) every permanent would enter
        // under one arbitrary player's control.
        ControllerRef::TargetPlayer | ControllerRef::TargetOpponent => None,
        // CR 102.2 (:252) / CR 102.3 (:254): `Opponent` is a CLASS of players,
        // not a player. `controller_ref_player` yields `None` for it, which
        // surfaces at runtime as an `InvalidParam` rather than a controller.
        ControllerRef::Opponent => None,
        // No Population-A card exercises the remainder; each becomes a one-line
        // arm the moment printed evidence for it exists. Enumerated rather than
        // wildcarded so adding a `ControllerRef` variant breaks the build here.
        ControllerRef::You
        | ControllerRef::ParentTargetController
        | ControllerRef::ParentTargetOwner
        | ControllerRef::DefendingPlayer
        | ControllerRef::SourceChosenPlayer
        | ControllerRef::EnchantedPlayer
        | ControllerRef::ActivePlayer => None,
    }
}

/// CR 608.2c: name the antecedent for a third-person control-clause anaphor.
///
/// N1-before-N2 (CR 608.2c, "read the whole text and apply the rules of
/// English"): the NEARER antecedent wins, and the moved object's own owner NP
/// sits inside the very clause the anaphor terminates.
pub(crate) fn name_entry_control_antecedent(
    moved_object: Option<&TargetFilter>,
    ctx: &ParseContext,
) -> ControlAnaphorAntecedent {
    if moved_object.is_some_and(filter_names_third_person_owner) {
        return ControlAnaphorAntecedent::MovedObjectOwner;
    }
    match ctx
        .relative_player_scope
        .as_ref()
        .and_then(map_relative_player_scope)
    {
        Some(r) => ControlAnaphorAntecedent::ContextPlayer(r),
        None => ControlAnaphorAntecedent::Unnameable,
    }
}

/// CR 108.3 (docs/MagicCompRules.txt:564): true when `filter` carries
/// `FilterProp::Owned { controller }` for a THIRD-PERSON player — i.e. the
/// clause's own noun phrase already named an owner that is not the resolving
/// player. Recursion mirrors `game/filter.rs::filter_contains_last_zone_changed`.
///
/// Deliberately EXCLUDED:
/// - `Owned { You }` — "you" is not third person, so there is no anaphor to
///   bind; synthetic-only, no printed card reaches it.
/// - `TypedFilter.controller` — that is a CONTROL noun phrase (CR 109.4 @ :594),
///   and control is not ownership. This is the branch The Beamtown Bullies
///   declines under, together with the "no `Owned` prop at all" case.
fn filter_names_third_person_owner(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.properties.iter().any(|p| {
            matches!(
                p,
                FilterProp::Owned { controller } if *controller != ControllerRef::You
            )
        }),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_names_third_person_owner)
        }
        TargetFilter::Not { filter } => filter_names_third_person_owner(filter),
        TargetFilter::TrackedSetFiltered { filter, .. } => filter_names_third_person_owner(filter),
        _ => false,
    }
}

/// CR 110.2a: the single binding authority. Turns a parsed possessor plus a
/// named antecedent into the `EntersUnderSpec` the IR carries.
///
/// | possessor | antecedent | result | CR |
/// |---|---|---|---|
/// | `You` | any | `Override(You)` | 109.5 (:610) |
/// | `Owner` | any | `Default` | 110.2 (:616), encoded as per-object owner |
/// | `TheirAnaphor` | `MovedObjectOwner` | `Override(ParentTargetOwner)` | 400.1+400.3+404.1+108.3 |
/// | `TheirAnaphor` | `ContextPlayer(r)` | `Override(r)` | 608.2c |
/// | `TheirAnaphor` | `Unnameable` | `UnboundAnaphor` | fail closed |
/// | `ThatPlayerDemonstrative` | `ContextPlayer(r)` | `Override(r)` | 608.2c |
/// | `ThatPlayerDemonstrative` | otherwise | `UnboundAnaphor` | fail closed |
///
/// `ThatPlayerDemonstrative` is licensed by the context-player source ONLY:
/// `ability_utils::parent_target_owner` skips `TargetRef::Player`, so binding a
/// demonstrative PLAYER reference to `ParentTargetOwner` would resolve to
/// `None` and raise `InvalidParam` at runtime.
pub(crate) fn bind_control_clause(
    possessor: Option<ControlClausePossessor>,
    antecedent: ControlAnaphorAntecedent,
) -> EntersUnderSpec {
    let Some(possessor) = possessor else {
        return EntersUnderSpec::Default;
    };
    match (possessor, antecedent) {
        // CR 109.5 (:610): "you"/"your" names the resolving player as an
        // explicit battlefield-entry controller.
        (ControlClausePossessor::You, _) => EntersUnderSpec::Override(ControllerRef::You),
        // CR 110.2 (:616): `Default` is the existing IR spelling for the
        // resolver's per-moved-object owner behavior. It must not be collapsed
        // to the CR 110.2a instructed-player default.
        (ControlClausePossessor::Owner, _) => EntersUnderSpec::Default,
        // CR 400.1 (:1933) + CR 400.3 (:1937) + CR 404.1 (:2030) + CR 108.3
        // (:564): a card in a graveyard is in ITS OWNER'S graveyard, so the
        // clause's own owner NP and the moved object's owner are the same
        // player. `ParentTargetOwner` reads exactly that.
        (ControlClausePossessor::TheirAnaphor, ControlAnaphorAntecedent::MovedObjectOwner) => {
            EntersUnderSpec::Override(ControllerRef::ParentTargetOwner)
        }
        // CR 608.2c: a mapped, context-declared referent.
        (
            ControlClausePossessor::TheirAnaphor | ControlClausePossessor::ThatPlayerDemonstrative,
            ControlAnaphorAntecedent::ContextPlayer(r),
        ) => EntersUnderSpec::Override(r),
        // Fail closed: no nameable antecedent (and, for the demonstrative, an
        // object-owner antecedent it may not legally use). Keep these arms
        // explicit so adding a possessor cannot silently inherit this result.
        (ControlClausePossessor::TheirAnaphor, ControlAnaphorAntecedent::Unnameable) => {
            EntersUnderSpec::UnboundAnaphor(ControlClausePossessor::TheirAnaphor)
        }
        (
            ControlClausePossessor::ThatPlayerDemonstrative,
            ControlAnaphorAntecedent::MovedObjectOwner | ControlAnaphorAntecedent::Unnameable,
        ) => EntersUnderSpec::UnboundAnaphor(ControlClausePossessor::ThatPlayerDemonstrative),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{TypeFilter, TypedFilter};

    fn possessor(text: &str) -> Option<ControlClausePossessor> {
        parse_control_clause(text).ok().map(|(_, p)| p)
    }

    #[test]
    fn parses_your_control() {
        assert_eq!(
            possessor("under your control"),
            Some(ControlClausePossessor::You)
        );
    }

    #[test]
    fn parses_every_printed_owner_spelling() {
        for text in [
            "under its owner's control",
            "under its owner\u{2019}s control",
            "under their owner's control",
            "under their owner\u{2019}s control",
            "under his owner's control",
            "under her owner's control",
            "under their owners' control",
            "under their owners\u{2019} control",
        ] {
            assert_eq!(
                possessor(text),
                Some(ControlClausePossessor::Owner),
                "{text}"
            );
        }
    }

    #[test]
    fn parses_bare_their_as_anaphor() {
        assert_eq!(
            possessor("under their control"),
            Some(ControlClausePossessor::TheirAnaphor)
        );
    }

    #[test]
    fn parses_that_player_demonstrative_both_apostrophes() {
        assert_eq!(
            possessor("under that player's control"),
            Some(ControlClausePossessor::ThatPlayerDemonstrative)
        );
        assert_eq!(
            possessor("under that player\u{2019}s control"),
            Some(ControlClausePossessor::ThatPlayerDemonstrative)
        );
    }

    /// The owner arms MUST precede the bare `their` arm: nom's `alt` does not
    /// re-enter once the outer `terminated(.., tag(" control"))` commits.
    /// Reordering them locally makes this assertion fail.
    #[test]
    fn owner_arms_win_over_bare_their() {
        assert_eq!(
            possessor("under their owner's control"),
            Some(ControlClausePossessor::Owner)
        );
        assert_eq!(
            possessor("under their owners' control"),
            Some(ControlClausePossessor::Owner)
        );
    }

    #[test]
    fn deferred_and_unrelated_forms_do_not_parse() {
        // Deferred explicit noun phrase — not handled at these seams.
        assert_eq!(possessor("under an opponent's control"), None);
        // NON-MATCH BY NON-BACKTRACKING: `tag("your")` matches, then the outer
        // `tag(" control")` fails on " opponents' control" and nom does not
        // re-enter the possessor alt. Pinned so a future `your`-sub-axis cannot
        // change this silently.
        assert_eq!(possessor("under your opponents' control"), None);
        // Separate templating ("under the control of ~"), out of scope.
        assert_eq!(possessor("under the control of an opponent"), None);
    }

    #[test]
    fn leading_clause_requires_the_separating_space() {
        assert_eq!(
            parse_leading_control_clause(" under their control").ok(),
            Some(("", ControlClausePossessor::TheirAnaphor))
        );
        assert!(parse_leading_control_clause("under their control").is_err());
    }

    #[test]
    fn leading_clause_leaves_trailing_riders_unconsumed() {
        assert_eq!(
            parse_leading_control_clause(" under your control face down and tapped").ok(),
            Some((" face down and tapped", ControlClausePossessor::You))
        );
    }

    /// B3 guard: the canonical recognizer has NO leading space, which is what
    /// lets `scan_at_word_boundaries` find it mid-sentence.
    #[test]
    fn scan_at_word_boundaries_finds_the_no_leading_space_form() {
        let found = nom_primitives::scan_at_word_boundaries(
            "return that card to the battlefield under their control",
            parse_control_clause,
        );
        assert_eq!(found, Some(ControlClausePossessor::TheirAnaphor));
    }

    #[test]
    fn fold_is_order_independent_and_you_wins() {
        let their_first = "put one onto the battlefield under their control and the rest \
                           onto the battlefield under your control";
        let your_first = "put one onto the battlefield under your control and the rest \
                          onto the battlefield under their control";
        assert_eq!(
            fold_control_clauses(their_first),
            Some(ControlClausePossessor::You)
        );
        assert_eq!(
            fold_control_clauses(your_first),
            Some(ControlClausePossessor::You)
        );
    }

    #[test]
    fn fold_prefers_an_anaphor_over_owner() {
        assert_eq!(
            fold_control_clauses(
                "to the battlefield under its owner's control, then to the battlefield \
                 under their control"
            ),
            Some(ControlClausePossessor::TheirAnaphor)
        );
    }

    #[test]
    fn fold_returns_none_when_no_clause_is_present() {
        assert_eq!(
            fold_control_clauses("return target creature card to the battlefield tapped"),
            None
        );
    }

    fn owned_by(controller: ControllerRef) -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Permanent],
            controller: None,
            properties: vec![
                FilterProp::Owned { controller },
                FilterProp::InZone {
                    zone: crate::types::zones::Zone::Graveyard,
                },
            ],
        })
    }

    #[test]
    fn ownership_licenses_n1_only_for_third_person() {
        // Jailbreak's real parsed filter.
        assert!(filter_names_third_person_owner(&owned_by(
            ControllerRef::Opponent
        )));
        // Synthetic-only: "you" is not third person, so no anaphor binds.
        assert!(!filter_names_third_person_owner(&owned_by(
            ControllerRef::You
        )));
    }

    #[test]
    fn a_control_noun_phrase_does_not_license_n1() {
        // The Beamtown Bullies' real filter: a CONTROL constraint, no `Owned`
        // prop at all. CR 109.4 (:594) — control is not ownership.
        let beamtown = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: Some(ControllerRef::You),
            properties: vec![FilterProp::InZone {
                zone: crate::types::zones::Zone::Graveyard,
            }],
        });
        assert!(!filter_names_third_person_owner(&beamtown));
        assert!(!filter_names_third_person_owner(&TargetFilter::SelfRef));
        assert!(!filter_names_third_person_owner(&TargetFilter::Any));
    }

    #[test]
    fn ownership_recursion_reaches_nested_filters() {
        let nested = TargetFilter::And {
            filters: vec![
                TargetFilter::Any,
                TargetFilter::Or {
                    filters: vec![owned_by(ControllerRef::Opponent)],
                },
            ],
        };
        assert!(filter_names_third_person_owner(&nested));
        let nested_not = TargetFilter::Not {
            filter: Box::new(owned_by(ControllerRef::Opponent)),
        };
        assert!(filter_names_third_person_owner(&nested_not));
    }

    fn ctx_with_scope(scope: Option<ControllerRef>) -> ParseContext {
        ParseContext {
            relative_player_scope: scope,
            ..ParseContext::default()
        }
    }

    /// BLOCKER-2's negative fixture: the full 13-input mapping table. Adding a
    /// wildcard arm to `map_relative_player_scope` makes this fail.
    #[test]
    fn relative_player_scope_maps_exhaustively_and_fails_closed() {
        let admitted = [
            (ControllerRef::ScopedPlayer, ControllerRef::ScopedPlayer),
            (
                ControllerRef::ChosenPlayer { index: 2 },
                ControllerRef::ChosenPlayer { index: 2 },
            ),
            (
                ControllerRef::TriggeringPlayer,
                ControllerRef::TriggeringPlayer,
            ),
        ];
        for (input, expected) in admitted {
            assert_eq!(
                map_relative_player_scope(&input),
                Some(expected),
                "{input:?}"
            );
        }
        for refused in [
            ControllerRef::You,
            ControllerRef::Opponent,
            ControllerRef::TargetPlayer,
            ControllerRef::TargetOpponent,
            ControllerRef::ParentTargetController,
            ControllerRef::ParentTargetOwner,
            ControllerRef::DefendingPlayer,
            ControllerRef::SourceChosenPlayer,
            ControllerRef::EnchantedPlayer,
            ControllerRef::ActivePlayer,
        ] {
            assert_eq!(map_relative_player_scope(&refused), None, "{refused:?}");
        }
    }

    #[test]
    fn antecedent_prefers_the_nearer_object_owner() {
        // The forced diagonal: BOTH an owning object NP and a mapped scope. The
        // nearer antecedent (CR 608.2c) is the object's owner. Swapping the
        // priority inside `name_entry_control_antecedent` fails this.
        let ctx = ctx_with_scope(Some(ControllerRef::ScopedPlayer));
        assert_eq!(
            name_entry_control_antecedent(Some(&owned_by(ControllerRef::Opponent)), &ctx),
            ControlAnaphorAntecedent::MovedObjectOwner
        );
    }

    #[test]
    fn antecedent_falls_through_to_the_mapped_scope() {
        let ctx = ctx_with_scope(Some(ControllerRef::ScopedPlayer));
        assert_eq!(
            name_entry_control_antecedent(Some(&TargetFilter::Any), &ctx),
            ControlAnaphorAntecedent::ContextPlayer(ControllerRef::ScopedPlayer)
        );
        assert_eq!(
            name_entry_control_antecedent(None, &ctx),
            ControlAnaphorAntecedent::ContextPlayer(ControllerRef::ScopedPlayer)
        );
    }

    #[test]
    fn antecedent_is_unnameable_without_a_licensed_source() {
        let none = ctx_with_scope(None);
        assert_eq!(
            name_entry_control_antecedent(Some(&TargetFilter::Any), &none),
            ControlAnaphorAntecedent::Unnameable
        );
        // The Turtle Tracks trap: a `TargetPlayer` scope is refused at the
        // table, so it never reaches the binder.
        let target_player = ctx_with_scope(Some(ControllerRef::TargetPlayer));
        assert_eq!(
            name_entry_control_antecedent(Some(&TargetFilter::Any), &target_player),
            ControlAnaphorAntecedent::Unnameable
        );
    }

    /// The full seven-row binding table.
    #[test]
    fn binding_table_is_complete() {
        use ControlAnaphorAntecedent::*;
        use ControlClausePossessor::*;
        assert_eq!(
            bind_control_clause(None, Unnameable),
            EntersUnderSpec::Default
        );
        assert_eq!(
            bind_control_clause(Some(You), Unnameable),
            EntersUnderSpec::Override(ControllerRef::You)
        );
        assert_eq!(
            bind_control_clause(Some(Owner), MovedObjectOwner),
            EntersUnderSpec::Default
        );
        assert_eq!(
            bind_control_clause(Some(TheirAnaphor), MovedObjectOwner),
            EntersUnderSpec::Override(ControllerRef::ParentTargetOwner)
        );
        assert_eq!(
            bind_control_clause(
                Some(TheirAnaphor),
                ContextPlayer(ControllerRef::ScopedPlayer)
            ),
            EntersUnderSpec::Override(ControllerRef::ScopedPlayer)
        );
        assert_eq!(
            bind_control_clause(Some(TheirAnaphor), Unnameable),
            EntersUnderSpec::UnboundAnaphor(TheirAnaphor)
        );
        assert_eq!(
            bind_control_clause(
                Some(ThatPlayerDemonstrative),
                ContextPlayer(ControllerRef::ChosenPlayer { index: 0 })
            ),
            EntersUnderSpec::Override(ControllerRef::ChosenPlayer { index: 0 })
        );
        // CR 108.3 / `parent_target_owner` skips `TargetRef::Player`: a
        // demonstrative PLAYER reference may not bind to the object's owner.
        assert_eq!(
            bind_control_clause(Some(ThatPlayerDemonstrative), MovedObjectOwner),
            EntersUnderSpec::UnboundAnaphor(ThatPlayerDemonstrative)
        );
        assert_eq!(
            bind_control_clause(Some(ThatPlayerDemonstrative), Unnameable),
            EntersUnderSpec::UnboundAnaphor(ThatPlayerDemonstrative)
        );
    }

    #[test]
    fn printed_clauses_round_trip_through_the_parser() {
        for p in [
            ControlClausePossessor::You,
            ControlClausePossessor::Owner,
            ControlClausePossessor::TheirAnaphor,
            ControlClausePossessor::ThatPlayerDemonstrative,
        ] {
            assert_eq!(possessor(p.printed_clause()), Some(p));
        }
    }
}
