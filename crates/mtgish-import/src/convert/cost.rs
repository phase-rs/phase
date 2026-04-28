//! Cost conversion: mtgish `Cost` → engine `AbilityCost` (Phase 7 narrow slice).
//!
//! mtgish has 257 Cost variants. The top ~10 (PayMana, TapPermanent,
//! SacrificeAPermanent, PayLife, DiscardACard, And, Or, ExileAPermanent)
//! cover the overwhelming majority of activated ability costs.

use engine::types::ability::{AbilityCost, QuantityExpr, TargetFilter};
use engine::types::ManaCost;
use engine::types::Zone;

use crate::convert::action::counter_type_name;
use crate::convert::filter::{
    cards_in_graveyard_to_filter, cards_to_filter, convert as convert_permanents, convert_permanent,
};
use crate::convert::mana;
use crate::convert::quantity;
use crate::convert::result::{ConvResult, ConversionGap};
use crate::schema::types::{CardInHand, Cost};

/// Return `Some(ManaCost)` when `cost` is `Cost::PayMana(...)`. Other
/// shapes return `Ok(None)` so callers can decide whether to fall
/// through to a gap or look for a different idiom.
pub fn as_pure_mana(cost: &Cost) -> ConvResult<Option<ManaCost>> {
    match cost {
        Cost::PayMana(symbols) => mana::convert(symbols).map(Some),
        _ => Ok(None),
    }
}

/// Full cost converter: any `Cost` → `AbilityCost`. Strict-failure on
/// unsupported variants.
pub fn convert(cost: &Cost) -> ConvResult<AbilityCost> {
    Ok(match cost {
        Cost::PayMana(symbols) => AbilityCost::Mana {
            cost: mana::convert(symbols)?,
        },
        Cost::TapPermanent(p) => match convert_permanent(p)? {
            // CR 602.5b: "{T}: ..." — most common: tapping the source.
            TargetFilter::SelfRef => AbilityCost::Tap,
            // Tapping a different specific permanent — fold into TapCreatures-shaped cost.
            other => AbilityCost::TapCreatures {
                count: 1,
                filter: other,
            },
        },
        Cost::TapAPermanent(filter) => AbilityCost::TapCreatures {
            count: 1,
            filter: convert_permanents(filter)?,
        },
        Cost::SacrificeAPermanent(filter) => AbilityCost::Sacrifice {
            target: convert_permanents(filter)?,
            count: 1,
        },
        Cost::SacrificePermanent(p) => AbilityCost::Sacrifice {
            target: convert_permanent(p)?,
            count: 1,
        },
        // CR 701.21: Sacrifice N permanents matching a filter. Engine
        // `Sacrifice.count` is `u32`; X-bound and other dynamic counts
        // strict-fail with a precise extension request rather than a
        // generic `MalformedIdiom`, since the converter is correct —
        // the engine slot is too narrow to express the cost.
        Cost::SacrificeNumberPermanents(n, filter) => AbilityCost::Sacrifice {
            target: convert_permanents(filter)?,
            count: fixed_count_or_engine_gap(
                n,
                "AbilityCost::Sacrifice",
                "count: QuantityExpr (X-bound / dynamic count)",
            )?,
        },
        Cost::PayLife(n) => AbilityCost::PayLife {
            amount: quantity::convert(n)?,
        },
        Cost::DiscardACard => AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            random: false,
            self_ref: false,
        },
        // CR 701.8 + CR 117.6: "Discard <specific card>" cost. Channel
        // (Boseiju, Throat Slitter) and similar self-discard activations use
        // `CardInHand::ThisCardInHand`, which maps to `AbilityCost::Discard`
        // with the `self_ref` flag — the engine resolver discards the source
        // card itself rather than prompting for a generic hand selection.
        // Other `CardInHand` refs (chosen-this-way / triggered-card / etc.)
        // need an engine slot beyond the current self-or-prompt dichotomy.
        Cost::DiscardCard(card) => match card {
            CardInHand::ThisCardInHand => AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                random: false,
                self_ref: true,
            },
            other => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "AbilityCost::Discard",
                    needed_variant: format!(
                        "discard a specific (context-bound) card from hand by reference: {other:?}"
                    ),
                });
            }
        },
        Cost::DiscardACardAtRandom => AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            random: true,
            self_ref: false,
        },
        Cost::DiscardNumberCards(n) => AbilityCost::Discard {
            count: quantity::convert(n)?,
            filter: None,
            random: false,
            self_ref: false,
        },
        // CR 701.9: Discard a card of a given card type.
        Cost::DiscardACardOfType(cards) => AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: Some(cards_to_filter(cards)?),
            random: false,
            self_ref: false,
        },
        // CR 701.9: Random discard of a fixed count.
        Cost::DiscardNumberCardsAtRandom(n) => AbilityCost::Discard {
            count: quantity::convert(n)?,
            filter: None,
            random: true,
            self_ref: false,
        },

        // CR 701.26 + CR 602.5b: Tap N permanents matching a filter.
        // Engine `TapCreatures.count` is `u32`; X-bound / dynamic counts
        // strict-fail with a precise extension request — the converter is
        // correct, the engine slot is too narrow.
        Cost::TapNumberPermanents(n, filter) => AbilityCost::TapCreatures {
            count: fixed_count_or_engine_gap(
                n,
                "AbilityCost::TapCreatures",
                "count: QuantityExpr (X-bound / dynamic count)",
            )?,
            filter: convert_permanents(filter)?,
        },

        // CR 122.1d: "Remove a {type} counter from ~" — single counter, source-anchored
        // when the target permanent is `ThisPermanent`; else the engine's typed target slot.
        Cost::RemoveACounterOfTypeFromPermanent(ct, target) => AbilityCost::RemoveCounter {
            count: 1,
            counter_type: counter_type_name(ct),
            target: counter_target(target)?,
        },
        // CR 122.1d: "Remove N {type} counters from ~" — fixed count form.
        // X-bound counts strict-fail with `EnginePrerequisiteMissing` since
        // `RemoveCounter.count` is `u32` (Chamber Sentry-class X activations).
        Cost::RemoveNumberCountersOfTypeFromPermanent(n, ct, target) => {
            AbilityCost::RemoveCounter {
                count: fixed_count_or_engine_gap(
                    n,
                    "AbilityCost::RemoveCounter",
                    "count: QuantityExpr (X-bound / dynamic count)",
                )?,
                counter_type: counter_type_name(ct),
                target: counter_target(target)?,
            }
        }
        // CR 122.1d: "Remove all {type} counters from ~". Engine uses `u32::MAX`
        // as the "all" sentinel (matches the native parser, see oracle_cost.rs).
        Cost::RemoveAllCountersOfTypeFromPermanent(ct, target) => AbilityCost::RemoveCounter {
            count: u32::MAX,
            counter_type: counter_type_name(ct),
            target: counter_target(target)?,
        },

        // CR 107.3a: Pay {X} mana for an activation cost. The mtgish encoding
        // duplicates X as a separate `GameNumber` arg — the engine's
        // `ManaCostShard::X` already carries the X-binding, so the second
        // argument is informational and ignored here. (The X value is
        // chosen at activation time per CR 107.3a.)
        Cost::PayManaX(symbols, _x_value) => AbilityCost::Mana {
            cost: mana::convert_x(symbols)?,
        },

        // CR 107.14: Pay {E} (energy counters). Engine `PayEnergy.amount`
        // is `u32`; X-bound payments strict-fail with a precise extension
        // request rather than a generic malformed-idiom.
        Cost::PayEnergy(n) => AbilityCost::PayEnergy {
            amount: fixed_count_or_engine_gap(
                n,
                "AbilityCost::PayEnergy",
                "amount: QuantityExpr (X-bound / dynamic count)",
            )?,
        },

        // CR 701.13: Exile a single named permanent. `Cost::ExilePermanent`
        // takes a singular `Permanent`; `Cost::ExileAPermanent` takes a
        // `Permanents` filter. Both flatten to `AbilityCost::Exile` —
        // `zone` defaults to None (battlefield context inferred from filter).
        Cost::ExilePermanent(p) => AbilityCost::Exile {
            count: 1,
            zone: None,
            filter: Some(convert_permanent(p)?),
        },
        Cost::ExileAPermanent(filter) => AbilityCost::Exile {
            count: 1,
            zone: None,
            filter: Some(convert_permanents(filter)?),
        },
        // CR 701.13: Exile a card from your hand (untyped — any card).
        Cost::ExileACardFromHand => AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Hand),
            filter: None,
        },
        // CR 701.13: Exile a card of a given type from hand.
        Cost::ExileACardOfTypeFromHand(cards) => AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Hand),
            filter: Some(cards_to_filter(cards)?),
        },
        // CR 701.13 + CR 404.1: Exile a card from a graveyard.
        Cost::ExileAGraveyardCard(cards) => AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Graveyard),
            filter: Some(cards_in_graveyard_to_filter(cards)?),
        },
        // CR 701.13: Generalized exile cost over a `Vec<Exilable>`. The
        // dominant pattern in this bucket is a single `Exilable::AGraveyardCard
        // (filter)` (Aphemia, Cabal Patriarch, etc.) — the same shape as
        // `Cost::ExileAGraveyardCard`. Other Exilable shapes (permanents,
        // hand, library) need separate arms; multi-element `Vec<Exilable>`
        // would require composite-cost lowering and strict-fails.
        Cost::Exile(items) => {
            use crate::schema::types::Exilable as E;
            match items.as_slice() {
                [E::AGraveyardCard(cards)] => AbilityCost::Exile {
                    count: 1,
                    zone: Some(Zone::Graveyard),
                    filter: Some(cards_in_graveyard_to_filter(cards)?),
                },
                [E::APermanent(filter)] => AbilityCost::Exile {
                    count: 1,
                    zone: Some(Zone::Battlefield),
                    filter: Some(convert_permanents(filter)?),
                },
                _ => {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "AbilityCost::Exile",
                        needed_variant: format!(
                            "Cost::Exile with {} Exilable element(s) — only single \
                             AGraveyardCard / APermanent shapes lift today",
                            items.len()
                        ),
                    });
                }
            }
        }
        // CR 701.13 + CR 404.1: Exile N graveyard cards. Engine `Exile.count`
        // is `u32`; X-bound counts strict-fail with `EnginePrerequisiteMissing`.
        Cost::ExileNumberGraveyardCards(n, cards) => AbilityCost::Exile {
            count: fixed_count_or_engine_gap(
                n,
                "AbilityCost::Exile",
                "count: QuantityExpr (X-bound / dynamic count)",
            )?,
            zone: Some(Zone::Graveyard),
            filter: Some(cards_in_graveyard_to_filter(cards)?),
        },
        // CR 701.13 + CR 401: Exile the top card of your library.
        Cost::ExileTopCardOfLibrary => AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Library),
            filter: None,
        },
        // CR 701.13: Exile a specific card chosen from hand. The engine's
        // `Exile` cost has no slot for a player-chosen specific card — the
        // `filter` field selects cards by predicate, not by reference. A
        // `CardInHand`-anchored cost needs a new `AbilityCost::ExileChosen`
        // variant or runtime card-binding plumbing.
        Cost::ExileCardFromHand(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCost::Exile",
                needed_variant: "exile a specific (player-chosen) card from hand by reference"
                    .into(),
            });
        }
        // CR 701.13: Exile a specific named graveyard card (by reference).
        // The dominant case is `CardInGraveyard::ThisGraveyardCard` — the
        // unearth/escape "exile this card from your graveyard" shape — which
        // lowers cleanly onto `AbilityCost::Exile { zone: Graveyard, filter:
        // SelfRef }` (the source's own card-in-graveyard ref). Other
        // graveyard-card refs (`Ref_TargetGraveyardCard*`,
        // `TheCardDiscardedThisWay`, etc.) need cost-time target refs the
        // engine doesn't have today and strict-fail.
        Cost::ExileGraveyardCard(gc) => {
            use crate::schema::types::CardInGraveyard as CG;
            match gc {
                CG::ThisGraveyardCard => AbilityCost::Exile {
                    count: 1,
                    zone: Some(Zone::Graveyard),
                    filter: Some(TargetFilter::SelfRef),
                },
                other => {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "AbilityCost::Exile",
                        needed_variant: format!(
                            "exile a specific (player-chosen) card from graveyard by reference: \
                             CardInGraveyard::{other:?}"
                        ),
                    });
                }
            }
        }

        Cost::And(parts) => AbilityCost::Composite {
            costs: parts.iter().map(convert).collect::<ConvResult<Vec<_>>>()?,
        },
        // CR 117.3 + CR 118: A choice-of-costs payment ("pay {2} or sacrifice
        // a creature"). The engine's `AbilityCost` has `Composite` (and-of-
        // costs) but no or-of-costs primitive — every existing cost slot is
        // unconditional. A new `AbilityCost::Choice { alternatives: Vec<…> }`
        // variant is required.
        Cost::Or(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCost",
                needed_variant: "Choice { alternatives: Vec<AbilityCost> } (or-of-costs)".into(),
            });
        }

        // CR 701.20: Reveal a card as a cost. Engine `AbilityCost::Reveal`
        // takes `count: u32` + optional `filter` — these mtgish shapes
        // map cleanly.
        Cost::RevealHand => AbilityCost::Reveal {
            count: u32::MAX,
            filter: None,
        },
        Cost::RevealACardOfTypeFromHand(cards) => AbilityCost::Reveal {
            count: 1,
            filter: Some(cards_to_filter(cards)?),
        },
        // CR 701.20: Reveal a specific (player-chosen) card from hand by
        // reference. Engine `Reveal` selects by predicate — no slot for
        // a card-reference identity.
        Cost::RevealCardFromHand(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCost::Reveal",
                needed_variant: "reveal a specific (player-chosen) card from hand by reference"
                    .into(),
            });
        }
        // CR 701.20: Reveal at random. Engine `Reveal` has no random flag.
        Cost::RevealACardFromHandAtRandom => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCost::Reveal",
                needed_variant: "random: bool (reveal a card at random)".into(),
            });
        }

        // CR 701.17: Mill as a cost. Engine `AbilityCost::Mill` takes
        // `count: u32` — fixed counts map directly; X-bound counts need
        // an engine extension.
        Cost::MillACard => AbilityCost::Mill { count: 1 },
        Cost::MillNumberCards(n) => AbilityCost::Mill {
            count: fixed_count_or_engine_gap(
                n,
                "AbilityCost::Mill",
                "count: QuantityExpr (X-bound / dynamic count)",
            )?,
        },

        // CR 701.21: "Sacrifice one or more X" — variable count chosen by
        // the player at activation time. Engine `Sacrifice.count` is a
        // single `u32`; representing a player-chosen count needs an
        // engine extension (e.g., `count: SacrificeCount::OneOrMore`).
        Cost::SacrificeOneOrMorePermanents(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCost::Sacrifice",
                needed_variant: "count: player-chosen one-or-more (variable)".into(),
            });
        }
        // CR 701.21: "Sacrifice N {group} permanents" — `GroupFilter` adds
        // a structural grouping (all-different, etc.) the engine doesn't
        // model on the cost path.
        Cost::SacrificeNumberGroupPermanents(_, _, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCost::Sacrifice",
                needed_variant: "GroupFilter (all-different / share-a-type) on sacrifice".into(),
            });
        }

        // CR 119.4: "Pay any amount of life" — variable life payment chosen
        // at activation time. Engine `PayLife.amount: QuantityExpr` has no
        // player-chosen-amount expression.
        Cost::PayAnyAmountOfLife => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCost::PayLife",
                needed_variant: "amount: player-chosen any-amount (variable)".into(),
            });
        }
        // CR 107.14: "Pay any amount of {E}" — same player-chosen-amount
        // gap as PayAnyAmountOfLife, on the energy slot.
        Cost::PayAnyAmountOfEnergy => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCost::PayEnergy",
                needed_variant: "amount: player-chosen any-amount (variable)".into(),
            });
        }
        // CR 107.14: "Pay one or more {E}". Player-chosen-amount with a
        // ≥1 floor — same engine gap as PayAnyAmountOfEnergy.
        Cost::PayOneOrMoreEnergy => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCost::PayEnergy",
                needed_variant: "amount: player-chosen one-or-more (variable)".into(),
            });
        }
        // CR 118: "Pay any amount of mana". Engine `Mana { cost: ManaCost }`
        // models a fixed mana shape — no any-amount slot.
        Cost::PayAnyAmountOfMana => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCost::Mana",
                needed_variant: "cost: player-chosen any-amount (variable)".into(),
            });
        }

        // CR 606.5: Planeswalker loyalty cost. The schema's signed `i32`
        // matches the engine's `amount: i32` directly (positive = add
        // counters, negative = remove counters, zero = no change).
        Cost::Loyalty(amount) => AbilityCost::Loyalty { amount: *amount },
        // CR 606.5 + CR 107.3b: "-X" loyalty cost. The engine's `Loyalty`
        // amount is `i32`; X resolves via the surrounding ability's X
        // binding at activation time. We emit a marker `i32::MIN` value to
        // strict-fail until the engine adds a `LoyaltyXVariable` variant —
        // this prevents silently materializing -0 (which would let X-cost
        // ultimates activate for free).
        Cost::LoyaltyMinusX => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCost::Loyalty",
                needed_variant: "LoyaltyMinusX (variable amount)".into(),
            });
        }
        _ => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: variant_tag(cost),
            });
        }
    })
}

fn variant_tag(c: &Cost) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| v.get("_Cost").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// Resolve a `GameNumber` cost-count argument to a fixed `u32`, or strict-fail
/// with `EnginePrerequisiteMissing` naming the engine slot that needs widening.
///
/// Most engine `AbilityCost::*` count slots are `u32` (TapCreatures, RemoveCounter,
/// Exile, Sacrifice, Mill, Reveal, PayEnergy), so X-bound or otherwise dynamic
/// counts cannot round-trip. Surfacing these as `EnginePrerequisiteMissing`
/// (rather than `MalformedIdiom`) correctly classifies them as engine-extension
/// requests in the gap report — the converter is doing its job; the engine type
/// is the limit.
fn fixed_count_or_engine_gap(
    n: &crate::schema::types::GameNumber,
    engine_type: &'static str,
    needed_variant: &str,
) -> ConvResult<u32> {
    let qty = quantity::convert(n)?;
    match qty {
        QuantityExpr::Fixed { value } if value >= 0 => Ok(value as u32),
        _ => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type,
            needed_variant: needed_variant.to_string(),
        }),
    }
}

/// Convert a counter-removal target. `ThisPermanent` collapses to
/// `target: None` (the cost is anchored on the ability's source). Anything
/// else uses the typed permanent filter.
fn counter_target(p: &crate::schema::types::Permanent) -> ConvResult<Option<TargetFilter>> {
    match convert_permanent(p)? {
        TargetFilter::SelfRef => Ok(None),
        other => Ok(Some(other)),
    }
}
