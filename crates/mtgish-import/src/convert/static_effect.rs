//! mtgish `StaticLayerEffect` → engine `ContinuousModification` (Phase 10 narrow slice).
//!
//! Layer-7 effects (P/T modifications) and layer-6 effects (keyword grants)
//! are the simplest and most common static modifications. Other layers
//! (copy, type-add, color-add) land later as we map StaticLayerEffect's
//! 85 variants.

use engine::types::ability::{
    AbilityKind, ContinuousModification, Duration, PlayerScope, QuantityExpr, StaticCondition,
    StaticDefinition,
};
use engine::types::statics::StaticMode;

use crate::convert::keyword::try_convert as keyword_try_convert;
use crate::convert::result::{ConvResult, ConversionGap};
use crate::convert::{
    action, build_ability_from_actions, condition, cost, filter, quantity, trigger,
};
use crate::schema::types::{
    CardType, CheckHasable, Expiration, LayerEffect, ModX, Permanent, Player, Rule, SettableColor,
    SimpleColor, StaticLayerEffect, PT,
};

/// CR 613: Convert one `StaticLayerEffect` into one or more engine
/// `ContinuousModification`s. AdjustPT splits to AddPower + AddToughness;
/// most other variants map 1:1.
pub fn convert_layer_effect(e: &StaticLayerEffect) -> ConvResult<Vec<ContinuousModification>> {
    Ok(match e {
        // CR 613.4c: Layer 7c — +N/+M.
        StaticLayerEffect::AdjustPT(p, t) => {
            let mut mods = Vec::new();
            if *p != 0 {
                mods.push(ContinuousModification::AddPower { value: *p });
            }
            if *t != 0 {
                mods.push(ContinuousModification::AddToughness { value: *t });
            }
            mods
        }
        // CR 613.4c: Layer 7c dynamic — "+P/+T for each [quantity]". The
        // engine expresses "factor × dynamic count" as `QuantityExpr::Multiply`,
        // and skips the multiply layer when factor == 1 (the common case:
        // "+1/+1 for each Goblin you control"). A factor of 0 elides the mod
        // entirely (mirrors the static/fixed AdjustPT zero-coefficient skip).
        StaticLayerEffect::AdjustPTForEach(p, t, g) => {
            let qty = quantity::convert(g)?;
            adjust_pt_for_each_mods(*p, *t, qty)
        }
        // CR 613.4c: Layer 7c dynamic — "gets +X/+X" where each axis carries
        // an independent `ModX` selector against a shared `GameNumber`. The
        // selector picks the axis-specific shape: `PlusX` passes the dynamic
        // quantity through, `MinusX` negates it via `QuantityExpr::Multiply`
        // (factor -1, mirroring `oracle_static`'s native handling of
        // `gets -X/-X`), `Integer(n)` is a flat constant on that axis
        // (independent of the dynamic quantity), and `Zero` elides the axis
        // (matches the AdjustPT(0,*) / AdjustPTForEach(0,*) skip discipline).
        StaticLayerEffect::AdjustPTX(mod_p, mod_t, g) => {
            let qty = quantity::convert(g)?;
            adjust_ptx_mods(mod_p, mod_t, qty)
        }
        // CR 613.4b: Layer 7b — set base P and T to a fixed quantity.
        StaticLayerEffect::SetPower(g) => {
            let qty = quantity::convert(g)?;
            vec![power_set_mod(qty)]
        }
        StaticLayerEffect::SetToughness(g) => {
            let qty = quantity::convert(g)?;
            vec![toughness_set_mod(qty)]
        }
        StaticLayerEffect::SetPowerAndToughnessBoth(g) => {
            let qty = quantity::convert(g)?;
            vec![power_set_mod(qty.clone()), toughness_set_mod(qty)]
        }

        // CR 613.1f: Layer 6 — ability grants. mtgish encodes grants as a
        // Vec<Rule>; each Rule may be a keyword (→ AddKeyword), an
        // activated ability (→ GrantAbility), or a triggered ability
        // (→ GrantTrigger). Other Rule shapes strict-fail.
        StaticLayerEffect::AddAbility(rules) => {
            let mut mods = Vec::new();
            for rule in rules {
                mods.push(rule_to_grant_mod(rule, "StaticLayerEffect::AddAbility")?);
            }
            mods
        }

        // CR 205.3: Layer 4 — type/subtype additions.
        StaticLayerEffect::AddCardtype(ct) => vec![ContinuousModification::AddType {
            core_type: card_type_to_core(ct)?,
        }],
        StaticLayerEffect::AddCreatureType(s) => vec![ContinuousModification::AddSubtype {
            subtype: format!("{s:?}"),
        }],
        // CR 205.3 + CR 613.1c: Layer 4 — strip a core card type. The
        // Athreos/Mistform Ultimus / loseall-types pattern. Engine
        // `RemoveType { core_type }` mirrors the `AddType` shape.
        StaticLayerEffect::RemoveCardtype(ct) => vec![ContinuousModification::RemoveType {
            core_type: card_type_to_core(ct)?,
        }],

        // CR 613.1f: Layer 6 — keyword removal on the static layer-effect
        // path (vs the activated transient form). Mirrors
        // `LayerEffect::LosesAbility`.
        StaticLayerEffect::LosesAbility(checkhasable) => {
            check_hasable_to_remove_keyword(checkhasable)?
        }

        // CR 613.4b: Layer 7b — base P/T set via the typed `PT` payload.
        // Static-shape `PT::PT(p, t)` (Mishra's Factory, Goblin Mountaineer
        // animation lands etc.) collapses to fixed `SetPower`+`SetToughness`;
        // `ZeroPT` collapses to `0/0`; `ManualPT(GameNumber, GameNumber)` and
        // `PTX` go through `quantity::convert` to dynamic set mods. PT shapes
        // that read another card's P/T (graveyard / exile / permanent ref)
        // need engine plumbing the converter doesn't yet have and strict-fail.
        StaticLayerEffect::SetPT(pt) => match pt {
            PT::PT(p, t) => vec![
                ContinuousModification::SetPower { value: *p },
                ContinuousModification::SetToughness { value: *t },
            ],
            PT::ZeroPT => vec![
                ContinuousModification::SetPower { value: 0 },
                ContinuousModification::SetToughness { value: 0 },
            ],
            PT::ManualPT(p_g, t_g) => {
                let p_qty = quantity::convert(p_g)?;
                let t_qty = quantity::convert(t_g)?;
                vec![power_set_mod(p_qty), toughness_set_mod(t_qty)]
            }
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "StaticLayerEffect::SetPT",
                    path: String::new(),
                    detail: format!("unsupported PT shape: {other:?}"),
                });
            }
        },

        // CR 105.2 + CR 613.1d: Layer 5 — color additions / replacements on a
        // *static* layer effect (vs the activated transient form). Same lift
        // as `LayerEffect::AddColor` / `LayerEffect::SetColor`; routes
        // through the shared SettableColor helpers.
        StaticLayerEffect::AddColor(c) => settable_color_to_add_mods(c)?,
        StaticLayerEffect::SetColor(c) => settable_color_to_set_mod(c)?,

        // CR 110.2 + CR 613.2: Layer 2 — controller change. mtgish encodes
        // the new controller as a `Player`; engine `ChangeController` always
        // flips to the source's controller, which is the correct mapping
        // for `Player::You` (the dominant case — Take Possession, Mind
        // Control, Domineering Will, etc.). Non-You axes need a different
        // engine variant (e.g., flip to opponent / target player) and
        // strict-fail.
        StaticLayerEffect::SetController(player) => {
            require_player_is_you(player, "StaticLayerEffect::SetController")?;
            vec![ContinuousModification::ChangeController]
        }

        _ => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: variant_tag(e),
            });
        }
    })
}

/// CR 613.4c: Build the layer-7c modification list for "+P/+T for each
/// [dynamic quantity]". A `factor` of 0 elides that axis entirely; a `factor`
/// of 1 passes the dynamic quantity straight through; any other factor wraps
/// it in `QuantityExpr::Multiply`. Shared between static and transient
/// layer-effect dispatch.
fn adjust_pt_for_each_mods(p: i32, t: i32, qty: QuantityExpr) -> Vec<ContinuousModification> {
    let mut mods = Vec::new();
    if p != 0 {
        mods.push(ContinuousModification::AddDynamicPower {
            value: scale_quantity(p, qty.clone()),
        });
    }
    if t != 0 {
        mods.push(ContinuousModification::AddDynamicToughness {
            value: scale_quantity(t, qty),
        });
    }
    mods
}

/// CR 613.4c: Layer 7c per-axis modification list for "gets +X/+X" shapes
/// (`AdjustPTX`). Each axis carries its own `ModX`:
///   * `PlusX` → add the dynamic quantity (`AddDynamicPower/Toughness`).
///   * `MinusX` → subtract the dynamic quantity (`AddDynamic*` over
///     `QuantityExpr::Multiply { factor: -1, .. }`, matching the native
///     parser's "-X" handling in `oracle_static.rs`).
///   * `Integer(n)` → flat additive on that axis only (`AddPower/Toughness`),
///     independent of the dynamic quantity. `n == 0` elides.
///   * `Zero` → elide that axis (mirrors the AdjustPT zero-skip discipline).
fn adjust_ptx_mods(mod_p: &ModX, mod_t: &ModX, qty: QuantityExpr) -> Vec<ContinuousModification> {
    let mut mods = Vec::new();
    if let Some(m) = modx_to_power_mod(mod_p, qty.clone()) {
        mods.push(m);
    }
    if let Some(m) = modx_to_toughness_mod(mod_t, qty) {
        mods.push(m);
    }
    mods
}

fn modx_to_power_mod(m: &ModX, qty: QuantityExpr) -> Option<ContinuousModification> {
    match m {
        ModX::Zero => None,
        ModX::Integer(0) => None,
        ModX::Integer(n) => Some(ContinuousModification::AddPower { value: *n }),
        ModX::PlusX => Some(ContinuousModification::AddDynamicPower { value: qty }),
        ModX::MinusX => Some(ContinuousModification::AddDynamicPower {
            value: scale_quantity(-1, qty),
        }),
    }
}

fn modx_to_toughness_mod(m: &ModX, qty: QuantityExpr) -> Option<ContinuousModification> {
    match m {
        ModX::Zero => None,
        ModX::Integer(0) => None,
        ModX::Integer(n) => Some(ContinuousModification::AddToughness { value: *n }),
        ModX::PlusX => Some(ContinuousModification::AddDynamicToughness { value: qty }),
        ModX::MinusX => Some(ContinuousModification::AddDynamicToughness {
            value: scale_quantity(-1, qty),
        }),
    }
}

/// Multiply a `QuantityExpr` by a fixed integer factor, eliding the wrapper
/// when `factor == 1`. Used by `AdjustPTForEach` and any other "+N per X"
/// shapes that share the layer-7c dynamic primitive.
fn scale_quantity(factor: i32, qty: QuantityExpr) -> QuantityExpr {
    if factor == 1 {
        qty
    } else {
        QuantityExpr::Multiply {
            factor,
            inner: Box::new(qty),
        }
    }
}

fn power_set_mod(qty: QuantityExpr) -> ContinuousModification {
    match qty {
        QuantityExpr::Fixed { value } => ContinuousModification::SetPower { value },
        dynamic => ContinuousModification::SetPowerDynamic { value: dynamic },
    }
}

fn toughness_set_mod(qty: QuantityExpr) -> ContinuousModification {
    match qty {
        QuantityExpr::Fixed { value } => ContinuousModification::SetToughness { value },
        dynamic => ContinuousModification::SetToughnessDynamic { value: dynamic },
    }
}

fn card_type_to_core(
    ct: &crate::schema::types::CardType,
) -> ConvResult<engine::types::card_type::CoreType> {
    use crate::schema::types::CardType as C;
    use engine::types::card_type::CoreType;
    Ok(match ct {
        C::Artifact => CoreType::Artifact,
        C::Creature => CoreType::Creature,
        C::Enchantment => CoreType::Enchantment,
        C::Instant => CoreType::Instant,
        C::Land => CoreType::Land,
        C::Planeswalker => CoreType::Planeswalker,
        C::Sorcery => CoreType::Sorcery,
        C::Battle => CoreType::Battle,
        _ => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "StaticLayerEffect/card_type_to_core",
                path: String::new(),
                detail: format!("non-core CardType: {ct:?}"),
            });
        }
    })
}

/// CR 113.6: Convert one `PermanentRule` (rules-modifying static effect,
/// distinct from layer-7 P/T mods) into a freestanding `StaticDefinition`
/// using the engine's typed `StaticMode` variants.
pub fn convert_permanent_rule(
    rule: &crate::schema::types::PermanentRule,
    affected: engine::types::ability::TargetFilter,
) -> ConvResult<StaticDefinition> {
    use crate::schema::types::PermanentRule as P;
    let mode = match rule {
        // CR 509.1b: Attack/block bare prohibitions/requirements.
        P::CantAttack => StaticMode::CantAttack,
        P::CantBlock => StaticMode::CantBlock,
        P::CantAttackAlone => StaticMode::CantAttackAlone,
        P::CantBlockAlone => StaticMode::CantBlockAlone,
        P::MustAttack => StaticMode::MustAttack,
        P::MustBlock => StaticMode::MustBlock,
        P::MustBeBlocked => StaticMode::MustBeBlocked,

        // CR 502.3: Untap-step variants. `DoesntUntapDuringControllersUntap`
        // and `CantBecomeUntapped` both express "this permanent doesn't untap
        // during its controller's untap step" — the engine collapses both into
        // `StaticMode::CantUntap` (matches `oracle_static.rs`'s "doesn't untap
        // during your untap step" mapping at line 1509).
        P::CantBecomeUntapped => StaticMode::CantUntap,
        P::DoesntUntapDuringControllersUntap => StaticMode::CantUntap,
        // CR 502.3: "You may choose not to untap this permanent during your
        // untap step." Mirrors `oracle_static.rs`'s `MayChooseNotToUntap` at
        // line 474.
        P::MayChooseNotToUntapDuringUntap => StaticMode::MayChooseNotToUntap,

        // CR 509.1b: Bare "can't be blocked" → `StaticMode::CantBeBlocked`.
        P::CantBeBlocked => StaticMode::CantBeBlocked,
        // CR 702.111: "Can't be blocked by more than one creature" — the
        // engine collapses this into Menace (canonical reminder text).
        P::CantBeBlockedByMoreThanOne => StaticMode::Menace,
        // CR 509.1b: "Can't be blocked by [Permanents]" — the inverse-form
        // restriction. `StaticMode::CantBeBlockedBy { filter }` is the
        // documented inverse of `CantBeBlockedExceptBy` (statics.rs:484).
        P::CantBeBlockedByDefenders(p) => StaticMode::CantBeBlockedBy {
            filter: filter::convert(p)?,
        },

        // CR 602.5 + CR 603.2a: "[This permanent's] activated abilities can't
        // be activated." Mirrors the parser's self-ref form (oracle_static.rs
        // line 1255) — `who = AllPlayers, source_filter = <affected>`. The
        // mtgish encoding is bare (no payload), and the `affected` filter
        // already identifies the source permanent for the static, so the
        // source filter is the same set.
        P::AbilitiesCantBeActivated => StaticMode::CantBeActivated {
            who: engine::types::statics::ProhibitionScope::AllPlayers,
            source_filter: affected.clone(),
            exemption: engine::types::statics::ActivationExemption::None,
        },

        _ => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: serde_json::to_value(rule)
                    .ok()
                    .and_then(|v| {
                        v.get("_PermanentRule")
                            .and_then(|t| t.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_else(|| "<unknown>".into()),
            });
        }
    };
    Ok(StaticDefinition::new(mode).affected(affected))
}

/// Build a `StaticDefinition` for a `Rule::PermanentLayerEffect` /
/// `Rule::EachPermanentLayerEffect`. Caller sets `affected`; this function
/// lifts the layer-effect list into `modifications`.
pub fn build_static(
    affected: engine::types::ability::TargetFilter,
    effects: &[StaticLayerEffect],
) -> ConvResult<StaticDefinition> {
    let mut all_mods = Vec::new();
    for eff in effects {
        all_mods.extend(convert_layer_effect(eff)?);
    }
    if all_mods.is_empty() {
        return Err(ConversionGap::MalformedIdiom {
            idiom: "StaticLayerEffect/build_static",
            path: String::new(),
            detail: "empty modification list".into(),
        });
    }
    Ok(StaticDefinition::new(StaticMode::Continuous)
        .affected(affected)
        .modifications(all_mods))
}

/// CR 613 + CR 514.2: Convert one `LayerEffect` (the broader, transient
/// version of `StaticLayerEffect` used by `CreatePermanentLayerEffectUntil`)
/// into one or more `ContinuousModification`s. Covers the layer-7 P/T arms,
/// layer-6 keyword grants, and layer-4 type/subtype additions — the same
/// subset that `convert_layer_effect` covers for static abilities. Other
/// LayerEffect variants (copy effects, controller-set, type-replacement)
/// strict-fail until dedicated mappings land.
pub fn convert_layer_effect_dynamic(e: &LayerEffect) -> ConvResult<Vec<ContinuousModification>> {
    Ok(match e {
        // CR 613.4c: Layer 7c.
        LayerEffect::AdjustPT(p, t) => {
            let mut mods = Vec::new();
            if *p != 0 {
                mods.push(ContinuousModification::AddPower { value: *p });
            }
            if *t != 0 {
                mods.push(ContinuousModification::AddToughness { value: *t });
            }
            mods
        }
        // CR 613.4c: Layer 7c dynamic — "+P/+T for each [quantity]". Mirrors
        // the `StaticLayerEffect::AdjustPTForEach` arm; shared via
        // `adjust_pt_for_each_mods` so scaling stays consistent.
        LayerEffect::AdjustPTForEach(p, t, g) => {
            let qty = quantity::convert(g)?;
            adjust_pt_for_each_mods(*p, *t, qty)
        }
        // CR 613.4c: Layer 7c dynamic — "gets +X/+X" via per-axis `ModX`
        // selector. Mirrors `StaticLayerEffect::AdjustPTX`; shared helper so
        // sign / fixed / zero handling stays consistent across static and
        // transient (CreatePermanentLayerEffectUntil) layer dispatch.
        LayerEffect::AdjustPTX(mod_p, mod_t, g) => {
            let qty = quantity::convert(g)?;
            adjust_ptx_mods(mod_p, mod_t, qty)
        }
        // CR 613.4b: Layer 7b — set base P/T.
        LayerEffect::SetPower(g) => vec![power_set_mod(quantity::convert(g)?)],
        LayerEffect::SetToughness(g) => vec![toughness_set_mod(quantity::convert(g)?)],
        LayerEffect::SetPowerAndToughnessBoth(g) => {
            let qty = quantity::convert(g)?;
            vec![power_set_mod(qty.clone()), toughness_set_mod(qty)]
        }

        // CR 613.1f: Layer 6 — ability grants via Vec<Rule>. Same shape
        // family as `StaticLayerEffect::AddAbility`; delegated to a shared
        // helper so keyword / activated / triggered grants stay aligned.
        LayerEffect::AddAbility(rules) => {
            let mut mods = Vec::new();
            for rule in rules {
                mods.push(rule_to_grant_mod(rule, "LayerEffect::AddAbility")?);
            }
            mods
        }

        // CR 205.3: Layer 4 — type / subtype additions.
        LayerEffect::AddCardtype(ct) => vec![ContinuousModification::AddType {
            core_type: card_type_to_core(ct)?,
        }],
        LayerEffect::AddCreatureType(s) => vec![ContinuousModification::AddSubtype {
            subtype: format!("{s:?}"),
        }],
        // CR 205.3 + CR 613.1c: Layer 4 — strip a core type on the
        // activated transient layer-effect path. Mirrors the static
        // counterpart.
        LayerEffect::RemoveCardtype(ct) => vec![ContinuousModification::RemoveType {
            core_type: card_type_to_core(ct)?,
        }],

        // CR 105.2 + CR 613.1d: Layer 5 — color addition. Each color in the
        // `SimpleColorList` payload becomes its own `ContinuousModification::
        // AddColor` mod (the engine's color slot is per-color, mirroring the
        // creature-token-color overlay path). Other `SettableColor` variants
        // (AllColors, Colorless, Devoid, TheChosenColor*) require chosen-
        // attribute / set-color shapes that don't lower onto a per-color
        // additive primitive and strict-fail until a dedicated arm lands.
        LayerEffect::AddColor(color) => settable_color_to_add_mods(color)?,

        // CR 105.2 + CR 613.1d: Layer 5 — color *replacement* ("becomes
        // [color]" rather than "is also [color]"). Engine has a dedicated
        // `ContinuousModification::SetColor { colors: Vec<ManaColor> }`
        // primitive that overrides the source's color identity. Same
        // `SimpleColorList` lift as AddColor; chosen-attribute / ALL-colors
        // shapes strict-fail.
        LayerEffect::SetColor(color) => settable_color_to_set_mod(color)?,

        // CR 110.2 + CR 613.2: Layer 2 — controller change on the activated
        // transient layer-effect path. Same You-only restriction as the
        // static counterpart.
        LayerEffect::SetController(player) => {
            require_player_is_you(player, "LayerEffect::SetController")?;
            vec![ContinuousModification::ChangeController]
        }

        // CR 613.1f: Layer 6 — keyword removal ("loses Flying until end of
        // turn"). The dominant transient pattern. CheckHasable variants
        // carrying parameter payloads (Enchant(filter), Landwalk(filter),
        // ProtectionFromColor) need engine-side parameterized RemoveKeyword
        // emission and strict-fail.
        LayerEffect::LosesAbility(checkhasable) => check_hasable_to_remove_keyword(checkhasable)?,

        // CR 613.4b: Layer 7b — typed PT set, mirrors `StaticLayerEffect::SetPT`.
        LayerEffect::SetPT(pt) => match pt {
            PT::PT(p, t) => vec![
                ContinuousModification::SetPower { value: *p },
                ContinuousModification::SetToughness { value: *t },
            ],
            PT::ZeroPT => vec![
                ContinuousModification::SetPower { value: 0 },
                ContinuousModification::SetToughness { value: 0 },
            ],
            PT::ManualPT(p_g, t_g) => {
                let p_qty = quantity::convert(p_g)?;
                let t_qty = quantity::convert(t_g)?;
                vec![power_set_mod(p_qty), toughness_set_mod(t_qty)]
            }
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "LayerEffect::SetPT",
                    path: String::new(),
                    detail: format!("unsupported PT shape: {other:?}"),
                });
            }
        },

        _ => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: layer_effect_tag(e),
            });
        }
    })
}

/// CR 113.6 + CR 514.2: Build the per-rule `StaticDefinition`s for a
/// `CreatePermanentRuleEffectUntil` / `CreateEachPermanentRuleEffectUntil`
/// action. Each `PermanentRule` lifts through `convert_permanent_rule` so
/// the rules-modifying mode (CantAttack, CantBlock, MustAttack, …) sits
/// directly on the static. Caller wraps these in `Effect::GenericEffect`
/// with the matching `Duration`. Empty rule lists strict-fail (mirrors
/// `build_static`).
pub fn build_rule_effect_statics(
    affected: engine::types::ability::TargetFilter,
    rules: &[crate::schema::types::PermanentRule],
) -> ConvResult<Vec<StaticDefinition>> {
    let mut out = Vec::with_capacity(rules.len());
    for r in rules {
        out.push(convert_permanent_rule(r, affected.clone())?);
    }
    if out.is_empty() {
        return Err(ConversionGap::MalformedIdiom {
            idiom: "PermanentRule/build_rule_effect_statics",
            path: String::new(),
            detail: "empty rule list".into(),
        });
    }
    Ok(out)
}

/// CR 611.2a / CR 611.2b / CR 514.2: Map a mtgish `Expiration` to an engine
/// `Duration`. Common timing shapes (end of turn, end of combat, your next
/// turn, host leaves play, until-controller-next-untap, "for as long as ~ is
/// tapped") translate cleanly. Player-bound and permanent-bound shapes only
/// translate when the bound entity is the source/self — engine `Duration`
/// has no general "other player's next turn" or "another permanent leaves"
/// timer, so non-self references strict-fail.
pub fn expiration_to_duration(exp: &Expiration) -> ConvResult<Duration> {
    Ok(match exp {
        // CR 514.2: "until end of turn" effects end at cleanup.
        Expiration::UntilEndOfTurn => Duration::UntilEndOfTurn,
        // CR 514.2 (combat): "until end of combat" effects end when combat ends.
        Expiration::UntilEndOfCombat => Duration::UntilEndOfCombat,
        // CR 611.2a: A continuous effect with no stated duration lasts until
        // end of game. Engine has no EndOfGame variant — `Permanent` is the
        // closest expressible match (effect persists).
        Expiration::UntilEndOfGame => Duration::Permanent,
        // CR 502.1 + CR 514.2: "until the next turn" — Π-2 parameterized
        // `UntilNextTurnOf { player: Controller }` is the controller's next
        // turn timer.
        Expiration::UntilEndOfTheNextTurn => Duration::UntilNextTurnOf {
            player: PlayerScope::Controller,
        },
        // CR 502.1 + CR 514.2: "until your next turn" when scoped to the
        // effect's controller. Player-scoped variants only convert for
        // `SelfPlayer` because the engine resolves `PlayerScope::Controller`
        // against the grantee/controller.
        Expiration::UntilPlayersNextTurn(p)
        | Expiration::UntilTheEndOfPlayersNextTurn(p)
        | Expiration::UntilEndOfNextTurn(p)
            if is_self_player(p) =>
        {
            Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            }
        }
        // CR 502.1: "during your next untap step" — Π-2 parameterized
        // `UntilNextUntapStepOf { player: Controller }` ends at the affected
        // permanent's controller's next untap step, matching the SelfPlayer scope.
        Expiration::DuringPlayersNextUntapStep(p) if is_self_player(p) => {
            Duration::UntilNextUntapStepOf {
                player: PlayerScope::Controller,
            }
        }
        // CR 611.2a: "until ~ leaves the battlefield" — engine
        // `UntilHostLeavesPlay` expires the effect when its host (the source
        // permanent) leaves. Only convertible when the bound permanent is the
        // source itself; tracking an unrelated permanent has no engine analog.
        Expiration::UntilPermanentLeavesBattlefield(p) if is_self_permanent(p) => {
            Duration::UntilHostLeavesPlay
        }
        Expiration::UntilItLeavesTheBattlefield => Duration::UntilHostLeavesPlay,
        // CR 611.2b: "for as long as ~ remains tapped" — `ForAsLongAs` with
        // `SourceIsTapped` expresses the predicate-driven duration. Only
        // convertible for self-bound permanents (the only condition variant
        // that exists today is source-bound).
        Expiration::ForAsLongAsPermanentRemainsTapped(p) if is_self_permanent(p) => {
            Duration::ForAsLongAs {
                condition: StaticCondition::SourceIsTapped,
            }
        }
        other => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "Expiration/expiration_to_duration",
                path: String::new(),
                detail: format!("unsupported Expiration: {}", expiration_tag(other)),
            });
        }
    })
}

/// True when the `Player` reference resolves to the effect's own controller —
/// the only `PlayerScope` (`Controller`) the `Duration::UntilNextTurnOf` /
/// `UntilNextUntapStepOf` parameterizations bind today.
fn is_self_player(p: &Player) -> bool {
    matches!(p, Player::SelfPlayer | Player::ItsController)
}

/// True when the `Permanent` reference resolves to the source object — the
/// only permanent scope that source-bound engine durations
/// (`UntilHostLeavesPlay`, `ForAsLongAs { SourceIsTapped }`) can express.
fn is_self_permanent(p: &Permanent) -> bool {
    matches!(
        p,
        Permanent::ThisPermanent | Permanent::Self_It | Permanent::HostPermanent
    )
}

pub(crate) fn expiration_tag(e: &Expiration) -> String {
    serde_json::to_value(e)
        .ok()
        .and_then(|v| {
            v.get("_Expiration")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn layer_effect_tag(e: &LayerEffect) -> String {
    serde_json::to_value(e)
        .ok()
        .and_then(|v| {
            v.get("_LayerEffect")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

#[allow(dead_code)]
fn _card_type_marker(c: &CardType) -> &'static str {
    match c {
        CardType::Creature => "Creature",
        _ => "_",
    }
}

fn variant_tag(e: &StaticLayerEffect) -> String {
    serde_json::to_value(e)
        .ok()
        .and_then(|v| {
            v.get("_StaticLayerEffect")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn rule_tag(r: &Rule) -> String {
    serde_json::to_value(r)
        .ok()
        .and_then(|v| v.get("_Rule").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// CR 613.1f + CR 602.1 + CR 603.1: Convert a single `Rule` appearing
/// inside a layer-6 `AddAbility` payload into one `ContinuousModification`.
///
/// Three classes are recognized:
///   * **Keyword Rules** (Flying, Vigilance, Lifelink, …) → `AddKeyword`.
///   * **Activated abilities** (`Rule::Activated(cost, actions)`) →
///     `GrantAbility { definition }` — the granted creature gains the
///     activated ability per CR 602.1.
///   * **Triggered abilities** (`Rule::TriggerA(trigger, actions)` or
///     `Rule::TriggerI(trigger, condition, actions)`) →
///     `GrantTrigger { trigger }` — the granted creature gains the trigger
///     per CR 603.1, with `execute` populated and (for `TriggerI`) the
///     intervening-if condition (CR 603.4) attached.
///
/// All other Rule shapes (replacement effects, recursive layer effects,
/// player effects, …) strict-fail with `MalformedIdiom`. Closing those
/// requires their own engine plumbing (`GrantReplacement`, gated wrapper
/// statics) and is deferred.
fn rule_to_grant_mod(rule: &Rule, idiom: &'static str) -> ConvResult<ContinuousModification> {
    if let Some(kw) = keyword_try_convert(rule, idiom)? {
        return Ok(ContinuousModification::AddKeyword { keyword: kw });
    }
    match rule {
        // CR 602.1: Granted activated ability.
        Rule::Activated(cost_box, actions) => {
            let c = cost::convert(cost_box)?;
            let conv = action::convert_actions(actions)?;
            let definition = build_ability_from_actions(AbilityKind::Activated, Some(c), conv)?;
            Ok(ContinuousModification::GrantAbility {
                definition: Box::new(definition),
            })
        }
        // CR 603.1: Granted triggered ability.
        Rule::TriggerA(trig, actions) => {
            let mut td = trigger::convert(trig)?;
            let conv = action::convert_actions(actions)?;
            let body = build_ability_from_actions(AbilityKind::Spell, None, conv)?;
            td.execute = Some(Box::new(body));
            Ok(ContinuousModification::GrantTrigger {
                trigger: Box::new(td),
            })
        }
        // CR 603.4 + CR 603.6 + CR 603.10: Granted triggered ability with
        // intervening-if condition. ETB-aware so snapshot-derivable
        // `EnteringPermanentPassesFilter` predicates merge into `valid_card`.
        Rule::TriggerI(trig, cond, actions) => {
            let mut td = trigger::convert(trig)?;
            let ext = condition::convert_trigger_with_etb_filter(cond)?;
            td.condition = ext.condition;
            if let Some(vc) = ext.valid_card {
                td.valid_card = Some(condition::merge_valid_card(td.valid_card.take(), vc));
            }
            let conv = action::convert_actions(actions)?;
            let body = build_ability_from_actions(AbilityKind::Spell, None, conv)?;
            td.execute = Some(Box::new(body));
            Ok(ContinuousModification::GrantTrigger {
                trigger: Box::new(td),
            })
        }
        _ => Err(ConversionGap::MalformedIdiom {
            idiom,
            path: String::new(),
            detail: format!("ungrantable Rule in AddAbility: {}", rule_tag(rule)),
        }),
    }
}

/// CR 105.2 + CR 613.1d: Lower a `SettableColor` into a `Vec<ContinuousModification::
/// AddColor>`. Only the `SimpleColorList` payload maps cleanly today —
/// chosen-color and ALL-colors variants need engine-side chosen-attribute
/// integration / a dedicated `SetAllColors` modification and strict-fail.
fn settable_color_to_add_mods(color: &SettableColor) -> ConvResult<Vec<ContinuousModification>> {
    use engine::types::mana::ManaColor;
    match color {
        SettableColor::SimpleColorList(colors) => Ok(colors
            .iter()
            .map(|c| ContinuousModification::AddColor {
                color: match c {
                    SimpleColor::White => ManaColor::White,
                    SimpleColor::Blue => ManaColor::Blue,
                    SimpleColor::Black => ManaColor::Black,
                    SimpleColor::Red => ManaColor::Red,
                    SimpleColor::Green => ManaColor::Green,
                },
            })
            .collect()),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ContinuousModification",
            needed_variant: format!("AddColor with SettableColor::{other:?}"),
        }),
    }
}

/// CR 105.2 + CR 613.1d: Lower a `SettableColor` into a single
/// `ContinuousModification::SetColor { colors }`. Same restrictions as
/// `settable_color_to_add_mods`: only `SimpleColorList` maps cleanly;
/// chosen-attribute / Devoid / AllColors strict-fail until engine grows
/// dedicated arms.
fn settable_color_to_set_mod(color: &SettableColor) -> ConvResult<Vec<ContinuousModification>> {
    use engine::types::mana::ManaColor;
    match color {
        SettableColor::SimpleColorList(colors) => {
            let mapped: Vec<ManaColor> = colors
                .iter()
                .map(|c| match c {
                    SimpleColor::White => ManaColor::White,
                    SimpleColor::Blue => ManaColor::Blue,
                    SimpleColor::Black => ManaColor::Black,
                    SimpleColor::Red => ManaColor::Red,
                    SimpleColor::Green => ManaColor::Green,
                })
                .collect();
            Ok(vec![ContinuousModification::SetColor { colors: mapped }])
        }
        SettableColor::Colorless => Ok(vec![ContinuousModification::SetColor { colors: vec![] }]),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ContinuousModification",
            needed_variant: format!("SetColor with SettableColor::{other:?}"),
        }),
    }
}

/// CR 110.2 + CR 613.2: Reject a `SetController` payload whose new-controller
/// `Player` axis is anything other than `Player::You`. Engine
/// `ContinuousModification::ChangeController` always flips to the source's
/// controller, which only matches the You axis. Other axes (target player,
/// opponent, named) need different engine variants and aren't here yet.
fn require_player_is_you(player: &Player, idiom: &'static str) -> ConvResult<()> {
    match player {
        Player::You => Ok(()),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ContinuousModification",
            needed_variant: format!("{idiom} with non-You axis: Player::{other:?}"),
        }),
    }
}

/// CR 613.1f: Lower a `CheckHasable` (the keyword-loss subject) onto a
/// `Vec<ContinuousModification::RemoveKeyword>`. Only the unparameterized
/// vanilla keywords map cleanly — `Enchant(filter)`, `Landwalk(filter)`,
/// `ProtectionFromColor(color)` and the `Any*` (kicker / disturb / cycling)
/// classes need engine-side parameterized RemoveKeyword emission and
/// strict-fail. Abstract subjects (ActivatedAbility, NonManaAbility,
/// ThisAbility, etc.) are scoped to a different StaticMode shape and also
/// strict-fail.
fn check_hasable_to_remove_keyword(c: &CheckHasable) -> ConvResult<Vec<ContinuousModification>> {
    use engine::types::keywords::Keyword;
    let kw = match c {
        CheckHasable::Flying => Keyword::Flying,
        CheckHasable::FirstStrike => Keyword::FirstStrike,
        CheckHasable::DoubleStrike => Keyword::DoubleStrike,
        CheckHasable::Trample => Keyword::Trample,
        CheckHasable::Deathtouch => Keyword::Deathtouch,
        CheckHasable::Lifelink => Keyword::Lifelink,
        CheckHasable::Vigilance => Keyword::Vigilance,
        CheckHasable::Haste => Keyword::Haste,
        CheckHasable::Reach => Keyword::Reach,
        CheckHasable::Defender => Keyword::Defender,
        CheckHasable::Menace => Keyword::Menace,
        CheckHasable::Indestructible => Keyword::Indestructible,
        CheckHasable::Flash => Keyword::Flash,
        CheckHasable::Fear => Keyword::Fear,
        CheckHasable::Skulk => Keyword::Skulk,
        CheckHasable::Shadow => Keyword::Shadow,
        CheckHasable::Horsemanship => Keyword::Horsemanship,
        CheckHasable::Infect => Keyword::Infect,
        CheckHasable::Cascade => Keyword::Cascade,
        CheckHasable::Flanking => Keyword::Flanking,
        CheckHasable::Phasing => Keyword::Phasing,
        CheckHasable::Decayed => Keyword::Decayed,
        CheckHasable::Convoke => Keyword::Convoke,
        CheckHasable::Devoid => Keyword::Devoid,
        CheckHasable::Banding => Keyword::Banding,
        CheckHasable::Soulbond => Keyword::Soulbond,
        CheckHasable::Shroud => Keyword::Shroud,
        CheckHasable::StartYourEngines => Keyword::StartYourEngines,
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ContinuousModification::RemoveKeyword",
                needed_variant: format!("CheckHasable::{other:?}"),
            });
        }
    };
    Ok(vec![ContinuousModification::RemoveKeyword { keyword: kw }])
}
