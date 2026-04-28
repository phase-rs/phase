//! mtgish `PlayerEffect` → engine `StaticDefinition` (narrow slice).
//!
//! Maps the most common static player flags to their engine `StaticMode`
//! analogue. `Rule::PlayerEffect(Player, Vec<PlayerEffect>)` and
//! `Rule::EachPlayerEffect(Players, Vec<PlayerEffect>)` both feed through
//! `apply` — the only difference is how the affected player set is
//! computed (`player_to_controller` vs `players_to_controller`). All
//! resulting statics are emitted with `affected = TypedFilter::default()
//! .controller(...)` so the engine resolves the player set at layer time.
//!
//! Strict-failure: any PlayerEffect we don't recognise propagates as
//! `UnknownVariant` so the report tracks the work queue.

use engine::types::ability::{
    CardPlayMode, ControllerRef, StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::statics::{
    CastFrequency, CastingProhibitionScope as ProhibitionScope, HandSizeModification, StaticMode,
};

use crate::convert::cost as cost_conv;
use crate::convert::filter::{player_to_controller, players_to_controller, spells_to_filter};
use crate::convert::mana as mana_conv;
use crate::convert::quantity as quantity_conv;
use crate::convert::result::{ConvResult, ConversionGap};
use crate::schema::types::{Cost, GameNumber, Player, PlayerEffect, Players, Spells};

/// CR 613.6 + CR 305.2 + CR 402.2: Apply a list of PlayerEffect entries
/// onto the face stub. Each recognised effect becomes a single
/// `StaticDefinition` whose `affected` filter resolves to the controller
/// reference. Unsupported effects strict-fail.
pub fn apply_for_player(
    p: &Player,
    effects: &[PlayerEffect],
    out: &mut Vec<StaticDefinition>,
) -> ConvResult<()> {
    let ctrl = player_to_controller(p)?;
    apply_with_controller(ctrl, effects, out)
}

/// Variant for `Rule::EachPlayerEffect(Players, ...)`.
pub fn apply_for_players(
    p: &Players,
    effects: &[PlayerEffect],
    out: &mut Vec<StaticDefinition>,
) -> ConvResult<()> {
    let ctrl = players_to_controller(p)?;
    apply_with_controller(ctrl, effects, out)
}

fn apply_with_controller(
    controller: ControllerRef,
    effects: &[PlayerEffect],
    out: &mut Vec<StaticDefinition>,
) -> ConvResult<()> {
    for eff in effects {
        out.push(player_effect_to_static(eff, &controller)?);
    }
    Ok(())
}

fn player_effect_to_static(
    eff: &PlayerEffect,
    controller: &ControllerRef,
) -> ConvResult<StaticDefinition> {
    // Default `affected` for player-axis statics — modes whose semantics
    // are "the player matched by `affected.controller` is the affected
    // player" (CantGainLife, NoMaximumHandSize, MayLookAtTopOfLibrary,
    // ...). Modes that carry their own player axis via a `who:
    // ProhibitionScope` field (CantBeCast, CantDraw, PerTurnCastLimit,
    // ...) build a different `affected` (typically the spell filter, or
    // None) and short-circuit via `return Ok(...)` below.
    let affected = TargetFilter::Typed(TypedFilter::default().controller(controller.clone()));
    let mode = match eff {
        // CR 401.4: "You may look at the top card of your library any time."
        PlayerEffect::MayLookAtTopCardOfLibraryAnyTime => StaticMode::MayLookAtTopOfLibrary,
        // CR 402.2: "You have no maximum hand size."
        PlayerEffect::HasNoMaximumHandSize => StaticMode::NoMaximumHandSize,
        // CR 305.2: "You may play an additional land on each of your turns."
        PlayerEffect::MayPlayAnAdditionalLand => StaticMode::MayPlayAdditionalLand,
        // CR 400.2: "Play with the top card of your library revealed."
        PlayerEffect::PlaysWithTopOfLibraryRevealed => {
            StaticMode::RevealTopOfLibrary { all_players: false }
        }
        // CR 119.6: "You can't gain life."
        PlayerEffect::CantGainLife => StaticMode::CantGainLife,
        // CR 502/503/504/506: skip-step / skip-phase modifiers.
        PlayerEffect::SkipsUntapStep => StaticMode::SkipStep { step: Phase::Untap },
        PlayerEffect::SkipsUpkeepStep => StaticMode::SkipStep {
            step: Phase::Upkeep,
        },
        PlayerEffect::SkipsDrawStep => StaticMode::SkipStep { step: Phase::Draw },
        // CR 104.3a/b: Platinum Angel — "[player] can't lose/win the game."
        PlayerEffect::DoesntLoseTheGameForHaving0OrLessLife => StaticMode::CantLoseTheGame,
        // CR 702.11: Hexproof — "[player] has hexproof". Player-scope
        // hexproof; permanent-scope grants flow through `AddKeyword` (Aegis
        // of the Gods, True Believer, Witchbane Orb…).
        PlayerEffect::Hexproof => StaticMode::Hexproof,
        // CR 702.18: Shroud — player-scope shroud (Sterling Grove, etc.).
        PlayerEffect::Shroud => StaticMode::Shroud,
        // CR 502.3 / CR 503 / CR 504: Skip-step modifiers for the
        // remaining beginning-phase steps. Combat / main / cleanup
        // skip variants need engine work (multi-step phases) and stay
        // unsupported below.
        // CR 601.2f: "Spells [player] casts of [type] cost {N} less to cast."
        // Maps directly to the engine's existing `ReduceCost` static. Player
        // scope rides on `affected` (set below); spell scope rides on
        // `spell_filter`.
        PlayerEffect::DecreaseSpellCost(spells, reduction) => StaticMode::ReduceCost {
            amount: mana_conv::convert_reduction(reduction)?,
            spell_filter: Some(spells_to_filter(spells)?),
            dynamic_count: None,
        },
        // CR 601.2f: "Spells [player] casts of [type] cost {N} more to cast."
        // `IncreaseSpellCost(Spells, Cost)` carries a generic `Cost`; only
        // pure-mana increases fit `RaiseCost.amount: ManaCost`.
        PlayerEffect::IncreaseSpellCost(spells, cost) => StaticMode::RaiseCost {
            amount: require_pure_mana_cost(cost, "PlayerEffect::IncreaseSpellCost")?,
            spell_filter: Some(spells_to_filter(spells)?),
            dynamic_count: None,
        },
        // CR 601.2f: "Spells [player] casts of [type] cost {N} less to cast for
        // each [thing]." Reuses `ReduceCost` with a non-None `dynamic_count`
        // — the engine multiplies the per-unit `amount` by the resolved
        // `QuantityRef` at cast time.
        PlayerEffect::DecreaseSpellCostForEach(spells, reduction, count) => {
            StaticMode::ReduceCost {
                amount: mana_conv::convert_reduction(reduction)?,
                spell_filter: Some(spells_to_filter(spells)?),
                dynamic_count: Some(quantity_ref_only(count)?),
            }
        }
        // CR 121.1: "[Player] can't draw cards." Player axis lives on the
        // mode's `who: ProhibitionScope`, NOT on `affected.controller` —
        // bail out of the player-axis-affected-filter pattern.
        PlayerEffect::CantDrawCards => {
            return Ok(StaticDefinition::new(StaticMode::CantDraw {
                who: controller_to_scope(controller),
            }));
        }
        // CR 104.3: "[Player] can't lose the game." (Platinum Angel side A)
        PlayerEffect::CantLoseTheGame => {
            return Ok(StaticDefinition::new(StaticMode::CantLoseTheGame).affected(affected));
        }
        // CR 104.2: "[Player] can't win the game." (Platinum Angel side B —
        // "your opponents can't win the game")
        PlayerEffect::CantWinTheGame => {
            return Ok(StaticDefinition::new(StaticMode::CantWinTheGame).affected(affected));
        }
        // CR 119.4: "[Player] can't lose life." Symmetric to CantGainLife.
        PlayerEffect::CantLoseLife => {
            return Ok(StaticDefinition::new(StaticMode::CantLoseLife).affected(affected));
        }
        // CR 402.2: "[Player]'s maximum hand size is N." Sets a hard cap.
        PlayerEffect::SetMaximumHandSize(n) => StaticMode::MaximumHandSize {
            modification: HandSizeModification::SetTo(non_neg_hand_size(n)?),
        },
        // CR 402.2: "[Player]'s maximum hand size is reduced by N."
        // Adjustment relative to the base hand size.
        PlayerEffect::ReduceMaximumHandSize(n) => StaticMode::MaximumHandSize {
            modification: HandSizeModification::AdjustedBy(-fixed_int(n)?),
        },
        // CR 402.2: "[Player]'s maximum hand size is increased by N."
        PlayerEffect::IncreaseMaximumHandSize(n) => StaticMode::MaximumHandSize {
            modification: HandSizeModification::AdjustedBy(fixed_int(n)?),
        },
        // CR 101.2 + CR 601.2: "[Player] can't cast [spells]." `who` is the
        // player axis (Controller/Opponents/AllPlayers); the spell filter
        // becomes `affected`.
        PlayerEffect::CantCastSpells(spells) => {
            return Ok(StaticDefinition::new(StaticMode::CantBeCast {
                who: controller_to_scope(controller),
            })
            .affected(spells_to_filter(spells)?));
        }
        // CR 101.2 + CR 601.2: "[Player] can't cast [spells] from [their]
        // graveyard." Same shape as CantCastSpells; the "from graveyard"
        // qualifier sits on the spell filter once spells_to_filter expands
        // it. Until then, this strict-fails on the spells_to_filter path —
        // but the variant dispatch lands here regardless.
        PlayerEffect::CantCastSpellsFromGraveyards(spells) => {
            return Ok(StaticDefinition::new(StaticMode::CantBeCast {
                who: controller_to_scope(controller),
            })
            .affected(spells_to_filter(spells)?));
        }
        // CR 101.2 + CR 604.1: "[Player] can't cast more than N [spells]
        // each turn." `PerTurnCastLimit` houses the spell filter inside the
        // mode (not on `affected`).
        PlayerEffect::CantCastMoreThanNumberSpellsEachTurn(count, spells) => {
            return Ok(StaticDefinition::new(StaticMode::PerTurnCastLimit {
                who: controller_to_scope(controller),
                max: u32_from_fixed(count)?,
                spell_filter: Some(spells_to_filter(spells)?),
            }));
        }
        // CR 702.8 + CR 702.51a: "[Spells controller casts of type] have
        // flash." Modeled as `CastWithKeyword(Flash)` whose `affected`
        // filter is the spell scope (controller + type filters merged).
        PlayerEffect::MayCastSpellsAsThoughTheyHadFlash(spells) => {
            return Ok(StaticDefinition::new(StaticMode::CastWithKeyword {
                keyword: Keyword::Flash,
            })
            .affected(spell_scope_for_caster(spells, controller)?));
        }
        // CR 305.1 + CR 604.2: "[Player] may play lands from [their]
        // graveyard." The "lands" axis lives in `play_mode = Play`; the
        // graveyard zone is implicit in `GraveyardCastPermission`. Cards
        // filter is currently parameterless (any land card in graveyard).
        // The schema's `Cards` argument restricts to lands by carrying a
        // type predicate — handled by the engine's runtime filter check on
        // top of GraveyardCastPermission.
        PlayerEffect::MayPlayLandsFromGraveyard(_cards) => {
            return Ok(StaticDefinition::new(StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Play,
            })
            .affected(affected));
        }
        other => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: variant_tag(other),
            });
        }
    };
    Ok(StaticDefinition::new(mode).affected(affected))
}

/// CR 601.2f: Bridge an mtgish `Cost` to a pure `ManaCost` for cost-
/// modifier statics. Non-mana increases (sacrifice, life, etc.) strict-
/// fail until the engine's `RaiseCost.amount` slot grows beyond `ManaCost`.
fn require_pure_mana_cost(cost: &Cost, idiom: &'static str) -> ConvResult<engine::types::ManaCost> {
    match cost_conv::as_pure_mana(cost)? {
        Some(mc) => Ok(mc),
        None => Err(ConversionGap::MalformedIdiom {
            idiom,
            path: String::new(),
            detail: "non-mana cost where pure mana is required".into(),
        }),
    }
}

/// CR 101.2: Map the player-axis `ControllerRef` (as resolved by
/// `player_to_controller` / `players_to_controller`) onto the
/// engine's `ProhibitionScope` enum used by `CantBeCast`,
/// `CantDraw`, `PerTurnCastLimit`, and friends.
///
/// `ControllerRef::TargetPlayer` arrives from `Players::AnyPlayer`
/// ("each player can't ...") which the rules treat as an
/// all-players prohibition.
fn controller_to_scope(c: &ControllerRef) -> ProhibitionScope {
    match c {
        ControllerRef::You => ProhibitionScope::Controller,
        ControllerRef::Opponent => ProhibitionScope::Opponents,
        ControllerRef::TargetPlayer => ProhibitionScope::AllPlayers,
    }
}

/// CR 601.2f + CR 702.51a: Build a `CastWithKeyword` /
/// keyword-grant spell scope filter — the engine's runtime treats
/// `affected` as the spell scope, with `affected.controller`
/// identifying the caster. This folds the `controller` (from
/// PlayerEffect dispatch) into the `Spells` filter so the engine
/// matches spells cast by the right player.
fn spell_scope_for_caster(spells: &Spells, controller: &ControllerRef) -> ConvResult<TargetFilter> {
    let base = spells_to_filter(spells)?;
    Ok(merge_controller(base, controller.clone()))
}

/// Fold a `ControllerRef` into a `TargetFilter`. For `Typed`
/// filters the controller field is set on the inner `TypedFilter`
/// without disturbing existing type/property constraints. Other
/// shapes (And/Or/Not/SelfRef/...) are wrapped via And so the
/// existing filter still applies and the controller axis is
/// added as a sibling typed constraint.
fn merge_controller(filter: TargetFilter, ctrl: ControllerRef) -> TargetFilter {
    match filter {
        TargetFilter::Typed(tf) => TargetFilter::Typed(tf.controller(ctrl)),
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter::default().controller(ctrl)),
            ],
        },
    }
}

/// CR 107.1a: Pull a fixed non-negative integer out of a
/// `GameNumber`. Used for hand-size and per-turn caps which are
/// expressed as concrete bounds in printed text — non-fixed
/// quantities (X, dynamic refs) strict-fail because they are not
/// expressible in `HandSizeModification::SetTo(u32)` /
/// `PerTurnCastLimit.max: u32`.
fn fixed_int(g: &GameNumber) -> ConvResult<i32> {
    match g {
        GameNumber::Integer(n) => Ok(*n),
        other => Err(ConversionGap::MalformedIdiom {
            idiom: "PlayerEffect/fixed_int",
            path: String::new(),
            detail: format!("expected fixed integer, got {other:?}"),
        }),
    }
}

fn non_neg_hand_size(g: &GameNumber) -> ConvResult<u32> {
    let n = fixed_int(g)?;
    u32::try_from(n).map_err(|_| ConversionGap::MalformedIdiom {
        idiom: "PlayerEffect/non_neg_hand_size",
        path: String::new(),
        detail: format!("negative hand size literal: {n}"),
    })
}

fn u32_from_fixed(g: &GameNumber) -> ConvResult<u32> {
    let n = fixed_int(g)?;
    u32::try_from(n).map_err(|_| ConversionGap::MalformedIdiom {
        idiom: "PlayerEffect/u32_from_fixed",
        path: String::new(),
        detail: format!("negative cap literal: {n}"),
    })
}

/// CR 107.3 + CR 601.2f: Resolve a `GameNumber` to a `QuantityRef`
/// (not a `QuantityExpr`) for slots that only accept the ref
/// shape — `StaticMode::ReduceCost.dynamic_count` is one such
/// slot. Wraps `quantity_conv::convert` and unwraps the inner
/// `QuantityRef`; non-Ref shapes (Fixed/Offset/Multiply/...)
/// strict-fail with `EnginePrerequisiteMissing` so the report
/// surfaces the gap if a card needs an arithmetic-bearing
/// dynamic count.
fn quantity_ref_only(g: &GameNumber) -> ConvResult<engine::types::ability::QuantityRef> {
    use engine::types::ability::QuantityExpr;
    match quantity_conv::convert(g)? {
        QuantityExpr::Ref { qty } => Ok(qty),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "QuantityRef",
            needed_variant: format!("dynamic_count requires Ref, got {other:?}"),
        }),
    }
}

fn variant_tag(eff: &PlayerEffect) -> String {
    serde_json::to_value(eff)
        .ok()
        .and_then(|v| {
            v.get("_PlayerEffect")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}
