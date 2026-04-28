//! mtgish `Condition` → engine `TriggerCondition` (Phase 8 narrow slice).
//!
//! mtgish has 250+ Condition variants; the engine has 48 `TriggerCondition`
//! variants and a separate `AbilityCondition` for activated/spell ability
//! intervening-ifs. This narrow slice handles the simplest constants and
//! turn-ownership checks. Recursive And/Or compose. Everything else
//! strict-fails so the report surfaces it.

use engine::types::ability::{
    AbilityCondition, AggregateFunction, Comparator, ControllerRef, CountScope, FilterProp,
    ParsedCondition, PlayerScope, QuantityExpr, QuantityRef, StaticCondition, TargetFilter,
    TriggerCondition, TypedFilter, ZoneRef,
};
use engine::types::card_type::CoreType;
use engine::types::zones::Zone;

use crate::convert::result::{ConvResult, ConversionGap};
use crate::schema::types::{
    CardType, Cards, Comparison, Condition, GameNumber, Permanent, Permanents, Player, Players,
    Spells,
};

/// CR 608.2c + CR 700.4: Convert an mtgish `Condition` for use as an
/// `AbilityCondition` (sub-ability gating: "if [cond], [effect]"). The
/// engine's `AbilityCondition` enum has no general boolean fan-out; only
/// `IsYourTurn { negated }` and `And { conditions }` are general-purpose.
/// Matched on the smallest viable subset that bridges Action::If /
/// Action::IfElse onto the existing engine condition surface; everything
/// else strict-fails (no `AlwaysTrue`/`Unconditional` permissive fallback).
pub fn convert_ability(c: &Condition) -> ConvResult<AbilityCondition> {
    Ok(match c {
        Condition::And(parts) => AbilityCondition::And {
            conditions: parts
                .iter()
                .map(convert_ability)
                .collect::<ConvResult<_>>()?,
        },
        // CR 608.2c: Compound disjunction — mirrors the trigger/static `Or` arms;
        // the engine evaluator short-circuits on the first true child.
        Condition::Or(parts) => AbilityCondition::Or {
            conditions: parts
                .iter()
                .map(convert_ability)
                .collect::<ConvResult<_>>()?,
        },
        // CR 608.2c: "If it's your turn" / "If it's not your turn".
        Condition::IsPlayersTurn(p) => match &**p {
            Player::You => AbilityCondition::IsYourTurn,
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Condition::IsPlayersTurn (ability)",
                    path: String::new(),
                    detail: format!("unsupported Player: {other:?}"),
                });
            }
        },
        Condition::IsNotPlayersTurn(p) => match &**p {
            Player::You => AbilityCondition::Not {
                condition: Box::new(AbilityCondition::IsYourTurn),
            },
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Condition::IsNotPlayersTurn (ability)",
                    path: String::new(),
                    detail: format!("unsupported Player: {other:?}"),
                });
            }
        },
        // CR 608.2c: "if [player] [passes predicate]" — dispatched per leaf
        // predicate so each maps onto an existing AbilityCondition.
        Condition::PlayerPassesFilter(player, predicate) => {
            convert_player_predicate_ability(player, predicate)?
        }
        // CR 608.2c + CR 603.4: "if a [player set] [passes predicate]" — the
        // existential plural form ("an opponent has 10 or less life"). Routes
        // to a sibling dispatcher that maps the predicate onto opponent-axis
        // QuantityRefs / opponent-controlled filters or, when the player set
        // collapses to the controller, delegates back to the singular
        // `PlayerPassesFilter` dispatcher.
        Condition::APlayerPassesFilter(player_set, predicate) => {
            convert_aplayer_predicate_ability(player_set, predicate)?
        }
        // CR 608.2c + CR 611.2b: "if [permanent] [passes predicate]" — leaf
        // dispatch on the permanent axis (source/target) and the predicate.
        // Source-aliased axes (`ThisPermanent`, `Trigger_That*`,
        // `ThatEnteringPermanent`, `ActionPermanent`, `Self_It`) refer to the
        // ability's source object → wrap as `SourceMatchesFilter` /
        // `SourceIsTapped` / `SourceIsAttacking` etc. Target-aliased axes
        // (`Ref_TargetPermanent*`, `RefOuter_TargetPermanent`,
        // `AnyTargetAsAPermanent`, `TheChosenPermanent`) refer to the chosen
        // target → `TargetMatchesFilter`. Other axes (host, attached, etc.)
        // strict-fail — no AbilityCondition variant scopes those today.
        Condition::PermanentPassesFilter(perm, pred) => permanent_filter_to_ability(perm, pred)?,
        // CR 603.6d + CR 608.2c: "if it [passes predicate]" inside an Action::If
        // body of an ETB triggered ability — the entering permanent IS the
        // surrounding trigger's source, so dispatch onto source-bound
        // AbilityCondition variants.
        Condition::EnteringPermanentPassesFilter(pred) => {
            entering_permanent_filter_to_ability(pred)?
        }
        // CR 608.2c + CR 702.34 (Flashback) + CR 702.143 (Foretell): "If you
        // cast it from [zone], [do A]. Otherwise, [do B]." — self-referential
        // check on the resolving spell's `cast_from_zone`. Maps the zone-bound
        // `Spells` predicates onto `AbilityCondition::CastFromZone { zone }`.
        // Non-zone predicates (`AlternateCostWasPaid`, `WasKicked`, `WasForetold`,
        // `WasBargained`, etc.) need separate engine slots and strict-fail.
        Condition::CastSpellPassesFilter(spells) => spells_to_cast_zone_ability(spells)?,
        _ => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: variant_tag(c),
            });
        }
    })
}

/// CR 608.2c + CR 702.34 + CR 702.143: Map a `Spells` predicate that gates
/// the resolving spell's effect onto `AbilityCondition::CastFromZone`. Only
/// zone-axis predicates translate today — the engine's per-spell context
/// stores `cast_from_zone` (a `Zone`) but no kicker/foretell/bargain-paid
/// flags reachable from `AbilityCondition`. Player-scoped variants
/// (`WasCastFromPlayersHand(p)`, `WasCastFromAPlayersGraveyard(ps)`) collapse
/// to the zone kind only when the player axis is unrestricted (`AnyPlayer` or
/// `Player::You`); narrower scopes strict-fail because the engine condition
/// has no player parameter.
fn spells_to_cast_zone_ability(spells: &Spells) -> ConvResult<AbilityCondition> {
    let zone = match spells {
        // CR 702.34 (Flashback): "if you cast it from a graveyard"
        Spells::WasCastFromAPlayersGraveyard(players) => match &**players {
            Players::AnyPlayer => Zone::Graveyard,
            other => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "AbilityCondition::CastFromZone",
                    needed_variant: format!(
                        "CastFromZone with player-scoped graveyard: {}",
                        players_variant_tag(other)
                    ),
                });
            }
        },
        // CR 601.2 + CR 702.143: "if you cast it from exile" (foretell, impulse).
        Spells::WasCastFromExile => Zone::Exile,
        // CR 601.2: "if you cast it from your hand" — controller-relative; the
        // resolving spell's controller IS its caster, so `WasCastFromTheirHand`
        // and `WasCastFromPlayersHand(You)` both collapse to Zone::Hand.
        Spells::WasCastFromTheirHand => Zone::Hand,
        Spells::WasCastFromPlayersHand(player) => match &**player {
            Player::You => Zone::Hand,
            other => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "AbilityCondition::CastFromZone",
                    needed_variant: format!("CastFromZone with non-You hand axis: {other:?}"),
                });
            }
        },
        // CR 601.2: "if you cast it from your library" (Panglacial Wurm etc.).
        Spells::WasCastFromTheirLibrary => Zone::Library,
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCondition::CastFromZone",
                needed_variant: format!(
                    "CastSpellPassesFilter with non-zone predicate: {}",
                    spells_variant_tag(other)
                ),
            });
        }
    };
    Ok(AbilityCondition::CastFromZone { zone })
}

/// CR 608.2c: Convert a condition for use as the negated form on
/// `Action::Unless`. The general path wraps `convert_ability(inner)` in
/// `AbilityCondition::Not { condition }`, mirroring the sibling
/// `TargetFilter::Not` and `StaticCondition::Not` constructions. Inner
/// conditions that don't translate (because `convert_ability` strict-fails
/// on them) propagate their gap up — `Action::Unless` is no more permissive
/// than `Action::If`.
pub fn convert_ability_negated(c: &Condition) -> ConvResult<AbilityCondition> {
    Ok(AbilityCondition::Not {
        condition: Box::new(convert_ability(c)?),
    })
}

/// CR 603.4: Convert an intervening-if Condition for a triggered ability.
pub fn convert_trigger(c: &Condition) -> ConvResult<TriggerCondition> {
    Ok(match c {
        Condition::And(parts) => TriggerCondition::And {
            conditions: parts
                .iter()
                .map(convert_trigger)
                .collect::<ConvResult<_>>()?,
        },
        Condition::Or(parts) => TriggerCondition::Or {
            conditions: parts
                .iter()
                .map(convert_trigger)
                .collect::<ConvResult<_>>()?,
        },

        // CR 603.4: Turn-owner gating — engine has DuringYourTurn/NotYourTurn.
        // mtgish encodes these as IsPlayersTurn(Player). Map "your turn" /
        // "not your turn"; everything else strict-fails for now.
        Condition::IsPlayersTurn(p) => match &**p {
            Player::You => TriggerCondition::DuringYourTurn,
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Condition::IsPlayersTurn",
                    path: String::new(),
                    detail: format!("unsupported Player: {other:?}"),
                });
            }
        },
        Condition::IsNotPlayersTurn(p) => match &**p {
            Player::You => TriggerCondition::Not {
                condition: Box::new(TriggerCondition::DuringYourTurn),
            },
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Condition::IsNotPlayersTurn",
                    path: String::new(),
                    detail: format!("unsupported Player: {other:?}"),
                });
            }
        },

        // CR 603.4: "if [player] [passes predicate]" — dispatched per leaf
        // predicate so each maps onto an existing TriggerCondition variant.
        Condition::PlayerPassesFilter(player, predicate) => {
            convert_player_predicate_trigger(player, predicate)?
        }
        // CR 603.4: "if a [player set] [passes predicate]" — existential plural
        // intervening-if ("if an opponent lost life this turn"). Sibling
        // dispatcher maps the predicate onto opponent-axis QuantityRefs and
        // opponent-controlled filter primitives, or delegates back to the
        // singular dispatcher when the set is `Players::You`.
        Condition::APlayerPassesFilter(player_set, predicate) => {
            convert_aplayer_predicate_trigger(player_set, predicate)?
        }
        // CR 603.4 + CR 611.2b: Source-aliased "if [permanent] passes filter"
        // intervening-if. Only source-axis variants (`ThisPermanent`,
        // `Trigger_That*`, `ThatEnteringPermanent`, `ActionPermanent`,
        // `Self_It`) are mappable — the trigger's source object is the natural
        // referent. Predicate dispatched onto existing source-bound
        // TriggerCondition variants (`SourceIsTapped`, `SourceIsAttacking`,
        // `HasCounters`). Other axes / non-source-bound predicates strict-fail
        // because TriggerCondition has no general `Source/TargetMatchesFilter`.
        Condition::PermanentPassesFilter(perm, pred) => permanent_filter_to_trigger(perm, pred)?,
        // CR 603.6d + CR 603.4: ETB intervening-if "when ~ enters, if it
        // [passes predicate]". The entering permanent IS the trigger source,
        // so source-bound predicates (`SourceIsTapped`, `SourceIsAttacking`,
        // `HasCounters`) apply directly. Type/subtype filter checks on the
        // *current* state strict-fail today — `WasType` is LKI-keyed and not
        // appropriate for ETB intervening-ifs (ETB checks current state).
        Condition::EnteringPermanentPassesFilter(pred) => {
            entering_permanent_filter_to_trigger(pred)?
        }

        _ => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: variant_tag(c),
            });
        }
    })
}

/// CR 613 + CR 608.2c: Convert a `Condition` for use as a `StaticCondition`
/// (continuous-effect gating: "as long as ~ is tapped"). The supported subset
/// is intentionally narrow for now; unknown variants strict-fail so the
/// report tracks remaining gaps.
pub fn convert_static(c: &Condition) -> ConvResult<StaticCondition> {
    Ok(match c {
        Condition::And(parts) => StaticCondition::And {
            conditions: parts
                .iter()
                .map(convert_static)
                .collect::<ConvResult<_>>()?,
        },
        Condition::Or(parts) => StaticCondition::Or {
            conditions: parts
                .iter()
                .map(convert_static)
                .collect::<ConvResult<_>>()?,
        },
        // CR 611.2b + CR 613: "as long as [permanent] is X" — when the
        // permanent axis aliases the source object, `permanent_filter_to_static`
        // produces a source-bound StaticCondition. Non-source axes
        // (host/attached/target) strict-fail because StaticCondition only has
        // `SourceMatchesFilter`, not `TargetMatchesFilter`.
        Condition::PermanentPassesFilter(perm, pred) => {
            require_source_axis(perm, "Condition::PermanentPassesFilter (static)")?;
            permanent_filter_to_static(pred)?
        }
        // CR 500: "during your turn" / "during an opponent's turn" gates a
        // static effect on whose turn is currently active.
        Condition::IsPlayersTurn(p) => match &**p {
            Player::You => StaticCondition::DuringYourTurn,
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Condition::IsPlayersTurn (static)",
                    path: String::new(),
                    detail: format!("unsupported Player: {other:?}"),
                });
            }
        },
        Condition::IsNotPlayersTurn(p) => match &**p {
            Player::You => StaticCondition::Not {
                condition: Box::new(StaticCondition::DuringYourTurn),
            },
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Condition::IsNotPlayersTurn (static)",
                    path: String::new(),
                    detail: format!("unsupported Player: {other:?}"),
                });
            }
        },
        // CR 613 + CR 608.2c: "as long as [player] [passes predicate]" — per-leaf
        // dispatch onto existing StaticCondition variants.
        Condition::PlayerPassesFilter(player, predicate) => {
            convert_player_predicate_static(player, predicate)?
        }
        // CR 613 + CR 603.4: "as long as a [player set] [passes predicate]" —
        // existential plural form on a continuous-effect gate. Maps the
        // predicate onto opponent-axis QuantityRefs / opponent-controlled
        // presence checks, or delegates back to the singular dispatcher when
        // the player set is `Players::You`.
        Condition::APlayerPassesFilter(player_set, predicate) => {
            convert_aplayer_predicate_static(player_set, predicate)?
        }
        _ => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: variant_tag(c),
            });
        }
    })
}

/// Classify the permanent-axis of a `Condition::PermanentPassesFilter`'s
/// first argument. The same Oracle phrase ("if it's a [type]") routes to
/// either source- or target-bound engine variants depending on what the
/// "it" refers to in mtgish's typed AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PermanentAxis {
    /// Aliases the source object of the surrounding ability or trigger.
    /// Engine routes to `Source*` conditions.
    Source,
    /// Aliases the chosen target of the surrounding ability/spell. Engine
    /// routes to `TargetMatchesFilter` (AbilityCondition only — Trigger
    /// and Static lack a target-axis condition).
    Target,
}

/// Map a `Permanent` AST node onto a `PermanentAxis` if it aliases either
/// the source or the chosen target. Returns `None` for unsupported axes
/// (host/attached/sacrificed/etc.) so callers can strict-fail with the
/// concrete variant in the gap report.
fn classify_permanent_axis(p: &Permanent) -> Option<PermanentAxis> {
    use Permanent as P;
    match p {
        // Source-aliased: the surrounding ability's own object. CR 109.5.
        P::ThisPermanent
        | P::Self_It
        | P::ActionPermanent
        | P::ApplyPermanentEffect_It
        | P::CreatePermanentEffect_It
        | P::EachPermanentEffect_It
        | P::ThatEnteringPermanent
        | P::Trigger_ThatPermanent
        | P::Trigger_ThatCreature
        | P::Trigger_ThatCreatureOrPlaneswalker
        | P::Trigger_ThatArtifact
        | P::Trigger_ThatVehicle
        | P::Trigger_ThatOtherCreature
        | P::Trigger_ThatOtherPermanent
        | P::Trigger_ThatDeadPermanent
        | P::Trigger_ThatSacrificedPermanent
        | P::Trigger_ThatLand
        | P::Trigger_TheAttackingCreature
        | P::Trigger_TheBlockingCreature => Some(PermanentAxis::Source),
        // Target-aliased: the chosen target of the surrounding ability. CR 115.
        P::Ref_TargetPermanent
        | P::Ref_TargetPermanent1
        | P::Ref_TargetPermanent2
        | P::Ref_TargetPermanent3
        | P::Ref_TargetPermanent4
        | P::Ref_TargetPermanent5
        | P::Ref_TargetPermanents_0
        | P::Ref_TargetPermanents_1
        | P::Ref_TargetPermanentOfPlayersChoice
        | P::RefOuter_TargetPermanent
        | P::AnyTargetAsAPermanent
        | P::TheChosenPermanent
        | P::TheFirstChosenPermanent
        | P::TheSecondChosenPermanent => Some(PermanentAxis::Target),
        _ => None,
    }
}

fn require_source_axis(p: &Permanent, idiom: &'static str) -> ConvResult<()> {
    match classify_permanent_axis(p) {
        Some(PermanentAxis::Source) => Ok(()),
        Some(PermanentAxis::Target) => Err(ConversionGap::MalformedIdiom {
            idiom,
            path: String::new(),
            detail: format!("target-axis permanent has no source-bound analog: {p:?}"),
        }),
        None => Err(ConversionGap::MalformedIdiom {
            idiom,
            path: String::new(),
            detail: format!("non-source/target-axis permanent: {p:?}"),
        }),
    }
}

/// CR 608.2c: AbilityCondition surface dispatch for
/// `Condition::PermanentPassesFilter(perm, pred)`. Source-axis routes to
/// source-bound conditions (`SourceMatchesFilter`, `SourceIsTapped`, etc.);
/// target-axis routes to `TargetMatchesFilter`. Predicates that are pure
/// state checks (tapped, attacking, blocking, has-counter) prefer the
/// dedicated condition variant when source-axis. Type/subtype/general
/// filters delegate to `filter::convert` and route via `*MatchesFilter`.
fn permanent_filter_to_ability(
    perm: &Permanent,
    pred: &Permanents,
) -> ConvResult<AbilityCondition> {
    let axis = classify_permanent_axis(perm).ok_or_else(|| ConversionGap::MalformedIdiom {
        idiom: "Condition::PermanentPassesFilter (ability)",
        path: String::new(),
        detail: format!("non-source/target-axis permanent: {perm:?}"),
    })?;
    // Predicate-specific source-axis shortcuts onto existing dedicated variants.
    // Counter checks (`HasACounter[OfType]`) fall through to the general
    // `SourceMatchesFilter` path via `filter::convert` (which maps them to
    // `FilterProp::HasAnyCounter`/typed counter filter props).
    if axis == PermanentAxis::Source {
        match pred {
            // CR 611.2b: "if ~ is tapped/untapped".
            Permanents::IsTapped => return Ok(AbilityCondition::SourceIsTapped),
            Permanents::IsUntapped => {
                return Ok(AbilityCondition::Not {
                    condition: Box::new(AbilityCondition::SourceIsTapped),
                })
            }
            _ => {}
        }
    }
    let filter = crate::convert::filter::convert(pred)?;
    Ok(match axis {
        PermanentAxis::Source => AbilityCondition::SourceMatchesFilter { filter },
        PermanentAxis::Target => AbilityCondition::TargetMatchesFilter {
            filter,
            use_lki: false,
        },
    })
}

/// CR 603.4 + CR 611.2b: TriggerCondition surface dispatch. Only source-axis
/// permanents are mappable — the engine has no `TargetMatchesFilter` /
/// `SourceMatchesFilter` for trigger intervening-ifs. Source-axis routes the
/// predicate onto dedicated source-bound `TriggerCondition` variants when
/// available. General filter predicates strict-fail with a precise detail
/// string so the report tracks remaining gaps.
fn permanent_filter_to_trigger(
    perm: &Permanent,
    pred: &Permanents,
) -> ConvResult<TriggerCondition> {
    require_source_axis(perm, "Condition::PermanentPassesFilter (trigger)")?;
    entering_permanent_filter_to_trigger(pred)
}

/// CR 603.6d + CR 603.4: ETB-style intervening-if where the trigger source
/// IS the entering/triggering permanent. Predicate dispatched onto
/// dedicated source-bound TriggerCondition variants.
/// CR 603.6d + CR 608.2c: AbilityCondition surface dispatch for the entering
/// permanent. Routes the predicate onto a source-bound AbilityCondition (the
/// ETB trigger's source IS the entering permanent), reusing the type/subtype
/// path via `filter::convert` + `SourceMatchesFilter`.
fn entering_permanent_filter_to_ability(pred: &Permanents) -> ConvResult<AbilityCondition> {
    Ok(match pred {
        // CR 611.2b: "if it is tapped/untapped".
        Permanents::IsTapped => AbilityCondition::SourceIsTapped,
        Permanents::IsUntapped => AbilityCondition::Not {
            condition: Box::new(AbilityCondition::SourceIsTapped),
        },
        // Type/subtype/general filter checks — delegate to filter::convert,
        // wrap as source-bound match.
        _ => {
            let filter = crate::convert::filter::convert(pred)?;
            AbilityCondition::SourceMatchesFilter { filter }
        }
    })
}

fn entering_permanent_filter_to_trigger(pred: &Permanents) -> ConvResult<TriggerCondition> {
    use engine::types::counter::CounterMatch;
    Ok(match pred {
        // CR 611.2b: "if it is tapped/untapped". Untapped wraps via `Not`.
        Permanents::IsTapped => TriggerCondition::SourceIsTapped,
        Permanents::IsUntapped => TriggerCondition::Not {
            condition: Box::new(TriggerCondition::SourceIsTapped),
        },
        // CR 701.27g: "if it is transformed" — source-bound transformed check.
        Permanents::IsTransformed => TriggerCondition::SourceIsTransformed,
        // CR 708.2: face-up / face-down source-state predicates.
        Permanents::IsFaceUp => TriggerCondition::SourceIsFaceUp,
        Permanents::IsFaceDown => TriggerCondition::SourceIsFaceDown,
        // CR 506.4: "if it's attacking".
        Permanents::IsAttacking => TriggerCondition::SourceIsAttacking,
        // CR 122.1 + CR 711.2: "if it has a counter on it" / typed counter.
        Permanents::HasACounter => TriggerCondition::HasCounters {
            counters: CounterMatch::Any,
            minimum: 1,
            maximum: None,
        },
        Permanents::HasACounterOfType(ct) => TriggerCondition::HasCounters {
            counters: counter_match_for(ct),
            minimum: 1,
            maximum: None,
        },
        // CR 122.1 + CR 711.2: "if it has no counters" / "if it has no [type] counters" —
        // matches HasCounters with minimum=0 AND maximum=Some(0).
        Permanents::HasNoCounters => TriggerCondition::HasCounters {
            counters: CounterMatch::Any,
            minimum: 0,
            maximum: Some(0),
        },
        Permanents::HasNoCountersOfType(ct) => TriggerCondition::HasCounters {
            counters: counter_match_for(ct),
            minimum: 0,
            maximum: Some(0),
        },
        // CR 122.1 + CR 711.2: "if it has N or more [type] counters" — GE-shaped
        // comparison maps onto HasCounters { minimum }. Other comparator shapes
        // (LE/EQ/NE/LT) lack a matching HasCounters sub-shape; strict-fail.
        Permanents::HasNumCounters(cmp) => match comparison_as_min_u32(cmp) {
            Some(minimum) => TriggerCondition::HasCounters {
                counters: CounterMatch::Any,
                minimum,
                maximum: None,
            },
            None => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Condition::EnteringPermanentPassesFilter/HasNumCounters",
                    path: String::new(),
                    detail: format!("non-GE comparator on counter count: {cmp:?}"),
                });
            }
        },
        Permanents::HasNumCountersOfType(cmp, ct) => match comparison_as_min_u32(cmp) {
            Some(minimum) => TriggerCondition::HasCounters {
                counters: counter_match_for(ct),
                minimum,
                maximum: None,
            },
            None => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Condition::EnteringPermanentPassesFilter/HasNumCountersOfType",
                    path: String::new(),
                    detail: format!("non-GE comparator on counter count: {cmp:?}"),
                });
            }
        },
        // CR 601.2: "if it was cast" / "if you cast it" — entering permanent
        // entered via the stack rather than a non-cast zone change. Engine's
        // `WasCast` predicate is zoneless (mirrors Discover ETB usage).
        Permanents::WasCast | Permanents::ItWasCast => TriggerCondition::WasCast,
        // CR 702.112a: "if ~ is renowned" — source-bound renowned check.
        Permanents::IsRenowned => TriggerCondition::SourceIsRenowned,
        // CR 208.3 + CR 603.4: "if its mana value is X" — comparison against the
        // source's current mana value via QuantityComparison.
        Permanents::ManaValueIs(cmp) => {
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SelfManaValue,
                },
                comparator,
                rhs,
            }
        }
        // CR 208.2 + CR 603.4: "if its power is X" / "if its toughness is X" —
        // source-bound power/toughness comparison via QuantityComparison.
        Permanents::PowerIs(cmp) => {
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: engine::types::ability::ObjectScope::Source,
                    },
                },
                comparator,
                rhs,
            }
        }
        Permanents::ToughnessIs(cmp) => {
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::Toughness {
                        scope: engine::types::ability::ObjectScope::Source,
                    },
                },
                comparator,
                rhs,
            }
        }
        // CR 603.4 + CR 608.2c: predicate-side conjunction/disjunction recurses
        // onto the trigger combinator surface (engine `TriggerCondition::And`/`Or`).
        Permanents::And(parts) => TriggerCondition::And {
            conditions: parts
                .iter()
                .map(entering_permanent_filter_to_trigger)
                .collect::<ConvResult<_>>()?,
        },
        Permanents::Or(parts) => TriggerCondition::Or {
            conditions: parts
                .iter()
                .map(entering_permanent_filter_to_trigger)
                .collect::<ConvResult<_>>()?,
        },
        // CR 608.2c: predicate-side negation ("if it isn't a [type]") wraps
        // the inner predicate's TriggerCondition in `TriggerCondition::Not`,
        // mirroring the sibling `StaticCondition::Not`/`AbilityCondition::Not`
        // wrapper construction. Inner predicates that don't translate
        // propagate their gap up — `Not` is no more permissive than the
        // unwrapped form.
        Permanents::Not(inner) => TriggerCondition::Not {
            condition: Box::new(entering_permanent_filter_to_trigger(inner)?),
        },
        other => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "Condition::EnteringPermanentPassesFilter/predicate",
                path: String::new(),
                detail: format!(
                    "no source-bound TriggerCondition for predicate: {}",
                    permanents_variant_tag(other)
                ),
            });
        }
    })
}

/// CR 603.6 + CR 603.10: ETB triggers "look back in time" to evaluate the
/// entering object's snapshot. When an ETB intervening-if predicate is purely
/// snapshot-derivable (type/subtype/color/keyword/CMC/power/toughness/historic/
/// token), it can be encoded directly into the `TriggerDefinition.valid_card`
/// `TargetFilter` rather than as a `TriggerCondition` — the engine evaluates
/// `valid_card` against the zone-change snapshot record (see
/// `game/trigger_matchers.rs::matches_target_filter_on_zone_change_record`
/// and `game/filter.rs::zone_change_record_matches_property`).
///
/// Live-state predicates (tapped/face-down/attacking/etc.) are NOT
/// snapshot-derivable — the snapshot evaluator returns `false` for them
/// (CR 603.6 + `game/filter.rs:1815-1869`). For those, this helper
/// strict-fails with `EnginePrerequisiteMissing` so they are routed to a
/// separate engine extension round (`TriggerCondition::SourceIs<X>` variants).
///
/// Mappable predicates that the trigger-condition catch-all rejected (because
/// they have no `TriggerCondition` analog) but DO map cleanly to a
/// snapshot-safe `TargetFilter` flow through here.
pub(crate) fn entering_permanent_filter_to_valid_card(
    pred: &Permanents,
) -> ConvResult<TargetFilter> {
    // Live-state predicates that map to filter props the snapshot evaluator
    // explicitly fails closed on (or that have no FilterProp at all). These
    // belong on `TriggerCondition::SourceIs<X>` variants in a follow-up engine
    // extension round — strict-fail with the precise needed_variant.
    if let Some(needed) = needed_trigger_source_variant(pred) {
        return Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "TriggerCondition",
            needed_variant: needed.to_string(),
        });
    }
    let filter = crate::convert::filter::convert(pred)?;
    if let Some(unsafe_prop) = first_unsafe_prop(&filter) {
        return Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "TriggerCondition",
            needed_variant: format!("SourceIs<{unsafe_prop}> (live-state predicate)"),
        });
    }
    Ok(filter)
}

/// Mtgish predicates that fundamentally need a live-battlefield evaluator —
/// no snapshot-safe encoding exists. Returns the proposed engine variant name
/// for the strict-fail `needed_variant` field. This list is intentionally
/// conservative: only predicates with no existing `TriggerCondition`
/// analog AND no snapshot-derivable filter prop appear here.
fn needed_trigger_source_variant(pred: &Permanents) -> Option<&'static str> {
    match pred {
        Permanents::IsMonstrous => Some("SourceIsMonstrous"),
        Permanents::IsSaddled => Some("SourceIsSaddled"),
        Permanents::IsSuspected => Some("SourceIsSuspected"),
        _ => None,
    }
}

/// Walk a `TargetFilter` and return the name of the first `FilterProp` that
/// the zone-change snapshot evaluator fails closed on (CR 603.6 +
/// `game/filter.rs:1815-1869`). Returns `None` if every leaf prop is
/// snapshot-derivable.
fn first_unsafe_prop(f: &TargetFilter) -> Option<&'static str> {
    match f {
        TargetFilter::Typed(TypedFilter { properties, .. }) => {
            properties.iter().find_map(unsafe_prop_name)
        }
        TargetFilter::Not { filter } => first_unsafe_prop(filter),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().find_map(first_unsafe_prop)
        }
        // Leaf forms with no embedded FilterProp. `None`/`Any`/`SelfRef`/Typed
        // are the only shapes `filter::convert(Permanents)` produces in
        // practice; the remaining variants are runtime-resolution forms
        // (TrackedSet, ParentTarget, AttachedTo, …) that have no defined
        // semantics against a zone-change snapshot. Treat as unsafe (fail
        // closed) so any future filter::convert path emitting them surfaces
        // a precise gap rather than silently breaking the trigger.
        TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SelfRef => None,
        other => Some(target_filter_variant_name(other)),
    }
}

fn target_filter_variant_name(f: &TargetFilter) -> &'static str {
    match f {
        TargetFilter::None => "None",
        TargetFilter::Any => "Any",
        TargetFilter::Player => "Player",
        TargetFilter::Controller => "Controller",
        TargetFilter::SelfRef => "SelfRef",
        TargetFilter::Typed(_) => "Typed",
        TargetFilter::Not { .. } => "Not",
        TargetFilter::Or { .. } => "Or",
        TargetFilter::And { .. } => "And",
        TargetFilter::StackAbility => "StackAbility",
        TargetFilter::StackSpell => "StackSpell",
        TargetFilter::SpecificObject { .. } => "SpecificObject",
        TargetFilter::SpecificPlayer { .. } => "SpecificPlayer",
        TargetFilter::AttachedTo => "AttachedTo",
        TargetFilter::LastCreated => "LastCreated",
        TargetFilter::TrackedSet { .. } => "TrackedSet",
        TargetFilter::TrackedSetFiltered { .. } => "TrackedSetFiltered",
        TargetFilter::ExiledBySource => "ExiledBySource",
        TargetFilter::TriggeringSpellController => "TriggeringSpellController",
        TargetFilter::TriggeringSpellOwner => "TriggeringSpellOwner",
        TargetFilter::TriggeringPlayer => "TriggeringPlayer",
        TargetFilter::TriggeringSource => "TriggeringSource",
        TargetFilter::ParentTarget => "ParentTarget",
        TargetFilter::ParentTargetController => "ParentTargetController",
        TargetFilter::PostReplacementSourceController => "PostReplacementSourceController",
        TargetFilter::DefendingPlayer => "DefendingPlayer",
        TargetFilter::HasChosenName => "HasChosenName",
        TargetFilter::Named { .. } => "Named",
        TargetFilter::Owner => "Owner",
    }
}

/// CR 603.6 + `game/filter.rs:1815-1869`: properties the zone-change snapshot
/// evaluator fails closed on. Routing any of these into `valid_card` would
/// silently break the trigger — strict-fail instead.
fn unsafe_prop_name(p: &FilterProp) -> Option<&'static str> {
    match p {
        FilterProp::Tapped => Some("Tapped"),
        FilterProp::Untapped => Some("Untapped"),
        FilterProp::Attacking => Some("Attacking"),
        FilterProp::AttackingController => Some("AttackingController"),
        FilterProp::Blocking => Some("Blocking"),
        FilterProp::Unblocked => Some("Unblocked"),
        FilterProp::AttackedThisTurn => Some("AttackedThisTurn"),
        FilterProp::BlockedThisTurn => Some("BlockedThisTurn"),
        FilterProp::AttackedOrBlockedThisTurn => Some("AttackedOrBlockedThisTurn"),
        FilterProp::EnchantedBy => Some("EnchantedBy"),
        FilterProp::EquippedBy => Some("EquippedBy"),
        FilterProp::AttachedToSource => Some("AttachedToSource"),
        FilterProp::AttachedToRecipient => Some("AttachedToRecipient"),
        FilterProp::HasAttachment { .. } => Some("HasAttachment"),
        FilterProp::HasAnyAttachmentOf { .. } => Some("HasAnyAttachmentOf"),
        FilterProp::FaceDown => Some("FaceDown"),
        FilterProp::CountersGE { .. } => Some("CountersGE"),
        FilterProp::HasAnyCounter => Some("HasAnyCounter"),
        FilterProp::NameMatchesAnyPermanent { .. } => Some("NameMatchesAnyPermanent"),
        // Group 4 (filter.rs:1842-1869) — known conservative gaps the snapshot
        // evaluator returns false on.
        FilterProp::IsChosenColor => Some("IsChosenColor"),
        FilterProp::IsChosenCardType => Some("IsChosenCardType"),
        FilterProp::HasSingleTarget => Some("HasSingleTarget"),
        FilterProp::Suspected => Some("Suspected"),
        FilterProp::Modified => Some("Modified"),
        FilterProp::DifferentNameFrom { .. } => Some("DifferentNameFrom"),
        FilterProp::InAnyZone { .. } => Some("InAnyZone"),
        FilterProp::SharesQuality { .. } => Some("SharesQuality"),
        FilterProp::WasDealtDamageThisTurn => Some("WasDealtDamageThisTurn"),
        FilterProp::EnteredThisTurn => Some("EnteredThisTurn"),
        FilterProp::TargetsOnly { .. } => Some("TargetsOnly"),
        FilterProp::Targets { .. } => Some("Targets"),
        FilterProp::HasXInManaCost => Some("HasXInManaCost"),
        FilterProp::IsCommander => Some("IsCommander"),
        FilterProp::Other { .. } => Some("Other"),
        _ => None,
    }
}

/// Result of an ETB-aware trigger condition conversion: a `TriggerCondition`
/// to set on `td.condition`, plus an optional `TargetFilter` to merge into
/// `td.valid_card` (for snapshot-derivable predicates that have no
/// `TriggerCondition` analog).
pub struct TriggerCondExt {
    pub condition: Option<TriggerCondition>,
    pub valid_card: Option<TargetFilter>,
}

/// CR 603.4 + CR 603.6 + CR 603.10: Wraps `convert_trigger` with an ETB
/// fallback path. When the input is `Condition::EnteringPermanentPassesFilter`
/// (either at top level or as an `And`-conjunct alongside other supported
/// conditions), and the predicate has no source-bound `TriggerCondition`
/// analog, route the snapshot-derivable predicate into the trigger's
/// `valid_card` filter instead of strict-failing the whole rule. Live-state
/// predicates still strict-fail (they need `TriggerCondition::SourceIs<X>`
/// engine variants in a separate round).
pub fn convert_trigger_with_etb_filter(c: &Condition) -> ConvResult<TriggerCondExt> {
    match c {
        Condition::EnteringPermanentPassesFilter(pred) => {
            // Try the trigger-condition path first; fall through to valid_card
            // only on the catch-all gap (other gaps — e.g. malformed counter
            // comparators — propagate unchanged).
            match entering_permanent_filter_to_trigger(pred) {
                Ok(tc) => Ok(TriggerCondExt {
                    condition: Some(tc),
                    valid_card: None,
                }),
                Err(ConversionGap::MalformedIdiom {
                    idiom: "Condition::EnteringPermanentPassesFilter/predicate",
                    ..
                }) => {
                    let filter = entering_permanent_filter_to_valid_card(pred)?;
                    Ok(TriggerCondExt {
                        condition: None,
                        valid_card: Some(filter),
                    })
                }
                Err(e) => Err(e),
            }
        }
        // CR 608.2c: Conjunction — partition conjuncts that map to TriggerCondition
        // from those that route to valid_card. Any non-ETB conjunct that fails
        // its trigger conversion strict-fails the whole rule (mirrors the
        // existing convert_trigger behaviour).
        Condition::And(parts) => {
            let mut conditions: Vec<TriggerCondition> = Vec::new();
            let mut valid_cards: Vec<TargetFilter> = Vec::new();
            for part in parts {
                let ext = convert_trigger_with_etb_filter(part)?;
                if let Some(tc) = ext.condition {
                    conditions.push(tc);
                }
                if let Some(vc) = ext.valid_card {
                    valid_cards.push(vc);
                }
            }
            let condition = match conditions.len() {
                0 => None,
                1 => Some(conditions.pop().unwrap()),
                _ => Some(TriggerCondition::And { conditions }),
            };
            let valid_card = match valid_cards.len() {
                0 => None,
                1 => Some(valid_cards.pop().unwrap()),
                _ => Some(TargetFilter::And {
                    filters: valid_cards,
                }),
            };
            Ok(TriggerCondExt {
                condition,
                valid_card,
            })
        }
        // Other shapes don't have a valid_card escape hatch — the entering
        // permanent isn't necessarily the matched object inside an `Or`/`Not`
        // boolean composition. Fall through to the existing trigger-condition
        // path (which will strict-fail uniformly on any unmappable conjunct).
        _ => Ok(TriggerCondExt {
            condition: Some(convert_trigger(c)?),
            valid_card: None,
        }),
    }
}

/// Merge a converted ETB filter into a `TriggerDefinition.valid_card`,
/// composing with any pre-existing filter via `TargetFilter::And`. Mirrors
/// the merge semantics used by trigger builders across this crate.
pub fn merge_valid_card(existing: Option<TargetFilter>, extra: TargetFilter) -> TargetFilter {
    match existing {
        None => extra,
        Some(prev) => TargetFilter::And {
            filters: vec![prev, extra],
        },
    }
}

/// Map a `Permanents` predicate (the second arg of `PermanentPassesFilter`)
/// to a `StaticCondition` evaluated against the source object.
fn permanent_filter_to_static(p: &Permanents) -> ConvResult<StaticCondition> {
    use engine::types::counter::CounterMatch;
    Ok(match p {
        // CR 611.2b: "is tapped" / "is untapped".
        Permanents::IsTapped => StaticCondition::SourceIsTapped,
        Permanents::IsUntapped => StaticCondition::Not {
            condition: Box::new(StaticCondition::SourceIsTapped),
        },
        // CR 506.4: "is attacking" / "is blocking".
        Permanents::IsAttacking => StaticCondition::SourceIsAttacking,
        Permanents::IsBlocking => StaticCondition::SourceIsBlocking,
        // CR 122.1 + CR 711.2: "has a counter on it" (any type) /
        // "has a [type] counter on it".
        Permanents::HasACounterOfType(ct) => StaticCondition::HasCounters {
            counters: counter_match_for(ct),
            minimum: 1,
            maximum: None,
        },
        Permanents::HasACounter => StaticCondition::HasCounters {
            counters: CounterMatch::Any,
            minimum: 1,
            maximum: None,
        },
        // Type / subtype / supertype predicates: delegate to the existing
        // `filter::convert` helper to build a `TargetFilter`, then wrap as
        // `SourceMatchesFilter` against the source object.
        Permanents::IsCardtype(_)
        | Permanents::IsCreatureType(_)
        | Permanents::IsSupertype(_)
        | Permanents::IsNonCardtype(_) => {
            let filter = crate::convert::filter::convert(p)?;
            StaticCondition::SourceMatchesFilter { filter }
        }
        // Predicates we haven't mapped yet — surface as a gap so the report
        // pinpoints what to extend next.
        _ => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: permanents_variant_tag(p),
            });
        }
    })
}

fn counter_match_for(
    ct: &crate::schema::types::CounterType,
) -> engine::types::counter::CounterMatch {
    use engine::types::counter::CounterMatch;
    use engine::types::counter::CounterType as EngineCT;
    // PTCounter doesn't have a parameterized variant in the engine — fall
    // back to a Generic string match keyed on the +N/+M label.
    if let crate::schema::types::CounterType::PTCounter(p, t) = ct {
        return CounterMatch::OfType(EngineCT::Generic(format!("{p:+}/{t:+}")));
    }
    let name = format!("{ct:?}");
    let stripped = name.strip_suffix("Counter").unwrap_or(&name);
    match stripped {
        "PlusOnePlusOne" => CounterMatch::OfType(EngineCT::Plus1Plus1),
        "MinusOneMinusOne" => CounterMatch::OfType(EngineCT::Minus1Minus1),
        "Lore" => CounterMatch::OfType(EngineCT::Lore),
        "Loyalty" => CounterMatch::OfType(EngineCT::Loyalty),
        "Defense" => CounterMatch::OfType(EngineCT::Defense),
        "Stun" => CounterMatch::OfType(EngineCT::Stun),
        "Time" => CounterMatch::OfType(EngineCT::Time),
        other => CounterMatch::OfType(EngineCT::Generic(other.to_lowercase())),
    }
}

fn permanents_variant_tag(p: &Permanents) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| {
            v.get("_Permanents")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn variant_tag(c: &Condition) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| {
            v.get("_Condition")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn players_variant_tag(p: &Players) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.get("_Players").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn spells_variant_tag(s: &Spells) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.get("_Spells").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// Reject anything other than `Player::You` on a player-predicate. mtgish's
/// non-You axes (`DefendingPlayer`, `Trigger_ThatPlayer`, `Ref_TargetPlayer`,
/// `TheActivePlayer`, etc.) require engine variants that bind those scopes
/// explicitly — none exist in the controller-relative `TriggerCondition` /
/// `AbilityCondition` surfaces today, so they strict-fail with a precise
/// detail string for the report.
fn require_you_player(player: &Player, idiom: &'static str) -> ConvResult<()> {
    match player {
        Player::You => Ok(()),
        other => Err(ConversionGap::MalformedIdiom {
            idiom,
            path: String::new(),
            detail: format!("non-You player axis: {other:?}"),
        }),
    }
}

/// CR 109.4: Reject anything other than `Players::You` on an
/// `APlayerPassesFilter` player-set axis. Used by surfaces (currently
/// `ParsedCondition`) that have no `Opponent*`-prefixed analog and so can
/// only collapse the existential to the controller's singular form.
fn require_you_players(player_set: &Players, idiom: &'static str) -> ConvResult<()> {
    if matches!(
        player_set,
        Players::SinglePlayer(p) if matches!(&**p, Player::You)
    ) {
        return Ok(());
    }
    Err(ConversionGap::MalformedIdiom {
        idiom,
        path: String::new(),
        detail: format!("non-You Players axis: {player_set:?}"),
    })
}

/// Bind `ControllerRef::You` onto the converted filter. Mirrors the
/// post-processing step in `oracle_trigger::parse_control_none_filter`
/// for `TriggerCondition::ControlsType` / `ControlsNone` /
/// `ControlCount` / `ControllerControlsMatching` filters whose runtime
/// matchers do not separately enforce a controller equality check.
fn bind_filter_controller_you(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut tf) => {
            tf.controller = Some(ControllerRef::You);
            TargetFilter::Typed(tf)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(bind_filter_controller_you)
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(bind_filter_controller_you)
                .collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(bind_filter_controller_you(*filter)),
        },
        other => other,
    }
}

/// CR 109.4: Mirror of `bind_filter_controller_you` for opponent-axis
/// existential predicates ("an opponent controls a [type]"). Stamps
/// `ControllerRef::Opponent` onto every `Typed` leaf so the runtime
/// filter evaluator (`matches_target_filter`) restricts the iterated
/// battlefield to opponent-controlled objects.
fn bind_filter_controller_opponent(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut tf) => {
            tf.controller = Some(ControllerRef::Opponent);
            TargetFilter::Typed(tf)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(bind_filter_controller_opponent)
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(bind_filter_controller_opponent)
                .collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(bind_filter_controller_opponent(*filter)),
        },
        other => other,
    }
}

/// Convert `Comparison(GameNumber)` → `(Comparator, QuantityExpr)` for use
/// with `QuantityComparison` / `QuantityCheck`. The `OneOf` / `AnyNumber` /
/// chosen-quality / parity (`Even`/`Odd`/`Prime`) shapes have no engine
/// comparator counterpart and strict-fail.
fn comparison_to_pair(cmp: &Comparison) -> ConvResult<(Comparator, QuantityExpr)> {
    let qty = |g: &GameNumber| crate::convert::quantity::convert(g);
    Ok(match cmp {
        Comparison::GreaterThanOrEqualTo(g) => (Comparator::GE, qty(g)?),
        Comparison::GreaterThan(g) => (Comparator::GT, qty(g)?),
        Comparison::LessThanOrEqualTo(g) => (Comparator::LE, qty(g)?),
        Comparison::LessThan(g) => (Comparator::LT, qty(g)?),
        Comparison::EqualTo(g) => (Comparator::EQ, qty(g)?),
        Comparison::NotEqualTo(g) => (Comparator::NE, qty(g)?),
        other => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "Comparison/comparison_to_pair",
                path: String::new(),
                detail: format!("unsupported shape: {other:?}"),
            });
        }
    })
}

/// CR 603.4: When the comparator-shape is `GE(n)` and `n` is a literal
/// integer, return `Some(n as u32)` for use with `LifeTotalGE`-shaped
/// trigger variants that take a `minimum: u32`. Returns `None` for any
/// shape that can't be expressed as a single non-negative integer floor.
fn comparison_as_min_u32(cmp: &Comparison) -> Option<u32> {
    match cmp {
        Comparison::GreaterThanOrEqualTo(g) => {
            if let GameNumber::Integer(n) = &**g {
                u32::try_from(*n).ok()
            } else {
                None
            }
        }
        Comparison::GreaterThan(g) => {
            if let GameNumber::Integer(n) = &**g {
                u32::try_from(n + 1).ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// CR 603.4: Trigger-side `PlayerPassesFilter` dispatcher. One arm per
/// recognized leaf predicate; combinators recurse; everything else strict-
/// fails with the unrecognized variant tag so the report surfaces it.
pub fn convert_player_predicate_trigger(
    player: &Player,
    predicate: &Players,
) -> ConvResult<TriggerCondition> {
    Ok(match predicate {
        Players::And(parts) => TriggerCondition::And {
            conditions: parts
                .iter()
                .map(|p| convert_player_predicate_trigger(player, p))
                .collect::<ConvResult<_>>()?,
        },
        Players::Or(parts) => TriggerCondition::Or {
            conditions: parts
                .iter()
                .map(|p| convert_player_predicate_trigger(player, p))
                .collect::<ConvResult<_>>()?,
        },

        // CR 500: "if it's [player]'s turn".
        Players::IsTheirTurn => {
            require_you_player(player, "Players::IsTheirTurn (trigger)")?;
            TriggerCondition::DuringYourTurn
        }
        Players::IsNotTheirTurn => {
            require_you_player(player, "Players::IsNotTheirTurn (trigger)")?;
            TriggerCondition::Not {
                condition: Box::new(TriggerCondition::DuringYourTurn),
            }
        }

        // CR 614.1d: "if [player] controls a [permanent]".
        Players::ControlsA(perms) => {
            require_you_player(player, "Players::ControlsA (trigger)")?;
            TriggerCondition::ControlsType {
                filter: bind_filter_controller_you(crate::convert::filter::convert(perms)?),
            }
        }
        // CR 603.8: "if [player] controls no [permanent]".
        Players::ControlsNo(perms) => {
            require_you_player(player, "Players::ControlsNo (trigger)")?;
            TriggerCondition::ControlsNone {
                filter: bind_filter_controller_you(crate::convert::filter::convert(perms)?),
            }
        }
        // CR 603.4: "if [player] controls N or more [permanent]".
        Players::ControlsNum(cmp, perms) => {
            require_you_player(player, "Players::ControlsNum (trigger)")?;
            if let Some(minimum) = comparison_as_min_u32(cmp) {
                TriggerCondition::ControlCount {
                    minimum,
                    filter: bind_filter_controller_you(crate::convert::filter::convert(perms)?),
                }
            } else {
                // Non-GE comparators fall back to QuantityComparison over ObjectCount.
                let (comparator, rhs) = comparison_to_pair(cmp)?;
                TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: bind_filter_controller_you(crate::convert::filter::convert(
                                perms,
                            )?),
                        },
                    },
                    comparator,
                    rhs,
                }
            }
        }

        // CR 603.4 + CR 119.1: life total comparison.
        Players::LifeTotalIs(cmp) => {
            require_you_player(player, "Players::LifeTotalIs (trigger)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }
        // CR 603.4 + CR 402.1: hand size comparison.
        Players::NumCardsInHandIs(cmp) => {
            require_you_player(player, "Players::NumCardsInHandIs (trigger)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }
        // CR 603.4 + CR 404.1: graveyard size comparison. Only `Cards::AnyCard`
        // maps to GraveyardSize; type-filtered counts strict-fail until a
        // typed-zone-count converter exists.
        Players::NumCardsInGraveyardIs(cmp, cards) => {
            require_you_player(player, "Players::NumCardsInGraveyardIs (trigger)")?;
            require_any_card(cards, "Players::NumCardsInGraveyardIs (trigger)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::GraveyardSize,
                },
                comparator,
                rhs,
            }
        }
        // CR 603.4 + CR 401.1: library size comparison.
        Players::NumCardsInLibraryIs(cmp) => {
            require_you_player(player, "Players::NumCardsInLibraryIs (trigger)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Library,
                        card_types: Vec::new(),
                        scope: CountScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }
        // CR 603.4 + CR 604.3: distinct card types in graveyard comparison.
        Players::NumCardTypesInGraveyardIs(cmp) => {
            require_you_player(player, "Players::NumCardTypesInGraveyardIs (trigger)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::DistinctCardTypesInZone {
                        zone: ZoneRef::Graveyard,
                        scope: CountScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }

        // CR 508.1a: "if [player] attacked this turn".
        Players::AttackedThisTurn => {
            require_you_player(player, "Players::AttackedThisTurn (trigger)")?;
            TriggerCondition::AttackedThisTurn
        }
        // CR 603.4 + CR 117.1: "if [player] cast a [spell] this turn".
        Players::CastASpellThisTurn(spells) => {
            require_you_player(player, "Players::CastASpellThisTurn (trigger)")?;
            let filter = match &**spells {
                Spells::AnySpell => None,
                other => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "Players::CastASpellThisTurn (trigger)",
                        path: String::new(),
                        detail: format!("non-trivial spell filter: {other:?}"),
                    });
                }
            };
            TriggerCondition::CastSpellThisTurn { filter }
        }
        // CR 603.4 + CR 117.1: "if [player] hasn't cast a spell this turn" —
        // count == 0 via QuantityComparison over SpellsCastThisTurn.
        Players::HasntCastASpellThisTurn(spells) => {
            require_you_player(player, "Players::HasntCastASpellThisTurn (trigger)")?;
            let filter = match &**spells {
                Spells::AnySpell => None,
                other => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "Players::HasntCastASpellThisTurn (trigger)",
                        path: String::new(),
                        detail: format!("non-trivial spell filter: {other:?}"),
                    });
                }
            };
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn { filter },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            }
        }
        // CR 117.1 + CR 603.4: "if [player] has cast N or more spells this turn".
        Players::CastNumSpellsThisTurn(cmp, spells) => {
            require_you_player(player, "Players::CastNumSpellsThisTurn (trigger)")?;
            let filter = match &**spells {
                Spells::AnySpell => None,
                other => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "Players::CastNumSpellsThisTurn (trigger)",
                        path: String::new(),
                        detail: format!("non-trivial spell filter: {other:?}"),
                    });
                }
            };
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn { filter },
                },
                comparator,
                rhs,
            }
        }

        // CR 119.3: "if [player] gained life this turn" / N or more.
        Players::GainedLifeThisTurn => {
            require_you_player(player, "Players::GainedLifeThisTurn (trigger)")?;
            TriggerCondition::GainedLife { minimum: 1 }
        }
        Players::GainedLifeAmountThisTurn(cmp) => {
            require_you_player(player, "Players::GainedLifeAmountThisTurn (trigger)")?;
            if let Some(minimum) = comparison_as_min_u32(cmp) {
                TriggerCondition::GainedLife { minimum }
            } else {
                let (comparator, rhs) = comparison_to_pair(cmp)?;
                TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::LifeGainedThisTurn {
                            player: engine::types::ability::PlayerScope::Controller,
                        },
                    },
                    comparator,
                    rhs,
                }
            }
        }
        Players::LostLifeThisTurn => {
            require_you_player(player, "Players::LostLifeThisTurn (trigger)")?;
            TriggerCondition::LostLife
        }
        Players::LostLifeLastTurn => {
            require_you_player(player, "Players::LostLifeLastTurn (trigger)")?;
            TriggerCondition::LostLifeLastTurn
        }

        // Designation/state predicates with direct engine analogs.
        Players::IsTheMonarch => {
            require_you_player(player, "Players::IsTheMonarch (trigger)")?;
            TriggerCondition::IsMonarch
        }
        Players::HasTheCitysBlessing => {
            require_you_player(player, "Players::HasTheCitysBlessing (trigger)")?;
            TriggerCondition::HasCityBlessing
        }
        Players::CompletedADungeon => {
            require_you_player(player, "Players::CompletedADungeon (trigger)")?;
            TriggerCondition::CompletedDungeon { specific: None }
        }
        Players::Descended => {
            require_you_player(player, "Players::Descended (trigger)")?;
            TriggerCondition::Descended
        }
        Players::HasMaxSpeed => {
            require_you_player(player, "Players::HasMaxSpeed (trigger)")?;
            TriggerCondition::HasMaxSpeed
        }
        // CR 122.1: "if [player] put a counter on a permanent this turn".
        // The engine variant ignores the per-filter scope (counts any
        // counter-add on any permanent), which strictly subsumes the mtgish
        // shape — accept any inner filter rather than failing.
        Players::HasPutACounterOnAPermanentThisTurn(_) => {
            require_you_player(
                player,
                "Players::HasPutACounterOnAPermanentThisTurn (trigger)",
            )?;
            TriggerCondition::CounterAddedThisTurn
        }

        other => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: format!("PlayerPasses/{}", players_variant_tag(other)),
            });
        }
    })
}

/// CR 608.2c: Ability-side `PlayerPassesFilter` dispatcher. AbilityCondition
/// has a narrower surface than TriggerCondition — most numeric predicates
/// route through `QuantityCheck`, and presence checks route through
/// `ControllerControlsMatching`. Predicates without an AbilityCondition
/// counterpart strict-fail.
pub fn convert_player_predicate_ability(
    player: &Player,
    predicate: &Players,
) -> ConvResult<AbilityCondition> {
    Ok(match predicate {
        Players::And(parts) => AbilityCondition::And {
            conditions: parts
                .iter()
                .map(|p| convert_player_predicate_ability(player, p))
                .collect::<ConvResult<_>>()?,
        },

        Players::IsTheirTurn => {
            require_you_player(player, "Players::IsTheirTurn (ability)")?;
            AbilityCondition::IsYourTurn
        }
        Players::IsNotTheirTurn => {
            require_you_player(player, "Players::IsNotTheirTurn (ability)")?;
            AbilityCondition::Not {
                condition: Box::new(AbilityCondition::IsYourTurn),
            }
        }

        Players::ControlsA(perms) => {
            require_you_player(player, "Players::ControlsA (ability)")?;
            AbilityCondition::ControllerControlsMatching {
                filter: bind_filter_controller_you(crate::convert::filter::convert(perms)?),
            }
        }
        Players::ControlsNo(perms) => {
            require_you_player(player, "Players::ControlsNo (ability)")?;
            AbilityCondition::Not {
                condition: Box::new(AbilityCondition::ControllerControlsMatching {
                    filter: bind_filter_controller_you(crate::convert::filter::convert(perms)?),
                }),
            }
        }
        Players::ControlsNum(cmp, perms) => {
            require_you_player(player, "Players::ControlsNum (ability)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: bind_filter_controller_you(crate::convert::filter::convert(perms)?),
                    },
                },
                comparator,
                rhs,
            }
        }

        Players::LifeTotalIs(cmp) => {
            require_you_player(player, "Players::LifeTotalIs (ability)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }
        Players::NumCardsInHandIs(cmp) => {
            require_you_player(player, "Players::NumCardsInHandIs (ability)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }
        Players::NumCardsInGraveyardIs(cmp, cards) => {
            require_you_player(player, "Players::NumCardsInGraveyardIs (ability)")?;
            require_any_card(cards, "Players::NumCardsInGraveyardIs (ability)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::GraveyardSize,
                },
                comparator,
                rhs,
            }
        }
        Players::NumCardsInLibraryIs(cmp) => {
            require_you_player(player, "Players::NumCardsInLibraryIs (ability)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Library,
                        card_types: Vec::new(),
                        scope: CountScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }
        Players::NumCardTypesInGraveyardIs(cmp) => {
            require_you_player(player, "Players::NumCardTypesInGraveyardIs (ability)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::DistinctCardTypesInZone {
                        zone: ZoneRef::Graveyard,
                        scope: CountScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }
        Players::AttackedThisTurn => {
            require_you_player(player, "Players::AttackedThisTurn (ability)")?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::AttackedThisTurn,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        }
        Players::CastASpellThisTurn(spells) => {
            require_you_player(player, "Players::CastASpellThisTurn (ability)")?;
            let filter = match &**spells {
                Spells::AnySpell => None,
                other => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "Players::CastASpellThisTurn (ability)",
                        path: String::new(),
                        detail: format!("non-trivial spell filter: {other:?}"),
                    });
                }
            };
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn { filter },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        }
        Players::HasntCastASpellThisTurn(spells) => {
            require_you_player(player, "Players::HasntCastASpellThisTurn (ability)")?;
            let filter = match &**spells {
                Spells::AnySpell => None,
                other => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "Players::HasntCastASpellThisTurn (ability)",
                        path: String::new(),
                        detail: format!("non-trivial spell filter: {other:?}"),
                    });
                }
            };
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn { filter },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            }
        }
        Players::CastNumSpellsThisTurn(cmp, spells) => {
            require_you_player(player, "Players::CastNumSpellsThisTurn (ability)")?;
            let filter = match &**spells {
                Spells::AnySpell => None,
                other => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "Players::CastNumSpellsThisTurn (ability)",
                        path: String::new(),
                        detail: format!("non-trivial spell filter: {other:?}"),
                    });
                }
            };
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn { filter },
                },
                comparator,
                rhs,
            }
        }

        Players::GainedLifeThisTurn => {
            require_you_player(player, "Players::GainedLifeThisTurn (ability)")?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: engine::types::ability::PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        }
        Players::GainedLifeAmountThisTurn(cmp) => {
            require_you_player(player, "Players::GainedLifeAmountThisTurn (ability)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: engine::types::ability::PlayerScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }
        Players::LostLifeThisTurn => {
            require_you_player(player, "Players::LostLifeThisTurn (ability)")?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeLostThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        }

        // Direct AbilityCondition analogs.
        Players::IsTheMonarch => {
            require_you_player(player, "Players::IsTheMonarch (ability)")?;
            AbilityCondition::IsMonarch
        }
        Players::HasMaxSpeed => {
            require_you_player(player, "Players::HasMaxSpeed (ability)")?;
            AbilityCondition::HasMaxSpeed
        }

        // QuantityCheck-routed counters & dungeon predicates.
        Players::CompletedADungeon => {
            require_you_player(player, "Players::CompletedADungeon (ability)")?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::DungeonsCompleted,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        }
        Players::Descended => {
            require_you_player(player, "Players::Descended (ability)")?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::DescendedThisTurn,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        }
        Players::HasPutACounterOnAPermanentThisTurn(_) => {
            require_you_player(
                player,
                "Players::HasPutACounterOnAPermanentThisTurn (ability)",
            )?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CounterAddedThisTurn,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        }

        other => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: format!("PlayerPasses/{}", players_variant_tag(other)),
            });
        }
    })
}

/// CR 613: Static-side `PlayerPassesFilter` dispatcher. The narrow
/// `StaticCondition` surface routes most numeric predicates through
/// `QuantityComparison` and presence checks through `IsPresent { filter }`.
pub fn convert_player_predicate_static(
    player: &Player,
    predicate: &Players,
) -> ConvResult<StaticCondition> {
    Ok(match predicate {
        Players::And(parts) => StaticCondition::And {
            conditions: parts
                .iter()
                .map(|p| convert_player_predicate_static(player, p))
                .collect::<ConvResult<_>>()?,
        },
        Players::Or(parts) => StaticCondition::Or {
            conditions: parts
                .iter()
                .map(|p| convert_player_predicate_static(player, p))
                .collect::<ConvResult<_>>()?,
        },

        Players::IsTheirTurn => {
            require_you_player(player, "Players::IsTheirTurn (static)")?;
            StaticCondition::DuringYourTurn
        }
        Players::IsNotTheirTurn => {
            require_you_player(player, "Players::IsNotTheirTurn (static)")?;
            StaticCondition::Not {
                condition: Box::new(StaticCondition::DuringYourTurn),
            }
        }

        // CR 614.1d: "as long as you control a [permanent]" → IsPresent.
        Players::ControlsA(perms) => {
            require_you_player(player, "Players::ControlsA (static)")?;
            StaticCondition::IsPresent {
                filter: Some(bind_filter_controller_you(crate::convert::filter::convert(
                    perms,
                )?)),
            }
        }
        // CR 613: "as long as you control no [permanent]".
        Players::ControlsNo(perms) => {
            require_you_player(player, "Players::ControlsNo (static)")?;
            StaticCondition::Not {
                condition: Box::new(StaticCondition::IsPresent {
                    filter: Some(bind_filter_controller_you(crate::convert::filter::convert(
                        perms,
                    )?)),
                }),
            }
        }
        // CR 613: "as long as you control N [permanent]".
        Players::ControlsNum(cmp, perms) => {
            require_you_player(player, "Players::ControlsNum (static)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: bind_filter_controller_you(crate::convert::filter::convert(perms)?),
                    },
                },
                comparator,
                rhs,
            }
        }

        Players::LifeTotalIs(cmp) => {
            require_you_player(player, "Players::LifeTotalIs (static)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }
        Players::NumCardsInHandIs(cmp) => {
            require_you_player(player, "Players::NumCardsInHandIs (static)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }
        Players::NumCardsInGraveyardIs(cmp, cards) => {
            require_you_player(player, "Players::NumCardsInGraveyardIs (static)")?;
            require_any_card(cards, "Players::NumCardsInGraveyardIs (static)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::GraveyardSize,
                },
                comparator,
                rhs,
            }
        }
        Players::NumCardsInLibraryIs(cmp) => {
            require_you_player(player, "Players::NumCardsInLibraryIs (static)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Library,
                        card_types: Vec::new(),
                        scope: CountScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }
        Players::NumCardTypesInGraveyardIs(cmp) => {
            require_you_player(player, "Players::NumCardTypesInGraveyardIs (static)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::DistinctCardTypesInZone {
                        zone: ZoneRef::Graveyard,
                        scope: CountScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }

        // Direct StaticCondition analogs.
        Players::IsTheMonarch => {
            require_you_player(player, "Players::IsTheMonarch (static)")?;
            StaticCondition::IsMonarch
        }
        Players::HasTheCitysBlessing => {
            require_you_player(player, "Players::HasTheCitysBlessing (static)")?;
            StaticCondition::HasCityBlessing
        }
        Players::CompletedADungeon => {
            require_you_player(player, "Players::CompletedADungeon (static)")?;
            StaticCondition::CompletedADungeon
        }
        Players::HasMaxSpeed => {
            require_you_player(player, "Players::HasMaxSpeed (static)")?;
            StaticCondition::HasMaxSpeed
        }

        // QuantityComparison-routed counter / event predicates.
        Players::AttackedThisTurn => {
            require_you_player(player, "Players::AttackedThisTurn (static)")?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::AttackedThisTurn,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        }
        Players::CastASpellThisTurn(spells) => {
            require_you_player(player, "Players::CastASpellThisTurn (static)")?;
            let filter = match &**spells {
                Spells::AnySpell => None,
                other => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "Players::CastASpellThisTurn (static)",
                        path: String::new(),
                        detail: format!("non-trivial spell filter: {other:?}"),
                    });
                }
            };
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn { filter },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        }
        Players::HasntCastASpellThisTurn(spells) => {
            require_you_player(player, "Players::HasntCastASpellThisTurn (static)")?;
            let filter = match &**spells {
                Spells::AnySpell => None,
                other => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "Players::HasntCastASpellThisTurn (static)",
                        path: String::new(),
                        detail: format!("non-trivial spell filter: {other:?}"),
                    });
                }
            };
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn { filter },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            }
        }
        Players::CastNumSpellsThisTurn(cmp, spells) => {
            require_you_player(player, "Players::CastNumSpellsThisTurn (static)")?;
            let filter = match &**spells {
                Spells::AnySpell => None,
                other => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "Players::CastNumSpellsThisTurn (static)",
                        path: String::new(),
                        detail: format!("non-trivial spell filter: {other:?}"),
                    });
                }
            };
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn { filter },
                },
                comparator,
                rhs,
            }
        }

        Players::GainedLifeThisTurn => {
            require_you_player(player, "Players::GainedLifeThisTurn (static)")?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: engine::types::ability::PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        }
        Players::GainedLifeAmountThisTurn(cmp) => {
            require_you_player(player, "Players::GainedLifeAmountThisTurn (static)")?;
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: engine::types::ability::PlayerScope::Controller,
                    },
                },
                comparator,
                rhs,
            }
        }
        Players::LostLifeThisTurn => {
            require_you_player(player, "Players::LostLifeThisTurn (static)")?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeLostThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        }
        Players::Descended => {
            require_you_player(player, "Players::Descended (static)")?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::DescendedThisTurn,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        }
        Players::HasPutACounterOnAPermanentThisTurn(_) => {
            require_you_player(
                player,
                "Players::HasPutACounterOnAPermanentThisTurn (static)",
            )?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CounterAddedThisTurn,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        }

        other => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: format!("PlayerPasses/{}", players_variant_tag(other)),
            });
        }
    })
}

fn require_any_card(cards: &Cards, idiom: &'static str) -> ConvResult<()> {
    match cards {
        Cards::AnyCard => Ok(()),
        other => Err(ConversionGap::MalformedIdiom {
            idiom,
            path: String::new(),
            detail: format!("non-trivial Cards filter: {other:?}"),
        }),
    }
}

// ---------------------------------------------------------------------------
// APlayerPassesFilter — existential plural ("a/an [player set]") dispatchers
// ---------------------------------------------------------------------------
//
// CR 603.4 + CR 608.2c: `APlayerPassesFilter(player_set, predicate)` encodes
// the existential plural form: "if a [player set] [passes predicate]" — i.e.
// at least one player satisfying the player-set filter also satisfies the
// inner predicate. mtgish encodes the singular controller-bound form as the
// sibling `PlayerPassesFilter(Player::You, _)`; the plural form is used for
// "an opponent has 10 or less life", "a player has N cards in hand", etc.
//
// Engine condition surfaces are controller-relative and do not have a
// general "exists a player such that" combinator. Mappable subset:
//   - `Players::You`     → collapses to the singular dispatcher.
//   - `Players::Opponent` → predicate maps onto opponent-axis QuantityRefs
//     (`OpponentLifeTotal`, `OpponentHandSize`, `OpponentLifeLostThisTurn`,
//     `OpponentDiscardedCardThisTurn`) and opponent-controlled filter
//     primitives (`ControlsType`/`IsPresent` with `ControllerRef::Opponent`-
//     stamped filters). Numeric predicates are only safely expressible when
//     the comparator direction matches the aggregate the QuantityRef
//     reports (e.g. `OpponentLifeTotal` is a MAX, so `GE/GT` are correct
//     for "an opponent has ≥ N life" but `LE/LT` are NOT — strict-fail).
//   - everything else (`AnyPlayer`, `Each*`, `SinglePlayer`, etc.) has no
//     engine analog today and strict-fails.

/// CR 603.4 + CR 109.4: Trigger-side `APlayerPassesFilter` dispatcher.
fn convert_aplayer_predicate_trigger(
    player_set: &Players,
    predicate: &Players,
) -> ConvResult<TriggerCondition> {
    // Collapse `SinglePlayer(You)` to the singular controller-bound surface.
    if matches!(
        player_set,
        Players::SinglePlayer(p) if matches!(&**p, Player::You)
    ) {
        return convert_player_predicate_trigger(&Player::You, predicate);
    }
    match player_set {
        Players::Opponent => opponent_predicate_trigger(predicate),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "TriggerCondition",
            needed_variant: format!("APlayerPasses/player_set/{}", players_variant_tag(other)),
        }),
    }
}

/// CR 608.2c + CR 109.4: Ability-side `APlayerPassesFilter` dispatcher.
fn convert_aplayer_predicate_ability(
    player_set: &Players,
    predicate: &Players,
) -> ConvResult<AbilityCondition> {
    if matches!(
        player_set,
        Players::SinglePlayer(p) if matches!(&**p, Player::You)
    ) {
        return convert_player_predicate_ability(&Player::You, predicate);
    }
    match player_set {
        Players::Opponent => opponent_predicate_ability(predicate),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "AbilityCondition",
            needed_variant: format!("APlayerPasses/player_set/{}", players_variant_tag(other)),
        }),
    }
}

/// CR 613 + CR 109.4: Static-side `APlayerPassesFilter` dispatcher.
fn convert_aplayer_predicate_static(
    player_set: &Players,
    predicate: &Players,
) -> ConvResult<StaticCondition> {
    if matches!(
        player_set,
        Players::SinglePlayer(p) if matches!(&**p, Player::You)
    ) {
        return convert_player_predicate_static(&Player::You, predicate);
    }
    match player_set {
        Players::Opponent => opponent_predicate_static(predicate),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "StaticCondition",
            needed_variant: format!("APlayerPasses/player_set/{}", players_variant_tag(other)),
        }),
    }
}

/// CR 119: Comparator direction for opponent-aggregate refs that report a
/// MAX over opponents. `OpponentLifeTotal` and `OpponentHandSize` answer
/// `MAX(opp_value)`, so the existential "an opponent has ≥ N" is correct
/// (∃ opp: v ≥ N ↔ max ≥ N) but "an opponent has ≤ N" is NOT correct
/// (∃ opp: v ≤ N requires MIN, not MAX). Returns `Ok((cmp, rhs))` for the
/// safe directions and strict-fails otherwise.
fn opponent_aggregate_max_pair(
    cmp: &Comparison,
    idiom: &'static str,
) -> ConvResult<(Comparator, QuantityExpr)> {
    let (comparator, rhs) = comparison_to_pair(cmp)?;
    match comparator {
        Comparator::GT | Comparator::GE => Ok((comparator, rhs)),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "QuantityRef::OpponentMin",
            needed_variant: format!("{idiom}/comparator/{other:?}"),
        }),
    }
}

/// CR 603.4 + CR 119 + CR 402 + CR 614.1d: Trigger-side opponent-axis predicate map.
fn opponent_predicate_trigger(predicate: &Players) -> ConvResult<TriggerCondition> {
    Ok(match predicate {
        Players::Or(parts) => TriggerCondition::Or {
            conditions: parts
                .iter()
                .map(opponent_predicate_trigger)
                .collect::<ConvResult<_>>()?,
        },
        Players::And(parts) => TriggerCondition::And {
            conditions: parts
                .iter()
                .map(opponent_predicate_trigger)
                .collect::<ConvResult<_>>()?,
        },
        // CR 119: "if an opponent has ≥ N life" — MAX(opp_life) ≥ N ↔ ∃ opp: life ≥ N.
        Players::LifeTotalIs(cmp) => {
            let (comparator, rhs) =
                opponent_aggregate_max_pair(cmp, "Players::LifeTotalIs (trigger/opp)")?;
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Opponent {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator,
                rhs,
            }
        }
        // CR 402.1: "if an opponent has ≥ N cards in hand" — MAX(opp_hand) ≥ N.
        Players::NumCardsInHandIs(cmp) => {
            let (comparator, rhs) =
                opponent_aggregate_max_pair(cmp, "Players::NumCardsInHandIs (trigger/opp)")?;
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Opponent {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator,
                rhs,
            }
        }
        // CR 119.3: "if an opponent lost life this turn".
        Players::LostLifeThisTurn => TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Sum,
                    },
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        },
        // CR 701.9: "if an opponent discarded a card this turn".
        Players::DiscardedACardThisTurn => TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::OpponentDiscardedCardThisTurn,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        },
        // CR 614.1d: "if an opponent controls a [type]" — opponent-stamped
        // filter passed through `ControlsType` (battlefield iteration with
        // controller-equality check encoded in the filter).
        Players::ControlsA(perms) => TriggerCondition::ControlsType {
            filter: bind_filter_controller_opponent(crate::convert::filter::convert(perms)?),
        },
        // CR 603.8: "if no opponent controls a [type]".
        Players::ControlsNo(perms) => TriggerCondition::ControlsNone {
            filter: bind_filter_controller_opponent(crate::convert::filter::convert(perms)?),
        },
        // CR 614.1d: "if an opponent controls ≥ N [type]" — count opponent-
        // controlled matches against the comparator. Lossy `LE/LT` shapes
        // strict-fail because `ObjectCount` returns the total, not a per-
        // opponent floor.
        Players::ControlsNum(cmp, perms) => {
            let filter = bind_filter_controller_opponent(crate::convert::filter::convert(perms)?);
            if let Some(minimum) = comparison_as_min_u32(cmp) {
                TriggerCondition::ControlCount { minimum, filter }
            } else {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "TriggerCondition",
                    needed_variant: "APlayerPasses/Opponent/ControlsNum/non-GE".into(),
                });
            }
        }
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TriggerCondition",
                needed_variant: format!("APlayerPasses/Opponent/{}", players_variant_tag(other)),
            });
        }
    })
}

/// CR 608.2c + CR 119 + CR 402 + CR 614.1d: Ability-side opponent-axis predicate map.
fn opponent_predicate_ability(predicate: &Players) -> ConvResult<AbilityCondition> {
    Ok(match predicate {
        Players::Or(parts) => AbilityCondition::Or {
            conditions: parts
                .iter()
                .map(opponent_predicate_ability)
                .collect::<ConvResult<_>>()?,
        },
        Players::And(parts) => AbilityCondition::And {
            conditions: parts
                .iter()
                .map(opponent_predicate_ability)
                .collect::<ConvResult<_>>()?,
        },
        Players::LifeTotalIs(cmp) => {
            let (comparator, rhs) =
                opponent_aggregate_max_pair(cmp, "Players::LifeTotalIs (ability/opp)")?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Opponent {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator,
                rhs,
            }
        }
        Players::NumCardsInHandIs(cmp) => {
            let (comparator, rhs) =
                opponent_aggregate_max_pair(cmp, "Players::NumCardsInHandIs (ability/opp)")?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Opponent {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator,
                rhs,
            }
        }
        Players::LostLifeThisTurn => AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Sum,
                    },
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        },
        Players::DiscardedACardThisTurn => AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::OpponentDiscardedCardThisTurn,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        },
        // CR 614.1d: opponent-axis "controls a/no [type]" via `ControllerControlsMatching`.
        Players::ControlsA(perms) => AbilityCondition::ControllerControlsMatching {
            filter: bind_filter_controller_opponent(crate::convert::filter::convert(perms)?),
        },
        Players::ControlsNo(perms) => AbilityCondition::Not {
            condition: Box::new(AbilityCondition::ControllerControlsMatching {
                filter: bind_filter_controller_opponent(crate::convert::filter::convert(perms)?),
            }),
        },
        Players::ControlsNum(cmp, perms) => {
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: bind_filter_controller_opponent(crate::convert::filter::convert(
                            perms,
                        )?),
                    },
                },
                comparator,
                rhs,
            }
        }
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCondition",
                needed_variant: format!("APlayerPasses/Opponent/{}", players_variant_tag(other)),
            });
        }
    })
}

/// CR 613 + CR 119 + CR 402 + CR 614.1d: Static-side opponent-axis predicate map.
fn opponent_predicate_static(predicate: &Players) -> ConvResult<StaticCondition> {
    Ok(match predicate {
        Players::Or(parts) => StaticCondition::Or {
            conditions: parts
                .iter()
                .map(opponent_predicate_static)
                .collect::<ConvResult<_>>()?,
        },
        Players::And(parts) => StaticCondition::And {
            conditions: parts
                .iter()
                .map(opponent_predicate_static)
                .collect::<ConvResult<_>>()?,
        },
        Players::LifeTotalIs(cmp) => {
            let (comparator, rhs) =
                opponent_aggregate_max_pair(cmp, "Players::LifeTotalIs (static/opp)")?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Opponent {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator,
                rhs,
            }
        }
        Players::NumCardsInHandIs(cmp) => {
            let (comparator, rhs) =
                opponent_aggregate_max_pair(cmp, "Players::NumCardsInHandIs (static/opp)")?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Opponent {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator,
                rhs,
            }
        }
        Players::LostLifeThisTurn => StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Sum,
                    },
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        },
        Players::DiscardedACardThisTurn => StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::OpponentDiscardedCardThisTurn,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        },
        Players::ControlsA(perms) => StaticCondition::IsPresent {
            filter: Some(bind_filter_controller_opponent(
                crate::convert::filter::convert(perms)?,
            )),
        },
        Players::ControlsNo(perms) => StaticCondition::Not {
            condition: Box::new(StaticCondition::IsPresent {
                filter: Some(bind_filter_controller_opponent(
                    crate::convert::filter::convert(perms)?,
                )),
            }),
        },
        Players::ControlsNum(cmp, perms) => {
            let (comparator, rhs) = comparison_to_pair(cmp)?;
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: bind_filter_controller_opponent(crate::convert::filter::convert(
                            perms,
                        )?),
                    },
                },
                comparator,
                rhs,
            }
        }
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "StaticCondition",
                needed_variant: format!("APlayerPasses/Opponent/{}", players_variant_tag(other)),
            });
        }
    })
}

// ---------------------------------------------------------------------------
// mtgish::Condition → engine::ParsedCondition bridge
// ---------------------------------------------------------------------------

/// CR 601.2c + CR 602.5: Convert a `Condition` for use as a `ParsedCondition`,
/// the typed condition surface stored on `CastingRestriction::RequiresCondition`,
/// `ActivationRestriction::RequiresCondition`, and `SpellCastingOption.condition`.
///
/// Engine `Option<ParsedCondition>` evaluates `None` as **always-pass** via
/// `is_none_or` (`game/restrictions.rs:494`), so dropping a condition to `None`
/// converts "cast only if X" into "always cast" — the round-3 audit bug. The
/// strict-failure contract here is therefore the same as for the trigger /
/// ability / static surfaces: every variant we cannot translate strict-fails
/// the whole rule with `EnginePrerequisiteMissing { engine_type: "ParsedCondition" }`.
///
/// Mappable surface (mirroring the constructions used by the native parser
/// at `parser/oracle_condition.rs`):
/// - Source-axis permanent state (`is attacking`, `is blocked`)
/// - `PlayerPassesFilter(You, _)` for predicates with direct `You*`-prefixed
///   ParsedCondition variants
/// - `IsPlayersTurn` and other phase/timing forms strict-fail (no
///   ParsedCondition timing surface — those belong on `CastingRestriction::*`
///   sibling variants like `DuringYourTurn`)
/// - `And`/`Or`/`Not` strict-fail (no compound ParsedCondition variant)
pub fn convert_parsed(c: &Condition) -> ConvResult<ParsedCondition> {
    match c {
        // CR 603.6d / CR 608.2c: ETB-form "if it's attacking/blocked" — the
        // entering/source permanent IS the source object.
        Condition::EnteringPermanentPassesFilter(pred) => entering_permanent_filter_to_parsed(pred),
        // CR 611.2b + CR 506.4: source-axis permanent state predicates (the
        // Permanent argument aliases the source object). Non-source axes have
        // no source-bound ParsedCondition counterpart.
        Condition::PermanentPassesFilter(perm, pred) => {
            require_source_axis(perm, "Condition::PermanentPassesFilter (parsed)")?;
            entering_permanent_filter_to_parsed(pred)
        }
        // CR 608.2c: "if [player] [passes predicate]" — only `Player::You` axis
        // routes onto the You-prefixed ParsedCondition variants.
        Condition::PlayerPassesFilter(player, predicate) => {
            require_you_player(player, "Condition::PlayerPassesFilter (parsed)")?;
            convert_player_predicate_parsed(predicate)
        }
        // CR 608.2c: "if a [player set] [passes predicate]" — the existential
        // plural form on a parsed casting/activation gate. Only `Players::You`
        // collapses to the singular controller-bound surface; other player
        // sets (Opponent, AnyPlayer) have no `You*`-prefixed ParsedCondition
        // counterpart and strict-fail.
        Condition::APlayerPassesFilter(player_set, predicate) => {
            require_you_players(player_set, "Condition::APlayerPassesFilter (parsed)")?;
            convert_player_predicate_parsed(predicate)
        }
        // CR 601.3 + CR 602.5 + CR 608.2c: Compound parsed conditions —
        // `ParsedCondition::{And,Or,Not}` was added to the engine in
        // commit 60fae1aa4 to mirror the `AbilityCondition` /
        // `TriggerCondition` / `StaticCondition` combinators. Recurse on
        // `convert_parsed` so each inner condition uses the same dispatch
        // (only Parsed-mappable shapes survive — anything inside that
        // strict-fails propagates the gap upward, matching the rules-
        // correctness rule that a compound is only as expressive as its
        // weakest component).
        Condition::And(parts) => Ok(ParsedCondition::And {
            conditions: parts
                .iter()
                .map(convert_parsed)
                .collect::<ConvResult<_>>()?,
        }),
        Condition::Or(parts) => Ok(ParsedCondition::Or {
            conditions: parts
                .iter()
                .map(convert_parsed)
                .collect::<ConvResult<_>>()?,
        }),
        // mtgish `Condition` has no `Not` variant — negation is expressed at
        // the surrounding-action layer (`Action::Unless`, the `IsNot*`
        // condition variants). `ParsedCondition::Not` is reachable only via
        // those upstream paths, not from a direct `Condition::Not`.
        // No general-purpose timing or arbitrary-event form exists in
        // `ParsedCondition`. Strict-fail with the variant tag so the report
        // tracks remaining shapes by name.
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ParsedCondition",
            needed_variant: format!("Condition::{}", variant_tag(other)),
        }),
    }
}

/// CR 603.6d / CR 611.2b: Map a `Permanents` predicate (the inner of
/// `PermanentPassesFilter` or `EnteringPermanentPassesFilter` when the
/// permanent axis aliases the source object) onto a source-bound
/// ParsedCondition variant.
fn entering_permanent_filter_to_parsed(pred: &Permanents) -> ConvResult<ParsedCondition> {
    Ok(match pred {
        // CR 506.4: "is attacking".
        Permanents::IsAttacking => ParsedCondition::SourceIsAttacking,
        // CR 509.1h: "is blocked".
        Permanents::IsBlocked => ParsedCondition::SourceIsBlocked,
        // CR 205.2a: "is a creature".
        Permanents::IsCardtype(CardType::Creature) => ParsedCondition::SourceIsCreature,
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ParsedCondition",
                needed_variant: format!("PermanentPasses/{}", permanents_variant_tag(other)),
            });
        }
    })
}

/// CR 608.2c: `PlayerPassesFilter(You, predicate)` → ParsedCondition. Only
/// predicates with a direct `You*`-prefixed ParsedCondition variant are
/// mappable; the rest strict-fail. No compound (`And`/`Or`) variant exists
/// on ParsedCondition, so those propagate as a missing-prerequisite gap.
fn convert_player_predicate_parsed(predicate: &Players) -> ConvResult<ParsedCondition> {
    Ok(match predicate {
        // CR 508.1a: "if you attacked this turn".
        Players::AttackedThisTurn => ParsedCondition::YouAttackedThisTurn,
        // CR 119.3: "if you gained life this turn".
        Players::GainedLifeThisTurn => ParsedCondition::YouGainedLifeThisTurn,
        // CR 402.1: "if you have exactly N cards in hand". Only the EQ /
        // literal-integer shape maps to `HandSizeExact`; ranges have no
        // ParsedCondition counterpart.
        Players::NumCardsInHandIs(cmp) => match &**cmp {
            Comparison::EqualTo(g) => match &**g {
                GameNumber::Integer(n) => {
                    let count = usize::try_from(*n).map_err(|_| ConversionGap::MalformedIdiom {
                        idiom: "Players::NumCardsInHandIs (parsed)",
                        path: String::new(),
                        detail: format!("negative hand-size literal: {n}"),
                    })?;
                    ParsedCondition::HandSizeExact { count }
                }
                _ => {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "ParsedCondition",
                        needed_variant: "HandSize/non-literal".into(),
                    });
                }
            },
            _ => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "ParsedCondition",
                    needed_variant: "HandSize/non-EQ".into(),
                });
            }
        },
        // CR 614.1d: "if you control N or more [type/subtype] permanents".
        // GE-shaped numeric thresholds map onto the `*CountAtLeast` family;
        // other comparator shapes have no parsed-condition counterpart.
        Players::ControlsNum(cmp, perms) => controls_num_to_parsed(cmp, perms)?,
        // CR 614.1d: "if you control a [type/subtype]" — count-at-least 1
        // shape, dispatched on the inner predicate.
        Players::ControlsA(perms) => controls_count_at_least(perms, 1)?,
        // CR 603.8: "if you control no creatures" is the only shape with a
        // dedicated ParsedCondition variant; broader ControlsNo predicates
        // strict-fail because the parsed surface has no general "no X"
        // primitive.
        Players::ControlsNo(perms) => match &**perms {
            Permanents::IsCardtype(CardType::Creature) => ParsedCondition::YouControlNoCreatures,
            other => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "ParsedCondition",
                    needed_variant: format!("ControlsNo/{}", permanents_variant_tag(other)),
                });
            }
        },
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ParsedCondition",
                needed_variant: format!("PlayerPasses/{}", players_variant_tag(other)),
            });
        }
    })
}

/// CR 614.1d: "you control N (or more) [type/subtype] permanents" → one of
/// the `YouControl*CountAtLeast` ParsedCondition variants. Only `GE(literal)`
/// and `GreaterThan(literal)` shapes are mappable (the `*CountAtLeast`
/// family is a one-sided floor); other comparator shapes strict-fail.
fn controls_num_to_parsed(cmp: &Comparison, perms: &Permanents) -> ConvResult<ParsedCondition> {
    let Some(minimum) = comparison_as_min_u32(cmp) else {
        return Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ParsedCondition",
            needed_variant: "ControlsNum/non-GE".into(),
        });
    };
    let count = minimum as usize;
    controls_count_at_least(perms, count)
}

/// CR 614.1d: dispatch the inner `Permanents` predicate to the matching
/// `YouControl*CountAtLeast` variant. Only single-axis type/subtype filters
/// are mappable today.
fn controls_count_at_least(perms: &Permanents, count: usize) -> ConvResult<ParsedCondition> {
    Ok(match perms {
        Permanents::IsCardtype(ct) => ParsedCondition::YouControlCoreTypeCountAtLeast {
            core_type: card_type_to_core(ct)?,
            count,
        },
        Permanents::IsCreatureType(st) => ParsedCondition::YouControlSubtypeCountAtLeast {
            subtype: format!("{st:?}"),
            count,
        },
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ParsedCondition",
                needed_variant: format!("ControlsCount/{}", permanents_variant_tag(other)),
            });
        }
    })
}

/// CR 205.2a: Map mtgish `CardType` → engine `CoreType`. Variants without
/// a CoreType analog (Conspiracy, Phenomenon, Plane, Scheme, Vanguard) have
/// no place in a permanent-count ParsedCondition and strict-fail.
fn card_type_to_core(ct: &CardType) -> ConvResult<CoreType> {
    Ok(match ct {
        CardType::Artifact => CoreType::Artifact,
        CardType::Battle => CoreType::Battle,
        CardType::Creature => CoreType::Creature,
        CardType::Dungeon => CoreType::Dungeon,
        CardType::Enchantment => CoreType::Enchantment,
        CardType::Instant => CoreType::Instant,
        CardType::Kindred => CoreType::Kindred,
        CardType::Land => CoreType::Land,
        CardType::Planeswalker => CoreType::Planeswalker,
        CardType::Sorcery => CoreType::Sorcery,
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "CoreType",
                needed_variant: format!("{other:?}"),
            });
        }
    })
}
