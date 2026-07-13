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
    CommanderOwnership, Comparator, ControllerRef, FilterProp, ParsedCondition, QuantityExpr,
    QuantityRef, StaticCondition, TargetFilter, TypedFilter,
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

    // P02-U3b CLOSED the phrasing-gap class that used to live here. The five families
    // named in that task (`you attacked with <N|filter>`, `an opponent had <N> <type>
    // enter`, `lands with the same name`, `an opponent searched their library`, and the
    // `exactly N or M cards in hand` disjunction) are now SPELLED by
    // `parse_inner_condition` and reach the restriction evaluator through the ordinary
    // `QuantityComparison` conversion. Their parsers are deleted, not moved.
    //
    // Two residuals remain below, and NEITHER is a phrasing gap. Both are JUSTIFICATION 2
    // (no shared vocabulary) — the same class as `parse_source_condition` above:
    //
    //  1. `BeenAttackedThisStep` — `StaticCondition` has no per-STEP attack history at
    //     all. Its whole vocabulary is per-TURN. There is nothing to spell.
    //  2. "you have no <kind> cards in hand" — an OWNER-relative count in a hidden zone.
    //     The shared reading would be an `ObjectCount` over a `TargetFilter` carrying
    //     `InZone { Hand }`, but `TargetFilter` has NO owner axis (see the
    //     `CommanderOwnership::Own` rejection in
    //     `static_condition_to_restriction_condition`), and `object_count_matching_ids`
    //     discriminates by CONTROLLER via `matches_target_filter` — not by owner. In an
    //     owner zone those differ precisely when a control-change effect has touched the
    //     card, which is why `matches_target_filter_in_owner_zone` exists as a separate
    //     entry point that the count path does not use. `ZoneCoreTypeCardCountAtLeast`
    //     reads `player_zone_ids(player, Hand)` and is exactly right; an `ObjectCount`
    //     would be a silently different predicate. Aligning this needs an owner axis on
    //     `TargetFilter` (or an owner-zone-aware count ref) — a vocabulary change, not a
    //     parser edit.
    //
    // The rule for anyone extending this file: a NEW restriction phrase never lands here.
    // Teach `parse_inner_condition` the phrasing — that is where every static ability
    // with the same words already looks.

    // "you have no land cards in hand" (Land Grant). See residual (2) above.
    if let Some(condition) = parse_hand_condition(text) {
        return Some(condition);
    }

    // "you've been attacked this step" (Assassin's Blade, Harsh Justice, …), plus the
    // "[type] entered under your control this turn" surface that ELIDES "the battlefield"
    // (Gargoyle Flock's "an artifact entered under your control this turn"). The shared
    // entered-grammar's suffix spells "the battlefield" literally, so it does not match
    // the elided form. See residual (1) above.
    if let Some(condition) = parse_event_condition(text) {
        return Some(condition);
    }

    None
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
/// CR 402.1 + CR 601.3: "you have no <kind> cards in hand" (Land Grant) — the ONE hand
/// predicate the shared grammar cannot own.
///
/// Every other hand surface this parser used to hold ("no cards in hand", "exactly N",
/// "exactly N or M", "one or fewer", "more cards in hand than each opponent") is now
/// spelled by `parse_hand_size_predicate` in the shared grammar and converts through
/// `QuantityComparison` / `Or`. They were deleted in P02-U3b, not moved.
///
/// This leaf survives on JUSTIFICATION 2 (no shared vocabulary), NOT as a phrasing gap:
/// it is an OWNER-relative count in a HIDDEN zone. `ZoneCoreTypeCardCountAtLeast` reads
/// `player_zone_ids(player, Hand)` — the cards *this player owns* in hand. The shared
/// reading would be an `ObjectCount` over a `TargetFilter` carrying `InZone { Hand }`,
/// but `TargetFilter` has no owner axis, and the count path (`object_count_matching_ids`)
/// discriminates by CONTROLLER through `matches_target_filter`. Those two predicates come
/// apart exactly when a control-change effect has touched a card in an owner zone — the
/// reason `matches_target_filter_in_owner_zone` exists as a separate entry point that the
/// count path does not call. Converting this leaf would therefore not be a re-spelling; it
/// would be a silently different question. Closing it needs an owner axis on
/// `TargetFilter` (or an owner-zone-aware count ref).
fn parse_hand_condition(text: &str) -> Option<ParsedCondition> {
    // Quick reject: must reference "hand" somewhere.
    if !text.contains("hand") {
        return None;
    }
    // "you have no [kind] cards in hand" — e.g. "you have no land cards in hand".
    // `Not(count >= 1)` rather than `count == 0` because a count-at-least-0 gate is
    // always true. CR 601.3: cast restriction on hand contents.
    let (rest, _) = tag::<_, _, OracleError<'_>>("you have no ")
        .parse(text)
        .ok()?;
    let (_, kind_raw) = terminated(
        take_until::<_, _, OracleError<'_>>(" card"),
        alt((tag(" cards in hand"), tag(" card in hand"))),
    )
    .parse(rest)
    .ok()?;
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
    if kind.is_empty() {
        return None;
    }
    Some(ParsedCondition::Not {
        condition: Box::new(ParsedCondition::ZoneSubtypeCardCountAtLeast {
            zone: Zone::Hand,
            subtype: kind.to_string(),
            count: 1,
        }),
    })
}

// ---------------------------------------------------------------------------
// Event condition combinators
// ---------------------------------------------------------------------------

/// CR 601.3 / CR 602.5: Event-history restriction leaves the shared conversion cannot
/// produce.
///
/// P02-U3b removed the two OPPONENT-scoped leaves this function used to own — "an
/// opponent had N <type> enter the battlefield under their control this turn" and "an
/// opponent searched their library this turn". Both are now spelled by
/// `parse_inner_condition`, scoped with `PlayerScope::Opponent { aggregate: Max }`, and
/// converted through `QuantityComparison`. (That port also FIXED them: the filter-carried
/// `controller: Opponent` they used to build made the runtime SUM entries across all
/// opponents, so in multiplayer two different opponents with one creature each satisfied
/// "an opponent had TWO OR MORE creatures enter". `Opponent { Max }` counts per opponent.)
///
/// What remains has no shared counterpart:
/// - `BeenAttackedThisStep` — `StaticCondition`'s attack history is per-TURN; there is no
///   per-STEP vocabulary to spell at all.
/// - the "[type] entered under your control this turn" surface that ELIDES "the
///   battlefield" (Gargoyle Flock). The shared entered-grammar spells "entered the
///   battlefield under your control this turn" literally, so the elided form never
///   reaches it.
fn parse_event_condition(text: &str) -> Option<ParsedCondition> {
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
    use crate::types::ability::{
        AggregateFunction, CountScope, PlayerScope, SharedQuality, TypeFilter,
    };
    use crate::types::card_type::Supertype;
    use crate::types::counter::CounterType;
    use crate::types::events::PlayerActionKind;

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
    ///
    /// P02-U3b INVERTED this test's architecture assertion. It used to `falls_back(…)` —
    /// pinning the family to the restriction-only fallback — and to expect the fixed
    /// `YouAttackedWithAtLeast` leaf. The shared grammar now SPELLS both surfaces, so the
    /// phrase must be OWNED by `parse_inner_condition` and arrive as the generic
    /// `QuantityComparison` over the filter-carrying `AttackedThisTurn` ref. The old
    /// assertions are kept only as this comment: reverting the port makes `shared(…)`
    /// panic with "NoMatch (fell to restriction-only grammar)".
    #[test]
    fn retained_attacked_with_family() {
        match shared("you attacked with three or more creatures this turn") {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::AttackedThisTurn {
                                scope: CountScope::Controller,
                                filter: None,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } => {}
            other => panic!("expected unfiltered AttackedThisTurn >= 3, got {other:?}"),
        }
        // Typed attacker (Thaumaton Torpedo). The trailing "this turn" may already be
        // stripped upstream, so both shapes must parse.
        for text in [
            "you attacked with a spacecraft this turn",
            "you attacked with a spacecraft",
        ] {
            // P02-U3b: the shared grammar now owns this phrase, so it arrives as a
            // QuantityComparison over the filter-carrying AttackedThisTurn ref rather
            // than the retired fixed `YouAttackedWithAtLeast` leaf.
            match parse_restriction_condition(text) {
                Some(ParsedCondition::QuantityComparison {
                    lhs:
                        QuantityExpr::Ref {
                            qty:
                                QuantityRef::AttackedThisTurn {
                                    scope: CountScope::Controller,
                                    filter: Some(TargetFilter::Typed(tf)),
                                },
                        },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
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

    /// The leaves that genuinely have NO shared counterpart, and so stay in the
    /// restriction-only fallback after P02-U3b.
    ///
    /// The four phrases this test used to also cover (lands-with-the-same-name, the
    /// exact-hand-size disjunction, opponent library search, opponent battlefield
    /// entries) moved to the shared grammar and are asserted there — see the
    /// `shared_grammar_owns_*` tests.
    #[test]
    fn retained_leaves_with_no_shared_counterpart() {
        // No per-STEP attack history exists in StaticCondition at all.
        assert_eq!(
            parse_restriction_condition("you've been attacked this step"),
            Some(ParsedCondition::BeenAttackedThisStep)
        );
        // OWNER-relative count in a hidden zone: TargetFilter has no owner axis, so an
        // ObjectCount would silently ask a controller-scoped question instead.
        assert_eq!(
            parse_restriction_condition("you have no land cards in hand"),
            Some(ParsedCondition::Not {
                condition: Box::new(ParsedCondition::ZoneCoreTypeCardCountAtLeast {
                    zone: Zone::Hand,
                    core_type: CoreType::Land,
                    count: 1,
                })
            })
        );
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

    // ---- P02-U3b: ported phrasing families now owned by the SHARED grammar ----
    //
    // Each of these asserts the phrase reaches `parse_inner_condition` AND converts.
    // Pre-port every one of them panics with "NoMatch (fell to restriction-only
    // grammar)" — that RED is the witness that the family was a real phrasing gap
    // and not already-shadowed dead code.

    /// CR 508.1a: the typed-attacker surface (Thaumaton Torpedo).
    #[test]
    fn shared_grammar_owns_you_attacked_with_typed_filter() {
        match shared("you attacked with a Spacecraft this turn") {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::AttackedThisTurn {
                                scope: CountScope::Controller,
                                filter: Some(_),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("expected filtered AttackedThisTurn >= 1, got {other:?}"),
        }
    }

    /// CR 508.1a: the numeric-threshold surface carries NO type qualifier.
    #[test]
    fn shared_grammar_owns_you_attacked_with_creature_count() {
        match shared("you attacked with three or more creatures this turn") {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::AttackedThisTurn {
                                scope: CountScope::Controller,
                                filter: None,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } => {}
            other => panic!("expected unfiltered AttackedThisTurn >= 3, got {other:?}"),
        }
    }

    /// HOSTILE: an unrecognized attacker qualifier must stay an honest gap rather
    /// than widening to "attacked with anything".
    #[test]
    fn unknown_attacker_qualifier_is_not_silently_widened() {
        let parsed = parse_restriction_condition("you attacked with a frob the wobble this turn");
        assert!(
            parsed.is_none(),
            "unknown attacker type must not produce a condition, got {parsed:?}"
        );
    }

    /// CR 201.2 + CR 109.3: shared-name land count (Endless Atlas, Sceptre of Eternal Glory).
    /// `aggregate: Max` is the load-bearing field — it is what makes this "some ONE
    /// name is shared by three lands" instead of "you control three lands".
    #[test]
    fn shared_grammar_owns_lands_with_the_same_name() {
        match shared("you control three or more lands with the same name") {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ObjectCountBySharedQuality {
                                quality: SharedQuality::Name,
                                aggregate: AggregateFunction::Max,
                                ..
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } => {}
            other => panic!("expected ObjectCountBySharedQuality[Name,Max] >= 3, got {other:?}"),
        }
    }

    /// CR 102.2 + CR 608.2h: the opponent-scoped entry tally (Whiplash Trap).
    ///
    /// The scope MUST ride on `PlayerScope::Opponent{Max}`, not on a controller
    /// injected into the type filter. A filter-carried controller sums entries
    /// across ALL opponents, so in multiplayer two different opponents with one
    /// creature each would satisfy "an opponent had TWO OR MORE creatures enter".
    #[test]
    fn shared_grammar_owns_opponent_battlefield_entries_per_opponent() {
        match shared(
            "an opponent had two or more creatures enter the battlefield under their control this turn",
        ) {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::BattlefieldEntriesThisTurn {
                                player:
                                    PlayerScope::Opponent {
                                        aggregate: AggregateFunction::Max,
                                    },
                                ..
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            } => {}
            other => panic!("expected per-opponent BattlefieldEntriesThisTurn >= 2, got {other:?}"),
        }
    }

    /// CR 701.23a: opponent library search (Archive Trap).
    #[test]
    fn shared_grammar_owns_opponent_searched_library() {
        match shared("an opponent searched their library this turn") {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::PlayerActionsThisTurn {
                                player:
                                    PlayerScope::Opponent {
                                        aggregate: AggregateFunction::Max,
                                    },
                                action: PlayerActionKind::SearchedLibrary,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("expected per-opponent SearchedLibrary >= 1, got {other:?}"),
        }
    }

    /// CR 402.1 + CR 608.2c: the exact-hand-size DISJUNCTION (The Biblioplex).
    /// Equivalent to the retired `HandSizeOneOf { counts }`, whose evaluator was
    /// literally `counts.contains(&hand_size)`.
    #[test]
    fn shared_grammar_owns_exact_hand_size_disjunction() {
        match shared("you have exactly zero or seven cards in hand") {
            ParsedCondition::Or { conditions } => {
                assert_eq!(conditions.len(), 2, "got {conditions:?}");
                for (expected, cond) in [0, 7].iter().zip(conditions.iter()) {
                    match cond {
                        ParsedCondition::QuantityComparison {
                            lhs:
                                QuantityExpr::Ref {
                                    qty: QuantityRef::HandSize { .. },
                                },
                            comparator: Comparator::EQ,
                            rhs: QuantityExpr::Fixed { value },
                        } => assert_eq!(value, expected),
                        other => panic!("expected HandSize EQ leaf, got {other:?}"),
                    }
                }
            }
            other => panic!("expected Or over two HandSize EQ leaves, got {other:?}"),
        }
    }

    /// The single-count "exactly N" surface must NOT become a one-armed `Or`
    /// (Triskaidekaphile) — arity 1 stays a bare `EQ`, so the port is additive.
    #[test]
    fn exact_hand_size_single_count_stays_a_bare_eq() {
        match shared("you have exactly seven cards in hand") {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::HandSize { .. },
                    },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 7 },
            } => {}
            other => panic!("expected a bare HandSize EQ 7, got {other:?}"),
        }
    }

    /// RETAINED, NOT PORTED: `BeenAttackedThisStep` has no `StaticCondition`
    /// counterpart (no per-STEP attack history), so it must still be produced by
    /// the restriction-only fallback. This pins the boundary of the port.
    #[test]
    fn been_attacked_this_step_stays_in_the_restriction_only_fallback() {
        assert!(matches!(
            parse_shared_restriction_condition("you've been attacked this step"),
            SharedRestrictionParse::NoMatch
        ));
        assert_eq!(
            parse_restriction_condition("You've been attacked this step"),
            Some(ParsedCondition::BeenAttackedThisStep)
        );
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
    const PINNED_RETAINED_FAMILIES: [&str; 4] = [
        // (a) restriction-context referent — the in-flight spell (CR 601.3d). PERMANENT.
        "parse_spell_targets_filter",
        // (b) vocabulary gap — ParsedCondition has no filter-carrying source predicate,
        //     so StaticCondition::SourceMatchesFilter cannot be converted. Root fix is a
        //     vocabulary alignment, not a parser edit.
        "parse_source_condition",
        // (b) vocabulary gap — OWNER-relative count in a HIDDEN zone. TargetFilter has no
        //     owner axis and the ObjectCount path discriminates by CONTROLLER, so the
        //     shared reading would be a different predicate, not a re-spelling.
        //     ("you have no <kind> cards in hand" — Land Grant.)
        "parse_hand_condition",
        // (b) vocabulary gap — StaticCondition's attack history is per-TURN; there is no
        //     per-STEP vocabulary for `BeenAttackedThisStep` to convert into.
        "parse_event_condition",
    ];

    // P02-U3b: the (c) PHRASING-GAP class is now EMPTY. `parse_you_control_condition` and
    // `parse_you_attacked_with` were deleted outright, and `parse_hand_condition` /
    // `parse_event_condition` were reduced to the (b) leaves above. Every family that
    // remains is here because the shared vocabulary genuinely cannot express it — not
    // because the grammar merely spells it differently.

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
