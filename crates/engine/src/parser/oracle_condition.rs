use std::str::FromStr;

use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::{multispace0, one_of};
use nom::combinator::{all_consuming, opt, value};
use nom::sequence::terminated;
use nom::Parser;

use super::oracle_nom::condition as nom_condition;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_target::parse_type_phrase;
use crate::types::ability::{
    CommanderOwnership, Comparator, ControllerRef, FilterProp, ParsedCondition, PlayerScope,
    QuantityExpr, QuantityRef, StaticCondition, TargetFilter, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterMatch;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

fn scan_source_zone_filter(text: &str) -> Option<Zone> {
    let mut offset = 0;
    while offset <= text.len() {
        if let Ok((rest, zone)) = super::oracle_nom::filter::parse_zone_filter(&text[offset..]) {
            if rest
                .chars()
                .next()
                .is_none_or(|ch| matches!(ch, ' ' | ',' | '.'))
            {
                return Some(zone);
            }
        }
        match text[offset..].find(' ') {
            Some(i) => offset += i + 1,
            None => break,
        }
    }
    None
}

/// CR 601.3 / CR 602.5: Parse a restriction condition from Oracle text into a typed
/// `ParsedCondition`. These conditions gate whether a spell can be cast (CR 601.3) or
/// an ability activated (CR 602.5).
///
/// The shared static-condition grammar (`parse_inner_condition`) is the PRIMARY
/// authority: a restriction condition is an ordinary game-state condition, so it must
/// be recognized by the same combinators that recognize the identical phrase in an
/// "as long as" / "if" static. Only when the shared grammar does not recognize the
/// phrase at all does the restriction-only fallback run — and that fallback holds only
/// forms whose *referent* is supplied by the restriction context (the in-flight spell)
/// or whose exact restriction evaluator has no `StaticCondition` counterpart.
///
/// Returns `None` when the phrase is unrecognized OR when it parses to a
/// `StaticCondition` that `ParsedCondition` cannot represent exactly. Callers must
/// treat `None` as "this candidate parse failed" and leave the source text for the
/// ordinary `Effect::Unimplemented` fallback — never store it as a permissive
/// `RequiresCondition { condition: None }`.
pub fn parse_restriction_condition(text: &str) -> Option<ParsedCondition> {
    let lower = text.trim().trim_end_matches('.').to_lowercase();
    match parse_shared_restriction_condition(&lower) {
        // The shared grammar recognized the phrase and `ParsedCondition` can hold it.
        SharedRestrictionParse::Converted(condition) => Some(condition),
        // The shared grammar recognized the phrase but the restriction evaluator has no
        // exact representation for it. Fail the parse — do NOT fall through to the
        // restriction-only grammar, which would reinterpret the same text under a
        // weaker reading and silently drop the part `ParsedCondition` cannot express.
        SharedRestrictionParse::Unsupported => None,
        SharedRestrictionParse::NoMatch => parse_restriction_only_condition(&lower),
    }
}

/// Tri-state outcome of running the shared grammar over a restriction phrase.
/// `Unsupported` is distinct from `NoMatch` on purpose: a phrase the shared grammar
/// *understood* must never be re-parsed by the narrower restriction-only grammar,
/// because that fallback would happily produce a different (lossy) condition from the
/// same words.
enum SharedRestrictionParse {
    /// The shared grammar did not recognize the phrase.
    NoMatch,
    /// Recognized, and exactly representable as a `ParsedCondition`.
    Converted(ParsedCondition),
    /// Recognized, but not exactly representable as a `ParsedCondition`.
    ///
    /// Carries no payload: the rejected `StaticCondition` is not needed to decide what to
    /// do (the caller fails the parse either way), and tests assert WHICH variant was
    /// rejected by calling `static_condition_to_restriction_condition` directly. Keeping a
    /// field only tests read would be dead weight in every production build.
    Unsupported,
}

/// CR 601.3 / CR 602.5: Run the shared static-condition grammar over the whole
/// restriction phrase, then convert. The parse must be all-consuming — a partial parse
/// means the tail carries semantics the condition would silently drop — and trailing
/// punctuation is consumed by the combinator rather than pre-stripped by the caller.
fn parse_shared_restriction_condition(text: &str) -> SharedRestrictionParse {
    let parsed = all_consuming(terminated(
        nom_condition::parse_inner_condition,
        (
            multispace0,
            opt(one_of::<_, _, OracleError<'_>>(".,;")),
            multispace0,
        ),
    ))
    .parse(text);
    match parsed {
        Err(_) => SharedRestrictionParse::NoMatch,
        Ok((_, condition)) => match static_condition_to_restriction_condition(condition) {
            Some(converted) => SharedRestrictionParse::Converted(converted),
            None => SharedRestrictionParse::Unsupported,
        },
    }
}

/// CR 601.3 / CR 602.5: The restriction-ONLY grammar. Runs only when the shared
/// static-condition grammar does not recognize the phrase at all.
///
/// Every parser reachable from here must justify its existence against
/// `parse_inner_condition` — see the per-parser notes. Two justifications are valid:
///
/// 1. **Restriction-context referent.** The phrase's subject is supplied by the
///    restriction context and does not exist in a static ability (`"it targets …"`,
///    where `it` is the in-flight spell of CR 601.3d).
/// 2. **No shared vocabulary.** `StaticCondition` has no variant for the concept at
///    all, so `parse_inner_condition` structurally cannot produce it.
///
/// A parser that exists merely because the shared grammar spells the phrase differently
/// is NOT justified — teach `parse_inner_condition` the phrasing instead (that is where
/// every static ability with the same words already looks).
fn parse_restriction_only_condition(text: &str) -> Option<ParsedCondition> {
    // JUSTIFICATION 1 — restriction-context referent (CR 601.3d).
    //
    // "it targets a [filter]" gates a casting permission on the chosen targets of the
    // spell BEING CAST (Timely Ward: "you may cast this spell as though it had flash if
    // it targets a commander"). The pronoun `it` denotes the in-flight spell, an object
    // that exists only during CR 601.2 proposal — there is no such referent when a
    // static ability is evaluated, so `parse_inner_condition` structurally cannot bind
    // it. `StaticCondition::SourceMatchesFilter` is NOT a substitute: its subject is the
    // ability's own source, not the pending spell's targets.
    //   positive: "it targets a commander" -> SpellTargetsFilter { IsCommander }
    //   hostile:  "it targets a frob the wobble" -> None (unknown filter stays unsupported)
    if let Some(condition) = parse_spell_targets_filter(text) {
        return Some(condition);
    }

    // JUSTIFICATION 2 — the shared grammar cannot round-trip the referent.
    //
    // `ParsedCondition` models source predicates as FIXED leaves (`SourceIsColor`,
    // `SourceLacksKeyword`, `SourceUntappedAttachedTo`). `parse_inner_condition` models
    // the same predicates as the filter-carrying `StaticCondition::SourceMatchesFilter`
    // (or, for the attached subject, as a recipient-relative condition). A
    // `ParsedCondition` cannot hold a `TargetFilter` for its source, so
    // `static_condition_to_restriction_condition` has nowhere to put one — and
    // destructuring individual filter shapes back into the fixed leaves would build for
    // the card, not the class. Until the two condition vocabularies are aligned, the
    // restriction reading of a source predicate must be produced here.
    //   positive: "~ is blue" -> SourceIsColor { Blue }
    //   hostile:  "~ is quixotic" -> None (unknown predicate stays unsupported)
    if let Some(condition) = parse_source_condition(text) {
        return Some(condition);
    }

    // JUSTIFICATION 3 — PHRASING GAP, NOT A VOCABULARY GAP. Port pending.
    //
    // Read this before adding anything below. Each parser here is UNJUSTIFIED under the
    // rules above: the shared vocabulary CAN express its condition
    // (`QuantityRef::{AttackedThisTurn { filter }, BattlefieldEntriesThisTurn { player,
    // filter }, ObjectCountBySharedQuality, PlayerActionsThisTurn, HandSize}` all exist),
    // and `static_condition_to_restriction_condition` would convert the result through
    // `QuantityComparison` unchanged. They survive only because `parse_inner_condition`
    // does not yet SPELL these phrasings — a gap in the grammar, not in the type system.
    //
    // They are not deleted here because moving them is not a parser edit: each port
    // changes the runtime condition representation for live cards (a fixed
    // `ParsedCondition` leaf becomes a `QuantityComparison` resolved through
    // `game::quantity`), which needs its own runtime-equivalence proof and full-pool
    // dual-run. That work is tracked as **P02-U3b (shared-grammar phrasing parity)**;
    // deleting them now would silently drop the cards listed beside each parser.
    //
    // NOTE for P02-U3b: the source-predicate family above is a PREREQUISITE, not a
    // sibling. Until `ParsedCondition` can hold a filter for its source, teaching the
    // shared grammar a new source phrasing REMOVES restriction support for it (the shared
    // parse starts succeeding, the conversion then rejects it). Align the vocabularies
    // first.
    //
    // The rule for anyone extending this file: a NEW restriction phrase never lands here.
    // Teach `parse_inner_condition` the phrasing — that is where every static ability
    // with the same words already looks.

    // "you control three or more lands with the same name" (Endless Atlas, Sceptre of
    // Eternal Glory). Shared target: `ObjectCountBySharedQuality { quality: Name }`.
    if let Some(condition) = parse_you_control_condition(text) {
        return Some(condition);
    }

    // "you have exactly zero or seven cards in hand"; "you have no land cards in hand".
    // Shared target: `Or` over `QuantityComparison { HandSize, EQ }` / an `ObjectCount`
    // filtered to `InZone { Hand }`.
    if let Some(condition) = parse_hand_condition(text) {
        return Some(condition);
    }

    // "an opponent had two or more creatures enter the battlefield under their control
    // this turn"; "an opponent searched their library this turn"; "you've been attacked
    // this step". Shared targets: `BattlefieldEntriesThisTurn { player, filter }` and
    // `PlayerActionsThisTurn { action }`. (`BeenAttackedThisStep` is the one leaf here
    // with NO shared counterpart — `StaticCondition` has no per-STEP attack history.)
    if let Some(condition) = parse_event_condition(text) {
        return Some(condition);
    }

    // CR 508.1a: "you attacked with [N or more creatures | a <filter>] this turn"
    // (Thaumaton Torpedo). Shared target: `QuantityComparison` over
    // `AttackedThisTurn { scope, filter }`, which already carries the attacker filter.
    if let Some(condition) = parse_you_attacked_with(text) {
        return Some(condition);
    }

    None
}

/// CR 508.1a: "you attacked with [N or more creatures | a/an <filter>] [this turn]".
/// One authority for the whole attacked-with family: the bare numeric threshold carries
/// no type qualifier (`filter: None`), the typed form carries the attacker filter.
fn parse_you_attacked_with(text: &str) -> Option<ParsedCondition> {
    if let Some(count) = parse_numeric_threshold(text, "you attacked with ", " creatures this turn")
    {
        return Some(ParsedCondition::YouAttackedWithAtLeast {
            count: count as u32,
            filter: None,
        });
    }
    if let Some(count) =
        parse_numeric_threshold(text, "you attacked with ", " or more creatures this turn")
    {
        return Some(ParsedCondition::YouAttackedWithAtLeast {
            count: count as u32,
            filter: None,
        });
    }
    // The trailing "this turn" may already be stripped upstream (an activated-ability
    // duration parser peels it before the cost-reduction condition is reparsed), so the
    // typed form accepts both the suffixed and bare shapes.
    parse_you_attacked_with_filter(text)
}

/// CR 508.1a: Parse "you attacked with a/an <filter>[ this turn]" into a
/// `ParsedCondition::YouAttackedWithAtLeast { count: 1, filter }`. The `<filter>`
/// is delegated to `parse_type_phrase` so the whole class of attacker qualifiers
/// (Spacecraft, Vehicle, a specific creature type, …) is covered by the shared
/// type-phrase combinator rather than a per-card literal. Returns `None` unless
/// the entire phrase after the filter is consumed (modulo an optional " this
/// turn"), keeping unrecognized qualifiers an honest gap.
fn parse_you_attacked_with_filter(text: &str) -> Option<ParsedCondition> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("you attacked with ")
        .parse(text)
        .ok()?;
    let (filter, remainder) = parse_type_phrase(rest);
    // Reject the bare/untyped case: `parse_type_phrase` returns `Any` when no
    // type word matched, which would over-match "you attacked with three or more
    // creatures this turn" (handled by the numeric thresholds above). Require a
    // concrete typed filter here.
    if matches!(filter, TargetFilter::Any) {
        return None;
    }
    // Consume an optional trailing " this turn" and any trailing punctuation with
    // combinators (no manual string trimming), then require the phrase to be fully
    // consumed so unrecognized qualifiers stay an honest gap. The duration suffix
    // may already be stripped upstream, so it is optional.
    let (remainder, _) = multispace0::<_, OracleError<'_>>(remainder).ok()?;
    let (remainder, _) = opt(tag::<_, _, OracleError<'_>>("this turn"))
        .parse(remainder)
        .ok()?;
    let (remainder, _) = multispace0::<_, OracleError<'_>>(remainder).ok()?;
    let (remainder, _) = opt(one_of::<_, _, OracleError<'_>>(".,;"))
        .parse(remainder)
        .ok()?;
    let (remainder, _) = multispace0::<_, OracleError<'_>>(remainder).ok()?;
    if !remainder.is_empty() {
        return None;
    }
    Some(ParsedCondition::YouAttackedWithAtLeast {
        count: 1,
        filter: Some(filter),
    })
}

/// CR 601.3 / CR 602.5: Convert a shared `StaticCondition` into the restriction
/// evaluator's `ParsedCondition`, or reject it.
///
/// The match is EXHAUSTIVE by design — no wildcard arm. `StaticCondition` and
/// `ParsedCondition` are two independently grown vocabularies with only a partial
/// overlap, so a new `StaticCondition` variant must not silently acquire a permissive
/// restriction reading. Adding a variant to `StaticCondition` must break this build and
/// force an explicit accept/reject decision here.
///
/// Rejection (`None`) is not a bug — it is the honest answer for a condition the
/// restriction evaluator cannot represent EXACTLY. The caller turns `None` into a
/// failed candidate parse, so the source clause stays visible as `Effect::Unimplemented`
/// rather than becoming a restriction that silently evaluates to "always true" or, worse,
/// to a weaker approximation of the printed text.
fn static_condition_to_restriction_condition(
    condition: StaticCondition,
) -> Option<ParsedCondition> {
    match condition {
        // ---- Exactly representable -------------------------------------------------
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => Some(ParsedCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }),
        // CR 608.2c: logical composition recurses. If ANY branch is nonrepresentable the
        // whole compound is rejected — converting `A or B` to just `A` would silently
        // narrow the printed condition.
        StaticCondition::And { conditions } => conditions
            .into_iter()
            .map(static_condition_to_restriction_condition)
            .collect::<Option<Vec<_>>>()
            .map(|conditions| ParsedCondition::And { conditions }),
        StaticCondition::Or { conditions } => conditions
            .into_iter()
            .map(static_condition_to_restriction_condition)
            .collect::<Option<Vec<_>>>()
            .map(|conditions| ParsedCondition::Or { conditions }),
        StaticCondition::Not { condition } => static_condition_to_restriction_condition(*condition)
            .map(|condition| ParsedCondition::Not {
                condition: Box::new(condition),
            }),
        // CR 601.3 + CR 602.5: a presence check ("a creature is attacking you",
        // "you control a [type]") is equivalent to "the count of matching
        // objects is at least one". `ParsedCondition` has no `IsPresent`
        // variant, so reuse its generic `QuantityComparison` over an
        // `ObjectCount` of the same filter — letting cast/activation
        // restrictions ("Cast this spell only if a creature is attacking you" —
        // Confront the Assault) reuse the full presence-condition vocabulary.
        StaticCondition::IsPresent {
            filter: Some(filter),
        } => Some(ParsedCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        }),
        // CR 102.1: "it's your turn" — the active player is the scoped player.
        // The `Not` recursion arm above yields `Not(IsYourTurn)` for
        // "it's not your turn".
        StaticCondition::DuringYourTurn => Some(ParsedCondition::IsYourTurn),
        // CR 903.3d: "If an effect refers to controlling a commander, it refers to a
        // permanent on the battlefield that is a commander" — regardless of who OWNS it.
        // That is exactly an `ObjectCount` over the `IsCommander` filter scoped to your
        // control, so it converts through the same presence bridge as `IsPresent`.
        //
        // `CommanderOwnership::Own` ("your commander") additionally requires you to own
        // the permanent, and `TargetFilter` has no owner axis — it is rejected below
        // rather than silently widened to "any commander you control", which would let
        // a STOLEN commander satisfy a condition the card restricts to your own.
        StaticCondition::ControlsCommander {
            ownership: CommanderOwnership::Any,
        } => Some(ParsedCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(TypedFilter {
                        controller: Some(ControllerRef::You),
                        properties: vec![FilterProp::IsCommander],
                        ..Default::default()
                    }),
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        }),
        // Source zone/state leaves with an exact restriction evaluator.
        StaticCondition::SourceInZone { zone } => Some(ParsedCondition::SourceInZone { zone }),
        StaticCondition::SourceIsAttacking => Some(ParsedCondition::SourceIsAttacking),
        StaticCondition::SourceIsBlocked => Some(ParsedCondition::SourceIsBlocked),
        StaticCondition::SourceEnteredThisTurn => Some(ParsedCondition::SourceEnteredThisTurn),
        // CR 301.5 + CR 602.5b: "this permanent is attached to a creature" (Reconfigure).
        StaticCondition::SourceAttachedToCreature => Some(ParsedCondition::SourceAttachedTo {
            required_type: CoreType::Creature,
        }),
        // Player-state leaves with an exact restriction evaluator.
        StaticCondition::HasCityBlessing => Some(ParsedCondition::HasCityBlessing),
        StaticCondition::OpponentPoisonAtLeast { count } => {
            Some(ParsedCondition::OpponentPoisonAtLeast { count })
        }
        // CR 122.1: source-counter activation gate — "Activate only if it has no time
        // counters on it" (Temple of Cyclical Time) and the counter-threshold restriction
        // class generally. Adopted from #5677 (the L02 condition lane), which solved this
        // strictly better than the version this unit first wrote.
        //
        // The constraint is "never widen a bounded band into an at-least that drops the
        // maximum". This unit satisfied it by REJECTING every band. #5677 satisfied it by
        // PRESERVING the maximum: a band lowers to `And[GE n, LE m]` over a
        // `CountersOn { Source }` quantity, so "one to three counters" stays false at four.
        // Rejecting was safe; preserving is correct. Same lowering as the `AbilityCondition`
        // peer (`oracle_effect::conditions::counter_threshold_to_condition`), so both paths
        // agree — and `CounterMatch::Any` ("a counter on it") is expressible too, via
        // `counter_type: None`, which the fixed `SourceHasCounterAtLeast` leaf could not hold.
        StaticCondition::HasCounters {
            counters,
            minimum,
            maximum,
        } => {
            let qty = QuantityExpr::Ref {
                qty: QuantityRef::CountersOn {
                    scope: crate::types::ability::ObjectScope::Source,
                    counter_type: match counters {
                        CounterMatch::OfType(ct) => Some(ct),
                        CounterMatch::Any => None,
                    },
                },
            };
            Some(counters_threshold_to_parsed_condition(
                qty, minimum, maximum,
            ))
        }

        // ---- Explicitly rejected ---------------------------------------------------
        // Not a condition at all: the parser failed to decompose the text. Evaluated
        // permissively (always true) as a static gate, which is exactly the lie a
        // restriction must not tell.
        StaticCondition::Unrecognized { .. } => None,
        // The absence of a condition. A restriction with no condition is not a restriction.
        StaticCondition::None => None,
        // CR 118.12a + CR 508.1d + CR 509.1c: an optional-cost combat tax, resolved via a
        // `WaitingFor::CombatTaxPayment` round-trip at declaration time. It is not a
        // game-state predicate and has no meaning as a cast/activation gate.
        StaticCondition::UnlessPay { .. } => None,
        // Recipient-relative: the referent is the object RECEIVING the continuous effect
        // (the enchanted/equipped creature), which only exists inside a continuous-effect
        // application. A cast/activation restriction is evaluated against the source and
        // its controller — there is no recipient — so these can never be evaluated here.
        StaticCondition::RecipientHasCounters { .. }
        | StaticCondition::RecipientMatchesFilter { .. }
        | StaticCondition::RecipientAttackingOwnerTarget { .. }
        | StaticCondition::EnchantedIsFaceDown => None,
        // No exact restriction evaluator. Each of these is a real game-state condition
        // the shared grammar understands, but `ParsedCondition` has no variant that
        // means the same thing, and approximating it would change what the card does.
        //
        // `SourceMatchesFilter` / `IsTapped` / `DefendingPlayerControls` carry an
        // arbitrary `TargetFilter`; `ParsedCondition`'s source predicates are fixed
        // leaves (`SourceIsCreature`, `SourceIsColor`, …) that cannot hold one. Picking
        // off individual filter shapes would build for the card, not the class.
        //
        // Closing any of these gaps means aligning the two vocabularies (a separate
        // migration), NOT adding a fallback here.
        StaticCondition::DevotionGE { .. }
        | StaticCondition::IsPresent { filter: None }
        | StaticCondition::ChosenColorIs { .. }
        | StaticCondition::ChosenLabelIs { .. }
        | StaticCondition::HasMaxSpeed
        | StaticCondition::SpeedGE { .. }
        | StaticCondition::DayNightIs { .. }
        | StaticCondition::CastVariantPaid { .. }
        | StaticCondition::ClassLevelGE { .. }
        | StaticCondition::DefendingPlayerControls { .. }
        | StaticCondition::SourceAttackingAlone
        | StaticCondition::SourceIsBlocking
        | StaticCondition::IsMonarch
        | StaticCondition::IsInitiative
        | StaticCondition::NoMonarch
        | StaticCondition::CompletedADungeon
        | StaticCondition::WasStartingPlayer { .. }
        | StaticCondition::SpellCastWithVariantThisTurn { .. }
        | StaticCondition::SharesColorWithMostCommonColorAmongPermanents
        | StaticCondition::SourceHasDealtDamage
        | StaticCondition::WasCast { .. }
        | StaticCondition::IsRingBearer
        | StaticCondition::RingLevelAtLeast { .. }
        | StaticCondition::ControlsCommander {
            ownership: CommanderOwnership::Own,
        }
        | StaticCondition::SourceIsTapped
        | StaticCondition::IsTapped { .. }
        | StaticCondition::SourceIsFaceUp
        | StaticCondition::SourceIsSaddled
        | StaticCondition::SourceControllerEquals { .. }
        | StaticCondition::SourceIsEquipped
        | StaticCondition::SourceIsEnchanted
        | StaticCondition::SourceIsMonstrous
        | StaticCondition::SourceIsHarnessed
        | StaticCondition::SourceMatchesFilter { .. }
        // CR 401.1 (#5692): "as long as the top card of your library is a <filter>"
        // (Vampire Nocturnus, Conspicuous Snoop). Another filter-carrying condition with
        // no `ParsedCondition` counterpart — same vocabulary asymmetry as
        // `SourceMatchesFilter`, and it lands in the same follow-up. Rejected rather than
        // approximated; a cast/activation gate that silently ignored the filter would be
        // a permissive lie.
        | StaticCondition::TopOfLibraryMatches { .. }
        | StaticCondition::SourceIsPaired
        | StaticCondition::AdditionalCostPaid
        | StaticCondition::CastingAsVariant { .. } => None,
    }
}

/// CR 122.1: Map a counter (minimum, maximum) range onto a `ParsedCondition`
/// comparison over a counter-count quantity. The restriction-side peer of
/// `oracle_effect::conditions::counter_threshold_to_condition` (which produces
/// the `AbilityCondition` form): both must agree on the (min,max)→comparator
/// lowering. A bounded range decomposes into `And[GE n, LE m]`.
fn counters_threshold_to_parsed_condition(
    qty: QuantityExpr,
    minimum: u32,
    maximum: Option<u32>,
) -> ParsedCondition {
    match (minimum, maximum) {
        // "no counters" — exactly zero.
        (0, Some(0)) => ParsedCondition::QuantityComparison {
            lhs: qty,
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        },
        // "exactly N counters".
        (n, Some(m)) if n == m => ParsedCondition::QuantityComparison {
            lhs: qty,
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
        // "N or fewer counters".
        (0, Some(n)) => ParsedCondition::QuantityComparison {
            lhs: qty,
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
        // "N or more counters" / "a counter" (1+).
        (n, None) => ParsedCondition::QuantityComparison {
            lhs: qty,
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
        // Bounded range "between N and M counters".
        (n, Some(m)) => ParsedCondition::And {
            conditions: vec![
                ParsedCondition::QuantityComparison {
                    lhs: qty.clone(),
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: n as i32 },
                },
                ParsedCondition::QuantityComparison {
                    lhs: qty,
                    comparator: Comparator::LE,
                    rhs: QuantityExpr::Fixed { value: m as i32 },
                },
            ],
        },
    }
}

/// CR 601.3 / CR 602.5: Source predicates whose `ParsedCondition` leaf the shared
/// conversion cannot produce.
///
/// The combat/counter/entered-this-turn predicates this function used to own
/// ("~ is attacking", "~ is blocked", "there are N counters on ~", "~ entered this turn",
/// "this card is suspended") are now parsed by `parse_inner_condition` and converted
/// exactly, so they are gone from here. What remains are the leaves the conversion has
/// nowhere to land: `parse_inner_condition` reads "~ is blue" / "~ doesn't have defender"
/// as `StaticCondition::SourceMatchesFilter { filter }`, and `ParsedCondition` has no
/// filter-carrying source variant to receive it.
fn parse_source_condition(text: &str) -> Option<ParsedCondition> {
    // Subjects: "~"/"this <noun>" (self-reference), "enchanted <noun>" (Aura-attached),
    // "from your <zone>" (zone predicate).
    if alt((
        tag::<_, _, OracleError<'_>>("this "),
        tag("enchanted "),
        tag("from your "),
        tag("in "),
        tag("on "),
        tag("~'s "),
        tag("~ "),
    ))
    .parse(text)
    .is_err()
    {
        return None;
    }
    // Zone-based source conditions: "from your graveyard", "[subject] in your graveyard",
    // "in exile", "from your hand", etc. Delegate to the shared zone-phrase scanner so
    // the full zone vocabulary (graveyard/hand/exile/library/battlefield) is covered
    // uniformly with word-boundary safety and the combinator-mandated parse path.
    if let Some((zone, _ctrl, _props)) = super::oracle_target::scan_zone_phrase(text) {
        return Some(ParsedCondition::SourceInZone { zone });
    }
    if let Some(zone) = scan_source_zone_filter(text) {
        return Some(ParsedCondition::SourceInZone { zone });
    }
    // "enchanted [type] is untapped"
    if text.contains("is untapped") {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("enchanted ").parse(text) {
            if let Some(type_text) = rest.strip_suffix(" is untapped") {
                if let Some(core_type) = parse_core_type_word(type_text) {
                    return Some(ParsedCondition::SourceUntappedAttachedTo {
                        required_type: core_type,
                    });
                }
            }
        }
    }
    // "this creature doesn't have [keyword]" / "~ doesn't have [keyword]"
    if let Ok((keyword_text, _)) = alt((
        tag::<_, _, OracleError<'_>>("this creature doesn't have "),
        tag("~ doesn't have "),
    ))
    .parse(text)
    {
        let keyword: Keyword = keyword_text.trim().parse().unwrap();
        if !matches!(keyword, Keyword::Unknown(_)) {
            return Some(ParsedCondition::SourceLacksKeyword { keyword });
        }
    }
    // "this creature is [color]" / "~ is [color]"
    if let Ok((color_text, _)) = alt((
        tag::<_, _, OracleError<'_>>("this creature is "),
        tag("~ is "),
    ))
    .parse(text)
    {
        if let Some(color) = parse_color_word(color_text) {
            return Some(ParsedCondition::SourceIsColor { color });
        }
    }
    // Power threshold: "this creature's power is N or greater" / "~'s power is N or greater"
    if let Some(power) = parse_source_power_threshold(text) {
        return Some(ParsedCondition::SourcePowerAtLeast { minimum: power });
    }
    None
}

fn parse_source_power_threshold(text: &str) -> Option<i32> {
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("this creature's power is "),
        tag("~'s power is "),
    ))
    .parse(text)
    .ok()?;
    let (rest, power) = nom_primitives::parse_number(rest).ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" or greater")
        .parse(rest)
        .ok()?;
    rest.trim().is_empty().then_some(power as i32)
}

/// CR 601.3 / CR 602.5: The one board-state restriction leaf the shared grammar cannot
/// yet produce — "you control N or more lands with the same name" (Endless Atlas,
/// Sceptre of Eternal Glory).
///
/// Everything else this function used to parse is now produced by `parse_inner_condition`
/// and converted through `QuantityComparison`, including four readings this parser got
/// WRONG and which are therefore deliberately not reproduced here:
///
/// - the bare-subtype catch-all dumped any unrecognized qualifier into a stringly-typed
///   `subtype` ("creature that fought this turn", "green permanents that share an
///   artist"), producing a subtype no permanent has and a restriction that could never
///   be satisfied;
/// - "you control fewer creatures than each opponent" built a `QuantityVsEachOpponent`
///   whose lhs and rhs were BOTH "creatures you control", so it compared a value to
///   itself and was constant-false;
/// - "you control a commander" became subtype `"commander"`, likewise unsatisfiable
///   (the shared grammar now reads it as `ControlsCommander`, which converts per CR 903.3d);
/// - "you control no creatures and only during your turn" returned
///   `YouControlNoCreatures` and silently swallowed the timing half of the restriction.
///
/// Those phrases now fail this parser and reach the ordinary `Effect::Unimplemented`
/// fallback, which is the honest answer for text the engine cannot represent.
fn parse_you_control_condition(text: &str) -> Option<ParsedCondition> {
    // Shared target once `parse_inner_condition` learns the phrasing:
    // `QuantityRef::ObjectCountBySharedQuality { quality: Name }`.
    parse_numeric_threshold(text, "you control ", " or more lands with the same name")
        .map(|count| ParsedCondition::YouControlLandsWithSameNameAtLeast { count })
}

fn parse_hand_condition(text: &str) -> Option<ParsedCondition> {
    // Quick reject: must reference "hand" somewhere
    if !text.contains("hand") {
        return None;
    }
    // "you have no cards in hand"
    if tag::<_, _, OracleError<'_>>("you have no cards")
        .parse(text)
        .is_ok()
    {
        return Some(ParsedCondition::HandSizeExact { count: 0 });
    }
    // "you have no [kind] cards in hand" — e.g. "you have no land cards in hand".
    // CR 601.3: Cast restriction — hand contains no cards of the given core type
    // or subtype. Use count: 1 + Not because count-at-least 0 is always true.
    // Verified: CR 601.3 (docs/MagicCompRules.txt:2475).
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you have no ").parse(text) {
        if let Ok((_, kind_raw)) = terminated(
            take_until::<_, _, OracleError<'_>>(" card"),
            alt((tag(" cards in hand"), tag(" card in hand"))),
        )
        .parse(rest)
        {
            let kind = kind_raw.trim();
            if let Some(core_type) = parse_core_type_word(kind) {
                return Some(ParsedCondition::Not {
                    condition: Box::new(ParsedCondition::ZoneCoreTypeCardCountAtLeast {
                        zone: Zone::Hand,
                        core_type,
                        count: 1,
                    }),
                });
            }
            if !kind.is_empty() {
                return Some(ParsedCondition::Not {
                    condition: Box::new(ParsedCondition::ZoneSubtypeCardCountAtLeast {
                        zone: Zone::Hand,
                        subtype: kind.to_string(),
                        count: 1,
                    }),
                });
            }
        }
    }
    if tag::<_, _, OracleError<'_>>("you have one or fewer cards in hand")
        .parse(text)
        .is_ok()
    {
        return Some(ParsedCondition::HandSizeOneOf { counts: vec![0, 1] });
    }
    // "you have more cards in hand than each opponent"
    if tag::<_, _, OracleError<'_>>("you have more cards in hand than")
        .parse(text)
        .is_ok()
    {
        return Some(ParsedCondition::QuantityVsEachOpponent {
            lhs: QuantityRef::HandSize {
                player: PlayerScope::Controller,
            },
            comparator: Comparator::GT,
            rhs: QuantityRef::HandSize {
                player: PlayerScope::Controller,
            },
        });
    }
    // "you have exactly N or M cards in hand"
    if let Some(rest) = tag::<_, _, OracleError<'_>>("you have exactly ")
        .parse(text)
        .ok()
        .and_then(|(rest, _)| rest.strip_suffix(" cards in hand"))
    {
        if rest.contains(" or ") {
            let counts: Vec<usize> = rest
                .split(" or ")
                .filter_map(|s| parse_count_word(s.trim()))
                .collect();
            if counts.len() >= 2 {
                return Some(ParsedCondition::HandSizeOneOf { counts });
            }
        }
        if let Some(count) = parse_count_word(rest) {
            return Some(ParsedCondition::HandSizeExact { count });
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Event condition combinators
// ---------------------------------------------------------------------------

/// CR 601.3 / CR 602.5: Event-history restriction leaves the shared conversion cannot
/// produce today.
///
/// The self-scoped event predicates this function used to own ("you attacked this turn",
/// "you gained life this turn", "a creature died this turn", "you've cast a <type> spell
/// this turn", "an opponent lost life this turn", …) are now parsed by
/// `parse_inner_condition` as `QuantityComparison`s over the event-history `QuantityRef`s
/// and converted exactly, so they are gone from here.
///
/// What remains: opponent-scoped battlefield entries and library searches, and
/// `BeenAttackedThisStep`. Of these only `BeenAttackedThisStep` is a true vocabulary gap
/// (`StaticCondition` has no per-STEP attack history); the others are phrasing gaps whose
/// shared targets are named in `parse_restriction_only_condition`.
fn parse_event_condition(text: &str) -> Option<ParsedCondition> {
    // "an opponent [verb phrase]" — prefix dispatch
    if let Ok((verb_phrase, _)) = tag::<_, _, OracleError<'_>>("an opponent ").parse(text) {
        if let Some(condition) = parse_opponent_had_entered_this_turn(verb_phrase) {
            return Some(condition);
        }
        if let Ok((_, condition)) = parse_opponent_event(verb_phrase) {
            return Some(condition);
        }
    }

    // "you've been attacked this step"
    if let Ok((_, _)) = alt((
        terminated(
            tag::<_, _, OracleError<'_>>("you've been attacked"),
            tag(" this step"),
        ),
        terminated(tag("been attacked"), tag(" this step")),
    ))
    .parse(text)
    {
        return Some(ParsedCondition::BeenAttackedThisStep);
    }

    // Battlefield entry tracking: "[type] enter(ed) the battlefield under your control this turn"
    if let Ok((_, condition)) = parse_etb_this_turn_condition(text) {
        return Some(condition);
    }

    None
}

fn parse_opponent_had_entered_this_turn(verb_phrase: &str) -> Option<ParsedCondition> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("had ")
        .parse(verb_phrase)
        .ok()?;
    parse_had_entered_this_turn(rest, ControllerRef::Opponent)
}

fn parse_had_entered_this_turn(text: &str, controller: ControllerRef) -> Option<ParsedCondition> {
    let suffix = "enter the battlefield under their control this turn";
    let (count, type_and_suffix) =
        if let Some((count, after_count)) = super::oracle_util::parse_number(text) {
            if let Ok((after_or_more, _)) =
                tag::<_, _, OracleError<'_>>("or more ").parse(after_count.trim_start())
            {
                (count, after_or_more)
            } else {
                (1, text)
            }
        } else {
            (1, text)
        };
    let (rest, type_text) = take_until::<_, _, OracleError<'_>>(suffix)
        .parse(type_and_suffix)
        .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(suffix).parse(rest).ok()?;
    if !rest.is_empty() {
        return None;
    }
    let (mut filter, _) = parse_type_phrase(type_text.trim());
    if let TargetFilter::Typed(typed) = &mut filter {
        typed.controller = Some(controller);
        typed.properties.push(FilterProp::InZone {
            zone: Zone::Battlefield,
        });
    }
    Some(ParsedCondition::BattlefieldEntriesThisTurn { filter, count })
}

/// "an opponent [verb phrase]" → typed condition.
///
/// Only the library-search predicate survives: "an opponent lost/gained life this turn"
/// is now produced by `parse_inner_condition` as a `QuantityComparison` and converted.
/// Shared target for this one: `QuantityRef::PlayerActionsThisTurn { action }`.
fn parse_opponent_event(verb_phrase: &str) -> nom::IResult<&str, ParsedCondition, OracleError<'_>> {
    value(
        ParsedCondition::OpponentSearchedLibraryThisTurn,
        alt((
            tag("searched their library this turn"),
            tag("searched a library this turn"),
            tag("has searched their library this turn"),
        )),
    )
    .parse(verb_phrase)
}

/// CR 603.6a: modern enters templating is written "When [this object] enters"
/// (the canonical form elides "the battlefield"), so "[type] entered under your
/// control this turn" is equivalent to the full form "[type] entered the
/// battlefield under your control this turn". Matches the optional
/// " the battlefield" then the mandatory control/this-turn suffix.
fn entered_under_your_control_suffix(text: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    value(
        (),
        (
            opt(tag(" the battlefield")),
            tag(" under your control this turn"),
        ),
    )
    .parse(text)
}

/// "[type] enter(ed) [the battlefield] under your control this turn"
fn parse_etb_this_turn_condition(
    text: &str,
) -> nom::IResult<&str, ParsedCondition, OracleError<'_>> {
    alt((
        value(
            ParsedCondition::YouHadCreatureEnterThisTurn,
            (
                alt((tag("a creature entered"), tag("creature enter"))),
                entered_under_your_control_suffix,
            ),
        ),
        value(
            ParsedCondition::YouHadAngelOrBerserkerEnterThisTurn,
            (
                tag("angel or berserker enter"),
                entered_under_your_control_suffix,
            ),
        ),
        value(
            ParsedCondition::YouHadArtifactEnterThisTurn,
            (
                alt((tag("an artifact entered"), tag("artifact entered"))),
                entered_under_your_control_suffix,
            ),
        ),
    ))
    .parse(text)
}

// ---------------------------------------------------------------------------
// Helpers (moved from restrictions.rs)
// ---------------------------------------------------------------------------

fn parse_numeric_threshold(text: &str, prefix: &str, suffix: &str) -> Option<usize> {
    let middle = text.strip_prefix(prefix)?.strip_suffix(suffix)?.trim();
    parse_count_word(middle)
}

/// Parse a count word using nom combinator for digit/English number matching.
fn parse_count_word(text: &str) -> Option<usize> {
    let trimmed = text.trim();
    if trimmed == "zero" {
        return Some(0);
    }
    // Delegate to nom combinator for number parsing (handles digits and English words).
    let lower = trimmed.to_lowercase();
    nom_primitives::parse_number
        .parse(&lower)
        .ok()
        .and_then(|(rest, n)| rest.is_empty().then_some(n as usize))
}

fn parse_core_type_word(text: &str) -> Option<CoreType> {
    CoreType::from_str(&capitalize_condition_word(
        text.trim().trim_end_matches('s'),
    ))
    .ok()
}

fn parse_color_word(text: &str) -> Option<ManaColor> {
    ManaColor::from_str(&capitalize_condition_word(
        text.trim().trim_end_matches('s'),
    ))
    .ok()
}

/// CR 601.3d + CR 608.2c: Parse `"it targets a <type_phrase>"` (or `"it targets <type_phrase>"`)
/// into a `ParsedCondition::SpellTargetsFilter` whose filter is derived from
/// `parse_type_phrase`. The pronoun `it` refers to the spell being cast — this
/// condition gates target-dependent casting permissions ("you may cast this spell
/// as though it had flash if it targets a commander" — Timely Ward). The trailing
/// remainder returned by `parse_type_phrase` must be empty for the parse to
/// succeed; otherwise we'd silently truncate qualifying clauses that the filter
/// layer hasn't absorbed.
pub(crate) fn parse_spell_targets_filter(text: &str) -> Option<ParsedCondition> {
    let rest = alt((
        tag::<_, _, OracleError<'_>>("it targets a "),
        tag("it targets an "),
        tag("it targets "),
    ))
    .parse(text)
    .ok()?
    .0;
    // CR 903.3: Bare "commander" / "commanders" without a possessive or
    // controller suffix is not lifted by `parse_type_phrase` (which expects
    // type words) or by the possessive arms of `parse_target` (which require
    // "your" / "their" / a trailing controller-suffix). Recognize it here
    // explicitly so "it targets a commander" maps to the `IsCommander`
    // FilterProp without forcing a controller scope. Timely Ward, Skullbriar's
    // sponsors, etc., all reach this arm.
    if let Ok((after, _)) =
        alt((tag::<_, _, OracleError<'_>>("commanders"), tag("commander"))).parse(rest)
    {
        if after.trim().is_empty() {
            return Some(ParsedCondition::SpellTargetsFilter {
                filter: TargetFilter::Typed(TypedFilter {
                    properties: vec![FilterProp::IsCommander],
                    ..Default::default()
                }),
            });
        }
    }
    // CR 115.1: "it targets a permanent or player" — proliferate-style pool
    // (Shiko and Narset, Unified Flurry gate). Matched before `parse_type_phrase`
    // so the "or player" half is not dropped.
    if rest.trim() == "permanent or player" {
        return Some(ParsedCondition::SpellTargetsFilter {
            filter: TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::permanent()),
                    TargetFilter::Player,
                ],
            },
        });
    }
    // CR 115.9b: "one or more" is redundant with .any() semantics (Orvar — "if it
    // targets one or more other permanents you control").
    let (rest, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("one or more "),
        tag("one or more"),
    )))
    .parse(rest)
    .ok()?;
    let (filter, remainder) = parse_type_phrase(rest);
    if !remainder.trim().is_empty() {
        return None;
    }
    // `parse_type_phrase` falls back to `TargetFilter::Any` when no type word
    // matched. A bare "it targets a frob" must not silently widen the gate to
    // "any target"; refuse the parse instead so the casting permission is not
    // emitted (strictly safe — the spell stays sorcery-speed until the
    // predicate is recognized).
    if matches!(filter, TargetFilter::Any | TargetFilter::None) {
        return None;
    }
    Some(ParsedCondition::SpellTargetsFilter { filter })
}

fn capitalize_condition_word(text: &str) -> String {
    let mut out = String::new();
    for (index, piece) in text.split_whitespace().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let mut chars = piece.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(chars);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{CountScope, TypeFilter};
    use crate::types::card_type::Supertype;
    use crate::types::counter::CounterType;

    /// Helper: assert the phrase reaches the SHARED grammar and converts.
    fn shared(text: &str) -> ParsedCondition {
        match parse_shared_restriction_condition(&text.to_lowercase()) {
            SharedRestrictionParse::Converted(c) => c,
            other => panic!(
                "{text:?}: expected the shared grammar to own this phrase, got {}",
                match other {
                    SharedRestrictionParse::NoMatch => "NoMatch (fell to restriction-only grammar)",
                    SharedRestrictionParse::Unsupported => "Unsupported",
                    SharedRestrictionParse::Converted(_) => unreachable!(),
                }
            ),
        }
    }

    /// Helper: assert the phrase is NOT recognized by the shared grammar, i.e. it is
    /// legitimately served by the restriction-only fallback.
    fn falls_back(text: &str) {
        assert!(
            matches!(
                parse_shared_restriction_condition(&text.to_lowercase()),
                SharedRestrictionParse::NoMatch
            ),
            "{text:?}: expected NoMatch from the shared grammar (restriction-only fallback)"
        );
    }

    // -----------------------------------------------------------------------
    // The shared grammar is the PRIMARY authority
    // -----------------------------------------------------------------------

    /// CR 601.3 / CR 602.5: A restriction condition is an ordinary game-state condition,
    /// so `parse_inner_condition` must own it. Each phrase below used to be claimed by a
    /// bespoke restriction leaf; it now flows through the shared grammar and converts to
    /// the generic `QuantityComparison` vocabulary.
    ///
    /// Fail-on-revert: restoring the legacy-first ordering makes every one of these
    /// return its old special-case variant (`YouControlSubtypeCountAtLeast`,
    /// `ControlsCreatureWithKeyword`, `HandSizeExact`, `YouAttackedThisTurn`, …).
    #[test]
    fn shared_grammar_owns_general_restriction_conditions() {
        for text in [
            "you control two or more vampires",
            "you control a legendary creature",
            "you control a creature with flying",
            "an opponent controls a creature with flying",
            "you control a snow land",
            "you control three or more creatures with different powers",
            "you have exactly seven cards in hand",
            "you attacked this turn",
            "a creature died this turn",
            "there are seven or more cards in your graveyard",
            "you've played a land this turn",
            "you have the city's blessing",
            "~ is attacking",
            "~ is blocked",
            "~ entered this turn",
            "this card is in your graveyard",
        ] {
            let parsed = shared(text);
            assert!(
                parse_restriction_condition(text).is_some(),
                "{text:?} must still produce a restriction condition"
            );
            // The shared readings are the generic vocabulary, not the old bespoke leaves.
            assert!(
                !matches!(
                    parsed,
                    ParsedCondition::YouControlSubtypeCountAtLeast { .. }
                        | ParsedCondition::ControlsCreatureWithKeyword { .. }
                        | ParsedCondition::YouControlCoreTypeCountAtLeast { .. }
                        | ParsedCondition::HandSizeExact { .. }
                ),
                "{text:?} still produced a legacy special-case leaf: {parsed:?}"
            );
        }
    }

    /// CR 601.3: "you control a creature with power 4 or greater" is a presence check —
    /// `IsPresent` bridges to `ObjectCount >= 1` over the same filter, so the P/T
    /// qualifier rides along inside the filter instead of needing its own variant.
    #[test]
    fn presence_conditions_bridge_to_object_count() {
        match shared("a creature is attacking you") {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => assert!(
                matches!(&filter, TargetFilter::Typed(tf) if tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Attacking { defender: Some(ControllerRef::You) }
                ))),
                "filter should be a creature attacking you, got {filter:?}"
            ),
            other => panic!("expected QuantityComparison(ObjectCount >= 1), got {other:?}"),
        }
    }

    /// CR 205.4a: a supertype adjective decomposes into `HasSupertype` + the core type,
    /// never a stringly-typed subtype (which no permanent has, leaving the restriction
    /// permanently unsatisfiable).
    #[test]
    fn supertype_permanent_decomposes_to_filter() {
        match shared("you control a snow land") {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ObjectCount {
                                filter: TargetFilter::Typed(tf),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert!(tf.properties.iter().any(
                    |p| matches!(p, FilterProp::HasSupertype { value } if *value == Supertype::Snow)
                ));
            }
            other => panic!("expected ObjectCount(snow land) >= 1, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Compound conditions are parsed by the shared grammar, not by string splitting
    // -----------------------------------------------------------------------

    /// CR 608.2c: conjunction and disjunction are one parameterized combinator in
    /// `parse_inner_condition`. The legacy implementation split the raw string on
    /// " and " / " or ", which cannot see that a connector sits INSIDE an atomic leaf.
    ///
    /// Fail-on-revert: deleting `parse_condition_connective` makes every phrase here
    /// fail the all-consuming shared parse and return `None`.
    #[test]
    fn compound_restrictions_parse_through_shared_grammar() {
        // Conjunction (the dual-land cast restrictions: Ancient Ziggurat cycle).
        match shared("an opponent controls a forest and you control a swamp") {
            ParsedCondition::And { conditions } => assert_eq!(conditions.len(), 2),
            other => panic!("expected And, got {other:?}"),
        }
        // Disjunction.
        match shared("~ is on the battlefield or in your graveyard") {
            ParsedCondition::Or { conditions } => assert_eq!(conditions.len(), 2),
            other => panic!("expected Or, got {other:?}"),
        }
        // A redundant "if" re-marker on the second half is grammatical scaffolding.
        match shared("~ entered this turn or if you control a basic land") {
            ParsedCondition::Or { conditions } => {
                assert_eq!(conditions.len(), 2);
                assert!(matches!(
                    conditions[0],
                    ParsedCondition::SourceEnteredThisTurn
                ));
            }
            other => panic!("expected Or with a re-marked second half, got {other:?}"),
        }
    }

    /// CR 608.2c: an n-ary chain nests right-associatively rather than leaving " and C"
    /// as an unconsumed — and therefore silently swallowed — tail.
    #[test]
    fn n_ary_conjunction_nests_instead_of_swallowing_the_tail() {
        match shared("you attacked this turn and you gained life this turn and you control a swamp")
        {
            ParsedCondition::And { conditions } => {
                assert_eq!(conditions.len(), 2, "outer And is binary");
                assert!(
                    matches!(conditions[1], ParsedCondition::And { .. }),
                    "third conjunct must nest, not vanish: {:?}",
                    conditions[1]
                );
            }
            other => panic!("expected nested And, got {other:?}"),
        }
    }

    /// The connector-inside-a-leaf trap the old string split fell into. "more cards in
    /// hand than each opponent" contains no connector, but "an artifact or enchantment"
    /// style leaves do — requiring BOTH sides to parse as complete conditions is what
    /// makes the decomposition safe.
    ///
    /// This phrase also fixes a real defect: the legacy `QuantityVsEachOpponent` reading
    /// put "cards in YOUR hand" on BOTH sides of the comparison.
    #[test]
    fn connector_inside_an_atomic_leaf_is_not_split() {
        match shared("you have more cards in hand than each opponent") {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::HandSize {
                                player: PlayerScope::Controller,
                            },
                    },
                comparator: Comparator::GT,
                rhs: QuantityExpr::Ref { qty: rhs },
            } => assert!(
                !matches!(
                    rhs,
                    QuantityRef::HandSize {
                        player: PlayerScope::Controller
                    }
                ),
                "rhs must be the OPPONENTS' hand size, not the controller's own \
                 (the legacy reading compared a value to itself): {rhs:?}"
            ),
            other => panic!("expected a single HandSize comparison, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Conversion is exhaustive: recognized-but-nonrepresentable must FAIL, not fall back
    // -----------------------------------------------------------------------

    /// A `StaticCondition` the restriction evaluator cannot represent exactly must yield
    /// `Unsupported` — and `parse_restriction_condition` must return `None` rather than
    /// let the restriction-only grammar produce a weaker reading of the same words.
    ///
    /// "~ is a creature" is the witness: the shared grammar reads it as the
    /// filter-carrying `SourceMatchesFilter`, which `ParsedCondition` has no variant to
    /// hold.
    ///
    /// "you control your commander" is the second, and it is the sharper one. The sibling
    /// phrase "you control **a** commander" (CR 903.3d — any commander you control,
    /// regardless of owner) DOES convert, to an `ObjectCount` over the `IsCommander`
    /// filter. The possessive form additionally requires you to OWN the permanent, and
    /// `TargetFilter` has no owner axis — so converting it with the same filter would
    /// silently let a STOLEN commander satisfy a condition the card restricts to your own.
    /// Reject beats approximate.
    ///
    /// Fail-on-revert: routing `Unsupported` back into `parse_restriction_only_condition`,
    /// or widening the `Own` arm to reuse the `Any` filter, makes these `Some(..)` again.
    #[test]
    fn recognized_but_nonrepresentable_condition_fails_the_parse() {
        // Assert WHICH `StaticCondition` is rejected by running the conversion directly.
        // That is a sharper claim than "the tri-state said Unsupported", and it lets
        // `Unsupported` stay payload-free — a field only tests read is dead weight in
        // every production build.
        fn shared_static(text: &str) -> StaticCondition {
            all_consuming(nom_condition::parse_inner_condition)
                .parse(text)
                .unwrap_or_else(|_| panic!("{text:?}: the shared grammar must RECOGNIZE this"))
                .1
        }

        // "~ is a creature" is read by the shared grammar as the filter-carrying
        // SourceMatchesFilter, which `ParsedCondition` has no variant to hold.
        let creature = shared_static("~ is a creature");
        assert!(matches!(
            creature,
            StaticCondition::SourceMatchesFilter { .. }
        ));
        assert_eq!(
            static_condition_to_restriction_condition(creature),
            None,
            "a filter-carrying source predicate has no exact restriction representation"
        );
        assert!(matches!(
            parse_shared_restriction_condition("~ is a creature"),
            SharedRestrictionParse::Unsupported
        ));
        assert_eq!(parse_restriction_condition("~ is a creature"), None);

        // The possessive commander form requires OWNERSHIP, which `TargetFilter` cannot
        // express; its sibling "you control A commander" DOES convert (test below).
        let own = shared_static("you control your commander");
        assert!(matches!(
            own,
            StaticCondition::ControlsCommander {
                ownership: CommanderOwnership::Own
            }
        ));
        assert_eq!(static_condition_to_restriction_condition(own), None);
        assert!(matches!(
            parse_shared_restriction_condition("you control your commander"),
            SharedRestrictionParse::Unsupported
        ));
        assert_eq!(
            parse_restriction_condition("you control your commander"),
            None
        );
    }

    /// CR 903.3d: "you control a commander" refers to a permanent on the battlefield that
    /// is a commander — regardless of owner. It converts to an `ObjectCount` over the
    /// `IsCommander` filter scoped to your control.
    ///
    /// The legacy restriction grammar read this as subtype `"commander"` — a subtype no
    /// permanent has — so Deflecting Swat's free-cast condition could NEVER be satisfied.
    #[test]
    fn controls_a_commander_converts_to_object_count() {
        match shared("you control a commander") {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ObjectCount {
                                filter: TargetFilter::Typed(tf),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::IsCommander));
            }
            other => panic!("expected ObjectCount(IsCommander) >= 1, got {other:?}"),
        }
    }

    /// CR 122.1 + CR 711.2a: a counter BAND must never be widened into an "at least"
    /// restriction. `HasCounters { minimum: 1, maximum: Some(3) }` ("one to three level
    /// counters") is FALSE at four; a bare `GE 1` is true at four, which the card forbids.
    ///
    /// This unit originally satisfied that constraint by REJECTING every band. #5677 (the
    /// L02 condition lane) satisfied it better, by PRESERVING the maximum: a band lowers to
    /// `And[GE n, LE m]` over a `CountersOn { Source }` quantity. Rejecting was safe;
    /// preserving is correct, and it converts cards rejection could not. This test now
    /// pins the stronger property — the maximum SURVIVES.
    ///
    /// Fail-on-revert: lowering a band to a bare `GE` (dropping the `LE` conjunct) makes
    /// the first assertion fail.
    #[test]
    fn bounded_counter_band_preserves_its_maximum() {
        let level = || CounterMatch::OfType(CounterType::Generic("level".to_string()));
        let band = static_condition_to_restriction_condition(StaticCondition::HasCounters {
            counters: level(),
            minimum: 1,
            maximum: Some(3),
        })
        .expect("a bounded band is representable as And[GE, LE]");
        match band {
            ParsedCondition::And { ref conditions } => {
                assert_eq!(conditions.len(), 2, "band must keep BOTH bounds: {band:?}");
                assert!(
                    conditions.iter().any(|c| matches!(
                        c,
                        ParsedCondition::QuantityComparison {
                            comparator: Comparator::LE,
                            rhs: QuantityExpr::Fixed { value: 3 },
                            ..
                        }
                    )),
                    "the MAXIMUM must survive — without the LE conjunct the restriction is \
                     true at four counters, where the card says false: {band:?}"
                );
                assert!(conditions.iter().any(|c| matches!(
                    c,
                    ParsedCondition::QuantityComparison {
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 1 },
                        ..
                    }
                )));
            }
            other => panic!("expected And[GE, LE] for a bounded band, got {other:?}"),
        }

        // Unbounded "N or more" is a bare GE.
        assert!(matches!(
            static_condition_to_restriction_condition(StaticCondition::HasCounters {
                counters: level(),
                minimum: 2,
                maximum: None,
            }),
            Some(ParsedCondition::QuantityComparison {
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
                ..
            })
        ));
        // "no counters" is an exact zero.
        assert!(matches!(
            static_condition_to_restriction_condition(StaticCondition::HasCounters {
                counters: level(),
                minimum: 0,
                maximum: Some(0),
            }),
            Some(ParsedCondition::QuantityComparison {
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
                ..
            })
        ));
        // `CounterMatch::Any` ("a counter on it", summed across every type) IS expressible
        // through `CountersOn { counter_type: None }` — the fixed `SourceHasCounterAtLeast`
        // leaf could not hold it, and this unit's first version therefore rejected it.
        assert!(matches!(
            static_condition_to_restriction_condition(StaticCondition::HasCounters {
                counters: CounterMatch::Any,
                minimum: 1,
                maximum: None,
            }),
            Some(ParsedCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        counter_type: None,
                        ..
                    }
                },
                comparator: Comparator::GE,
                ..
            })
        ));
    }

    /// The explicit rejects named by the design: these are not conditions a cast/activation
    /// gate can evaluate, and must never acquire a permissive reading.
    #[test]
    fn non_restriction_static_conditions_are_rejected() {
        for condition in [
            StaticCondition::Unrecognized {
                text: "whatever".to_string(),
            },
            StaticCondition::None,
            StaticCondition::RecipientHasCounters {
                counters: CounterMatch::Any,
                minimum: 1,
                maximum: None,
            },
        ] {
            assert_eq!(
                static_condition_to_restriction_condition(condition.clone()),
                None,
                "{condition:?} must not convert to a restriction"
            );
        }
    }

    /// CR 122.1: counter thresholds on the source convert through the shared grammar to a
    /// `QuantityComparison` over `CountersOn { Source }` — the representation #5677 shares
    /// with the `AbilityCondition` peer, so the restriction and effect paths agree on one
    /// lowering instead of each keeping a private counter leaf.
    #[test]
    fn source_counter_thresholds_convert() {
        assert!(matches!(
            parse_restriction_condition("there are three or more brick counters on ~"),
            Some(ParsedCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn { .. }
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            })
        ));
        assert!(matches!(
            parse_restriction_condition("there are no charge counters on ~"),
            Some(ParsedCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn { .. }
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            })
        ));
    }

    // -----------------------------------------------------------------------
    // Misparses the old restriction grammar produced are now honest gaps
    // -----------------------------------------------------------------------

    /// Each phrase below used to produce a CONFIDENTLY WRONG restriction. They now return
    /// `None`, so the source clause survives as `Effect::Unimplemented` instead of
    /// shipping as a supported card whose restriction can never be satisfied (or, worse,
    /// silently drops half its text).
    ///
    /// Fail-on-revert: restoring the bare-subtype catch-all / `QuantityVsEachOpponent`
    /// arms in `parse_you_control_condition` makes these `Some(..)` again.
    #[test]
    fn legacy_misparses_are_now_honest_gaps() {
        for text in [
            // Dumped the whole qualifier into a stringly-typed subtype no permanent has.
            "you control a creature that fought this turn",
            "you control two or more green permanents that share an artist",
            "you control an urza's mine, an urza's power-plant, and an urza's tower",
            // Compared "creatures you control" against ITSELF.
            "you control fewer creatures than each opponent",
            // Returned YouControlNoCreatures and swallowed the timing half.
            "you control no creatures and only during your turn",
        ] {
            assert_eq!(
                parse_restriction_condition(text),
                None,
                "{text:?} must be an honest gap, not a confidently wrong restriction"
            );
        }
    }

    #[test]
    fn unrecognized_returns_none() {
        assert_eq!(
            parse_restriction_condition("something completely unknown"),
            None
        );
    }

    // -----------------------------------------------------------------------
    // The retained restriction-only grammar
    // -----------------------------------------------------------------------

    /// CR 601.3d: "it targets …" — the referent is the in-flight spell, which no static
    /// ability has. This is the one parser the shared grammar structurally cannot absorb.
    #[test]
    fn it_targets_retains_pending_spell_identity() {
        falls_back("it targets a commander");

        match parse_restriction_condition("it targets a commander") {
            Some(ParsedCondition::SpellTargetsFilter {
                filter: TargetFilter::Typed(filter),
            }) => {
                assert!(filter.properties.contains(&FilterProp::IsCommander));
                assert!(filter.controller.is_none());
            }
            other => panic!("expected SpellTargetsFilter(IsCommander), got {other:?}"),
        }
        match parse_restriction_condition("it targets one or more other permanents you control") {
            Some(ParsedCondition::SpellTargetsFilter {
                filter: TargetFilter::Typed(tf),
            }) => {
                assert!(tf.type_filters.contains(&TypeFilter::Permanent));
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::Another));
            }
            other => panic!("expected SpellTargetsFilter(permanent), got {other:?}"),
        }
        match parse_restriction_condition("it targets a permanent or player") {
            Some(ParsedCondition::SpellTargetsFilter {
                filter: TargetFilter::Or { filters },
            }) => assert!(filters.contains(&TargetFilter::Player)),
            other => panic!("expected SpellTargetsFilter(Or), got {other:?}"),
        }
        // Hostile: a predicate that does not lift to a typed filter must NOT widen the
        // gate to "any target" — fail loud so the casting permission is simply not emitted.
        assert_eq!(
            parse_restriction_condition("it targets a frob the wobble"),
            None
        );
    }

    /// Source predicates `ParsedCondition` models as fixed leaves. The shared grammar
    /// reads these as `SourceMatchesFilter`, which the conversion cannot receive, so the
    /// restriction-only parser remains the authority.
    #[test]
    fn retained_source_predicates() {
        assert_eq!(
            parse_restriction_condition("~ is blue"),
            Some(ParsedCondition::SourceIsColor {
                color: ManaColor::Blue
            })
        );
        assert_eq!(
            parse_restriction_condition("~ doesn't have defender"),
            Some(ParsedCondition::SourceLacksKeyword {
                keyword: Keyword::Defender
            })
        );
        assert_eq!(
            parse_restriction_condition("~'s power is 4 or greater"),
            Some(ParsedCondition::SourcePowerAtLeast { minimum: 4 })
        );
        assert_eq!(
            parse_restriction_condition("~ is on the stack"),
            Some(ParsedCondition::SourceInZone { zone: Zone::Stack })
        );
        assert_eq!(
            parse_restriction_condition("enchanted land is untapped"),
            Some(ParsedCondition::SourceUntappedAttachedTo {
                required_type: CoreType::Land
            })
        );
        // Hostile: an unknown source predicate stays unsupported.
        assert_eq!(parse_restriction_condition("~ is quixotic"), None);
    }

    /// CR 508.1a: the attacked-with family — numeric threshold and typed attacker filter.
    #[test]
    fn retained_attacked_with_family() {
        falls_back("you attacked with three or more creatures this turn");
        assert_eq!(
            parse_restriction_condition("you attacked with three or more creatures this turn"),
            Some(ParsedCondition::YouAttackedWithAtLeast {
                count: 3,
                filter: None,
            })
        );
        // Typed attacker (Thaumaton Torpedo). The trailing "this turn" may already be
        // stripped upstream, so both shapes must parse.
        for text in [
            "you attacked with a spacecraft this turn",
            "you attacked with a spacecraft",
        ] {
            match parse_restriction_condition(text) {
                Some(ParsedCondition::YouAttackedWithAtLeast {
                    count: 1,
                    filter: Some(TargetFilter::Typed(tf)),
                }) => assert!(tf
                    .type_filters
                    .iter()
                    .any(|f| matches!(f, TypeFilter::Subtype(s) if s == "Spacecraft"))),
                other => panic!("expected filtered attacked-with for {text:?}, got {other:?}"),
            }
        }
        // Hostile: an unrecognized attacker qualifier stays an honest gap.
        assert_eq!(
            parse_restriction_condition("you attacked with a frob this turn"),
            None
        );
    }

    /// The remaining retained leaves, each with no exact shared counterpart today.
    #[test]
    fn retained_board_hand_and_event_leaves() {
        assert_eq!(
            parse_restriction_condition("you control three or more lands with the same name"),
            Some(ParsedCondition::YouControlLandsWithSameNameAtLeast { count: 3 })
        );
        assert_eq!(
            parse_restriction_condition("you have exactly zero or seven cards in hand"),
            Some(ParsedCondition::HandSizeOneOf { counts: vec![0, 7] })
        );
        assert_eq!(
            parse_restriction_condition("you've been attacked this step"),
            Some(ParsedCondition::BeenAttackedThisStep)
        );
        assert_eq!(
            parse_restriction_condition("an opponent searched their library this turn"),
            Some(ParsedCondition::OpponentSearchedLibraryThisTurn)
        );
        assert!(matches!(
            parse_restriction_condition(
                "an opponent had two or more creatures enter the battlefield under their control this turn"
            ),
            Some(ParsedCondition::BattlefieldEntriesThisTurn { count: 2, .. })
        ));
    }

    /// Existential opponent comparisons (Weathered Wayfarer, Isolated Watchtower) flow
    /// through the shared grammar's player-count vocabulary.
    #[test]
    fn opponent_controls_more_than_you_conditions() {
        use crate::types::ability::{PlayerFilter, PlayerRelation};
        match shared("an opponent controls more lands than you") {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::PlayerCount {
                                filter:
                                    PlayerFilter::ControlsCount {
                                        relation: PlayerRelation::Opponent,
                                        comparator: Comparator::GT,
                                        ..
                                    },
                            },
                    },
                ..
            } => {}
            other => panic!("expected existential opponent ControlsCount GT, got {other:?}"),
        }
    }

    /// Spell-history conditions keep their filters through the shared grammar.
    #[test]
    fn spell_history_conditions_keep_their_filter() {
        match shared("you've cast three or more instant and/or sorcery spells this turn") {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::SpellsCastThisTurn {
                                scope: CountScope::Controller,
                                filter: Some(TargetFilter::Or { .. }),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } => {}
            other => panic!("expected filtered SpellsCastThisTurn >= 3, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod retained_family_gate {
    /// STRUCTURAL GATE (not prose): the restriction-only fallback may dispatch to exactly
    /// these six parser families, and no others.
    ///
    /// `parse_restriction_only_condition`'s doc comment tells contributors not to add a
    /// new phrase there. A comment stops nobody. This test does: it reads this module's
    /// own source, extracts the fallback's body, and pins the dispatch set. Adding a
    /// seventh arm turns it red and forces the author to answer the only question that
    /// matters — is the new phrase (a) restriction-context-referential, (b) a leaf the
    /// shared vocabulary genuinely cannot express, or (c) merely a phrasing the shared
    /// grammar does not spell yet? Only (a) and (b) belong here. (c) belongs in
    /// `parse_inner_condition` — see task P02-U3b, which is porting the five families
    /// below that are already known to be (c).
    ///
    /// If you are here because this test went red: do not just append your parser to the
    /// list. Justify it, or teach the shared grammar the phrasing instead.
    const PINNED_RETAINED_FAMILIES: [&str; 6] = [
        // (a) restriction-context referent — the in-flight spell (CR 601.3d). PERMANENT.
        "parse_spell_targets_filter",
        // (b) vocabulary gap — ParsedCondition has no filter-carrying source predicate,
        //     so StaticCondition::SourceMatchesFilter cannot be converted. Root fix is a
        //     vocabulary alignment, not a parser edit.
        "parse_source_condition",
        // (c) PHRASING GAPS — port pending (P02-U3b). Each has an existing shared target.
        "parse_you_control_condition",
        "parse_hand_condition",
        "parse_event_condition",
        "parse_you_attacked_with",
    ];

    #[test]
    fn restriction_only_fallback_dispatches_exactly_the_pinned_families() {
        let source = include_str!("oracle_condition.rs");
        // allow-noncombinator: scans RUST SOURCE, not Oracle text. nom parses card text;
        // this gate parses this module's own bytes to pin its dispatch set.
        let start = source
            .find("fn parse_restriction_only_condition") // allow-noncombinator: scans RUST SOURCE, not Oracle text
            .expect("fallback dispatcher must exist");
        // The body ends at the first column-0 closing brace after the signature.
        // allow-noncombinator: Rust source scan (see above).
        let body_len = source[start..]
            .find("\n}") // allow-noncombinator: Rust source scan
            .expect("fallback dispatcher must be closed");
        let body = &source[start..start + body_len];

        let dispatched: Vec<&str> = PINNED_RETAINED_FAMILIES
            .iter()
            .copied()
            .filter(|family| body.contains(&format!("{family}(text)")))
            .collect();
        assert_eq!(
            dispatched.len(),
            PINNED_RETAINED_FAMILIES.len(),
            "a pinned family is no longer dispatched — if you REMOVED one (e.g. finished \
             its port into parse_inner_condition), delete it from PINNED_RETAINED_FAMILIES \
             too. Dispatched: {dispatched:?}"
        );

        // Now the direction that actually guards the boundary: count every `parse_*(text)`
        // call in the body and require that none is unpinned.
        let mut calls = 0usize;
        let mut rest = body;
        // allow-noncombinator: Rust source scan (see above).
        while let Some(i) = rest.find("parse_") {
            rest = &rest[i..];
            // allow-noncombinator: Rust source scan (see above).
            let end = rest
                .find("(text)") // allow-noncombinator: Rust source scan
                .filter(|end| !rest[..*end].contains(char::is_whitespace));
            if let Some(end) = end {
                let name = &rest[..end];
                assert!(
                    PINNED_RETAINED_FAMILIES.contains(&name),
                    "UNPINNED restriction-only parser `{name}` was added to the fallback.\n\
                     The restriction-only grammar is closed. A new restriction phrase almost \
                     always belongs in `parse_inner_condition` (the shared static-condition \
                     grammar), because that is where every static ability with the same words \
                     already looks. Only two things may live here: a referent the shared \
                     grammar structurally cannot bind (the in-flight spell of CR 601.3d), or \
                     a leaf `StaticCondition` genuinely has no vocabulary for. If yours is \
                     neither, teach the shared grammar the phrasing."
                );
                calls += 1;
            }
            rest = &rest["parse_".len()..];
        }
        assert_eq!(
            calls,
            PINNED_RETAINED_FAMILIES.len(),
            "the fallback dispatch count changed; pin it deliberately"
        );
    }
}
