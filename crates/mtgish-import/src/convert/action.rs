//! mtgish `Action` → engine `Effect` (Phase 6 narrow slice).
//!
//! mtgish has 1,411 Action variants; the top 30 cover ~64% of occurrences. This
//! module currently handles the simplest single-effect actions — Draw, GainLife,
//! LoseLife, DealDamage, Destroy, Tap/Untap, Discard, Mill, Scry, Surveil. The
//! long tail (token creation, replacement effects, modal/distributed effects)
//! lands phase by phase.

use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ChoiceType,
    ContinuousModification, ControllerRef, DelayedTriggerCondition, Duration, Effect,
    GainLifePlayer, LibraryPosition, ManaSpendRestriction, ModalSelectionConstraint, PlayerFilter,
    PlayerScope, QuantityExpr, SearchSelectionConstraint, StaticDefinition, TargetFilter,
    TriggerDefinition,
};
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;
use engine::types::Phase;

use crate::convert::condition;
use crate::convert::filter::{
    self as filter_mod, convert as convert_permanents, convert_permanent,
};
use crate::convert::mana;
use crate::convert::quantity;
use crate::convert::result::{ConvResult, ConversionGap};
use crate::convert::static_effect;
use crate::convert::token;
use crate::convert::trigger as trigger_mod;
use crate::schema::types::{
    Action, Actions, CardInExile, CardInGraveyard, CardType, CardsInHand, CounterType,
    DamageRecipient, FutureTrigger, GameNumber, ManaUseModifier, Permanent, Player, Players,
    ReplacementActionWouldEnter, Rule, SearchLibraryAction, Spells, TokenFlag,
};

/// Modal-choice arity for `ActionsConversion::Modal`. Mirrors the engine's
/// `ModalChoice { min_choices, max_choices, allow_repeat_modes }` shape, but
/// raised to the converter layer as a typed enum (not a bool-bag) so callers
/// can pattern-match the exact spell shape.
///
/// CR 700.2 / CR 700.2d: Modal spells / abilities — "choose one [of —]".
#[derive(Debug, Clone)]
pub enum ChooseSpec {
    /// "Choose one —" (min=max=1).
    One,
    /// "Choose up to N —" (min=0, max=n).
    UpToN { n: usize },
    /// "Choose N —" (min=max=n).
    Exactly { n: usize },
    /// "Choose one or more —" (min=1, max=mode_count; caller fills max from
    /// `modes.len()`). Charm-family pattern.
    OneOrMore,
    /// CR 700.2: "Choose any number —" (min=0, max=mode_count). Distinct from
    /// `OneOrMore` because zero choices is legal.
    AnyNumber,
}

/// CR 117.5 + CR 117.6 + CR 605.1c: Optionality marker for a chain segment.
/// `Mandatory` is the default; `Optional` corresponds to a mid-list
/// `Action::MayAction` ("you may [do X]"); `OptionalWithCost` corresponds to
/// the canonical mid-list speculative-payment idiom
/// `[Action::MayCost(cost), Action::If(CostWasPaid, body)]` (collapsed into
/// one segment that owns both the cost and its gated payload). Mirrors the
/// `Optional` / `OptionalWithCost` arms on `ActionsConversion` at the
/// sole-action position.
#[derive(Debug, Clone)]
pub enum SegmentOptional {
    Mandatory,
    Optional,
    OptionalWithCost(AbilityCost),
}

/// One link in a linear effect chain. Most segments are unconditional
/// (`condition: None`, `optional: Mandatory`); a segment with
/// `condition: Some(_)` corresponds to a mid-list `Action::If` /
/// `Action::Unless` body, a segment with `else_effects: Some(_)` corresponds
/// to a mid-list `Action::IfElse`, and a segment with
/// `optional: Optional` / `OptionalWithCost(_)` corresponds to a mid-list
/// `Action::MayAction` / `[MayCost, If(CostWasPaid, body)]` pair.
///
/// Each `ChainSegment` materializes as one or more `AbilityDefinition` links
/// in the `sub_ability` chain — the head AD of the segment carries
/// `condition` (and `else_ability` for `IfElse`, plus `optional` /
/// per-segment `cost` for the optional forms), and any tail effects within
/// the same segment chain unconditionally beneath it.
///
/// CR 700.4 + CR 608.2c: Conditional resolution within an instruction list.
/// CR 117.5 + CR 117.6 + CR 605.1c: Optional / optional-with-cost mid-list.
#[derive(Debug, Clone)]
pub struct ChainSegment {
    pub condition: Option<AbilityCondition>,
    pub effects: Vec<Effect>,
    pub else_effects: Option<Vec<Effect>>,
    pub optional: SegmentOptional,
    /// CR 119.1 + CR 119.3 + CR 608.2c: When `Some`, the segment's effects
    /// run with `AbilityDefinition::player_scope` set so the engine iterates
    /// the sub-AD over each matching player (each becomes the acting
    /// controller). Mirrors the `ActionsConversion::Scoped` materialization
    /// at the sole-action layer. Used for mid-list `Action::EachPlayerAction`
    /// / `Action::EachPlayerActions` / non-You `Action::PlayerAction`.
    pub player_scope: Option<PlayerFilter>,
}

/// Result of converting an `Actions` body. Each variant maps to a distinct
/// `AbilityDefinition` shape — modal spells, optional abilities, optional-with-
/// cost abilities, conditionals, and branches — that `apply_actions_to_ability`
/// in `convert::mod` materializes onto a base `AbilityDefinition`.
///
/// CR 700.2 (Modal), CR 117.5 ("may"), CR 117.6 / CR 605.1c (optional cost),
/// CR 700.4 (conditional resolution).
#[derive(Debug, Clone)]
pub enum ActionsConversion {
    /// CR 608.2c: Sequential effect chain — the legacy `convert_list` shape.
    Linear { effects: Vec<Effect> },
    /// CR 608.2c + CR 700.4: Sequential chain with mid-list conditional gates.
    /// Each segment is a contiguous run of effects that share an optional
    /// `condition`. The chain is rendered by linking segments via
    /// `sub_ability`, with the head AD of each segment carrying its
    /// `condition` (and `else_ability` for IfElse segments).
    LinearChain { segments: Vec<ChainSegment> },
    /// CR 700.2 / CR 700.2d: Modal — each mode is a sub-ability body.
    /// `constraints` propagates `ModalSelectionConstraint`s (e.g.
    /// `NoRepeatThisTurn`, `NoRepeatThisGame`) onto the engine's
    /// `ModalChoice::constraints`. `entwine_cost` (CR 702.42) is set when the
    /// outer modal carries an entwine rider so the engine can offer
    /// "all modes for [cost]" as the alternative selection.
    /// `allow_repeat_modes` (CR 700.2d) is set when the outer modal allows
    /// the same mode to be chosen more than once.
    Modal {
        modes: Vec<Vec<Effect>>,
        choose: ChooseSpec,
        constraints: Vec<ModalSelectionConstraint>,
        entwine_cost: Option<ManaCost>,
        allow_repeat_modes: bool,
    },
    /// CR 117.5: "You may [do X]." — optional with no extra cost.
    Optional { effects: Vec<Effect> },
    /// CR 117.6 + CR 605.1c: "You may [pay cost] to [do X]." — optional
    /// gated on payment of an additional cost.
    OptionalWithCost {
        cost: AbilityCost,
        effects: Vec<Effect>,
    },
    /// CR 117.6 + CR 605.1c + CR 603.12: "You may [pay cost]. When you do, [body]."
    /// Reflexive form of optional-with-cost: the parent ability resolves the
    /// cost choice (via `Effect::PayCost`), and the body is queued as a
    /// sub_ability gated on `AbilityCondition::WhenYouDo`. Distinct from
    /// `OptionalWithCost` because target selection in `inner` happens at the
    /// reflexive trigger's resolution time, not at the parent's resolution
    /// time. Mirrors the native parser's reflexive-trigger lowering at
    /// `oracle.rs:4272-4290`.
    OptionalWithCostReflexive {
        cost: AbilityCost,
        inner: Box<ActionsConversion>,
    },
    /// CR 700.4 + CR 608.2c: "If [condition], [do X]." (positive form) or
    /// "Unless [condition], [do X]." (negated form, when expressible).
    Conditional {
        condition: AbilityCondition,
        effects: Vec<Effect>,
    },
    /// CR 700.4 + CR 608.2c: "If [condition], [do A]. Otherwise, [do B]."
    Branched {
        condition: AbilityCondition,
        then_effects: Vec<Effect>,
        else_effects: Vec<Effect>,
    },
    /// CR 608.2c: Player-scoped action — "[scope]
    /// does [inner]" / "each [scope] does [inner]". The inner conversion
    /// resolves once and is materialized with the wrapping
    /// `AbilityDefinition::player_scope` set, so the engine iterates the
    /// effect over the matching players (each becomes the acting
    /// controller). Mirrors the engine's `player_scope` field
    /// (ability.rs:5372).
    Scoped {
        inner: Box<ActionsConversion>,
        player_scope: PlayerFilter,
    },
}

/// Top-level entry point: convert an `Actions` body into a typed
/// `ActionsConversion`. Detects modal/optional/conditional shapes at the
/// outer wrapper and at the head of an `ActionList`. All other sequential
/// shapes fall through to `Linear`.
// CR 119.1 + CR 117.5 dispatch: the early-exit `if predicate.is_none()`
// guards are more readable in long match arms than match guards or
// `let ... else` because they short-circuit cleanly on the
// target-player-ref shape without nesting the entire body.
#[allow(clippy::collapsible_if, clippy::collapsible_match)]
pub fn convert_actions(actions: &Actions) -> ConvResult<ActionsConversion> {
    // Outer Actions-level wrappers: Modal_*. Future: Targeted_Modal,
    // Modal_ChooseTwo, etc.
    match actions {
        // CR 700.2: "Choose one —" — each branch is a Modal mode.
        Actions::Modal_ChooseOne(modes) => {
            return convert_modal(modes, ChooseSpec::One, "Modal_ChooseOne");
        }
        // CR 700.2: "Choose two —" / "Choose three —".
        Actions::Modal_ChooseTwo(modes) => {
            return convert_modal(modes, ChooseSpec::Exactly { n: 2 }, "Modal_ChooseTwo");
        }
        Actions::Modal_ChooseThree(modes) => {
            return convert_modal(modes, ChooseSpec::Exactly { n: 3 }, "Modal_ChooseThree");
        }
        // CR 700.2: "Choose up to one —" / "Choose up to two —" (UpToN).
        Actions::Modal_ChooseUptoOne(modes) => {
            return convert_modal(modes, ChooseSpec::UpToN { n: 1 }, "Modal_ChooseUptoOne");
        }
        // CR 700.2: "Choose one or more —" — charm-family modal (min=1,
        // max=mode_count; the caller fills max from `modes.len()`).
        Actions::Modal_ChooseOneOrMore(modes) => {
            return convert_modal(modes, ChooseSpec::OneOrMore, "Modal_ChooseOneOrMore");
        }
        // CR 700.2: "Choose one or both —" — 2-mode charm pattern (min=1,
        // max=2). Maps onto `OneOrMore` — engine derives max from
        // `mode_count` (filled at materialization).
        Actions::Modal_ChooseOneOrBoth(modes) => {
            return convert_modal(modes, ChooseSpec::OneOrMore, "Modal_ChooseOneOrBoth");
        }
        // CR 700.2: "Choose both —" — both modes mandatory. Equivalent to
        // `Exactly { n = mode_count }`. Strict-fail if `modes.is_empty()`
        // (handled inside `convert_modal_with`).
        Actions::Modal_ChooseBoth(modes) => {
            return convert_modal(
                modes,
                ChooseSpec::Exactly { n: modes.len() },
                "Modal_ChooseBoth",
            );
        }
        // CR 700.2: "Choose any number —" (min=0, max=mode_count). Distinct
        // from `OneOrMore` because zero is legal.
        Actions::Modal_ChooseAnyNumber(modes) => {
            return convert_modal(modes, ChooseSpec::AnyNumber, "Modal_ChooseAnyNumber");
        }
        // CR 700.2: "Choose one that hasn't been chosen" — single-choice
        // modal with a per-source uniqueness constraint over the game.
        // Engine: `ModalSelectionConstraint::NoRepeatThisGame`.
        Actions::Modal_ChooseOneThatHasntBeenChosen(modes) => {
            return convert_modal_with(
                modes,
                ChooseSpec::One,
                vec![ModalSelectionConstraint::NoRepeatThisGame],
                None,
                false,
                "Modal_ChooseOneThatHasntBeenChosen",
            );
        }
        // CR 700.2: "Choose one that hasn't been chosen this turn" — single-
        // choice modal with a per-turn uniqueness constraint.
        Actions::Modal_ChooseOneThatHasntBeenChosenThisTurn(modes) => {
            return convert_modal_with(
                modes,
                ChooseSpec::One,
                vec![ModalSelectionConstraint::NoRepeatThisTurn],
                None,
                false,
                "Modal_ChooseOneThatHasntBeenChosenThisTurn",
            );
        }
        // CR 702.42 + CR 700.2: Entwine — modal whose controller may pay an
        // additional cost to choose all modes. Cost must be pure mana per
        // engine `ModalChoice::entwine_cost: Option<ManaCost>`.
        Actions::Modal_ChooseOne_Entwine(cost, modes) => {
            let mana_cost = crate::convert::cost::as_pure_mana(cost)?.ok_or(
                ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "ModalChoice::entwine_cost",
                    needed_variant: "non-mana entwine cost".into(),
                },
            )?;
            return convert_modal_with(
                modes,
                ChooseSpec::One,
                Vec::new(),
                Some(mana_cost),
                false,
                "Modal_ChooseOne_Entwine",
            );
        }
        Actions::Modal_ChooseTwo_Entwine(cost, modes) => {
            let mana_cost = crate::convert::cost::as_pure_mana(cost)?.ok_or(
                ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "ModalChoice::entwine_cost",
                    needed_variant: "non-mana entwine cost".into(),
                },
            )?;
            return convert_modal_with(
                modes,
                ChooseSpec::Exactly { n: 2 },
                Vec::new(),
                Some(mana_cost),
                false,
                "Modal_ChooseTwo_Entwine",
            );
        }
        // CR 700.2: "Choose up to N —" — `UpToN` only when N is a fixed
        // integer at parse time. Dynamic counts (e.g., "the number of cards
        // in your graveyard") have no engine slot today; strict-fail.
        Actions::Modal_ChooseUptoNumber(n, modes) => {
            let qty = quantity::convert(n)?;
            match qty {
                QuantityExpr::Fixed { value } if (0..=255).contains(&value) => {
                    return convert_modal(
                        modes,
                        ChooseSpec::UpToN { n: value as usize },
                        "Modal_ChooseUptoNumber",
                    );
                }
                _ => {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "ModalChoice::max_choices",
                        needed_variant: "dynamic upper bound (QuantityExpr)".into(),
                    });
                }
            }
        }
        // CR 700.2 + CR 700.2d: "Choose N — You may choose the same mode
        // more than once." Static-N variant; dynamic strict-fails per
        // `ChooseUptoNumber`.
        Actions::Modal_ChooseNumberMayChooseSameModeMoreThanOnce(n, modes) => {
            let qty = quantity::convert(n)?;
            match qty {
                QuantityExpr::Fixed { value } if (0..=255).contains(&value) => {
                    return convert_modal_with(
                        modes,
                        ChooseSpec::Exactly { n: value as usize },
                        Vec::new(),
                        None,
                        true,
                        "Modal_ChooseNumberMayChooseSameModeMoreThanOnce",
                    );
                }
                _ => {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "ModalChoice::max_choices",
                        needed_variant: "dynamic exact count (QuantityExpr)".into(),
                    });
                }
            }
        }
        // CR 700.2 + CR 601.2c: `Targeted_Modal` is a modal whose modes each
        // declare their own targets internally (each mode body is a
        // `Targeted` wrapper). Equivalent to `Modal_ChooseOne` over the
        // modes — `convert_list` already treats `Targeted` as a transparent
        // wrapper to the inner ActionList.
        Actions::Targeted_Modal(modes) => {
            return convert_modal(modes, ChooseSpec::One, "Targeted_Modal");
        }
        // CR 115.1 + CR 601.2c: `Targeted_DifferentTargets` is a single-body
        // targeting wrapper that constrains the chosen targets to be
        // distinct. Inner-body targets resolve through the existing
        // `Targeted` path; the distinctness constraint is a runtime
        // targeting concern that the engine doesn't model on this slot
        // today, so we recurse transparently. Conservative — preserves
        // resolution semantics, drops only the distinctness gate (the
        // native parser does the same).
        Actions::Targeted_DifferentTargets(_targets, inner) => {
            return convert_actions(inner);
        }
        // CR 700.4: `Modal_IfElse(cond, then_actions, else_actions)` is the
        // Actions-level branch — distinct from `Action::IfElse`, which lives
        // mid-list. Both branches must collapse to `Linear` (a flat effect
        // chain) so `Branched` can carry them; nested modal/optional bodies
        // strict-fail (the engine has no nested-conversion slot here).
        Actions::Modal_IfElse(cond, then_actions, else_actions) => {
            let condition = condition::convert_ability(cond)?;
            let then_conv = convert_actions(then_actions)?;
            let else_conv = convert_actions(else_actions)?;
            match (then_conv, else_conv) {
                (
                    ActionsConversion::Linear { effects: t },
                    ActionsConversion::Linear { effects: e },
                ) => {
                    return Ok(ActionsConversion::Branched {
                        condition,
                        then_effects: t,
                        else_effects: e,
                    });
                }
                _ => {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "ActionsConversion::Branched",
                        needed_variant: "non-linear branch bodies (modal/optional/conditional)"
                            .into(),
                    });
                }
            }
        }
        // CR 700.2 + CR 700.2e: Variants whose semantics need engine slots
        // we don't have yet. Strict-fail with `EnginePrerequisiteMissing`
        // so the gap report names the missing slot rather than reading as
        // a generic `MalformedIdiom`.
        Actions::Modal_ChooseOneAtRandom(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ModalChoice",
                needed_variant: "random mode selection".into(),
            });
        }
        Actions::Modal_APlayerChoosesOne(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ModalChoice",
                needed_variant: "non-controller mode chooser (CR 700.2e)".into(),
            });
        }
        Actions::Modal_ChooseOneOrBothIf(_, _)
        | Actions::Modal_ChooseOneOrChooseOneOrMoreIf(_, _)
        | Actions::Modal_ChooseOneOrMayChooseTwoIf(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ModalChoice",
                needed_variant: "condition-gated max_choices expansion".into(),
            });
        }
        Actions::Modal_ChooseOneOrMore_DifferentTargets(_)
        | Actions::Modal_ChooseTwo_DifferentTargets(_)
        | Actions::Modal_MayChooseTwo_DifferentTargets(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ModalSelectionConstraint",
                needed_variant: "DifferentTargetPermanents (per-mode target distinctness)".into(),
            });
        }
        Actions::Modal_ChooseOneOrMore_Escalate(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ModalChoice",
                needed_variant: "escalate cost (CR 702.121) — mode_costs additive variant".into(),
            });
        }
        Actions::Modal_ChooseOneThatWasntChosenDuringPlayersLastCombat(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ModalSelectionConstraint",
                needed_variant: "NoRepeatDuringPlayersLastCombat".into(),
            });
        }
        Actions::Modal_ChooseUptoNumberPawsMayChooseSameModeMoreThanOnce(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ModalChoice",
                needed_variant: "PawMode mode bodies (Bloomburrow paw-print modal)".into(),
            });
        }
        Actions::AdditionalCost_Modal(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ModalChoice",
                needed_variant: "AdditionalCost-keyed modes (Spree-adjacent shape)".into(),
            });
        }
        Actions::WithX(_, _) | Actions::X(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityDefinition",
                needed_variant: "X-cost / X-comparison wrapper".into(),
            });
        }
        _ => {}
    }

    // ActionList head-wrapper detection: a single-element Action::If /
    // Action::Unless / Action::IfElse / Action::MayAction / Action::MayCost
    // at the head of an ActionList collapses the whole ActionList into the
    // matching ActionsConversion variant. Multi-element ActionLists fall
    // through to the Linear path.
    // CR 119.1 + CR 119.3: Outer player-scoped wrappers — collapse to
    // `Scoped { inner, player_scope }` by recursing on the inner action.
    // Tries `Action::PlayerAction` (single player), `Action::EachPlayerAction`
    // (filtered set of players), and `Action::APlayerAction` (existential —
    // mapped to `Opponent` for "an opponent" / `All` for "any player"). The
    // inner Action is rewrapped as a single-element ActionList so it
    // re-enters the recursion as a structurally-valid Actions body.
    if let Actions::ActionList(actions_vec) = actions {
        if let [head] = actions_vec.as_slice() {
            match head {
                Action::PlayerAction(player, inner) => {
                    // CR 115.2 + CR 601.2c: Target-player refs rebind the
                    // inner Effect's player-target slot rather than wiring
                    // through `player_scope` (which iterates a static set).
                    // Skip the head-pattern Scoped collapse for these refs
                    // — they fall through to the chain-segment path below
                    // where `apply_player_target` runs per-effect.
                    if player_to_target_filter(player).is_none() {
                        if let Some(scope) = player_to_scope_opt(player)? {
                            let inner_actions = Actions::ActionList(vec![(**inner).clone()]);
                            let inner_conv = convert_actions(&inner_actions)?;
                            return Ok(ActionsConversion::Scoped {
                                inner: Box::new(inner_conv),
                                player_scope: scope,
                            });
                        }
                    }
                }
                Action::EachPlayerAction(players, inner) => {
                    if let Some(scope) = players_to_scope_opt(players)? {
                        let inner_actions = Actions::ActionList(vec![(**inner).clone()]);
                        let inner_conv = convert_actions(&inner_actions)?;
                        return Ok(ActionsConversion::Scoped {
                            inner: Box::new(inner_conv),
                            player_scope: scope,
                        });
                    }
                }
                // CR 119.1 + CR 119.3 + CR 608.2c: Plural body variant —
                // `Action::EachPlayerActions(players, Vec<Action>)` is the
                // multi-action sibling of `EachPlayerAction`. Wrap the body
                // as an ActionList so the inner shape detector still runs
                // (modal/optional/conditional inner shapes survive scoping).
                Action::EachPlayerActions(players, body) => {
                    if let Some(scope) = players_to_scope_opt(players)? {
                        let inner_actions = Actions::ActionList(body.clone());
                        let inner_conv = convert_actions(&inner_actions)?;
                        return Ok(ActionsConversion::Scoped {
                            inner: Box::new(inner_conv),
                            player_scope: scope,
                        });
                    }
                }
                // CR 119.1 + CR 119.3: `Action::PlayerActions(player, Vec<Action>)`
                // is the plural-body sibling of `PlayerAction`. Mirrors the
                // singular arm at the top of this dispatch — target-player
                // refs hoist through `apply_player_target` (skipped here),
                // scope-resolvable players collapse to `Scoped`, and `You`
                // (scope = None) is a transparent passthrough since every
                // Effect already defaults to Controller.
                Action::PlayerActions(player, body) => {
                    if player_to_target_filter(player).is_none() {
                        let scope_opt = player_to_scope_opt(player)?;
                        let inner_actions = Actions::ActionList(body.clone());
                        let inner_conv = convert_actions(&inner_actions)?;
                        return Ok(match scope_opt {
                            Some(scope) => ActionsConversion::Scoped {
                                inner: Box::new(inner_conv),
                                player_scope: scope,
                            },
                            None => inner_conv,
                        });
                    }
                }
                // CR 117.5 + CR 119.1: "[Player] may [do X]" — single-player
                // optional wrapper. `You` collapses to a plain `Optional`;
                // scope-resolvable players (Opponent, etc.) wrap as
                // `Scoped { inner: Optional, player_scope }` so the engine
                // iterates the optional ability over each matching player.
                // Target-player refs strict-fail — they need
                // `apply_player_target` threading at the chain-segment level.
                Action::PlayerMayAction(player, inner) => {
                    if player_to_target_filter(player).is_none() {
                        let scope_opt = player_to_scope_opt(player)?;
                        let effects = convert_action_vec(std::slice::from_ref(inner))?;
                        let optional = ActionsConversion::Optional { effects };
                        return Ok(match scope_opt {
                            Some(scope) => ActionsConversion::Scoped {
                                inner: Box::new(optional),
                                player_scope: scope,
                            },
                            None => optional,
                        });
                    }
                }
                // CR 117.5 + CR 119.1: Filtered-set optional wrappers — "[each
                // player who matches] may [do X]" / "[a player who matches]
                // may [do X]". Both lower onto `Scoped { Optional, scope }`
                // because the engine's player_scope iterates the optional
                // ability over each matching player.
                Action::EachPlayerMayAction(players, inner) => {
                    if let Some(scope) = players_to_scope_opt(players)? {
                        let effects = convert_action_vec(std::slice::from_ref(inner))?;
                        return Ok(ActionsConversion::Scoped {
                            inner: Box::new(ActionsConversion::Optional { effects }),
                            player_scope: scope,
                        });
                    }
                }
                Action::APlayerMayAction(players, inner) => {
                    if let Some(scope) = players_to_scope_opt(players)? {
                        let effects = convert_action_vec(std::slice::from_ref(inner))?;
                        return Ok(ActionsConversion::Scoped {
                            inner: Box::new(ActionsConversion::Optional { effects }),
                            player_scope: scope,
                        });
                    }
                }
                // CR 117.5 + CR 119.1 + CR 608.2c: Plural-body siblings —
                // `EachPlayerMayActions(players, Vec<Action>)` mirrors
                // `EachPlayerActions` but wraps the body in Optional first.
                Action::EachPlayerMayActions(players, body) => {
                    if let Some(scope) = players_to_scope_opt(players)? {
                        let effects = convert_action_vec(body)?;
                        return Ok(ActionsConversion::Scoped {
                            inner: Box::new(ActionsConversion::Optional { effects }),
                            player_scope: scope,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    if let Actions::ActionList(actions_vec) = actions {
        // CR 117.6 + CR 605.1c: "[Pay cost]. If you do, [do X]." — the
        // canonical speculative-payment idiom. mtgish encodes this as a
        // 2-element ActionList: a leading `Action::MayCost(Cost)` (the
        // optional cost) followed by `Action::If(Condition::CostWasPaid,
        // body)` (the gated payload). Both elements share the same cost
        // identity — the `If` condition reads the prior `MayCost` outcome
        // — so we collapse them into `OptionalWithCost { cost, effects }`,
        // the engine's existing primitive for "you may pay {C}: do X"
        // optional-payment shape. Multi-element bodies after the `If`
        // remain inside the same payload `Vec<Action>`.
        // CR 117.6 + CR 700.4 + CR 608.2c: `[MustCost(action_cost), If(CostWasPaid, body)]`
        // — mtgish encodes "do X. when you do, [body]" idioms using `MustCost`
        // when the prefix action ALWAYS succeeds (PutCounter on this, bounce
        // the target, attach, sacrifice the source). Distinct from
        // `MayCost`'s optional payment: the "cost" here is itself a
        // state-changing action, not a real cost; `CostWasPaid` is
        // tautologically true once the action resolves. We map the inner
        // `Cost` shape to its `Action` analog (Cost::PutACounterOfTypeOnPermanent
        // → Action::PutACounterOfTypeOnPermanent, etc.), convert that to an
        // Effect, and prepend it to the converted body.
        //
        // Bodies that contain `ReflexiveTrigger(inner)` strict-fail at
        // `convert_action_vec` (the leaf-Effect path) — the inner reflexive
        // pattern needs the same `WhenYouDo` sub_ability lowering as
        // OptionalWithCostReflexive but for an action-shaped parent effect.
        // Defer that combined shape to a follow-up.
        if let [Action::MustCost(cost_box), Action::If(cond, body)] = actions_vec.as_slice() {
            if matches!(cond, crate::schema::types::Condition::CostWasPaid) {
                let cost_action = mustcost_action_equivalent(cost_box)?;
                let cost_effect = convert(&cost_action)?;
                let body_effects = convert_action_vec(body)?;
                let mut effects = Vec::with_capacity(1 + body_effects.len());
                effects.push(cost_effect);
                effects.extend(body_effects);
                return Ok(ActionsConversion::Linear { effects });
            }
        }
        if let [Action::MayCost(cost_box), Action::If(cond, body)] = actions_vec.as_slice() {
            if matches!(cond, crate::schema::types::Condition::CostWasPaid) {
                let cost = crate::convert::cost::convert(cost_box)?;
                // CR 603.12: Reflexive form — body is a sole `ReflexiveTrigger`
                // wrapper. Lower as parent (`Effect::PayCost` + optional cost)
                // + sub_ability (`AbilityCondition::WhenYouDo` + inner). Target
                // selection happens at the reflexive trigger's resolution.
                if let [Action::ReflexiveTrigger(inner_actions)] = body.as_slice() {
                    let inner = convert_actions(inner_actions)?;
                    return Ok(ActionsConversion::OptionalWithCostReflexive {
                        cost,
                        inner: Box::new(inner),
                    });
                }
                let effects = convert_action_vec(body)?;
                return Ok(ActionsConversion::OptionalWithCost { cost, effects });
            }
        }
        if let [head] = actions_vec.as_slice() {
            match head {
                // CR 117.5: "You may [do X]" — sole-action wrapper. The
                // inner is consumed via `convert_many` so multi-effect inner
                // shapes (notably `Action::SearchLibrary`, which expands to
                // `SearchLibrary → ChangeZone → [Shuffle]`) propagate as a
                // chained effect list rather than collapsing through the
                // single-Effect `convert`.
                Action::MayAction(inner) => {
                    let effects = convert_many(inner)?;
                    return Ok(ActionsConversion::Optional { effects });
                }
                // CR 117.5 + CR 608.2c: Plural-body sibling of `MayAction` —
                // "You may [do X, then Y, then Z]." Optional wrapper over a
                // multi-action body.
                Action::MayActions(body) => {
                    let effects = convert_action_vec(body)?;
                    return Ok(ActionsConversion::Optional { effects });
                }
                // CR 117.6 + CR 605.1c: A bare `MayCost` with no paired
                // payload (no following `If(CostWasPaid, ...)`) cannot be
                // collapsed onto OptionalWithCost — there's nothing to
                // gate. Strict-fail. The 2-element pattern above handles
                // the canonical idiom.
                Action::MayCost(_) => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "ActionList/MayCost-without-body",
                        path: String::new(),
                        detail: "MayCost as sole action — no payload to gate".into(),
                    });
                }
                // CR 700.4: "If [condition], [do X]" — sole-action wrapper.
                Action::If(cond, body) => {
                    let condition = condition::convert_ability(cond)?;
                    let effects = convert_action_vec(body)?;
                    return Ok(ActionsConversion::Conditional { condition, effects });
                }
                // CR 608.2c: "Unless [condition], [do X]" — invert the inner
                // condition where the existing engine `AbilityCondition`
                // variant carries a `negated` slot (e.g. `IsYourTurn`),
                // reusing the same `Conditional` shape as `Action::If`. No
                // general `Not` wrapper on `AbilityCondition` today, so
                // non-invertible inner conditions strict-fail through
                // `convert_ability_negated`.
                Action::Unless(cond, body) => {
                    let condition = condition::convert_ability_negated(cond)?;
                    let effects = convert_action_vec(body)?;
                    return Ok(ActionsConversion::Conditional { condition, effects });
                }
                // CR 700.4: "If [cond], [do A]. Otherwise, [do B]."
                Action::IfElse(cond, then_body, else_body) => {
                    let condition = condition::convert_ability(cond)?;
                    let then_effects = convert_action_vec(then_body)?;
                    let else_effects = convert_action_vec(else_body)?;
                    return Ok(ActionsConversion::Branched {
                        condition,
                        then_effects,
                        else_effects,
                    });
                }
                _ => {}
            }
        }
    }

    // Default: Linear chain. Walk the action list segment-by-segment so
    // mid-list `Action::If` / `Action::Unless` / `Action::IfElse` open
    // conditional segments inside the chain. When there are no mid-list
    // conditionals the segment list collapses to a single unconditional run,
    // equivalent to the legacy `Linear { effects }` shape.
    //
    // Targeted / TargetedDistributed wrappers are transparent at the
    // chain-shape layer (per `convert_list`'s comment): the inner ActionList
    // is the structural unit. We unwrap them here so chain-segment
    // recognition reaches their bodies too. Non-ActionList shapes that have
    // no inner ActionList (none today, but reserved for future variants)
    // strict-fail through `convert_chain_segments`.
    let inner = unwrap_targeted(actions);
    if let Actions::ActionList(_) = inner {
        let segments = convert_chain_segments(inner)?;
        return Ok(ActionsConversion::LinearChain { segments });
    }
    Ok(ActionsConversion::Linear {
        effects: convert_list(actions)?,
    })
}

/// CR 115.1 + CR 601.2c: Unwrap `Actions::Targeted` and
/// `Actions::TargetedDistributed` to reach the inner `ActionList`. The
/// outer Target descriptors govern target-prompting at cast time but are
/// transparent at the chain-shape layer (target refs in the body collapse
/// to the appropriate `TargetFilter` axes in leaf converters).
fn unwrap_targeted(actions: &Actions) -> &Actions {
    match actions {
        Actions::Targeted(_, inner) => unwrap_targeted(inner),
        Actions::TargetedDistributed(_, _, inner) => unwrap_targeted(inner),
        other => other,
    }
}

/// CR 608.2c + CR 700.4: Walk a flat `Actions::ActionList` and split it at
/// each mid-list `Action::If(cond, body)` / `Action::Unless(cond, body)` /
/// `Action::IfElse(cond, then, else)` into a sequence of `ChainSegment`s.
///
/// Unconditional actions accumulate into the current segment; a conditional
/// action closes the current segment (if any) and emits a new conditional
/// segment for its body. The next unconditional action opens a fresh
/// unconditional segment. Strict-fails on any inner condition that
/// `convert_ability` cannot translate — conditionals are never silently
/// dropped (see `condition::convert_ability_negated` for the `Unless`
/// inversion path; `AbilityCondition::Not` wraps the inner condition when
/// it has no inversion-parameter slot).
pub fn convert_chain_segments(list: &Actions) -> ConvResult<Vec<ChainSegment>> {
    let actions_vec = match list {
        Actions::ActionList(v) => v,
        _ => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "Actions/convert_chain_segments",
                path: String::new(),
                detail: format!("expected ActionList, got {}", actions_tag(list)),
            });
        }
    };

    let mut segments: Vec<ChainSegment> = Vec::new();
    let mut current: Vec<Effect> = Vec::new();

    let flush_unconditional = |current: &mut Vec<Effect>, segments: &mut Vec<ChainSegment>| {
        if !current.is_empty() {
            segments.push(ChainSegment {
                condition: None,
                effects: std::mem::take(current),
                else_effects: None,
                optional: SegmentOptional::Mandatory,
                player_scope: None,
            });
        }
    };

    let mut i = 0;
    while i < actions_vec.len() {
        let a = &actions_vec[i];
        match a {
            // CR 117.6 + CR 605.1c: Mid-list speculative-payment idiom —
            // `[MayCost(cost), If(CostWasPaid, body)]` (positive form) or
            // `[MayCost(cost), Unless(CostWasPaid, body)]` (negative form,
            // e.g., "If you don't, [counter the spell]"). The pair collapses
            // into one segment that owns the cost and (for the positive
            // form) its gated body. The negative form is materialized as
            // an `Optional` parent + a follow-on segment carrying the
            // negated payload — but mtgish only emits the positive form
            // for bare `MayCost`; the negative form belongs to the
            // `PlayerMayCost` family which is a separate dispatch.
            Action::MayCost(cost_box) => {
                let next = actions_vec.get(i + 1);
                let is_cost_was_paid = |c: &crate::schema::types::Condition| -> bool {
                    matches!(c, crate::schema::types::Condition::CostWasPaid)
                };
                // CR 117.6 + CR 605.1c: positive form `[MayCost, If(CostWasPaid, body)]`
                // — cost is optional; body runs when paid.
                if let Some(Action::If(cond, body)) = next {
                    if is_cost_was_paid(cond) {
                        let cost = crate::convert::cost::convert(cost_box)?;
                        let body_effects = convert_action_vec(body)?;
                        flush_unconditional(&mut current, &mut segments);
                        segments.push(ChainSegment {
                            condition: None,
                            effects: body_effects,
                            else_effects: None,
                            optional: SegmentOptional::OptionalWithCost(cost),
                            player_scope: None,
                        });
                        i += 2;
                        continue;
                    }
                }
                // CR 117.6 + CR 605.1c + CR 608.2c: paired form
                // `[MayCost, IfElse(CostWasPaid, then, else)]` — cost is
                // optional; `then` runs when paid, `else` when not.
                // Materializes onto the segment's `else_effects` slot which
                // the chain builder lowers to `else_ability`.
                if let Some(Action::IfElse(cond, then_body, else_body)) = next {
                    if is_cost_was_paid(cond) {
                        let cost = crate::convert::cost::convert(cost_box)?;
                        let then_effects = convert_action_vec(then_body)?;
                        let else_effects = convert_action_vec(else_body)?;
                        flush_unconditional(&mut current, &mut segments);
                        segments.push(ChainSegment {
                            condition: None,
                            effects: then_effects,
                            else_effects: Some(else_effects),
                            optional: SegmentOptional::OptionalWithCost(cost),
                            player_scope: None,
                        });
                        i += 2;
                        continue;
                    }
                }
                // CR 117.6 + CR 605.1c + CR 608.2c: negative form
                // `[MayCost, Unless(CostWasPaid, body)]` — "you may pay X.
                // If you don't, [body]." Cost is offered as optional via
                // `OptionalWithCost`; the body is gated on the existing
                // `AbilityCondition::AdditionalCostNotPaid` parameter
                // (engine-side parameter form of the inversion — the cost
                // choice is recorded on `SpellContext.additional_cost_paid`
                // at activation, and `evaluate_condition` reads it at
                // resolution per `effects/mod.rs:1558`). No `else_effects`
                // because there's no "if paid, do X" branch — the
                // optional-cost choice itself absorbs the paid path.
                if let Some(Action::Unless(cond, body)) = next {
                    if is_cost_was_paid(cond) {
                        let cost = crate::convert::cost::convert(cost_box)?;
                        let body_effects = convert_action_vec(body)?;
                        flush_unconditional(&mut current, &mut segments);
                        segments.push(ChainSegment {
                            condition: Some(engine::types::ability::AbilityCondition::Not {
                                condition: Box::new(
                                    engine::types::ability::AbilityCondition::AdditionalCostPaid,
                                ),
                            }),
                            effects: body_effects,
                            else_effects: None,
                            optional: SegmentOptional::OptionalWithCost(cost),
                            player_scope: None,
                        });
                        i += 2;
                        continue;
                    }
                }
                // Bare MayCost with no paired CostWasPaid gate has no
                // payload — strict-fail so the shape surfaces in the
                // report rather than collapsing silently.
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "ChainSegment/MayCost-without-CostWasPaid-If/IfElse/Unless",
                    path: String::new(),
                    detail: "mid-list MayCost not followed by If/IfElse/Unless(CostWasPaid, ...)"
                        .into(),
                });
            }
            // CR 118 + CR 605.1c: Player-scoped speculative-payment idiom —
            // `[PlayerMayCost(player, cost), If/IfElse/Unless(CostWasPaid, body)]`.
            // The Player-scope variant of `Action::MayCost`: composes the
            // same three pair shapes (If / IfElse / Unless of CostWasPaid)
            // with `OptionalWithCost(cost)` segment optionality, but
            // additionally sets `player_scope` so the engine offers the
            // optional payment to the named player rather than the
            // controller. Mirrors the bare `MayCost` arm above; the only
            // delta is the resolved `PlayerFilter` on the segment.
            //
            // Player references that don't reduce to a static
            // `PlayerFilter` (e.g. `ControllerOfTargetSpell`,
            // `OwnerOfPermanent`) strict-fail via `player_to_scope_opt`
            // returning `None`, surfacing the gap in the report rather
            // than silently dropping the scope.
            Action::PlayerMayCost(player_box, cost_box) => {
                let scope = player_to_scope_opt(player_box)?.ok_or_else(|| {
                    ConversionGap::MalformedIdiom {
                        idiom: "ChainSegment/PlayerMayCost",
                        path: String::new(),
                        detail: format!("non-scopable player: {player_box:?}"),
                    }
                })?;
                let next = actions_vec.get(i + 1);
                let is_cost_was_paid = |c: &crate::schema::types::Condition| -> bool {
                    matches!(c, crate::schema::types::Condition::CostWasPaid)
                };
                // CR 118 + CR 605.1c: positive form — body runs when paid.
                if let Some(Action::If(cond, body)) = next {
                    if is_cost_was_paid(cond) {
                        let cost = crate::convert::cost::convert(cost_box)?;
                        let body_effects = convert_action_vec(body)?;
                        flush_unconditional(&mut current, &mut segments);
                        segments.push(ChainSegment {
                            condition: None,
                            effects: body_effects,
                            else_effects: None,
                            optional: SegmentOptional::OptionalWithCost(cost),
                            player_scope: Some(scope),
                        });
                        i += 2;
                        continue;
                    }
                }
                // CR 118 + CR 605.1c + CR 608.2c: branched form —
                // `then` runs when paid, `else` when not.
                if let Some(Action::IfElse(cond, then_body, else_body)) = next {
                    if is_cost_was_paid(cond) {
                        let cost = crate::convert::cost::convert(cost_box)?;
                        let then_effects = convert_action_vec(then_body)?;
                        let else_effects = convert_action_vec(else_body)?;
                        flush_unconditional(&mut current, &mut segments);
                        segments.push(ChainSegment {
                            condition: None,
                            effects: then_effects,
                            else_effects: Some(else_effects),
                            optional: SegmentOptional::OptionalWithCost(cost),
                            player_scope: Some(scope),
                        });
                        i += 2;
                        continue;
                    }
                }
                // CR 118 + CR 605.1c + CR 608.2c: negative form — "[player]
                // may pay X. If they don't, [body]." The cost choice is
                // recorded on `SpellContext.additional_cost_paid` and the
                // body gates on `AbilityCondition::AdditionalCostNotPaid`,
                // matching the bare-`MayCost` negative-form materialization.
                if let Some(Action::Unless(cond, body)) = next {
                    if is_cost_was_paid(cond) {
                        let cost = crate::convert::cost::convert(cost_box)?;
                        let body_effects = convert_action_vec(body)?;
                        flush_unconditional(&mut current, &mut segments);
                        segments.push(ChainSegment {
                            condition: Some(engine::types::ability::AbilityCondition::Not {
                                condition: Box::new(
                                    engine::types::ability::AbilityCondition::AdditionalCostPaid,
                                ),
                            }),
                            effects: body_effects,
                            else_effects: None,
                            optional: SegmentOptional::OptionalWithCost(cost),
                            player_scope: Some(scope),
                        });
                        i += 2;
                        continue;
                    }
                }
                // Bare PlayerMayCost with no paired CostWasPaid gate has
                // no payload — strict-fail so the shape surfaces.
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "ChainSegment/PlayerMayCost-without-CostWasPaid-If/IfElse/Unless",
                    path: String::new(),
                    detail:
                        "mid-list PlayerMayCost not followed by If/IfElse/Unless(CostWasPaid, ...)"
                            .into(),
                });
            }
            // CR 117.5: Mid-list "you may [do X]" — optional segment with
            // no extra cost. Multi-emit inner shapes (e.g., SearchLibrary)
            // propagate via `convert_many`.
            Action::MayAction(inner) => {
                let body_effects = convert_many(inner)?;
                flush_unconditional(&mut current, &mut segments);
                segments.push(ChainSegment {
                    condition: None,
                    effects: body_effects,
                    else_effects: None,
                    optional: SegmentOptional::Optional,
                    player_scope: None,
                });
            }
            // CR 119.1 + CR 119.3 + CR 608.2c: Mid-list player-scoped wrapper.
            // `Action::PlayerAction(non-You, inner)` / `Action::EachPlayerAction(players, inner)`
            // / `Action::EachPlayerActions(players, body)` close the current
            // unconditional run and open a new mandatory segment whose effects
            // run with `player_scope` set, so the engine iterates the
            // sub-AD per matching player. Mirrors the sole-action
            // `ActionsConversion::Scoped` materialization. Inner shapes that
            // don't reduce to a known scope (predicate-filtered players,
            // dynamic refs) strict-fail via the scope-resolver `Option`s.
            Action::PlayerAction(player, inner) if !matches!(**player, Player::You) => {
                // CR 119.1 + CR 119.3: `Player::You` is the controller default;
                // it falls through to the unconditional `other` arm below
                // where `convert_many` does the transparent passthrough.
                //
                // CR 115.2 + CR 601.2c: Target-player references rebind the
                // inner Effect's player-target slot (no `player_scope` —
                // `player_scope` iterates a static set of players, but a
                // chosen target needs per-resolution binding). Non-target
                // scopes (Opponent, EachablePlayer) route via `player_scope`.
                if let Some(filter) = player_to_target_filter(player) {
                    let body_effects: Vec<Effect> = convert_many(inner)?
                        .into_iter()
                        .map(|e| apply_player_target(e, filter.clone()))
                        .collect::<ConvResult<_>>()?;
                    flush_unconditional(&mut current, &mut segments);
                    segments.push(ChainSegment {
                        condition: None,
                        effects: body_effects,
                        else_effects: None,
                        optional: SegmentOptional::Mandatory,
                        player_scope: None,
                    });
                } else {
                    let scope = player_to_scope_opt(player)?.ok_or_else(|| {
                        ConversionGap::MalformedIdiom {
                            idiom: "ChainSegment/PlayerAction",
                            path: String::new(),
                            detail: format!("non-scopable player: {player:?}"),
                        }
                    })?;
                    let body_effects = convert_many(inner)?;
                    flush_unconditional(&mut current, &mut segments);
                    segments.push(ChainSegment {
                        condition: None,
                        effects: body_effects,
                        else_effects: None,
                        optional: SegmentOptional::Mandatory,
                        player_scope: Some(scope),
                    });
                }
            }
            Action::EachPlayerAction(players, inner) => {
                let scope = players_to_scope_opt(players)?.ok_or_else(|| {
                    ConversionGap::MalformedIdiom {
                        idiom: "ChainSegment/EachPlayerAction",
                        path: String::new(),
                        detail: format!("non-scopable players: {players:?}"),
                    }
                })?;
                let body_effects = convert_many(inner)?;
                flush_unconditional(&mut current, &mut segments);
                segments.push(ChainSegment {
                    condition: None,
                    effects: body_effects,
                    else_effects: None,
                    optional: SegmentOptional::Mandatory,
                    player_scope: Some(scope),
                });
            }
            Action::EachPlayerActions(players, body) => {
                let scope = players_to_scope_opt(players)?.ok_or_else(|| {
                    ConversionGap::MalformedIdiom {
                        idiom: "ChainSegment/EachPlayerActions",
                        path: String::new(),
                        detail: format!("non-scopable players: {players:?}"),
                    }
                })?;
                let body_effects = convert_action_vec(body)?;
                flush_unconditional(&mut current, &mut segments);
                segments.push(ChainSegment {
                    condition: None,
                    effects: body_effects,
                    else_effects: None,
                    optional: SegmentOptional::Mandatory,
                    player_scope: Some(scope),
                });
            }
            // CR 119.1 + CR 119.3: Mid-list plural-body player wrapper —
            // `Action::PlayerActions(player, Vec<Action>)`. Mirrors the
            // singular-body `PlayerAction` chain-segment arm above, plus
            // the `EachPlayerActions` Vec-body materialization. `Player::You`
            // falls through to the unconditional `other` arm (where
            // `convert_many` provides the transparent passthrough).
            Action::PlayerActions(player, body) if !matches!(**player, Player::You) => {
                if let Some(filter) = player_to_target_filter(player) {
                    let body_effects: Vec<Effect> = convert_action_vec(body)?
                        .into_iter()
                        .map(|e| apply_player_target(e, filter.clone()))
                        .collect::<ConvResult<_>>()?;
                    flush_unconditional(&mut current, &mut segments);
                    segments.push(ChainSegment {
                        condition: None,
                        effects: body_effects,
                        else_effects: None,
                        optional: SegmentOptional::Mandatory,
                        player_scope: None,
                    });
                } else {
                    let scope = player_to_scope_opt(player)?.ok_or_else(|| {
                        ConversionGap::MalformedIdiom {
                            idiom: "ChainSegment/PlayerActions",
                            path: String::new(),
                            detail: format!("non-scopable player: {player:?}"),
                        }
                    })?;
                    let body_effects = convert_action_vec(body)?;
                    flush_unconditional(&mut current, &mut segments);
                    segments.push(ChainSegment {
                        condition: None,
                        effects: body_effects,
                        else_effects: None,
                        optional: SegmentOptional::Mandatory,
                        player_scope: Some(scope),
                    });
                }
            }
            // CR 117.5 + CR 119.1 + CR 608.2c: Mid-list "[player] may [do X]"
            // — close current run, open optional segment under the player's
            // scope. Mirrors the chain-segment `PlayerAction` arm but emits
            // `SegmentOptional::Optional` so the engine renders the segment's
            // ability with `optional = true`. Target-player refs thread
            // through `apply_player_target`; non-target scopes route via
            // `player_scope`. `Player::You` collapses to a plain mid-list
            // optional with no scope (controller default).
            Action::PlayerMayAction(player, inner) => {
                if let Some(filter) = player_to_target_filter(player) {
                    let body_effects: Vec<Effect> = convert_many(inner)?
                        .into_iter()
                        .map(|e| apply_player_target(e, filter.clone()))
                        .collect::<ConvResult<_>>()?;
                    flush_unconditional(&mut current, &mut segments);
                    segments.push(ChainSegment {
                        condition: None,
                        effects: body_effects,
                        else_effects: None,
                        optional: SegmentOptional::Optional,
                        player_scope: None,
                    });
                } else {
                    let scope_opt = player_to_scope_opt(player)?;
                    let body_effects = convert_many(inner)?;
                    flush_unconditional(&mut current, &mut segments);
                    segments.push(ChainSegment {
                        condition: None,
                        effects: body_effects,
                        else_effects: None,
                        optional: SegmentOptional::Optional,
                        player_scope: scope_opt,
                    });
                }
            }
            // CR 117.5 + CR 119.1 + CR 608.2c: Mid-list filtered-set optional
            // wrappers. Same shape as the head-pattern arms but mid-list.
            Action::EachPlayerMayAction(players, inner) => {
                let scope = players_to_scope_opt(players)?.ok_or_else(|| {
                    ConversionGap::MalformedIdiom {
                        idiom: "ChainSegment/EachPlayerMayAction",
                        path: String::new(),
                        detail: format!("non-scopable players: {players:?}"),
                    }
                })?;
                let body_effects = convert_many(inner)?;
                flush_unconditional(&mut current, &mut segments);
                segments.push(ChainSegment {
                    condition: None,
                    effects: body_effects,
                    else_effects: None,
                    optional: SegmentOptional::Optional,
                    player_scope: Some(scope),
                });
            }
            Action::APlayerMayAction(players, inner) => {
                let scope = players_to_scope_opt(players)?.ok_or_else(|| {
                    ConversionGap::MalformedIdiom {
                        idiom: "ChainSegment/APlayerMayAction",
                        path: String::new(),
                        detail: format!("non-scopable players: {players:?}"),
                    }
                })?;
                let body_effects = convert_many(inner)?;
                flush_unconditional(&mut current, &mut segments);
                segments.push(ChainSegment {
                    condition: None,
                    effects: body_effects,
                    else_effects: None,
                    optional: SegmentOptional::Optional,
                    player_scope: Some(scope),
                });
            }
            Action::EachPlayerMayActions(players, body) => {
                let scope = players_to_scope_opt(players)?.ok_or_else(|| {
                    ConversionGap::MalformedIdiom {
                        idiom: "ChainSegment/EachPlayerMayActions",
                        path: String::new(),
                        detail: format!("non-scopable players: {players:?}"),
                    }
                })?;
                let body_effects = convert_action_vec(body)?;
                flush_unconditional(&mut current, &mut segments);
                segments.push(ChainSegment {
                    condition: None,
                    effects: body_effects,
                    else_effects: None,
                    optional: SegmentOptional::Optional,
                    player_scope: Some(scope),
                });
            }
            // CR 117.5 + CR 608.2c: Mid-list `MayActions(Vec<Action>)` —
            // multi-action optional. Equivalent to `MayAction` with a
            // composite body.
            Action::MayActions(body) => {
                let body_effects = convert_action_vec(body)?;
                flush_unconditional(&mut current, &mut segments);
                segments.push(ChainSegment {
                    condition: None,
                    effects: body_effects,
                    else_effects: None,
                    optional: SegmentOptional::Optional,
                    player_scope: None,
                });
            }
            // CR 700.4: "If [cond], [do X]" — close current run, open
            // conditional segment with body's effects. A body that is
            // itself a single nested `Action::If` / `Action::Unless`
            // collapses into a compound `AbilityCondition::And` so nested
            // gates flatten into one segment (CR 608.2c sequencing of
            // intervening-if checks).
            Action::If(cond, body) => {
                let outer = condition::convert_ability(cond)?;
                let (compound, body_effects) = compound_nested_if(outer, body)?;
                flush_unconditional(&mut current, &mut segments);
                segments.push(ChainSegment {
                    condition: Some(compound),
                    effects: body_effects,
                    else_effects: None,
                    optional: SegmentOptional::Mandatory,
                    player_scope: None,
                });
            }
            // CR 608.2c: "Unless [cond], [do X]" — invert the inner
            // condition (parameter form via `convert_ability_negated`,
            // which falls back to `AbilityCondition::Not` wrapper). Nested
            // If inside an Unless body compounds via `And` the same way.
            Action::Unless(cond, body) => {
                let outer = condition::convert_ability_negated(cond)?;
                let (compound, body_effects) = compound_nested_if(outer, body)?;
                flush_unconditional(&mut current, &mut segments);
                segments.push(ChainSegment {
                    condition: Some(compound),
                    effects: body_effects,
                    else_effects: None,
                    optional: SegmentOptional::Mandatory,
                    player_scope: None,
                });
            }
            // CR 700.4: "If [cond], [do A]. Otherwise, [do B]." — single
            // conditional segment carrying both branches; the head AD will
            // wear `condition` + `else_ability`.
            Action::IfElse(cond, then_body, else_body) => {
                let condition = condition::convert_ability(cond)?;
                let then_effects = convert_action_vec(then_body)?;
                let else_effects = convert_action_vec(else_body)?;
                flush_unconditional(&mut current, &mut segments);
                segments.push(ChainSegment {
                    condition: Some(condition),
                    effects: then_effects,
                    else_effects: Some(else_effects),
                    optional: SegmentOptional::Mandatory,
                    player_scope: None,
                });
            }
            // CR 603.12 + CR 603.7: Reflexive triggered ability — "When you
            // do, [effect]." The engine encodes this as a `sub_ability` with
            // `condition: AbilityCondition::WhenYouDo` attached at the deepest
            // sub_ability slot of the running chain. We materialize this by
            // closing the current unconditional run (so the prior segment's
            // tail AD is the attachment point per `attach_at_chain_tail`) and
            // emitting a new `ChainSegment` whose `condition` is
            // `WhenYouDo`. The reflexive cannot stand alone — without a
            // preceding effect there's nothing to "do", so a leading reflexive
            // strict-fails.
            Action::ReflexiveTrigger(body) => {
                flush_unconditional(&mut current, &mut segments);
                if segments.is_empty() {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "ChainSegment",
                        needed_variant:
                            "ReflexiveTrigger at chain head — no preceding ability to attach \
                             sub_ability to"
                                .into(),
                    });
                }
                let body_effects = convert_list(body)?;
                segments.push(ChainSegment {
                    condition: Some(AbilityCondition::WhenYouDo),
                    effects: body_effects,
                    else_effects: None,
                    optional: SegmentOptional::Mandatory,
                    player_scope: None,
                });
            }
            // CR 603.12 + CR 603.7 + CR 608.2c: Conditional reflexive trigger
            // — "When you do, if [cond], [effect]." Same chain-attachment as
            // `ReflexiveTrigger` plus an inner condition gate composed onto
            // `WhenYouDo` via `combine_and`.
            Action::ReflexiveTriggerI(cond, body) => {
                flush_unconditional(&mut current, &mut segments);
                if segments.is_empty() {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "ChainSegment",
                        needed_variant:
                            "ReflexiveTriggerI at chain head — no preceding ability to attach \
                             sub_ability to"
                                .into(),
                    });
                }
                let inner = condition::convert_ability(cond)?;
                let compound = combine_and(AbilityCondition::WhenYouDo, inner);
                let body_effects = convert_list(body)?;
                segments.push(ChainSegment {
                    condition: Some(compound),
                    effects: body_effects,
                    else_effects: None,
                    optional: SegmentOptional::Mandatory,
                    player_scope: None,
                });
            }
            // CR 603.12 + CR 603.7: N-times reflexive trigger — "When you do
            // this N times, [effect]." For fixed small `N` we lower as N
            // identical reflexive sub_ability links (one fires per "do").
            // Dynamic-N (`QuantityExpr::Variable`/`Ref`) has no engine
            // count-gated WhenYouDo primitive today; strict-fail so the gap
            // surfaces in the report.
            Action::ReflexiveTriggerNumberTimes(n, body) => {
                let count_expr = quantity::convert(n)?;
                let count = match count_expr {
                    QuantityExpr::Fixed { value } if (1..=8).contains(&value) => value as usize,
                    _ => {
                        return Err(ConversionGap::EnginePrerequisiteMissing {
                            engine_type: "AbilityCondition",
                            needed_variant:
                                "count-gated WhenYouDo (ReflexiveTriggerNumberTimes with dynamic \
                                 or out-of-range N)"
                                    .into(),
                        });
                    }
                };
                flush_unconditional(&mut current, &mut segments);
                if segments.is_empty() {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "ChainSegment",
                        needed_variant:
                            "ReflexiveTriggerNumberTimes at chain head — no preceding ability to \
                             attach sub_ability to"
                                .into(),
                    });
                }
                let body_effects = convert_list(body)?;
                for _ in 0..count {
                    segments.push(ChainSegment {
                        condition: Some(AbilityCondition::WhenYouDo),
                        effects: body_effects.clone(),
                        else_effects: None,
                        optional: SegmentOptional::Mandatory,
                        player_scope: None,
                    });
                }
            }
            // Unconditional leaf — append to the current run via the
            // multi-emit lowering (preserves SearchLibrary's chain
            // expansion).
            other => {
                current.extend(convert_many(other)?);
            }
        }
        i += 1;
    }
    flush_unconditional(&mut current, &mut segments);

    if segments.is_empty() {
        return Err(ConversionGap::MalformedIdiom {
            idiom: "Actions/convert_chain_segments",
            path: String::new(),
            detail: "empty ActionList".into(),
        });
    }
    Ok(segments)
}

/// CR 700.2: Convert a `Vec<Actions>` of modes into `ActionsConversion::Modal`.
/// Each mode is converted via `convert_list` to `Vec<Effect>`. Any failed mode
/// strict-fails the whole modal spell — modes are not silently dropped.
fn convert_modal(
    modes: &[Actions],
    choose: ChooseSpec,
    idiom: &'static str,
) -> ConvResult<ActionsConversion> {
    convert_modal_with(modes, choose, Vec::new(), None, false, idiom)
}

/// CR 700.2 + CR 700.2d + CR 702.42: Modal converter with full metadata —
/// constraints (`NoRepeatThisTurn` / `NoRepeatThisGame`), entwine cost, and
/// `allow_repeat_modes`. Most callers go through `convert_modal`; the entwine
/// / hasn't-been-chosen / repeat-mode arms feed extra metadata in here.
fn convert_modal_with(
    modes: &[Actions],
    choose: ChooseSpec,
    constraints: Vec<ModalSelectionConstraint>,
    entwine_cost: Option<ManaCost>,
    allow_repeat_modes: bool,
    idiom: &'static str,
) -> ConvResult<ActionsConversion> {
    if modes.is_empty() {
        return Err(ConversionGap::MalformedIdiom {
            idiom,
            path: String::new(),
            detail: "empty modal mode list".into(),
        });
    }
    let mut out = Vec::with_capacity(modes.len());
    for m in modes {
        out.push(convert_list(m)?);
    }
    Ok(ActionsConversion::Modal {
        modes: out,
        choose,
        constraints,
        entwine_cost,
        allow_repeat_modes,
    })
}

/// CR 608.2c + CR 700.4: Flatten a single-element nested
/// `Action::If(inner_cond, inner_body)` / `Action::Unless(inner_cond,
/// inner_body)` body into a compound `AbilityCondition::And { conditions }`.
/// Recurses so arbitrarily deep nesting (e.g.
/// `If(c1, [If(c2, [If(c3, [body])])])`) flattens into a single segment with
/// `And { c1, c2, c3 }`. Multi-element bodies that happen to start with an
/// inner `If` cannot be flattened (the trailing actions wouldn't share the
/// inner gate), so we only collapse when the body is exactly one
/// `If` / `Unless` and the inner condition translates.
fn compound_nested_if(
    outer: AbilityCondition,
    body: &[Action],
) -> ConvResult<(AbilityCondition, Vec<Effect>)> {
    if let [head] = body {
        match head {
            Action::If(inner_cond, inner_body) => {
                let inner = condition::convert_ability(inner_cond)?;
                let combined = combine_and(outer, inner);
                return compound_nested_if(combined, inner_body);
            }
            Action::Unless(inner_cond, inner_body) => {
                let inner = condition::convert_ability_negated(inner_cond)?;
                let combined = combine_and(outer, inner);
                return compound_nested_if(combined, inner_body);
            }
            _ => {}
        }
    }
    let effects = convert_action_vec(body)?;
    Ok((outer, effects))
}

/// Combine two `AbilityCondition`s with `And` semantics, flattening when
/// either operand is already an `And` so we don't build a deeply nested
/// tree of single-element ands.
fn combine_and(left: AbilityCondition, right: AbilityCondition) -> AbilityCondition {
    let mut conditions = match left {
        AbilityCondition::And { conditions } => conditions,
        other => vec![other],
    };
    match right {
        AbilityCondition::And { conditions: more } => conditions.extend(more),
        other => conditions.push(other),
    }
    AbilityCondition::And { conditions }
}

/// Convert a flat `Vec<Action>` (from `Action::If` / `Action::IfElse` /
/// `Action::Unless` bodies) into an effect chain. Mid-list head-wrappers are
/// not re-recognized — those bodies are pure leaf actions per the schema.
fn convert_action_vec(actions: &[Action]) -> ConvResult<Vec<Effect>> {
    let mut out = Vec::with_capacity(actions.len());
    for a in actions {
        out.extend(convert_many(a)?);
    }
    Ok(out)
}

/// Convert one `Action` into one or more `Effect`s. Most actions emit a
/// single effect (delegating to `convert`). `Action::SearchLibrary` is the
/// principal multi-emit shape: a tutor expands into the engine's
/// `SearchLibrary → ChangeZone → [Shuffle]` chain (CR 701.23 + CR 701.24).
fn convert_many(a: &Action) -> ConvResult<Vec<Effect>> {
    match a {
        Action::SearchLibrary(actions) => convert_search_library(actions),
        // CR 110.1 + CR 603.7c + CR 614.12: "Exile target permanent until
        // [expiration]" — Banishing Light family. mtgish encodes this as a
        // single `ExilePermanentUntil` action, but the engine's primitive is
        // a two-step composition: an immediate `ChangeZone` to Exile + a
        // `CreateDelayedTrigger` that schedules the return when the
        // expiration condition fires. Mirrors the native parser's
        // earthbend / "exile target permanent" lowering at
        // `oracle_effect/mod.rs:1118-1161`.
        Action::ExilePermanentUntil(perm, expiration) => {
            let target = crate::convert::filter::convert_permanent(perm)?;
            let condition = expiration_to_delayed_trigger_condition(expiration)?;
            // The return effect references the *parent ability's* target —
            // the same permanent we just exiled. `TargetFilter::ParentTarget`
            // is the engine's name for that ref. Returns to the original
            // owner under their control (CR 110.2).
            let return_effect = Effect::ChangeZone {
                origin: Some(engine::types::zones::Zone::Exile),
                destination: engine::types::zones::Zone::Battlefield,
                target: TargetFilter::ParentTarget,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            };
            let return_ability = AbilityDefinition::new(AbilityKind::Spell, return_effect);
            Ok(vec![
                Effect::ChangeZone {
                    origin: None,
                    destination: engine::types::zones::Zone::Exile,
                    target,
                    owner_library: false,
                    enter_transformed: false,
                    under_your_control: false,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
                Effect::CreateDelayedTrigger {
                    condition,
                    effect: Box::new(return_ability),
                    uses_tracked_set: false,
                },
            ])
        }
        // CR 119.1 + CR 119.3: `Action::PlayerAction(You, inner)` is a
        // transparent passthrough at the multi-emit layer too — propagate
        // multi-effect inner shapes (notably SearchLibrary) instead of
        // collapsing through the single-Effect `convert`.
        Action::PlayerAction(p, inner) if matches!(**p, Player::You) => convert_many(inner),
        // CR 119.1 + CR 119.3: Plural-body sibling passthrough for
        // `Action::PlayerActions(You, body)` — flatten via the multi-emit
        // path so SearchLibrary expansion and other multi-effect shapes
        // inside the body propagate correctly.
        Action::PlayerActions(p, body) if matches!(**p, Player::You) => {
            let mut out = Vec::new();
            for inner in body {
                out.extend(convert_many(inner)?);
            }
            Ok(out)
        }
        _ => Ok(vec![convert(a)?]),
    }
}

/// CR 603.7 + CR 603.7c: Map an mtgish `Expiration` to the engine's
/// `DelayedTriggerCondition`. Most `ExilePermanentUntil` cards (the
/// CR 117.6 + CR 700.4: Map an action-shaped `Cost` (mtgish encodes some
/// state-change actions like `PutACounterOfType`, `SacrificePermanent`,
/// `PutPermanentIntoItsOwnersHand`, `AttachPermanentToPermanent` under the
/// `Cost` type when used inside `Action::MustCost`) to its `Action`
/// equivalent. This lets `[MustCost(c), If(CostWasPaid, body)]` lower as a
/// flat chain `[c_as_action_effect, ...body_effects]` — `CostWasPaid` is
/// tautologically true for action-shaped costs (they always succeed once
/// reached). Real payment costs (Mana / Life / Speed / Energy) and
/// state-actions without a matching `Action` analog (or with a different
/// arity) strict-fail.
fn mustcost_action_equivalent(
    cost: &crate::schema::types::Cost,
) -> ConvResult<crate::schema::types::Action> {
    use crate::schema::types::{Action as A, Cost as C};
    Ok(match cost {
        C::PutACounterOfTypeOnPermanent(ct, perm) => {
            A::PutACounterOfTypeOnPermanent(ct.clone(), perm.clone())
        }
        C::PutPermanentIntoItsOwnersHand(perm) => A::PutPermanentIntoItsOwnersHand(perm.clone()),
        C::SacrificePermanent(perm) => A::SacrificePermanent(perm.clone()),
        C::AttachPermanentToPermanent(p1, p2) => {
            A::AttachPermanentToPermanent(p1.clone(), p2.clone())
        }
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "Action",
                needed_variant: format!("MustCost/cost_as_action: {other:?}"),
            });
        }
    })
}

/// Banishing Light family) use `UntilItLeavesTheBattlefield` — "It" being
/// the source object that created the delayed trigger. Other Expirations
/// strict-fail until phase-tied delayed-trigger conditions land
/// (`UntilEndOfTurn`, `UntilEndOfCombat`, etc. need `AtNextPhase` mapping
/// with the right Phase value, which is a separate bridging task).
fn expiration_to_delayed_trigger_condition(
    expiration: &crate::schema::types::Expiration,
) -> ConvResult<engine::types::ability::DelayedTriggerCondition> {
    use crate::schema::types::Expiration as E;
    use engine::types::ability::DelayedTriggerCondition as D;
    match expiration {
        // CR 603.7c: "until ~ leaves the battlefield" — the source object
        // that created the delayed trigger leaving play. `SelfRef` on a
        // delayed trigger filter resolves against that creating source.
        E::UntilItLeavesTheBattlefield => Ok(D::WhenLeavesPlayFiltered {
            filter: TargetFilter::SelfRef,
        }),
        // CR 603.7c: "until [permanent] leaves the battlefield" — the
        // dominant `ExilePermanentUntil` Expiration. mtgish encodes the
        // source enchantment as `Permanent::ThisPermanent` here, which
        // collapses to the SelfRef filter (same semantics as
        // `UntilItLeavesTheBattlefield`). Non-source permanent refs (target
        // permanents, host, attached) need bespoke filters and strict-fail.
        E::UntilPermanentLeavesBattlefield(perm) => match &**perm {
            crate::schema::types::Permanent::ThisPermanent
            | crate::schema::types::Permanent::Self_It => Ok(D::WhenLeavesPlayFiltered {
                filter: TargetFilter::SelfRef,
            }),
            other => Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "DelayedTriggerCondition",
                needed_variant: format!("Expiration: UntilPermanentLeavesBattlefield/{other:?}"),
            }),
        },
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "DelayedTriggerCondition",
            needed_variant: format!("Expiration: {other:?}"),
        }),
    }
}

/// Convert one `Action` to an `Effect`. Strict-failure: unsupported variants
/// return `ConversionGap::UnknownVariant` (with the serde tag).
pub fn convert(a: &Action) -> ConvResult<Effect> {
    Ok(match a {
        Action::DrawACard => Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Action::DrawNumberCards(n) => Effect::Draw {
            count: quantity::convert(n)?,
            target: TargetFilter::Controller,
        },
        Action::GainLife(n) => Effect::GainLife {
            amount: quantity::convert(n)?,
            player: GainLifePlayer::Controller,
        },
        Action::LoseLife(n) => Effect::LoseLife {
            amount: quantity::convert(n)?,
            target: None,
        },
        // CR 701.14a: "[Subject] fights [target]." Each creature deals damage
        // equal to its power to the other. mtgish encodes the subject as the
        // first `Permanent` argument and the target as the second; the engine
        // mirrors this with `Effect::Fight { subject, target }`. The default
        // subject `SelfRef` (CR 701.14a — "the source creature") is set when
        // mtgish names `ThisPermanent`/`Self_It`; the
        // `AttachedTo`/`HostPermanent` cases collapse to `SelfRef` via
        // `convert_permanent` (the equipped/enchanted-creature axis is the
        // source's own attachment binding, which the engine resolves at
        // `SelfRef`).
        Action::HaveCreaturesFight(subject, target) => Effect::Fight {
            subject: convert_permanent(subject)?,
            target: convert_permanent(target)?,
        },
        Action::PermanentDealsDamage(_source, amount, recipient) => Effect::DealDamage {
            amount: quantity::convert(amount)?,
            target: damage_recipient_to_filter(recipient)?,
            damage_source: None,
        },
        // CR 119.3 + CR 115.1: "[This spell] deals N damage to <recipient>"
        // — the spell-source variant of PermanentDealsDamage. The source-
        // spell argument is dropped because Effect::DealDamage's resolver
        // already binds the damage source to the spell on the stack.
        Action::SpellDealsDamage(_source, amount, recipient) => Effect::DealDamage {
            amount: quantity::convert(amount)?,
            target: damage_recipient_to_filter(recipient)?,
            damage_source: None,
        },
        // CR 120.1 + CR 603.7c + CR 603.10a: "When this dies, [it] deals N
        // damage to <recipient>." The damage source is the dying permanent,
        // bound implicitly via the dies-trigger event context. `damage_source:
        // None` falls through to the ability source (which IS the dying
        // permanent for a dies-trigger on `ThisPermanent`).
        Action::DeadPermanentDealsDamage(amount, recipient) => Effect::DealDamage {
            amount: quantity::convert(amount)?,
            target: damage_recipient_to_filter(recipient)?,
            damage_source: None,
        },

        Action::DestroyPermanent(p) => Effect::Destroy {
            target: convert_permanent(p)?,
            cant_regenerate: false,
        },
        Action::DestroyPermanentNoRegen(p) => Effect::Destroy {
            target: convert_permanent(p)?,
            cant_regenerate: true,
        },
        Action::DestroyAPermanentAtRandom(filter) => Effect::Destroy {
            target: convert_permanents(filter)?,
            cant_regenerate: false,
        },
        Action::DestroyAPermanentNoRegen(filter) => Effect::Destroy {
            target: convert_permanents(filter)?,
            cant_regenerate: true,
        },
        Action::DestroyEachPermanent(filter) => Effect::Destroy {
            target: convert_permanents(filter)?,
            cant_regenerate: false,
        },
        Action::DestroyEachPermanentNoRegen(filter) => Effect::Destroy {
            target: convert_permanents(filter)?,
            cant_regenerate: true,
        },
        Action::TapPermanent(p) => Effect::Tap {
            target: convert_permanent(p)?,
        },
        Action::UntapPermanent(p) => Effect::Untap {
            target: convert_permanent(p)?,
        },

        // CR 701.26: Mass tap — "Tap each <filter>" (Sleep, Cryptic Command-class).
        // Mirrors `TapPermanent`; multi-match `TargetFilter` selects the set.
        Action::TapEachPermanent(filter) => Effect::Tap {
            target: convert_permanents(filter)?,
        },
        // CR 701.26: Mass untap — "Untap each <filter>" (Wake the Dead, Awakening-class).
        Action::UntapEachPermanent(filter) => Effect::Untap {
            target: convert_permanents(filter)?,
        },
        Action::DiscardACard => Effect::DiscardCard {
            count: 1,
            target: TargetFilter::Controller,
        },
        // CR 701.8: Discard. `Effect::DiscardCard` carries a fixed `u32` count
        // today (sibling-divergence with `Effect::Mill { count: QuantityExpr }`
        // pending engine parameterization), so dynamic counts strict-fail with
        // an explicit prerequisite. Static integer counts cover the bulk of
        // observed cards (n=166).
        Action::DiscardNumberCards(n) => {
            let qty = quantity::convert(n)?;
            match qty {
                QuantityExpr::Fixed { value } if value >= 0 => Effect::DiscardCard {
                    count: value as u32,
                    target: TargetFilter::Controller,
                },
                _ => {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "Effect::DiscardCard",
                        needed_variant: "count: QuantityExpr (lift from u32)".into(),
                    });
                }
            }
        }
        Action::MillNumberCards(n) => Effect::Mill {
            count: quantity::convert(n)?,
            target: TargetFilter::Controller,
            destination: engine::types::zones::Zone::Graveyard,
        },
        Action::Scry(n) => Effect::Scry {
            count: quantity::convert(n)?,
            target: TargetFilter::Controller,
        },
        Action::Surveil(n) => Effect::Surveil {
            count: quantity::convert(n)?,
            target: TargetFilter::Controller,
        },

        // CR 701.20e + CR 608.2c: "Look at the top N cards of your library.
        // Then [dispositions]." Maps onto a single `Effect::Dig` whose
        // (keep_count, up_to, filter, destination, rest_destination, reveal)
        // fields are derived from the disposition sequence. Unrecognized
        // combinations strict-fail with `EnginePrerequisiteMissing`.
        Action::LookAtTheTopNumberCardsOfLibrary(n, dispositions) => {
            convert_look_at_top(quantity::convert(n)?, dispositions)?
        }

        // CR 122.1: Counters.
        Action::PutACounterOfTypeOnPermanent(ct, target) => Effect::AddCounter {
            counter_type: counter_type_name(ct),
            count: QuantityExpr::Fixed { value: 1 },
            target: convert_permanent(target)?,
        },
        Action::PutACounterOfTypeOnAPermanent(ct, filter) => Effect::AddCounter {
            counter_type: counter_type_name(ct),
            count: QuantityExpr::Fixed { value: 1 },
            target: convert_permanents(filter)?,
        },
        // CR 122.1: Counters — N counters of a specific type on a single
        // target permanent. Mirrors `PutACounterOfTypeOnPermanent` above
        // with a dynamic count derived from `quantity::convert(n)`.
        Action::PutNumberCountersOfTypeOnPermanent(n, ct, target) => Effect::AddCounter {
            counter_type: counter_type_name(ct),
            count: quantity::convert(n)?,
            target: convert_permanent(target)?,
        },
        Action::RemoveACounterOfTypeFromPermanent(ct, target) => Effect::RemoveCounter {
            counter_type: counter_type_name(ct),
            count: 1,
            target: convert_permanent(target)?,
        },

        // CR 701.10: Exile via ChangeZone.
        Action::ExilePermanent(target) => Effect::ChangeZone {
            origin: Some(engine::types::zones::Zone::Battlefield),
            destination: engine::types::zones::Zone::Exile,
            target: convert_permanent(target)?,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },
        Action::ExileAPermanent(filter) => Effect::ChangeZone {
            origin: Some(engine::types::zones::Zone::Battlefield),
            destination: engine::types::zones::Zone::Exile,
            target: convert_permanents(filter)?,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },

        // CR 701.13 + CR 400.7: Mass exile — "Exile each <filter>"
        // (Planar Cleansing, Akroma's Vengeance-class). Mirrors
        // `ExileAPermanent` — same `Effect::ChangeZone { Battlefield → Exile }`
        // shape; the multi-match `TargetFilter` from `convert_permanents`
        // selects the full set at resolution.
        Action::ExileEachPermanent(filter) => Effect::ChangeZone {
            origin: Some(engine::types::zones::Zone::Battlefield),
            destination: engine::types::zones::Zone::Exile,
            target: convert_permanents(filter)?,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },

        // CR 701.18: Return to owner's hand (Bounce).
        Action::ReturnAnyNumberOfPermanentsToTheirOwnersHands(filter) => Effect::Bounce {
            target: convert_permanents(filter)?,
            destination: None,
        },
        // CR 400.7: Return to owner's hand — mass variant.
        // "Return each <filter> to its owner's hand." Mirrors
        // `ReturnAnyNumberOfPermanentsToTheirOwnersHands` — same engine
        // primitive (`Effect::Bounce`); the multi-match `TargetFilter`
        // from `convert_permanents` selects the full set at resolution.
        Action::PutEachPermanentIntoItsOwnersHand(filter) => Effect::Bounce {
            target: convert_permanents(filter)?,
            destination: None,
        },

        // CR 701.32: Sacrifice an effect on a permanent (rare; usually via cost).
        Action::SacrificeAPermanent(filter) => Effect::Sacrifice {
            target: convert_permanents(filter)?,
            count: QuantityExpr::Fixed { value: 1 },
        },
        // CR 701.16a: "Sacrifice N <filter>" — N may be a literal or any
        // dynamic quantity (Variable, ObjectCount, etc.). Lowers onto the
        // engine's `count: QuantityExpr` slot directly.
        Action::SacrificeNumberPermanents(n, filter) => Effect::Sacrifice {
            target: convert_permanents(filter)?,
            count: quantity::convert(n)?,
        },

        // CR 701.27: Counter spell.
        Action::CounterSpell(_spell) => Effect::Counter {
            target: TargetFilter::StackSpell,
            source_static: None,
            unless_payment: None,
        },

        // CR 800.4 / CR 110.2: Gain control.
        Action::GainControlOfPermanent(p) => Effect::GainControl {
            target: convert_permanent(p)?,
        },

        // CR 701.19: Regenerate — create a regeneration shield on a single
        // permanent (Regrowth-on-self / Asceticism / regenerate-target-creature
        // patterns). Same engine slot as the Effect::Regenerate variant the
        // native parser emits for "regenerate target creature".
        Action::RegeneratePermanent(p) => Effect::Regenerate {
            target: convert_permanent(p)?,
        },

        // CR 701.19: Regenerate — mass / each-of-filter variant ("Regenerate
        // each Sliver", "Regenerate each creature you control"). Engine
        // Effect::Regenerate's `target` slot is a `TargetFilter`, so the
        // multi-match filter from `convert_permanents` selects the full set
        // at resolution time. Mirrors the singular variant above.
        Action::RegenerateEachPermanent(filter) => Effect::Regenerate {
            target: convert_permanents(filter)?,
        },

        // CR 400.7 + CR 611.2c: "Return [target permanent] to its owner's
        // hand." Engine primitive is `Effect::Bounce` (battlefield → hand);
        // already used above for the multi-permanent variant
        // `ReturnAnyNumberOfPermanentsToTheirOwnersHands`. The `Permanent`
        // payload is the target ref.
        Action::PutPermanentIntoItsOwnersHand(p) => Effect::Bounce {
            target: convert_permanent(p)?,
            destination: None,
        },

        // CR 401.1 + CR 608.2c: "Put [target permanent] on top of its owner's
        // library." Engine primitive is `Effect::PutAtLibraryPosition` with
        // `position: Top`, the precise-placement counterpart to
        // `Effect::ChangeZone { destination: Library }` (which auto-shuffles
        // per CR 401.3). Bounce-to-library form covers cards like Hex, Memory
        // Lapse-on-resolve, and tuck triggers.
        Action::PutPermanentOnTopOfOwnersLibrary(p) => Effect::PutAtLibraryPosition {
            target: convert_permanent(p)?,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::Top,
        },

        // CR 400.7: "Return target card from your graveyard to your hand"
        // (Regrowth / Raise Dead / Stitcher's Supplier-style reanimation
        // into hand). Maps to the same `Effect::ChangeZone { Graveyard →
        // Hand }` shape the native parser emits for "return target ... from
        // graveyard to ... hand" (oracle_effect/imperative.rs:738-754).
        // Replacements are not part of a hand-destination return, so no
        // enter-tapped axis applies.
        Action::PutGraveyardCardIntoHand(card) => Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Hand,
            target: card_in_graveyard_to_filter(card)?,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },

        // CR 400.7: Mass return-from-graveyard-to-hand — "Return each <filter>
        // card from your graveyard to your hand" (Haunting Voyage / Breath of
        // Life-class mass reanimate-to-hand). Mirrors
        // `PutGraveyardCardIntoHand`; same `Effect::ChangeZone { Graveyard →
        // Hand }` shape, with the multi-match `TargetFilter` derived from the
        // plural `CardsInGraveyard` filter.
        Action::PutEachGraveyardCardIntoHand(cards) => Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Hand,
            target: filter_mod::cards_in_graveyard_to_filter(cards)?,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },

        // CR 400.7 + CR 614.12: Mass reanimate from a specific player's
        // graveyard to the battlefield ("Return all artifact and enchantment
        // cards from your graveyard to the battlefield" — Brilliant Restoration
        // / Living Death-style). The Player axis combines with the CardsIn-
        // Graveyard filter via TargetFilter::And. Replacements decode through
        // the shared `extract_enter_replacements` helper.
        Action::ReturnEachCardFromPlayersGraveyardToBattlefield(cards, player, repls) => {
            let r = extract_enter_replacements(repls)?;
            let cards_filter = filter_mod::cards_in_graveyard_to_filter(cards)?;
            let player_ctrl = filter_mod::player_to_controller(player)?;
            let owner_filter = TargetFilter::Typed(
                engine::types::ability::TypedFilter::default().controller(player_ctrl),
            );
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::And {
                    filters: vec![cards_filter, owner_filter],
                },
                owner_library: false,
                enter_transformed: r.enter_transformed,
                under_your_control: r.under_your_control,
                enter_tapped: r.enter_tapped,
                enters_attacking: r.enters_attacking,
                up_to: false,
                enter_with_counters: vec![],
            }
        }

        // CR 400.7 + CR 614.1 + CR 614.12: Reanimate target — "Return target
        // creature card from your graveyard to the battlefield [under your
        // control]" (Reanimate / Animate Dead / Beacon of Unrest / Ashen
        // Powder class). Maps to `Effect::ChangeZone { Graveyard →
        // Battlefield }`, mirroring the native parser's `ReturnToBattlefield`
        // AST (oracle_effect/imperative.rs:728-737). The accompanying
        // replacement list is decoded by `extract_enter_replacements` into
        // typed flags (CR 614.12); any unrecognized replacement strict-fails.
        Action::PutGraveyardCardOntoBattlefield(card, repls) => {
            let r = extract_enter_replacements(repls)?;
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: card_in_graveyard_to_filter(card)?,
                owner_library: false,
                enter_transformed: r.enter_transformed,
                under_your_control: r.under_your_control,
                enter_tapped: r.enter_tapped,
                enters_attacking: r.enters_attacking,
                up_to: false,
                enter_with_counters: vec![],
            }
        }

        // CR 119.1 + CR 119.3: Scoped player action — "[player] does [Action]".
        // For Player::You we transparently delegate to the inner action since
        // most Effect variants already default `target: Controller`.
        //
        // CR 115.2 + CR 601.2c: For target-player references
        // (`Ref_TargetPlayer*`), the announced player is wired onto the
        // inner Effect's player-target slot via `apply_player_target`.
        // The engine extracts the chosen player at resolution time via
        // `ResolvedAbility::target_player()`. Other non-You scopes that
        // can't be expressed as a per-effect target (predicate-filtered,
        // dynamic anaphor, etc.) strict-fail.
        Action::PlayerAction(player, inner) => match &**player {
            Player::You => convert(inner)?,
            other => {
                if let Some(filter) = player_to_target_filter(other) {
                    let inner_effect = convert(inner)?;
                    apply_player_target(inner_effect, filter)?
                } else {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "Action::PlayerAction",
                        path: String::new(),
                        detail: format!("non-You player: {other:?}"),
                    });
                }
            }
        },

        // CR 613 + CR 514.2: Temporary continuous effect on a single
        // permanent — Giant Growth pattern (gets +N/+M and gains [keyword]
        // until end of turn). Maps to Effect::GenericEffect carrying a
        // StaticDefinition + Duration so the static is auto-cleaned at
        // the expiration boundary.
        Action::CreatePermanentLayerEffectUntil(target, effects, expiration) => {
            let affected = convert_permanent(target)?;
            build_layer_effect_until(affected, effects, expiration)?
        }

        // CR 613 + CR 514.2: Mass version — "Each [filter] gets +N/+M and
        // gains [keyword] until end of turn" (Overrun, Falter family).
        // Same pipeline as the singular variant; the affected filter is
        // the multi-target Permanents predicate.
        Action::CreateEachPermanentLayerEffectUntil(filter, effects, expiration) => {
            let affected = convert_permanents(filter)?;
            build_layer_effect_until(affected, effects, expiration)?
        }

        // CR 113.6 + CR 514.2: Temporary rules-modifying effect on a single
        // permanent — "Target creature can't attack this turn" / "Target
        // creature must attack this turn" pattern. Mirrors
        // `CreatePermanentLayerEffectUntil` but each `PermanentRule` produces
        // a typed `StaticDefinition` (CantAttack/CantBlock/MustAttack/…) via
        // `convert_permanent_rule`. The whole bundle is housed in a single
        // `Effect::GenericEffect` so the duration cleans every static at the
        // expiration boundary.
        Action::CreatePermanentRuleEffectUntil(target, rules, expiration) => {
            let affected = convert_permanent(target)?;
            build_rule_effect_until(affected, rules, expiration)?
        }

        // CR 113.6 + CR 514.2: Mass version — "Creatures your opponents
        // control can't block this turn" (Falter / Briarhorn-bound family).
        Action::CreateEachPermanentRuleEffectUntil(filter, rules, expiration) => {
            let affected = convert_permanents(filter)?;
            build_rule_effect_until(affected, rules, expiration)?
        }

        // CR 111.1 + CR 111.5: Token creation. Singleton token specs map
        // 1:1 onto Effect::Token; multi-spec specs strict-fail (a single
        // Effect can carry only one token shape). Predefined artifact
        // tokens, generic CreatureToken, and the NumberTokens multiplier
        // are handled by `token::convert`.
        Action::CreateTokens(specs) => {
            let [single] = specs.as_slice() else {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Action::CreateTokens",
                    path: String::new(),
                    detail: format!("expected single token spec, got {}", specs.len()),
                });
            };
            token::convert(single)?
        }

        // CR 605.1 + CR 106.1: Mana production. Atom and simple compositions
        // (And/Or, AnyManaColor, ChosenColor) map to ManaProduction; dynamic
        // shapes (color-of-permanent etc.) strict-fail in mana::convert_produce.
        Action::AddMana(produce) => Effect::Mana {
            produced: mana::convert_produce(produce)?,
            restrictions: Vec::new(),
            grants: Vec::new(),
            expiry: None,
            target: None,
        },
        // CR 717.1: The monarch designation. The acting player becomes the
        // monarch (singleton — replaces any existing monarch), opting into
        // the end-step draw and the take-damage-yields-monarchy interactions.
        Action::BecomeTheMonarch => Effect::BecomeMonarch,

        // CR 100.6 / "Time Travel" planar mechanic: travel to an adjacent
        // plane / step a time counter. Engine slot is the zero-arg
        // `Effect::TimeTravel`; planar mechanic semantics live there.
        Action::TimeTravel => Effect::TimeTravel,

        // CR 701.27: Proliferate. Each permanent and player with a counter
        // on it has the option to receive an additional counter of one of
        // its existing counter kinds. Engine slot is the zero-arg
        // `Effect::Proliferate`. Multi-step proliferate
        // (`ProliferateNumberTimes(n)`) needs a parameterized engine slot
        // and strict-fails today.
        Action::Proliferate => Effect::Proliferate,

        // CR 701.30: Populate. Create a copy token of a creature token you
        // control. Engine slot `Effect::Populate` (zero-arg) covers the
        // basic case; `PopulateNumberTimes` and `PopulateWithFlags` need
        // engine extensions and strict-fail.
        Action::Populate => Effect::Populate,

        // CR 605.1 + CR 106.1b: Dynamic-count mana production —
        // "Add N <color> mana" / "Add N C". Maps to `Effect::Mana` with a
        // dynamically-counted `ManaProduction` variant via
        // `mana::convert_repeated_produce`. Cabal Coffers / Crypt of the
        // Eternals / Black Lotus class.
        Action::AddManaRepeated(produce, n) => {
            let count = quantity::convert(n)?;
            Effect::Mana {
                produced: mana::convert_repeated_produce(produce, count)?,
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            }
        }

        // CR 605.1 + CR 106.1 + CR 106.4: Mana production with usage
        // restrictions ("add {G}. Spend this mana only to cast creature
        // spells"). Maps the supported `ManaUseModifier` subset to
        // `ManaSpendRestriction`; unsupported modifiers (cross-type OR,
        // permanent-spell-effect grants, "don't lose at end of phase",
        // trigger-on-spend) strict-fail to surface them in the report.
        Action::AddManaWithModifiers(produce, modifier) => {
            let restrictions = convert_mana_use_modifier(modifier)?;
            Effect::Mana {
                produced: mana::convert_produce(produce)?,
                restrictions,
                grants: Vec::new(),
                expiry: None,
                target: None,
            }
        }

        // CR 107.14: "{E} represents one energy counter." `Effect::GainEnergy`
        // takes a static `u32` (the count of {E} symbols in Oracle text), so
        // we accept literal integer quantities and strict-fail dynamic ones —
        // the engine slot for "gain X energy" / "gain energy equal to ..."
        // does not yet exist (would require widening `GainEnergy.amount` to
        // `QuantityExpr`, which is an engine extension out of scope here).
        Action::GetEnergy(n) => match quantity::convert(n)? {
            QuantityExpr::Fixed { value } if value >= 0 => Effect::GainEnergy {
                amount: value as u32,
            },
            other => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "Effect::GainEnergy",
                    needed_variant: format!(
                        "GainEnergy.amount widened to QuantityExpr — got dynamic {other:?}"
                    ),
                });
            }
        },

        // CR 701.21: Sacrifice a single targeted permanent. Mirrors
        // `SacrificeAPermanent(filter)` (which takes a multi-match
        // `Permanents` filter); this variant takes a singular `Permanent`
        // ref ("sacrifice ~", "sacrifice that creature").
        Action::SacrificePermanent(p) => Effect::Sacrifice {
            target: convert_permanent(p)?,
            count: QuantityExpr::Fixed { value: 1 },
        },

        // CR 701.13 + CR 400.7: Exile a single targeted card from a
        // graveyard. Mirrors `PutGraveyardCardIntoHand` / `PutGraveyardCard
        // OntoBattlefield` — same Graveyard-origin `Effect::ChangeZone`
        // shape with `destination: Exile`. The native parser emits the
        // same shape for "exile target card from a graveyard" lines.
        Action::ExileGraveyardCard(card) => Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Exile,
            target: card_in_graveyard_to_filter(card)?,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },

        // CR 122.1: Mass counter placement — "Put a [counter] on each
        // [filter]." Mirrors `PutACounterOfTypeOnAPermanent` (which selects
        // one matching permanent); the each-variant uses the same filter
        // semantics — the multi-match `TargetFilter` from
        // `convert_permanents` selects the full set at resolution time.
        Action::PutACounterOfTypeOnEachPermanent(ct, filter) => Effect::AddCounter {
            counter_type: counter_type_name(ct),
            count: QuantityExpr::Fixed { value: 1 },
            target: convert_permanents(filter)?,
        },

        // CR 701.23 + CR 701.24: SearchLibrary is intrinsically multi-effect
        // — it expands to `SearchLibrary → ChangeZone → [Shuffle]`. Single-
        // effect callers cannot consume it; route through `convert_many` /
        // `convert_list` / `convert_action_vec` instead.
        Action::SearchLibrary(_) => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "Action::SearchLibrary",
                path: String::new(),
                detail: "SearchLibrary is multi-effect; caller must use convert_many".into(),
            });
        }

        // CR 615.1 + CR 514.2: Until-end-of-turn damage prevention/redirection
        // replacement. The engine's `Effect::PreventDamage` is the single
        // primitive for "prevent damage" shields with EOT cleanup; the
        // converter delegates the event/action decomposition to
        // `convert::replacement` and returns the prevention effect.
        Action::CreateReplaceWouldDealDamageUntil(event, actions, expiration) => {
            crate::convert::replacement::convert_create_replace_would_deal_damage_until(
                event, actions, expiration,
            )?
        }

        // CR 615.1 + CR 514.2: Future ("next time / next N damage") damage
        // prevention replacement. Same primitive as the Until variant but
        // the prevention amount is packed into the event side.
        Action::CreateFutureReplaceWouldDealDamage(event, actions) => {
            crate::convert::replacement::convert_create_future_replace_would_deal_damage(
                event, actions,
            )?
        }

        // CR 603.7: Delayed triggered ability — "When/Whenever/At [next event],
        // [body]." The `FutureTrigger` describes when the delayed trigger
        // fires; the body is the effect run at that time. Maps onto
        // `Effect::CreateDelayedTrigger` whose embedded `AbilityDefinition`
        // carries the body and whose `condition` is the lowered
        // `DelayedTriggerCondition`.
        Action::CreateFutureTrigger(future, body) => {
            let condition = future_trigger_to_condition(future)?;
            let conv = convert_actions(body)?;
            let effect_ad =
                crate::convert::build_ability_from_actions(AbilityKind::Spell, None, conv)?;
            Effect::CreateDelayedTrigger {
                condition,
                effect: Box::new(effect_ad),
                uses_tracked_set: false,
            }
        }

        // CR 114.1 + CR 114.2 + CR 114.4: "[Player] gets an emblem with
        // [abilities]." Emit `Effect::CreateEmblem` carrying the parsed
        // statics and triggers. Inner Rule shapes that require the
        // condition-decorating recursion in `convert_rule` (If/Unless/IfElse,
        // graveyard-grant, etc.) strict-fail with a typed gap so the report
        // surfaces them — handling those would require threading `&mut Ctx`
        // through the entire action-conversion API. The covered shapes
        // (TriggerA / TriggerI / EachPermanentLayerEffect /
        // PermanentLayerEffect) carry the dominant emblem-body patterns
        // (planeswalker ultimates).
        Action::GetAnEmblem(rules) => convert_emblem_body(rules)?,

        // CR 701.27: Transform — flip a double-faced permanent. Maps directly
        // to `Effect::Transform { target }`; the engine's resolver enforces
        // CR 701.27c (only DFC permanents) and CR 701.27e (once-per-stack
        // ordering) at resolution time.
        Action::TransformPermanent(p) => Effect::Transform {
            target: convert_permanent(p)?,
        },

        // CR 400.7 + CR 611.2c: "Return a [filter] to its owner's hand" — single-
        // match filter variant of the existing `PutEachPermanentIntoItsOwnersHand`
        // arm. Same engine slot (`Effect::Bounce`) — the multi-match
        // `TargetFilter` from `convert_permanents` selects the resolution-time
        // permanent.
        Action::PutAPermanentIntoItsOwnersHand(filter) => Effect::Bounce {
            target: convert_permanents(filter)?,
            destination: None,
        },

        // CR 701.3a + CR 701.3b: Attach. Engine's `Effect::Attach { target }`
        // expresses "the source attaches to <target>". The mtgish shape is
        // `(first, second)` = "attach <first> to <second>". When `first` is the
        // source (`ThisPermanent`), the mapping is direct: `target =
        // convert_permanent(second)`. Other shapes (e.g., "attach target
        // Equipment to target creature" — Auriok Windwalker, Amy Rose) involve
        // a non-source mover and have no engine slot today; strict-fail with
        // an explicit prerequisite so the gap surfaces.
        Action::AttachPermanentToPermanent(first, second) => match &**first {
            Permanent::ThisPermanent => Effect::Attach {
                target: convert_permanent(second)?,
            },
            other => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "Effect::Attach",
                    needed_variant: format!(
                        "non-source mover: attach {other:?} to <target> (Effect::Attach \
                         takes only one target — needs an `attached_object` slot)"
                    ),
                });
            }
        },

        // CR 110.2 + CR 613.1 (Layer 2) + CR 800.4: Temporary control change
        // — "Gain control of [permanent] until [expiration]." Maps to
        // `Effect::GenericEffect` carrying a `StaticDefinition` whose layer-2
        // `ChangeController` modification flips the controller for the
        // duration. Mirrors the Act of Treason pattern documented in the CR
        // 613.1 layering example. The non-Until counterpart
        // (`Action::GainControlOfPermanent`) maps to the dedicated
        // `Effect::GainControl` slot above (a permanent control change with no
        // automatic cleanup).
        Action::GainControlOfPermanentUntil(target, expiration) => {
            let affected = convert_permanent(target)?;
            let static_def = StaticDefinition::new(StaticMode::Continuous)
                .affected(affected.clone())
                .modifications(vec![ContinuousModification::ChangeController]);
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(static_effect::expiration_to_duration(expiration)?),
                target: Some(affected),
            }
        }

        // CR 613 + CR 611.2: Continuous static effect on a permanent with no
        // explicit expiration — applies to the affected permanent indefinitely
        // (until the source leaves play / is removed). Common in saga chapters
        // and emblem-spawning permanent buffs (Awakening of Vitu-Ghazi class:
        // "Target land becomes a 9/9 Elemental creature with haste"). Mirrors
        // `CreatePermanentLayerEffectUntil` but stamps `Duration::Permanent`
        // so the engine's default UntilEndOfTurn fallback (CR 611.2b) does
        // not strip the effect.
        Action::CreatePermanentLayerEffect(target, effects) => {
            let affected = convert_permanent(target)?;
            let mut mods = Vec::new();
            for eff in effects {
                mods.extend(static_effect::convert_layer_effect_dynamic(eff)?);
            }
            if mods.is_empty() {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Action::CreatePermanentLayerEffect",
                    path: String::new(),
                    detail: "empty modification list".into(),
                });
            }
            let static_def = StaticDefinition::new(StaticMode::Continuous)
                .affected(affected.clone())
                .modifications(mods);
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::Permanent),
                target: Some(affected),
            }
        }

        // CR 613.10 + CR 611.2: Per-player continuous static effect with an
        // expiration. Mirrors `Rule::PlayerEffect` (the printed-static form)
        // but lifted to an Action so it can fire as a one-shot resolution.
        // Each `PlayerEffect` lowers to a `StaticDefinition` whose `affected`
        // resolves to the named player; the bundle is housed in a single
        // `GenericEffect` so the duration cleans every static at the
        // expiration boundary. Strict-fails through `apply_for_player` if any
        // inner `PlayerEffect` lacks an engine `StaticMode` mapping.
        Action::CreatePlayerEffectUntil(player, effects, expiration) => {
            let mut statics = Vec::new();
            crate::convert::player_effect::apply_for_player(player, effects, &mut statics)?;
            Effect::GenericEffect {
                static_abilities: statics,
                duration: Some(static_effect::expiration_to_duration(expiration)?),
                target: None,
            }
        }

        // CR 400.7 + CR 701.13: "Put [exiled card] onto the battlefield [under
        // your control]." The Flicker / Acrobatic Maneuver / blink class —
        // typically paired with a preceding `Action::Exile` whose target is
        // referenced here via `CardInExile::TheLastExiledCard` /
        // `Ref_TargetExiledCard`. Maps to `Effect::ChangeZone { Exile →
        // Battlefield }` mirroring the native parser's "return that exiled
        // card to the battlefield" idiom. Replacements (enter-tapped /
        // enter-transformed / enter-attacking / enters-under-your-control)
        // decode through the shared `extract_enter_replacements` helper.
        Action::PutExiledCardOntoBattlefield(card, repls) => {
            let r = extract_enter_replacements(repls)?;
            Effect::ChangeZone {
                origin: Some(Zone::Exile),
                destination: Zone::Battlefield,
                target: card_in_exile_to_filter(card)?,
                owner_library: false,
                enter_transformed: r.enter_transformed,
                under_your_control: r.under_your_control,
                enter_tapped: r.enter_tapped,
                enters_attacking: r.enters_attacking,
                up_to: false,
                enter_with_counters: vec![],
            }
        }

        // CR 400.7: "Return [card] from your graveyard to your hand" — same
        // `Effect::ChangeZone { Graveyard → Hand }` shape as
        // `Action::PutGraveyardCardIntoHand` above (mtgish encodes the same
        // rules-fact under both names). Hand-destination returns carry no
        // ETB-replacement axis.
        Action::ReturnGraveyardCardToHand(card) => Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Hand,
            target: card_in_graveyard_to_filter(card)?,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },

        // CR 111.1 + CR 111.5: Token creation with attached `TokenFlag`s.
        // Single-spec multi-flag shape — multi-spec strict-fails through the
        // same single-spec gate as `Action::CreateTokens`. Each `TokenFlag`
        // lowers onto an existing `Effect::Token` slot (`tapped`,
        // `enters_attacking`, `attach_to`, `enter_with_counters`); flags that
        // don't reduce to those slots strict-fail with an explicit
        // prerequisite.
        Action::CreateTokensWithFlags(specs, flags) => {
            let [single] = specs.as_slice() else {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Action::CreateTokensWithFlags",
                    path: String::new(),
                    detail: format!("expected single token spec, got {}", specs.len()),
                });
            };
            apply_token_flags(token::convert(single)?, flags)?
        }

        // CR 701.13 + CR 400.7: "Exile the top card of your library." Maps onto
        // `Effect::ExileTop` (player = controller, count = 1). Mirrors the
        // native parser's exile-top handler — the runtime reads the top of the
        // matched player's library and moves it to exile.
        Action::ExileTopCardOfLibrary => Effect::ExileTop {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 1 },
        },

        // CR 701.13 + CR 400.7: "Exile the top N cards of your library." Same
        // as `ExileTopCardOfLibrary` with a dynamic count; `quantity::convert`
        // strict-fails on unsupported `GameNumber` shapes.
        Action::ExileTheTopNumberCardsOfLibrary(n) => Effect::ExileTop {
            player: TargetFilter::Controller,
            count: quantity::convert(n)?,
        },

        // CR 701.20a: "Reveal the top card of your library." Maps onto
        // `Effect::RevealTop` (player = controller, count = 1). Subsequent
        // dispositions (move to hand, exile, etc.) are encoded as separate
        // chain segments by mtgish — this arm only emits the public reveal.
        Action::RevealTopCardOfLibrary => Effect::RevealTop {
            player: TargetFilter::Controller,
            count: 1,
        },

        // CR 701.20a: "Reveal your hand" — zero-arg variant. Maps onto
        // `Effect::RevealHand` with `target: Controller`, no card filter,
        // and `count: None` (entire hand). Composite reveal-and-act
        // siblings (`RevealHandAndPlayerChoosesACardToDiscard`, etc.) are
        // separate chain shapes and strict-fail until lowered.
        Action::RevealHand => Effect::RevealHand {
            target: TargetFilter::Controller,
            card_filter: TargetFilter::Any,
            count: None,
        },

        // CR 701.20a: "Reveal the top N cards of your library." Engine
        // `Effect::RevealTop.count` is `u32` (not `QuantityExpr`), so dynamic
        // counts strict-fail. Bodies (`RevealTheTopNumberCardsOfLibraryAction`)
        // encode disposition rules over the revealed pile (Dig-shape, separate
        // primitive); a non-empty body strict-fails until that lowering exists.
        Action::RevealTheTopNumberCardsOfLibrary(n, body) => {
            if !body.is_empty() {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "Effect",
                    needed_variant: "RevealTheTopNumberCardsOfLibrary/non-empty-body \
                                     (disposition lowering — needs Dig-shape conversion)"
                        .to_string(),
                });
            }
            match quantity::convert(n)? {
                QuantityExpr::Fixed { value } if value >= 0 => Effect::RevealTop {
                    player: TargetFilter::Controller,
                    count: value as u32,
                },
                other => {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "Effect::RevealTop",
                        needed_variant: format!(
                            "RevealTop.count widened to QuantityExpr — got dynamic {other:?}"
                        ),
                    });
                }
            }
        }

        // CR 701.16a: "Investigate" creates a Clue artifact token. The engine's
        // `Effect::Investigate` is a unit variant whose resolver synthesizes
        // the Clue token spec at resolution time.
        Action::Investigate => Effect::Investigate,

        // CR 701.16a: "Investigate N times" — N-fold repetition has no engine
        // primitive (unlike `Effect::FlipCoins.count`). Strict-fail until
        // `Effect::Investigate` is widened to carry a `count: QuantityExpr`.
        Action::InvestigateTimes(n) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "Effect::Investigate",
                needed_variant: format!(
                    "Investigate.count: QuantityExpr — got InvestigateTimes({})",
                    quantity_tag(n)
                ),
            });
        }

        // CR 701.9a + CR 119.1: "Discard your hand" — emit
        // `Effect::Discard { count: HandSize { player: Controller }, target: Controller }`.
        // For non-You forms ("target opponent discards their hand", "each
        // opponent discards their hand"), the surrounding PlayerAction /
        // EachPlayerAction wrapper sets `AbilityDefinition.player_scope`, and
        // the engine iterates the ability over each matching player — each
        // becomes the controller for that iteration, so
        // `HandSize { player: PlayerScope::Controller }` resolves against the
        // discarding player (not the original ability controller). The
        // target-deref case is structurally distinct from the iteration case,
        // but both reduce to the same per-iteration controller-bound resolve.
        // Engine round Π-3 unified `HandSize`/`OpponentHandSize` into the
        // parameterized form (`PlayerScope::{Controller,Target,Opponent}`),
        // making this stale strict-fail unnecessary.
        Action::DiscardHand => Effect::Discard {
            count: QuantityExpr::Ref {
                qty: engine::types::ability::QuantityRef::HandSize {
                    player: engine::types::ability::PlayerScope::Controller,
                },
            },
            target: TargetFilter::Controller,
            random: false,
            unless_filter: None,
            filter: None,
        },

        // CR 400.7 + CR 701.18: "Put a card from your hand onto the
        // battlefield." Maps to `Effect::ChangeZone { Hand → Battlefield }`,
        // mirroring `PutGraveyardCardOntoBattlefield` / `PutExiledCardOnto…`.
        // Filter conversion is limited to `CardsInHand::AnyCard` /
        // single-card refs today — typed predicates (`IsCardtype`,
        // `IsCreatureType`, etc.) strict-fail until a `cards_in_hand →
        // TargetFilter` converter is built.
        Action::PutACardFromHandOnBattlefield(cards, repls) => {
            let r = extract_enter_replacements(repls)?;
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: cards_in_hand_to_filter(cards)?,
                owner_library: false,
                enter_transformed: r.enter_transformed,
                under_your_control: r.under_your_control,
                enter_tapped: r.enter_tapped,
                enters_attacking: r.enters_attacking,
                up_to: false,
                enter_with_counters: vec![],
            }
        }

        // CR 119.3 + CR 107.3: "Gain N life for each X." Composes onto
        // `QuantityExpr::Multiply { factor, inner }` when the per-each amount
        // is a literal (the common shape — "1 life", "2 life", "3 life"); a
        // dynamic per-each amount has no engine analog and strict-fails.
        Action::GainLifeForEach(n_per, count_qty) => {
            let per_each = quantity::convert(n_per)?;
            let count = quantity::convert(count_qty)?;
            let amount = match per_each {
                QuantityExpr::Fixed { value: 1 } => count,
                QuantityExpr::Fixed { value } => QuantityExpr::Multiply {
                    factor: value,
                    inner: Box::new(count),
                },
                other => {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "QuantityExpr::Multiply",
                        needed_variant: format!(
                            "Multiply.factor widened to QuantityExpr — got dynamic per-each {other:?}"
                        ),
                    });
                }
            };
            Effect::GainLife {
                amount,
                player: GainLifePlayer::Controller,
            }
        }

        // CR 502.1 + CR 514.2: "Target permanent doesn't untap during its
        // controller's next untap step." Lowered as a one-shot continuous
        // effect that grants `StaticMode::CantUntap` for the
        // `UntilControllerNextUntapStep` window. Mirrors the
        // `GainControlOfPermanentUntil` / `CreatePermanentLayerEffect`
        // GenericEffect-with-static idiom.
        Action::PermanentDoesntUntapDuringControllersNextUntap(p) => {
            let affected = convert_permanent(p)?;
            let static_def = StaticDefinition::new(StaticMode::Continuous)
                .affected(affected.clone())
                .modifications(vec![ContinuousModification::AddStaticMode {
                    mode: StaticMode::CantUntap,
                }]);
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilNextUntapStepOf {
                    player: PlayerScope::Controller,
                }),
                target: Some(affected),
            }
        }

        // CR 502.1 + CR 514.2: Mass form of
        // `PermanentDoesntUntapDuringControllersNextUntap` ("each [filter]
        // doesn't untap during its controller's next untap step"). Same shape
        // with a multi-match `Permanents` filter.
        Action::EachPermanentDoesntUntapDuringControllersNextUntap(filter) => {
            let affected = convert_permanents(filter)?;
            let static_def = StaticDefinition::new(StaticMode::Continuous)
                .affected(affected.clone())
                .modifications(vec![ContinuousModification::AddStaticMode {
                    mode: StaticMode::CantUntap,
                }]);
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilNextUntapStepOf {
                    player: PlayerScope::Controller,
                }),
                target: Some(affected),
            }
        }

        // CR 107.3a: "X is N" sets the value of X for the rest of the
        // resolving spell or ability. Engine's `QuantityRef::Variable { name:
        // "X" }` resolves at use-site against a context binding; mtgish
        // encodes the binding as a separate `Action::CreateValueX(n)` step
        // with no equivalent setter primitive. Strict-fail until the engine
        // grows an explicit X-binding effect (or the converter learns to
        // splice the X expression into downstream effects directly).
        Action::CreateValueX(_n) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "Effect",
                needed_variant: "BindVariable { name: \"X\", value: QuantityExpr } — \
                                 mtgish encodes X-setter as a standalone Action; engine \
                                 has no equivalent setter (X is bound at use-site)"
                    .to_string(),
            });
        }

        // CR 603.12: "When you do, [effect]" — reflexive triggered ability
        // attached to the parent effect. The engine encodes this as a
        // `sub_ability` with `condition: AbilityCondition::WhenYouDo` on the
        // parent `AbilityDefinition`, *not* as a sibling `Effect` in the
        // chain. Wiring this requires chain-level threading (splice into the
        // parent AD's sub_ability rather than emit a peer Effect), which
        // belongs in `convert_actions` / `ChainSegment` — not in the single-
        // effect `convert` arm. Strict-fail until that lowering is added.
        Action::ReflexiveTrigger(_actions) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "Effect",
                needed_variant: "ReflexiveTrigger (sub_ability with AbilityCondition::WhenYouDo) \
                                 — needs chain-level lowering, not Effect-arm"
                    .to_string(),
            });
        }

        // CR 603.12: Conditional reflexive trigger ("when you do, if [cond],
        // [effect]"). Same chain-level threading problem as `ReflexiveTrigger`
        // plus an inner condition gate.
        Action::ReflexiveTriggerI(_cond, _actions) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "Effect",
                needed_variant: "ReflexiveTriggerI (sub_ability with WhenYouDo + inner condition) \
                                 — needs chain-level lowering"
                    .to_string(),
            });
        }

        // CR 603.12: N-times reflexive trigger ("when you do this N times,
        // [effect]"). Same chain-level threading problem; engine has no
        // count-gated reflexive trigger primitive.
        Action::ReflexiveTriggerNumberTimes(_n, _actions) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "Effect",
                needed_variant: "ReflexiveTriggerNumberTimes — needs chain-level lowering \
                                 + count-gated WhenYouDo variant"
                    .to_string(),
            });
        }

        // CR 603.7 + CR 603.7c: "Until [expiration], whenever [event], [effect]" —
        // a recurring delayed triggered ability scoped to the rest of the
        // current turn. The engine slot is `Effect::CreateDelayedTrigger`
        // with `DelayedTriggerCondition::WheneverEvent { trigger }` (mirrors
        // the native parser at oracle_effect/mod.rs:306). The runtime hard-
        // codes WheneverEvent's lifetime to until-end-of-turn cleanup
        // (`game/effects/delayed_trigger.rs:88-92`), so only the
        // `UntilEndOfTurn` expiration shape maps cleanly. Other expirations
        // strict-fail with a refined tag — the engine has no parameterized
        // expiration slot for recurring delayed triggers today.
        Action::CreateTriggerUntil(trigger, body, expiration) => {
            build_create_trigger_until(trigger, None, body, expiration)?
        }

        // CR 603.7 + CR 603.7c + CR 603.4: Conditional form of
        // `CreateTriggerUntil` ("until [expiration], if [cond], whenever
        // [event], [effect]"). The intervening-if rides on the embedded
        // `TriggerDefinition.condition` slot (`ability.rs:6125`), checked
        // when the event fires per CR 603.4.
        Action::CreateTriggerUntilI(trigger, intervening_if, body, expiration) => {
            build_create_trigger_until(trigger, Some(intervening_if), body, expiration)?
        }

        // CR 608.2d: Resolution-time named choices made by the controller of
        // the resolving spell or ability. Each wireable arm emits
        // `Effect::Choose { .., persist: true }`, mirroring Round U's ETB
        // dispatch in `replacement.rs`. Persisting the choice on the source's
        // `chosen_attributes` lets downstream "the chosen color/type/..."
        // references read it. The Action enum's Choose-set is a strict subset
        // of `ReplacementActionWouldEnter::Choose*` (the ETB axis), so several
        // variants Round U handled (BasicLandType, LandType, CardName,
        // EvenOrOdd, TwoColors, Direction, *FromList, Word, paired forms,
        // SecretlyChoose*, etc.) have no Action-side counterpart and are not
        // wired here — see schema/types.rs:4814-4826 for the full Action
        // Choose enumeration.

        // CR 105.4: choosing a color picks one of the five colors.
        Action::ChooseAColor(_) => Effect::Choose {
            choice_type: ChoiceType::Color,
            persist: true,
        },
        // CR 608.2d: choose a creature type — the bounded creature-type
        // registry resolves the option set at runtime.
        Action::ChooseACreatureType => Effect::Choose {
            choice_type: ChoiceType::CreatureType,
            persist: true,
        },
        // CR 205.2: choose a card type from the bounded card-type set.
        Action::ChooseACardtype => Effect::Choose {
            choice_type: ChoiceType::CardType,
            persist: true,
        },
        // CR 201.3 + CR 608.2d: "choose a card name". Engine
        // `ChoiceType::CardName` is a unit variant (no filter slot), so the
        // mtgish `Cards` constraint is supported only for the dominant
        // `AnyCard` case. Filtered shapes ("choose a creature card name")
        // need engine-side filter integration and strict-fail.
        Action::ChooseACardName(cards) => {
            use crate::schema::types::Cards as C;
            match &**cards {
                C::AnyCard => Effect::Choose {
                    choice_type: ChoiceType::CardName,
                    persist: true,
                },
                other => {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "ChoiceType::CardName",
                        needed_variant: format!(
                            "ChooseACardName with constraint Cards::{}",
                            crate::convert::filter::cards_variant_tag(other)
                        ),
                    });
                }
            }
        }
        // CR 608.2d: "Choose a number between X and Y" — engine's
        // `NumberRange` carries u8 bounds. Strict-fail if the schema values
        // are out of range or inverted (defensive — the engine would generate
        // a degenerate option list).
        Action::ChooseANumberBetween(min, max) => {
            let (Ok(min_u8), Ok(max_u8)) = (u8::try_from(*min), u8::try_from(*max)) else {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "ChoiceType::NumberRange",
                    needed_variant: format!("number-range bounds out of u8 ({min}, {max})"),
                });
            };
            if min_u8 > max_u8 {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "ChoiceType::NumberRange",
                    needed_variant: format!("inverted number-range bounds ({min}, {max})"),
                });
            }
            Effect::Choose {
                choice_type: ChoiceType::NumberRange {
                    min: min_u8,
                    max: max_u8,
                },
                persist: true,
            }
        }
        // CR 608.2d: opponent-scoped player choice when the schema filter
        // narrows to opponents; broader player choice otherwise. Re-uses the
        // existing `players_to_controller` bridge for opponent detection.
        Action::ChooseAPlayer(players) => {
            let choice_type = match filter_mod::players_to_controller(players.as_ref()) {
                Ok(ControllerRef::Opponent) => ChoiceType::Opponent,
                _ => ChoiceType::Player,
            };
            Effect::Choose {
                choice_type,
                persist: true,
            }
        }

        // CR 608.2d strict-fails — each gets its own refined tag so the
        // report attributes the missing engine prerequisite to the exact
        // schema variant.

        // CR 608.2d: randomized choice — engine's named-choice slot is
        // controller-driven; randomized selection requires a separate
        // primitive (a random-pick effect that writes to chosen_attributes).
        Action::ChooseAPlayerAtRandom(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "random player choice (controller-driven slot only)".into(),
            });
        }
        // CR 700.2 / CR 614.12a: target-style permanent selection (vs.
        // named-choice). Belongs in a target slot, not `Effect::Choose`,
        // and the engine has no "select-and-persist a permanent reference"
        // ChoiceType. Strict-fail until the engine grows that slot.
        Action::ChooseAPermanent(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "permanent choice (target-style, not named-choice)".into(),
            });
        }
        // CR 608.2d: "Choose a card" from a hand / graveyard / exile zone —
        // a card-pick within a hidden or specified zone. The engine's
        // `ChoiceType` enumerates abstract-attribute choices (color, type,
        // number, ...) — not card-pick within a zone, which belongs to a
        // target/zone-pick primitive instead.
        Action::ChooseACardInHand(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "card-in-hand pick (zone-scoped target, not named-choice)".into(),
            });
        }
        Action::ChooseACardInPlayersGraveyard(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "card-in-graveyard pick (zone-scoped target, not named-choice)"
                    .into(),
            });
        }
        Action::ChooseACardFromPlayersRevealedHand(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "card-from-revealed-hand pick (zone-scoped target)".into(),
            });
        }
        Action::ChooseAnExiledCard(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "exiled-card pick (zone-scoped target, not named-choice)".into(),
            });
        }
        // CR 608.2d: "choose an ability from this list" — the option set is
        // a list of abilities (Vec<CheckHasable>), not labels/strings. The
        // engine's `ChoiceType::Labeled` carries `Vec<String>`; ability
        // identifiers don't have a stable string form here. Strict-fail.
        Action::ChooseACheckableAbility(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "ability-from-list choice (CheckHasable, not Vec<String>)".into(),
            });
        }
        // CR 608.2d: "choose colors" without a fixed count — distinct from
        // the `ChooseTwoColors` ETB-axis variant (which has its own engine
        // ChoiceType). The unbounded multi-color form has no engine slot.
        Action::ChooseColors => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "unbounded colors choice (multi-select, no fixed N)".into(),
            });
        }

        _ => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: variant_tag(a),
            });
        }
    })
}

/// CR 603.7 + CR 603.7a: Map a mtgish `FutureTrigger` to the engine's
/// `DelayedTriggerCondition`. The high-frequency "at the beginning of the
/// next [phase]" and "at the beginning of [player]'s next [phase]" shapes
/// land directly on `AtNextPhase` / `AtNextPhaseForPlayer`. Combat-window
/// variants (`AtNextEndOfCombat...`, `AtTheEndOfThisCombat`) all collapse
/// onto the engine's single `Phase::EndCombat` slot — the runtime gating
/// of "this combat" vs "next combat" is the resolver's responsibility once
/// the delayed trigger is installed.
///
/// `Player::You` collapses to `PlayerId(0)`, the controller-rewrite
/// placeholder used by the native parser at `oracle_effect/mod.rs:7502`.
/// Other Player refs strict-fail until the controller-resolution layer
/// can carry typed scopes.
///
/// Variants that require event-based filtering (`When [player] casts ...`,
/// `When [permanent] becomes untapped`, `Or(...)` disjunctions, "first
/// upkeep / first main phase / extra-turn end step" — none of which the
/// engine's `DelayedTriggerCondition` enumerates today) strict-fail with
/// `EnginePrerequisiteMissing` so the gap surfaces in the report.
fn future_trigger_to_condition(t: &FutureTrigger) -> ConvResult<DelayedTriggerCondition> {
    use FutureTrigger as F;
    let prereq = |needed: &str| ConversionGap::EnginePrerequisiteMissing {
        engine_type: "DelayedTriggerCondition",
        needed_variant: needed.to_string(),
    };
    Ok(match t {
        // CR 513.1: "at the beginning of the next end step".
        F::AtTheBeginningOfTheNextEndStep => {
            DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
        }
        // CR 503.1 + CR 502.1: Upkeep variants (turn-scoped, not player-scoped).
        F::AtTheBeginningOfTheNextUpkeep | F::AtTheBeginningOfTheNextTurnsUpkeep => {
            DelayedTriggerCondition::AtNextPhase {
                phase: Phase::Upkeep,
            }
        }
        // CR 514.1: "at the beginning of the next cleanup step".
        F::AtTheBeginningOfTheNextCleanupStep => DelayedTriggerCondition::AtNextPhase {
            phase: Phase::Cleanup,
        },
        // CR 507.1: "at the beginning of the next combat" — the start of
        // combat is the BeginCombat step.
        F::AtTheBeginningOfTheNextCombat | F::AtTheBeginningOfTheNextCombatPhaseThisTurn => {
            DelayedTriggerCondition::AtNextPhase {
                phase: Phase::BeginCombat,
            }
        }
        // CR 511.1: "at end of combat" / "next end of combat" / "at the end
        // of this combat" — all three collapse to EndCombat. The "this
        // combat" vs "next combat" distinction is a runtime gating concern.
        F::AtNextEndOfCombatThisTurn | F::AtTheEndOfThisCombat | F::AtTheNextEndOfCombat => {
            DelayedTriggerCondition::AtNextPhase {
                phase: Phase::EndCombat,
            }
        }
        // CR 505.1: "at the beginning of the next main phase this turn".
        F::AtTheBeginningOfTheNextMainPhaseThisTurn => DelayedTriggerCondition::AtNextPhase {
            phase: Phase::PreCombatMain,
        },

        // CR 503.1 + CR 503.2: Player-scoped upkeep — "at the beginning of
        // [player]'s next upkeep". `Player::You` becomes `PlayerId(0)`,
        // the controller placeholder rewritten at resolve time
        // (matches the native parser convention at oracle_effect/mod.rs:7502).
        F::AtTheBeginningOfPlayersNextUpkeep(p) => DelayedTriggerCondition::AtNextPhaseForPlayer {
            phase: Phase::Upkeep,
            player: future_trigger_player_id(p)?,
        },
        // CR 513.1: Player-scoped end step.
        F::AtTheBeginningOfPlayersNextEndStep(p) => DelayedTriggerCondition::AtNextPhaseForPlayer {
            phase: Phase::End,
            player: future_trigger_player_id(p)?,
        },
        // CR 504.1: Player-scoped draw step.
        F::AtTheBeginningOfPlayersNextDrawStep(p) => {
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::Draw,
                player: future_trigger_player_id(p)?,
            }
        }
        // CR 505.1: Player-scoped main phase. Both "next main phase" and
        // "next first main phase" collapse to `PreCombatMain` — the engine
        // doesn't distinguish first vs second main on the delayed-trigger
        // condition (the runtime fires on the first matching phase).
        F::AtTheBeginningOfPlayersNextMainPhase(p)
        | F::AtTheBeginningOfPlayersNextFirstMainPhase(p) => {
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::PreCombatMain,
                player: future_trigger_player_id(p)?,
            }
        }
        // CR 508.1: Player-scoped declare attackers step.
        F::AtTheBeginningOfPlayersNextDeclareAttackersStep(p) => {
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::DeclareAttackers,
                player: future_trigger_player_id(p)?,
            }
        }

        // Variants the engine's `DelayedTriggerCondition` doesn't enumerate
        // today. Each flagged with the proposed shape so the gap is
        // self-documenting in the report.
        F::Or(_) => return Err(prereq("Or { conditions: Vec<DelayedTriggerCondition> }")),
        F::AtTheBeginningOfTheFirstUpkeep
        | F::AtTheBeginningOfPlayersFirstUpkeep(_)
        | F::AtTheBeginningOfPlayersFirstMainPhaseOfTheGame(_) => {
            return Err(prereq("AtFirstPhase { phase, scope }"));
        }
        F::AtTheBeginningOfTheEndStepOfTheExtraTurnCreatedThisWay => {
            return Err(prereq("AtExtraTurnEndStep"));
        }
        F::AtTheBeginningOfPlayersDeclareAttackersStepOnTheirNextTurn(_)
        | F::AtTheBeginningOfPlayersEndStepNextTurn(_) => {
            return Err(prereq("AtNextTurnPhaseForPlayer { phase, player }"));
        }
        F::WhenAPlayerNextAttacksThisTurn(_)
        | F::WhenAPlayerPlaneswalks(_)
        | F::WhenPlayerNextActivatesAnAbilityThisTurn(_, _)
        | F::WhenAPlayerNextActivatesAnAbilityThisTurn(_, _)
        | F::WhenPlayerNextActivatesAnAbilityBySpendingAnAmountOfMana(_, _, _)
        | F::WhenPlayerCastsTheirNextSpellOrActivatesTheirNextAbilityThisTurn(_, _)
        | F::WhenPlayerCastsTheirNextSpellThisGame(_, _)
        | F::WhenPlayerCastsTheirNextSpellThisTurn(_, _)
        | F::WhenPlayerCastsTheirNextSpellFromTheirHandThisTurn(_, _) => {
            return Err(prereq(
                "WhenNextEvent { trigger } (player-cast/activate event)",
            ));
        }
        F::WhenCreatureOrPlaneswalkerDies(_)
        | F::WhenPermanentBecomesUntapped(_)
        | F::WhenPermanentLeavesTheBattlefield(_)
        | F::WhenPermanentIsPutIntoAPlayersGraveyard(_, _)
        | F::WhenPlayerLosesControlOfPermanent(_, _) => {
            return Err(prereq("WhenLeavesPlayFiltered/WhenDies (permanent-event)"));
        }
    })
}

/// CR 603.7e + CR 119.3: Map a mtgish `Player` to the `PlayerId`
/// placeholder used by `DelayedTriggerCondition::AtNextPhaseForPlayer`.
/// `Player::You` becomes `PlayerId(0)` (the controller-rewrite placeholder
/// — the engine resolver substitutes the ability's controller at fire
/// time; see `oracle_effect/mod.rs:7502`). Other `Player` refs strict-fail
/// — there's no static `PlayerId` for "an opponent" / "that player" et al.;
/// they require a typed scope which the engine field is not yet
/// parameterized over.
fn future_trigger_player_id(p: &Player) -> ConvResult<PlayerId> {
    match p {
        Player::You => Ok(PlayerId(0)),
        _ => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "DelayedTriggerCondition::AtNextPhaseForPlayer.player",
            needed_variant: format!(
                "non-controller Player ref: {}",
                serde_json::to_value(p)
                    .ok()
                    .and_then(|v| v.get("_Player").and_then(|t| t.as_str()).map(String::from))
                    .unwrap_or_else(|| "<unknown>".to_string())
            ),
        }),
    }
}

/// CR 114.1 + CR 114.2 + CR 114.4: Lower the `Vec<Rule>` body of an
/// emblem-creation effect into the `Effect::CreateEmblem { statics,
/// triggers }` shape. The covered inner Rule arms mirror the leaf cases
/// from `convert_rule` that don't need `&mut Ctx`-driven recursion:
///
/// - `Rule::TriggerA` / `Rule::TriggerI` — emblem-hosted triggered
///   abilities (Chandra, Awakened Inferno's "at the beginning of your
///   upkeep, this emblem deals 1 damage to you").
/// - `Rule::PermanentLayerEffect` / `Rule::EachPermanentLayerEffect` —
///   continuous static effects on the controller's permanents (Sorin,
///   Lord of Innistrad's "creatures you control get +1/+0").
///
/// Per CR 114.4, emblem-hosted triggers function in the command zone, so
/// each `TriggerDefinition` is decorated with
/// `trigger_zones = [Zone::Command]` (mirrors the native parser at
/// `oracle_effect/mod.rs:4813`). Statics inherit the engine's emblem
/// runtime — `create_emblem::resolve` installs them on the Command-zone
/// emblem object so they apply globally.
///
/// Inner Rule arms that require the condition-decorating recursion in
/// `convert_rule` (If/Unless/IfElse, graveyard-grant, etc.) strict-fail
/// with a typed gap. Threading `&mut Ctx` through the entire
/// action-conversion API to support those is a separate, larger refactor.
fn convert_emblem_body(rules: &[Rule]) -> ConvResult<Effect> {
    let mut statics: Vec<StaticDefinition> = Vec::new();
    let mut triggers: Vec<TriggerDefinition> = Vec::new();

    for rule in rules {
        match rule {
            // CR 603 + CR 114.4: Triggered ability hosted on the emblem.
            Rule::TriggerA(trig, actions) => {
                let tds = trigger_mod::convert_many(trig)?;
                let conv = convert_actions(actions)?;
                let body =
                    crate::convert::build_ability_from_actions(AbilityKind::Spell, None, conv)?;
                push_emblem_triggers(&mut triggers, tds, &body, None);
            }
            // CR 603.4 + CR 114.4: Triggered ability with intervening-if.
            Rule::TriggerI(trig, cond, actions) => {
                let tds = trigger_mod::convert_many(trig)?;
                let tcond = condition::convert_trigger(cond)?;
                let conv = convert_actions(actions)?;
                let body =
                    crate::convert::build_ability_from_actions(AbilityKind::Spell, None, conv)?;
                push_emblem_triggers(&mut triggers, tds, &body, Some(tcond));
            }
            // CR 613 + CR 114.4: Continuous static on a single permanent.
            Rule::PermanentLayerEffect(target, effects) => {
                let affected = filter_mod::convert_permanent(target)?;
                let s = static_effect::build_static(affected, effects)?;
                statics.push(s);
            }
            // CR 613 + CR 114.4: Continuous static on each matching permanent.
            Rule::EachPermanentLayerEffect(filter_box, effects) => {
                let affected = filter_mod::convert(filter_box)?;
                let s = static_effect::build_static(affected, effects)?;
                statics.push(s);
            }
            // Inner Rule shapes that need `convert_rule` recursion or
            // additional context strict-fail with a typed gap so the report
            // surfaces them. Coverage of these arms requires threading
            // `&mut Ctx` through the action conversion API.
            other => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "Effect::CreateEmblem (inner Rule)",
                    needed_variant: format!(
                        "emblem inner Rule arm not yet covered: {}",
                        rule_variant_tag(other)
                    ),
                });
            }
        }
    }

    if statics.is_empty() && triggers.is_empty() {
        return Err(ConversionGap::MalformedIdiom {
            idiom: "Action::GetAnEmblem/empty",
            path: String::new(),
            detail: "emblem body produced no statics or triggers".into(),
        });
    }

    Ok(Effect::CreateEmblem { statics, triggers })
}

/// CR 113.1c + CR 114.4: Push triggers onto an emblem with
/// `trigger_zones = [Zone::Command]` so the engine's
/// `collect_matching_triggers` zone gate admits them. Mirrors the
/// `push_triggers` helper in `convert/mod.rs` but applies the Command-zone
/// decorator inline so emblem trigger definitions don't share-clone with
/// battlefield-only siblings.
fn push_emblem_triggers(
    out: &mut Vec<TriggerDefinition>,
    tds: Vec<TriggerDefinition>,
    body: &AbilityDefinition,
    condition: Option<engine::types::ability::TriggerCondition>,
) {
    for mut td in tds {
        td.execute = Some(Box::new(body.clone()));
        if let Some(c) = &condition {
            td.condition = Some(c.clone());
        }
        td.trigger_zones = vec![Zone::Command];
        out.push(td);
    }
}

fn rule_variant_tag(r: &Rule) -> String {
    serde_json::to_value(r)
        .ok()
        .and_then(|v| v.get("_Rule").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// CR 701.20e + CR 608.2c: Lower a "look at the top N cards" disposition
/// sequence onto a single `Effect::Dig`.
///
/// `Effect::Dig` is the engine's single primitive for the "look at top N,
/// keep some, send the rest somewhere" family — `keep_count` / `up_to` /
/// `filter` / `destination` / `rest_destination` / `reveal` together encode
/// every shape the parser already mints for Brainstorm, Augur of Bolas,
/// Sensei's Divining Top, sift-style mill-dig, and pure-peek.
///
/// Recognized disposition sequences ship as `Effect::Dig` with the matching
/// field tuple. Unrecognized combinations strict-fail with
/// `EnginePrerequisiteMissing { engine_type: "Effect", needed_variant }`
/// so the surrounding card drops out of conversion (per the strict-failure
/// discipline in `convert/result.rs`) rather than mis-rendering.
fn convert_look_at_top(
    count: QuantityExpr,
    dispositions: &[crate::schema::types::LookAtTopOfLibraryAction],
) -> ConvResult<Effect> {
    use crate::schema::types::LookAtTopOfLibraryAction as L;

    let prereq = |needed: String| ConversionGap::EnginePrerequisiteMissing {
        engine_type: "Effect",
        needed_variant: needed,
    };

    // The dispositions are emitted in resolution order: the "keep" / pick step
    // first (if any), then the "rest goes here" step. Slot one or two arms.
    match dispositions {
        // Pure peek — "look at the top N cards of your library, then put them
        // back in any order" (or in the same order). Engine resolver treats
        // `keep_count: Some(0)` + non-reveal as a pure-peek pattern that sets
        // `last_revealed_ids` without prompting for a selection.
        [L::PutTheRemainingCardsOnTopOfLibraryInAnyOrder]
        | [L::LeaveRemainingCardsOnTopOfLibraryInSameOrder] => Ok(Effect::Dig {
            count,
            destination: None,
            keep_count: Some(0),
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: None,
            reveal: false,
        }),

        // Brainstorm-style "put one into your hand and the rest on the
        // bottom of your library" without a card-type filter on the kept
        // card. (Random vs any-order on the bottom is irrelevant when only
        // one card lands there, but the resolver also handles N>1.)
        [L::PutAGenericCardIntoHand, L::PutTheRemainingCardsOnTheBottomOfLibraryInARandomOrder]
        | [L::PutAGenericCardIntoHand, L::PutTheRemainingCardsOnTheBottomOfLibraryInAnyOrder] => {
            Ok(Effect::Dig {
                count,
                destination: Some(Zone::Hand),
                keep_count: Some(1),
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: Some(Zone::Library),
                reveal: false,
            })
        }

        // "Put one of them into your graveyard and the rest on top of your
        // library in any order" — mill-pick (e.g., Mystical Teachings
        // family).
        [L::PutAGenericCardIntoGraveyard, L::PutTheRemainingCardsOnTopOfLibraryInAnyOrder] => {
            Ok(Effect::Dig {
                count,
                destination: Some(Zone::Graveyard),
                keep_count: Some(1),
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: None,
                reveal: false,
            })
        }

        // Augur of Bolas / Sensei's Divining Top family — "you may reveal a
        // [type] card from among them and put it into your hand. Put the
        // rest on the bottom of your library in any order."
        // `MayRevealACardOfTypeAndPutIntoHand` is "may reveal up-to-1", so
        // `up_to: true`.
        [L::MayRevealACardOfTypeAndPutIntoHand(cards), L::PutTheRemainingCardsOnTheBottomOfLibraryInARandomOrder]
        | [L::MayRevealACardOfTypeAndPutIntoHand(cards), L::PutTheRemainingCardsOnTheBottomOfLibraryInAnyOrder] => {
            Ok(Effect::Dig {
                count,
                destination: Some(Zone::Hand),
                keep_count: Some(1),
                up_to: true,
                filter: filter_mod::cards_to_filter(cards)?,
                rest_destination: Some(Zone::Library),
                reveal: true,
            })
        }

        // Sift-pattern: "may reveal a [type] card and put it into your hand,
        // then put the rest into your graveyard."
        [L::MayRevealACardOfTypeAndPutIntoHand(cards), L::PutTheRemainingCardsIntoGraveyard] => {
            Ok(Effect::Dig {
                count,
                destination: Some(Zone::Hand),
                keep_count: Some(1),
                up_to: true,
                filter: filter_mod::cards_to_filter(cards)?,
                rest_destination: Some(Zone::Graveyard),
                reveal: true,
            })
        }

        // "Put any number of them into your graveyard and the rest on top of
        // your library in any order" — variable-mill self-scry. The engine
        // expresses an "up to N" keep via `up_to: true` on a `keep_count` of
        // N; we don't have a "kept = any number" primitive, so strict-fail.
        // Same for the symmetric "any number into hand" / "any number on
        // bottom" patterns: each requires runtime to read `count` as the
        // upper bound for `keep_count`, which `Effect::Dig` doesn't model.
        [L::PutAnyNumberOfGenericCardsIntoHand, ..]
        | [L::PutAnyNumberOfGenericCardsOnBottomOfLibraryAnyOrder, ..] => Err(prereq(format!(
            "Dig::any-number-keep ({})",
            disposition_tag_list(dispositions)
        ))),

        // Single disposition that sends every card to a specific zone with
        // no selection step — "put all N cards into your graveyard"
        // (self-mill from look) / "put all into hand" (Demonic Consultation
        // tail). These are pure mill / pure draw and don't need a Dig at
        // all, but we don't reach a clean strict-fail here without more
        // disposition context, so route them through the prereq channel
        // until we add the matching arms.
        [L::PutTheRemainingCardsIntoGraveyard]
        | [L::PutTheRemainingCardsIntoHand]
        | [L::PutTheRemainingCardsBackIntoLibraryAndShuffle]
        | [L::PutRemainingCardsInHand] => Err(prereq(format!(
            "Dig::all-cards-to-zone ({})",
            disposition_tag_list(dispositions)
        ))),

        _ => Err(prereq(format!(
            "Dig::dispositions[{}]",
            disposition_tag_list(dispositions)
        ))),
    }
}

/// Compact comma-joined disposition serde-tag list for diagnostic
/// `EnginePrerequisiteMissing` payloads. Mirrors the `variant_tag` idiom
/// used by `Action` / `Cards` strict-fail breadcrumbs.
fn disposition_tag_list(dispositions: &[crate::schema::types::LookAtTopOfLibraryAction]) -> String {
    use crate::schema::types::LookAtTopOfLibraryAction as L;
    let tags: Vec<&'static str> = dispositions
        .iter()
        .map(|d| match d {
            L::PutRemainingSetAsideCardsIntoHand => "PutRemainingSetAsideCardsIntoHand",
            L::PutSetAsideCardsOfTypeOntoBattlefield(..) => "PutSetAsideCardsOfTypeOntoBattlefield",
            L::MayPutUptoNumberGroupCardsOntoTheBattlefield(..) => {
                "MayPutUptoNumberGroupCardsOntoTheBattlefield"
            }
            L::MayRevealUptoNumberCardsOfTypeAndSetAside(..) => {
                "MayRevealUptoNumberCardsOfTypeAndSetAside"
            }
            L::ExileNumberGenericCardsFaceDown(..) => "ExileNumberGenericCardsFaceDown",
            L::PutAnyNumberOfCardsOntoTheBattlefield(..) => "PutAnyNumberOfCardsOntoTheBattlefield",
            L::MayAction(..) => "MayAction",
            L::PutTheRemainingCardsOnTopOfLibraryInAnyOrder => {
                "PutTheRemainingCardsOnTopOfLibraryInAnyOrder"
            }
            L::ShuffleAndPutTheRemainingCardsOnTopOfLibraryInAnyOrder => {
                "ShuffleAndPutTheRemainingCardsOnTopOfLibraryInAnyOrder"
            }
            L::ConjureADuplicateOfCardOntoTheBattlefield(..) => {
                "ConjureADuplicateOfCardOntoTheBattlefield"
            }
            L::RevealACardOfType(..) => "RevealACardOfType",
            L::PutFoundCardOntoBattlefield(..) => "PutFoundCardOntoBattlefield",
            L::MayRevealAndPutACardOfTypeOntoTheBattlefield(..) => {
                "MayRevealAndPutACardOfTypeOntoTheBattlefield"
            }
            L::MayPutFoundCardOntoBattlefield(..) => "MayPutFoundCardOntoBattlefield",
            L::ExileAGenericCard => "ExileAGenericCard",
            L::PutFoundCardIntoHand => "PutFoundCardIntoHand",
            L::ExileTheRemainingCardsFaceDown => "ExileTheRemainingCardsFaceDown",
            L::CloakNumberGenericCards(..) => "CloakNumberGenericCards",
            L::CreateExiledCardEffect(..) => "CreateExiledCardEffect",
            L::PutRemainingCardsInHand => "PutRemainingCardsInHand",
            L::ExileAnyNumberOfGenericCardsInAFaceDownPile => {
                "ExileAnyNumberOfGenericCardsInAFaceDownPile"
            }
            L::ExileTheRemainingCardsInAFaceUpPile => "ExileTheRemainingCardsInAFaceUpPile",
            L::PutUptoNumberGenericCardsOnTopOfLibraryInAnyOrder(..) => {
                "PutUptoNumberGenericCardsOnTopOfLibraryInAnyOrder"
            }
            L::MayPutAnyNumberOfGroupCardsOntoTheBattlefield(..) => {
                "MayPutAnyNumberOfGroupCardsOntoTheBattlefield"
            }
            L::APlayerChoosesAPileTopPutIntoHand(..) => "APlayerChoosesAPileTopPutIntoHand",
            L::ExileAGenericCardWithACounter(..) => "ExileAGenericCardWithACounter",
            L::MayExileUptoNumberCardsOfType(..) => "MayExileUptoNumberCardsOfType",
            L::PutAGenericCardAndAllCardsWithTheSameNameIntoHand => {
                "PutAGenericCardAndAllCardsWithTheSameNameIntoHand"
            }
            L::LoseLifeForEach(..) => "LoseLifeForEach",
            L::ExileAGenericCardFaceDown => "ExileAGenericCardFaceDown",
            L::ExileAnyNumberOfGenericCards => "ExileAnyNumberOfGenericCards",
            L::ExileTheRemainingCards => "ExileTheRemainingCards",
            L::ManifestAGenericCard => "ManifestAGenericCard",
            L::MayExileACardOfType(..) => "MayExileACardOfType",
            L::MayExileAGenericCard => "MayExileAGenericCard",
            L::MayExileAnyNumberOfGenericCards => "MayExileAnyNumberOfGenericCards",
            L::MayPutACardOfTypeOntoTheBattlefield(..) => "MayPutACardOfTypeOntoTheBattlefield",
            L::MayPutAGenericCardIntoHand => "MayPutAGenericCardIntoHand",
            L::MayPutAnyNumberOfCardsOntoTheBattlefield(..) => {
                "MayPutAnyNumberOfCardsOntoTheBattlefield"
            }
            L::MayRevealACardOfTypeAndPutIntoHand(..) => "MayRevealACardOfTypeAndPutIntoHand",
            L::MayRevealACardOfTypeAndPutOnTopOfLibrary(..) => {
                "MayRevealACardOfTypeAndPutOnTopOfLibrary"
            }
            L::MayRevealAnyNumberOfCardOfTypeAndPutOnTopOfLibrary(..) => {
                "MayRevealAnyNumberOfCardOfTypeAndPutOnTopOfLibrary"
            }
            L::MayRevealAnyNumberOfCardsOfTypeAndPutOnTopOfLibraryInAnyOrder(..) => {
                "MayRevealAnyNumberOfCardsOfTypeAndPutOnTopOfLibraryInAnyOrder"
            }
            L::MayRevealAnyNumberOfCardsOfTypeAndPutThemIntoHand(..) => {
                "MayRevealAnyNumberOfCardsOfTypeAndPutThemIntoHand"
            }
            L::PutAGenericCardIntoGraveyard => "PutAGenericCardIntoGraveyard",
            L::PutAGenericCardIntoHand => "PutAGenericCardIntoHand",
            L::PutAGenericCardOnBottomOfLibrary => "PutAGenericCardOnBottomOfLibrary",
            L::PutAGenericCardOnTopOfLibrary => "PutAGenericCardOnTopOfLibrary",
            L::PutAnyNumberOfGenericCardsIntoHand => "PutAnyNumberOfGenericCardsIntoHand",
            L::PutAnyNumberOfGenericCardsOnBottomOfLibraryAnyOrder => {
                "PutAnyNumberOfGenericCardsOnBottomOfLibraryAnyOrder"
            }
            L::PutNumberGenericCardsIntoHand(..) => "PutNumberGenericCardsIntoHand",
            L::PutRemainingCardsOnTheTopOrBottomOfLibraryInAnyOrder => {
                "PutRemainingCardsOnTheTopOrBottomOfLibraryInAnyOrder"
            }
            L::PutTheRemainingCardsBackIntoLibraryAndShuffle => {
                "PutTheRemainingCardsBackIntoLibraryAndShuffle"
            }
            L::PutTheRemainingCardsIntoGraveyard => "PutTheRemainingCardsIntoGraveyard",
            L::PutTheRemainingCardsIntoHand => "PutTheRemainingCardsIntoHand",
            L::PutTheRemainingCardsOnTheBottomOfLibraryInARandomOrder => {
                "PutTheRemainingCardsOnTheBottomOfLibraryInARandomOrder"
            }
            L::PutTheRemainingCardsOnTheBottomOfLibraryInAnyOrder => {
                "PutTheRemainingCardsOnTheBottomOfLibraryInAnyOrder"
            }
            L::MayRevealUptoNumberCardsOfTypeAndPutIntoHand(..) => {
                "MayRevealUptoNumberCardsOfTypeAndPutIntoHand"
            }
            L::SeparateIntoFaceUpFileAndFaceDownPile => "SeparateIntoFaceUpFileAndFaceDownPile",
            L::PlayerChoosesPileTopPutIntoHand(..) => "PlayerChoosesPileTopPutIntoHand",
            L::LeaveRemainingCardsOnTopOfLibraryInSameOrder => {
                "LeaveRemainingCardsOnTopOfLibraryInSameOrder"
            }
            L::SeparateIntoTwoFaceDownPiles => "SeparateIntoTwoFaceDownPiles",
            L::PlayerExilesAPile(..) => "PlayerExilesAPile",
            L::PlayerLooksAtRemainingCardsAndPutsAGenericCardIntoHand(..) => {
                "PlayerLooksAtRemainingCardsAndPutsAGenericCardIntoHand"
            }
            L::MayRevealMultipleCardsOfTypeAndPutIntoHand(..) => {
                "MayRevealMultipleCardsOfTypeAndPutIntoHand"
            }
            L::CreatePermanentLayerEffectUntil(..) => "CreatePermanentLayerEffectUntil",
            L::If(..) => "If",
            L::Unless(..) => "Unless",
            L::MayActions(..) => "MayActions",
            L::IfElse(..) => "IfElse",
            L::AttachPermanentToAPermanent(..) => "AttachPermanentToAPermanent",
            L::RepeatableActions(..) => "RepeatableActions",
            L::MayCost(..) => "MayCost",
            L::LookAtTheNextNumberCardsOnTopOfLibrary(..) => {
                "LookAtTheNextNumberCardsOnTopOfLibrary"
            }
            L::RepeatThisProcess => "RepeatThisProcess",
            L::PutUptoNumberGenericCardsIntoHand(..) => "PutUptoNumberGenericCardsIntoHand",
            L::MayCastASpellFromAmongThemWithoutPaying(..) => {
                "MayCastASpellFromAmongThemWithoutPaying"
            }
            L::ForEachCardPutIntoGraveyardUnlessCost(..) => "ForEachCardPutIntoGraveyardUnlessCost",
            L::ExileNumberGenericCardsAtRandom(..) => "ExileNumberGenericCardsAtRandom",
            L::CreatePerpetualPermanentEffect(..) => "CreatePerpetualPermanentEffect",
            L::MayPutUptoNumberCardsOntoTheBattlefield(..) => {
                "MayPutUptoNumberCardsOntoTheBattlefield"
            }
            L::GainLife(..) => "GainLife",
        })
        .collect();
    tags.join(",")
}

/// Convert a list of actions into an `Effect` chain. Returns the head and a
/// tail of follow-on effects to be wired into `sub_ability` chains by the
/// caller (since `sub_ability` lives on `AbilityDefinition`, not `Effect`).
///
/// For the spine slice, callers expect a `Vec<Effect>`; chaining is the
/// caller's responsibility (it knows the surrounding `AbilityDefinition`
/// shape).
pub fn convert_list(list: &Actions) -> ConvResult<Vec<Effect>> {
    match list {
        Actions::ActionList(actions) => {
            let mut out = Vec::with_capacity(actions.len());
            for a in actions {
                out.extend(convert_many(a)?);
            }
            Ok(out)
        }

        // CR 115.1: Targeted spell wrapper. The outer Targeted slot declares
        // the target descriptors; inner actions reference them via
        // `Ref_AnyTarget*` / `Ref_TargetPermanents*` / `DamageRecipient::Ref_*`,
        // which already collapse to the appropriate `TargetFilter` axes in
        // the leaf converters. Recursing into the inner Actions yields the
        // effect chain with the target bindings preserved.
        //
        // CR 608.2b notes that target choice is made at cast/activation time;
        // the typed constraint described by the outer Target descriptor is
        // surfaced when the engine prompts for targets. Today the inner
        // refs collapse to `TargetFilter::Any`, which loses the typed
        // constraint but preserves resolution semantics.
        Actions::Targeted(_targets, inner) => convert_list(inner),

        // CR 601.2c + CR 115.1: Distributed-damage / distributed-counters
        // wrapper (e.g. "Distribute three +1/+1 counters among any number
        // of target creatures"). Inner actions reference the targets the
        // same way as `Targeted`; the distribution mechanic is a runtime
        // concern handled by the engine's target-prompting machinery.
        Actions::TargetedDistributed(_targets, _distribution, inner) => convert_list(inner),

        _ => Err(ConversionGap::MalformedIdiom {
            idiom: "Actions/convert_list",
            path: String::new(),
            detail: format!("unsupported Actions shape: {}", actions_tag(list)),
        }),
    }
}

/// CR 613 + CR 514.2: Build a `GenericEffect` carrying a single
/// `StaticDefinition` over `affected` with `Until <expiration>` lifetime.
/// Shared between the singular and `Each*` variants.
fn build_layer_effect_until(
    affected: TargetFilter,
    effects: &[crate::schema::types::LayerEffect],
    expiration: &crate::schema::types::Expiration,
) -> ConvResult<Effect> {
    let mut mods = Vec::new();
    for eff in effects {
        mods.extend(static_effect::convert_layer_effect_dynamic(eff)?);
    }
    if mods.is_empty() {
        return Err(ConversionGap::MalformedIdiom {
            idiom: "LayerEffectUntil/build",
            path: String::new(),
            detail: "empty modification list".into(),
        });
    }
    let static_def = StaticDefinition::new(StaticMode::Continuous)
        .affected(affected.clone())
        .modifications(mods);
    Ok(Effect::GenericEffect {
        static_abilities: vec![static_def],
        duration: Some(static_effect::expiration_to_duration(expiration)?),
        target: Some(affected),
    })
}

/// CR 603.7 + CR 603.7c: Build an `Effect::CreateDelayedTrigger` for the
/// "until [expiration], whenever [event], [effect]" idiom (mtgish
/// `Action::CreateTriggerUntil` and its intervening-if sibling
/// `Action::CreateTriggerUntilI`).
///
/// Maps onto the engine's `DelayedTriggerCondition::WheneverEvent` slot —
/// a recurring delayed trigger fired by `trigger`'s event filters until the
/// runtime's hard-coded end-of-turn cleanup (`game/effects/delayed_trigger.rs:88-92`).
/// Because that lifetime is fixed, only the `UntilEndOfTurn` expiration shape
/// is convertible today; other expirations strict-fail with a refined tag
/// naming the unsupported shape.
///
/// The trigger's `execute` field is cleared (the body lives in
/// `CreateDelayedTrigger.effect`, mirroring the native parser at
/// `oracle_effect/mod.rs:303`). When `intervening_if` is supplied, it is
/// translated via `condition::convert_trigger` and parked on the embedded
/// `TriggerDefinition.condition` slot (CR 603.4 — intervening-if checked at
/// trigger time).
fn build_create_trigger_until(
    trigger: &crate::schema::types::Trigger,
    intervening_if: Option<&crate::schema::types::Condition>,
    body: &Actions,
    expiration: &crate::schema::types::Expiration,
) -> ConvResult<Effect> {
    use crate::schema::types::Expiration;

    // CR 603.7c: WheneverEvent is hard-coded to until-end-of-turn lifetime in
    // the engine runtime. Other expirations have no parameterized slot today.
    match expiration {
        Expiration::UntilEndOfTurn => {}
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "DelayedTriggerCondition",
                needed_variant: format!(
                    "WheneverEvent with non-end-of-turn expiration ({}) — runtime hard-codes \
                     end-of-turn cleanup; needs parameterized lifetime slot",
                    static_effect::expiration_tag(other),
                ),
            });
        }
    }

    let mut trigger_def = trigger_mod::convert(trigger)?;
    // CR 603.7: The body lives in `CreateDelayedTrigger.effect`, not on the
    // embedded trigger's execute slot.
    trigger_def.execute = None;

    if let Some(cond) = intervening_if {
        // CR 603.4: intervening-if rides on the trigger's condition slot.
        trigger_def.condition = Some(condition::convert_trigger(cond)?);
    }

    let body_effects = convert_list(body)?;
    let body_ability = crate::convert::build_ability_chain(AbilityKind::Spell, None, body_effects)?;

    Ok(Effect::CreateDelayedTrigger {
        condition: DelayedTriggerCondition::WheneverEvent {
            trigger: Box::new(trigger_def),
        },
        effect: Box::new(body_ability),
        uses_tracked_set: false,
    })
}

/// CR 113.6 + CR 514.2: Build a `GenericEffect` carrying one
/// `StaticDefinition` per `PermanentRule` (CantAttack / CantBlock /
/// MustAttack / …) with `Until <expiration>` lifetime. Shared between the
/// singular `CreatePermanentRuleEffectUntil` and the `Each*` variant.
fn build_rule_effect_until(
    affected: TargetFilter,
    rules: &[crate::schema::types::PermanentRule],
    expiration: &crate::schema::types::Expiration,
) -> ConvResult<Effect> {
    let statics = static_effect::build_rule_effect_statics(affected.clone(), rules)?;
    Ok(Effect::GenericEffect {
        static_abilities: statics,
        duration: Some(static_effect::expiration_to_duration(expiration)?),
        target: Some(affected),
    })
}

fn actions_tag(a: &Actions) -> String {
    serde_json::to_value(a)
        .ok()
        .and_then(|v| v.get("_Actions").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn damage_recipient_to_filter(r: &DamageRecipient) -> ConvResult<TargetFilter> {
    use engine::types::ability::{ControllerRef, TypedFilter};
    Ok(match r {
        DamageRecipient::Permanent(p) => convert_permanent(p)?,
        DamageRecipient::Player(_) => TargetFilter::Player,
        DamageRecipient::Ref_AnyTarget
        | DamageRecipient::Ref_AnyTarget1
        | DamageRecipient::Ref_AnyTarget2
        | DamageRecipient::Ref_AnyTargets
        | DamageRecipient::Ref_AnyTargets_1
        | DamageRecipient::Ref_AnyTargets_2
        | DamageRecipient::Ref_AnyTargets_3 => TargetFilter::Any,
        DamageRecipient::EachPermanent(filter) => convert_permanents(filter)?,

        // CR 119.3: "deals N damage to each player [matching ~]" — player
        // axis. Mirror the native parser's idiom in
        // `crates/engine/src/parser/oracle_target.rs` "each opponent" handler:
        // a typed-default filter scoped by ControllerRef encodes the player
        // iteration set. AnyPlayer collapses to the generic TargetFilter::Player.
        DamageRecipient::EachPlayer(players) => match &**players {
            crate::schema::types::Players::AnyPlayer => TargetFilter::Player,
            crate::schema::types::Players::Opponent => {
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
            }
            crate::schema::types::Players::SinglePlayer(p) => {
                let ctrl = filter_mod::player_to_controller(p)?;
                TargetFilter::Typed(TypedFilter::default().controller(ctrl))
            }
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "DamageRecipient/convert",
                    path: String::new(),
                    detail: format!("unsupported EachPlayer Players: {other:?}"),
                });
            }
        },

        // CR 119.3: "deals N damage to each X and each Y" — union of recipient
        // sets. The schema's MultipleRecipients carries Vec<DamageRecipient>;
        // recurse and combine via `TargetFilter::Or`. Empty lists strict-fail
        // (no damage scope means a malformed rule). Single-element lists
        // collapse to the inner filter (no spurious Or wrapper).
        DamageRecipient::MultipleRecipients(parts) => {
            if parts.is_empty() {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "DamageRecipient/convert",
                    path: String::new(),
                    detail: "MultipleRecipients with empty list".into(),
                });
            }
            let mut filters = Vec::with_capacity(parts.len());
            for part in parts {
                filters.push(damage_recipient_to_filter(part)?);
            }
            if filters.len() == 1 {
                filters.into_iter().next().unwrap()
            } else {
                TargetFilter::Or { filters }
            }
        }

        // CR 115.1 + CR 608.2b: outer-Targeted player-or-permanent slot
        // reference / generic any-target. Resolution-time the Effect::DealDamage
        // resolver binds whichever target slot the outer Action::Targeted
        // produced; engine-side these collapse to TargetFilter::Any.
        DamageRecipient::Ref_TargetPlayerOrPermanent
        | DamageRecipient::EachableTarget
        | DamageRecipient::TheChosenDamageRecipient => TargetFilter::Any,

        // CR 603.7: trigger-bound recipient (e.g. "deals damage to that
        // player"). Maps to the engine's TriggeringSource axis.
        DamageRecipient::Trigger_ThatRecipient => TargetFilter::TriggeringSource,

        // CR 119.3 + CR 508.1: "the player or planeswalker that creature is
        // attacking". Engine has no dedicated "attacked-defender" axis yet;
        // collapse to TargetFilter::Any so the resolver picks the bound
        // attack-defense slot.
        DamageRecipient::PlayerOrPlaneswalkerPermanentIsAttacking(_) => TargetFilter::Any,

        // CR 706.2: "creature or planeswalker chosen at random from <set>" —
        // the random selection is engine-side runtime behavior. Forward the
        // inner Permanents filter as the choice domain.
        DamageRecipient::CreatureOrPlaneswalkerChosenAtRandom(filter) => {
            convert_permanents(filter)?
        }
    })
}

/// Render a `CounterType` as the engine's string identifier. Engine stores
/// counters as freeform strings; we use the schema's variant name (PascalCase)
/// for everything except the special PT counters which encode the +N/+M.
pub(crate) fn counter_type_name(ct: &CounterType) -> String {
    if let CounterType::PTCounter(p, t) = ct {
        return format!("{p:+}/{t:+}");
    }
    format!("{ct:?}")
        .strip_suffix("Counter")
        .map(str::to_string)
        .unwrap_or_else(|| format!("{ct:?}"))
}

/// Outcome of attempting to lower an mtgish `Player` / `Players` reference
/// to an `AbilityDefinition::player_scope`. Three-state:
///
/// * `Ok(Some(filter))` — static scope, wire it.
/// * `Ok(None)` — `Player::You` controller default. Head-pattern detector
///   routes through the leaf path; strict-fail call sites keep their
///   previous fallthrough behavior (treat as "not a scope-bearing shape").
/// * `Err(gap)` — variant cannot be expressed as a static scope. Either a
///   target-reference (belongs in `Effect::*::target_player` slots, deferred
///   to a separate round) or an engine gap (no matching `PlayerFilter`
///   variant exists yet).
type ScopeOutcome = Result<Option<PlayerFilter>, ConversionGap>;

/// CR 119.1 + CR 119.3 + CR 101.4 (APNAP): Map an mtgish `Player` to a
/// `PlayerFilter` for use as `AbilityDefinition::player_scope`. Variants
/// fall into three buckets:
///
/// 1. **Static scope** — directly wirable to a `PlayerFilter` variant.
/// 2. **Target reference** (`Ref_TargetPlayer*`, `OwnerOfPermanent`, etc.) —
///    these are dynamic targets that belong on effect-level `target_player`
///    slots, not on `player_scope`. Strict-fail with a marker that says
///    "deferred to per-effect target_player wiring."
/// 3. **Engine gap** — no `PlayerFilter` variant covers this concept yet
///    (e.g. defending player, attacking player, monarch). Strict-fail with
///    `EnginePrerequisiteMissing` so the gap surfaces as a separate engine
///    extension round.
fn player_to_scope_opt(player: &Player) -> ScopeOutcome {
    use Player as P;

    // Helper: target-reference variants go via per-effect target_player,
    // not player_scope. Marked with a distinct `engine_type` so reports
    // bucket them separately from real engine gaps.
    let target_ref = |variant: &'static str| -> ConversionGap {
        ConversionGap::EnginePrerequisiteMissing {
            engine_type: "PlayerFilter",
            needed_variant: format!("target-ref via Effect::*::target_player ({variant})"),
        }
    };
    let engine_gap = |proposed: &str| -> ConversionGap {
        ConversionGap::EnginePrerequisiteMissing {
            engine_type: "PlayerFilter",
            needed_variant: proposed.to_string(),
        }
    };

    match player {
        // `Player::You` is the controller default; every Effect already
        // defaults to Controller. Returning Ok(None) routes the head-pattern
        // detector through the leaf path so we don't double-wrap the trivial
        // controller-scoped case. Strict-fail call sites already gate on
        // `!matches!(**player, Player::You)` so this branch is never hit
        // there.
        P::You => Ok(None),

        // CR 113.3c + CR 603.2: "that player" anaphor on triggers — resolves
        // to the player named in the current trigger event.
        P::Trigger_ThatPlayer => Ok(Some(PlayerFilter::TriggeringPlayer)),

        // CR 102.1 + CR 101.4: "each player" iterates APNAP over all players.
        P::EachablePlayer => Ok(Some(PlayerFilter::All)),

        // Self-reference + alternate spellings of "you" — also map to the
        // controller default. CR 109.5: card text referring to its controller.
        P::SelfPlayer | P::HostController | P::HostPlayer => Ok(None),

        // Engine gaps — no matching `PlayerFilter` variant exists. Each of
        // these is a category of cards (defending player triggers, attacker
        // costs, active-player phase effects, monarch/initiative triggers,
        // etc.). Surface as engine prerequisite so a future round can extend
        // `PlayerFilter`.
        P::TheActivePlayer => Err(engine_gap("Active")),
        P::DefendingPlayer | P::Trigger_DefendingPlayer => Err(engine_gap("Defending")),
        P::AttackingPlayer | P::TheAttackingPlayer => Err(engine_gap("Attacking")),
        P::TheMonarch => Err(engine_gap("Monarch")),
        P::ThePlayerWithTheInitiative => Err(engine_gap("Initiative")),
        P::ItsController => Err(engine_gap("ItsController")),
        P::NextOpponentInTurnOrder => Err(engine_gap("NextOpponentInTurnOrder")),
        P::OpponentToTheLeftOfYou => Err(engine_gap("OpponentToTheLeft")),
        P::ThePlayerWithTheMostLife => Err(engine_gap("MostLife")),
        P::ThePlayerWithTheMostCardsInHand => Err(engine_gap("MostCardsInHand")),

        // Target references — dynamic targets, not static scopes. Belong on
        // `Effect::*::target_player` slots. Deferred to per-effect target
        // wiring round.
        P::Ref_TargetPlayer => Err(target_ref("Ref_TargetPlayer")),
        P::Ref_TargetPlayer1 => Err(target_ref("Ref_TargetPlayer1")),
        P::Ref_TargetPlayer2 => Err(target_ref("Ref_TargetPlayer2")),
        P::Ref_TargetPlayer3 => Err(target_ref("Ref_TargetPlayer3")),
        P::Ref_TargetPlayers_0 => Err(target_ref("Ref_TargetPlayers_0")),
        P::Ref_TargetPlayers_1 => Err(target_ref("Ref_TargetPlayers_1")),
        P::ControllerOfTargetPermanent => Err(target_ref("ControllerOfTargetPermanent")),
        P::ControllerOfTargetPermanent2 => Err(target_ref("ControllerOfTargetPermanent2")),
        P::ControllerOfTargetSpell => Err(target_ref("ControllerOfTargetSpell")),
        P::OwnerOfTargetPermanent => Err(target_ref("OwnerOfTargetPermanent")),
        P::OwnerOfPermanent(_) => Err(target_ref("OwnerOfPermanent")),
        P::ControllerOfPermanent(_) => Err(target_ref("ControllerOfPermanent")),
        P::OwnerOfSpell(_) => Err(target_ref("OwnerOfSpell")),
        P::ControllerOfSpell(_) => Err(target_ref("ControllerOfSpell")),
        P::ControllerOfSpellOrAbility(_) => Err(target_ref("ControllerOfSpellOrAbility")),
        P::ControllerOfAbility(_) => Err(target_ref("ControllerOfAbility")),
        P::ControllerOfTriggeredAbility(_) => Err(target_ref("ControllerOfTriggeredAbility")),
        P::OwnerOfDeadPermanent => Err(target_ref("OwnerOfDeadPermanent")),
        P::OwnerOfExiledCard(_) => Err(target_ref("OwnerOfExiledCard")),
        P::OwnerOfGraveyrdCard(_) => Err(target_ref("OwnerOfGraveyrdCard")),
        P::ControllerOfDeadPermanent => Err(target_ref("ControllerOfDeadPermanent")),
        P::ControllerOfDestroyedPermanent => Err(target_ref("ControllerOfDestroyedPermanent")),
        P::ControllerOfLeavingPermanent => Err(target_ref("ControllerOfLeavingPermanent")),
        P::ControllerOfEachableDestroyedPermanent => {
            Err(target_ref("ControllerOfEachableDestroyedPermanent"))
        }
        P::ControllerOfEachableExiledPermanent => {
            Err(target_ref("ControllerOfEachableExiledPermanent"))
        }
        P::ControllerOfEachableRemovedPermanent => {
            Err(target_ref("ControllerOfEachableRemovedPermanent"))
        }

        // Remaining non-static refs (chosen-this-way / remembered / dynamic
        // anaphor / vote / clash / etc.). Surface as engine gaps so the
        // distribution shows up in the report; each cluster will need its
        // own scope-or-target-slot answer.
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "PlayerFilter",
            needed_variant: format!("Player::{}", variant_name_player(other)),
        }),
    }
}

/// CR 119.1 + CR 119.3 + CR 101.4 (APNAP): Map an mtgish `Players` to a
/// `PlayerFilter` for use as `AbilityDefinition::player_scope` on
/// `EachPlayerAction` / `APlayerAction` shapes. See `player_to_scope_opt`
/// for the three-bucket classification.
fn players_to_scope_opt(players: &Players) -> ScopeOutcome {
    use Players as Ps;
    match players {
        Ps::SinglePlayer(p) => player_to_scope_opt(p),
        // CR 102.1: opponents of the controller.
        Ps::Opponent => Ok(Some(PlayerFilter::Opponent)),
        // CR 101.4: "any player" / "each player" iterates APNAP — the engine
        // expands `PlayerFilter::All` over all players in turn order.
        Ps::AnyPlayer => Ok(Some(PlayerFilter::All)),
        // `Players::Trigger_ThosePlayers` is the plural sibling of
        // `Player::Trigger_ThatPlayer` — both anchor on the trigger event
        // (CR 113.3c + CR 603.2).
        Ps::Trigger_ThosePlayers => Ok(Some(PlayerFilter::TriggeringPlayer)),
        Ps::Trigger_IsDefendingPlayer => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "PlayerFilter",
            needed_variant: "Defending".to_string(),
        }),
        Ps::DefendingPlayerThisCombat | Ps::PossibleDefendingPlayerThisCombat => {
            Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "PlayerFilter",
                needed_variant: "Defending".to_string(),
            })
        }
        Ps::SpellDefendingPlayer => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "PlayerFilter",
            needed_variant: "Defending".to_string(),
        }),
        Ps::OpponentOf(_) => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "PlayerFilter",
            needed_variant: "OpponentOf(reference)".to_string(),
        }),
        Ps::Other(_) => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "PlayerFilter",
            needed_variant: "OtherThan(reference)".to_string(),
        }),
        Ps::Ref_TargetPlayers => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "PlayerFilter",
            needed_variant: "target-ref via Effect::*::target_player (Ref_TargetPlayers)"
                .to_string(),
        }),
        // Predicate-filtered players (`HasMaxSpeed`, `LifeTotalIs`, etc.),
        // plural compositions (`And`, `Or`, `ExceptFor`), and other dynamic
        // groupings. Each is a category requiring its own engine-side scope
        // primitive (or a different ability shape entirely). Strict-fail with
        // the variant name so the report shows the distribution.
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "PlayerFilter",
            needed_variant: format!("Players::{}", variant_name_players(other)),
        }),
    }
}

/// CR 115.2 + CR 601.2c: Map an mtgish `Player` reference to the
/// `TargetFilter` that wires it onto an effect-level player-target slot
/// (e.g. `Effect::Draw.target`, `Effect::Mill.target`,
/// `Effect::LoseLife.target`). Distinct from `player_to_scope_opt`, which
/// returns `PlayerFilter` for static `AbilityDefinition::player_scope`
/// iteration.
///
/// Per CR 601.2c, `Ref_TargetPlayer*` denotes a target chosen at cast
/// announcement; the engine extracts it via `ResolvedAbility::target_player()`
/// from `ability.targets` when the effect's `target` slot resolves to
/// `TargetFilter::Player`.
///
/// Returns `None` for variants that have no static target-filter equivalent
/// (e.g. `Player::You`, scope-only refs, predicate-filtered refs); callers
/// fall through to `player_to_scope_opt` or strict-fail.
fn player_to_target_filter(player: &Player) -> Option<TargetFilter> {
    use crate::schema::types::{Permanent, Spell};
    use Player as P;
    match player {
        // CR 601.2c: All `Ref_TargetPlayer*` slot references resolve via
        // the announced Player target. The engine doesn't distinguish
        // numbered target indices here — `target_player()` returns the
        // first `TargetRef::Player` in `ability.targets`, which is
        // sufficient for every single-Player-target spell. Multi-Player-
        // target spells (rare) are not exercised by this path.
        P::Ref_TargetPlayer
        | P::Ref_TargetPlayer1
        | P::Ref_TargetPlayer2
        | P::Ref_TargetPlayer3
        | P::Ref_TargetPlayers_0
        | P::Ref_TargetPlayers_1 => Some(TargetFilter::Player),
        // CR 608.2c + CR 113.10: "Its controller [does X]" when the
        // antecedent is the announced target spell or permanent.
        // `Player::ControllerOfSpell(Ref_TargetSpell)` /
        // `Player::ControllerOfPermanent(Ref_TargetPermanent)` /
        // `Player::ControllerOfTargetSpell` /
        // `Player::ControllerOfTargetPermanent` all resolve to
        // `TargetFilter::ParentTargetController` — the engine extracts
        // the parent ability's first object target and looks up its
        // controller (see `oracle_target.rs` for the native parser
        // emission of the same filter).
        P::ControllerOfTargetSpell | P::ControllerOfTargetPermanent => {
            Some(TargetFilter::ParentTargetController)
        }
        P::ControllerOfSpell(s) if matches!(**s, Spell::Ref_TargetSpell) => {
            Some(TargetFilter::ParentTargetController)
        }
        P::ControllerOfPermanent(p) if matches!(**p, Permanent::Ref_TargetPermanent) => {
            Some(TargetFilter::ParentTargetController)
        }
        _ => None,
    }
}

/// CR 115.2 + CR 601.2c + CR 608.2c: Rewrite an `Effect`'s player-target
/// slot to `target_filter`, preserving every other field. Used when
/// converting `Action::PlayerAction(Ref_TargetPlayer, inner)` — the inner
/// converts to a `default-controller`-targeted effect, then this function
/// re-binds the player target to the announced player.
///
/// Strict-fails for `Effect` arms whose shape has no player-target slot
/// (e.g. battlefield-only effects like `Effect::Destroy`, `Effect::Tap`).
/// In those cases the upstream `Action::PlayerAction(non-You, ...)` is
/// shape-mismatched: an opponent can't "destroy a permanent" — the inner
/// would have to be re-anchored some other way, which today no Rule
/// emits. The strict-fail surfaces such cases for future analysis.
fn apply_player_target(effect: Effect, target_filter: TargetFilter) -> ConvResult<Effect> {
    Ok(match effect {
        // CR 121.1 + CR 115.2: "Target player draws N cards."
        Effect::Draw { count, .. } => Effect::Draw {
            count,
            target: target_filter,
        },
        // CR 119.3 + CR 115.2: "Target player gains N life."
        // The `target_filter` parameter is consumed implicitly: the
        // engine reads the announced player from `ability.targets` via
        // `target_player()` when `player` is `TargetPlayer`. The filter
        // value itself doesn't carry through (`GainLifePlayer` is enum-
        // tagged, not filter-parameterized) — this branch only fires
        // for target-player refs which all map to the same announced
        // slot, so the loss of `target_filter` distinction is sound.
        Effect::GainLife { amount, .. } => Effect::GainLife {
            amount,
            player: GainLifePlayer::TargetPlayer,
        },
        // CR 119.3 + CR 115.2: "Target player loses N life."
        Effect::LoseLife { amount, .. } => Effect::LoseLife {
            amount,
            target: Some(target_filter),
        },
        // CR 701.9 + CR 115.2: "Target player discards N cards."
        Effect::DiscardCard { count, .. } => Effect::DiscardCard {
            count,
            target: target_filter,
        },
        // CR 701.17 + CR 115.2: "Target player mills N cards."
        Effect::Mill {
            count, destination, ..
        } => Effect::Mill {
            count,
            target: target_filter,
            destination,
        },
        // CR 701.22 + CR 115.2: "Target player scries N."
        Effect::Scry { count, .. } => Effect::Scry {
            count,
            target: target_filter,
        },
        // CR 701.25 + CR 115.2: "Target player surveils N."
        Effect::Surveil { count, .. } => Effect::Surveil {
            count,
            target: target_filter,
        },
        // CR 701.21 + CR 115.2: "Target player sacrifices a [filter]."
        // Routes via the existing `target` slot — at runtime, when the
        // resolved target is a Player, the sacrificing player is the
        // announced target and the choice of permanent uses the implicit
        // creature/permanent filter from the action body. Today the
        // mtgish `SacrificeAPermanent(filter)` shape doesn't carry a
        // separate "by which player" axis, so this is conservative —
        // strict-fail when reached so the call site is forced to
        // disambiguate before we silently re-anchor the wrong subject.
        Effect::Sacrifice { .. } => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "Effect::Sacrifice",
                needed_variant: "by-target-player sacrifice (subject + actor split)".into(),
            });
        }
        // CR 701.20a + CR 115.2: "Target player reveals the top N cards
        // of their library."
        Effect::RevealTop { count, .. } => Effect::RevealTop {
            player: target_filter,
            count,
        },
        // CR 701.20a + CR 115.2: "Target player reveals their hand."
        // Re-bind the `target` slot to the announced player; preserve
        // `card_filter` and `count` (entire-hand vs filtered subset).
        Effect::RevealHand {
            card_filter, count, ..
        } => Effect::RevealHand {
            target: target_filter,
            card_filter,
            count,
        },
        // CR 701.10 + CR 115.2: "Target player exiles the top N cards
        // of their library."
        Effect::ExileTop { count, .. } => Effect::ExileTop {
            player: target_filter,
            count,
        },
        // CR 111.10 + CR 115.2: "Target player creates a [token]." The
        // `Effect::Token.owner` slot is already a `TargetFilter` —
        // re-bind it to the announced player while preserving every
        // other shape field.
        Effect::Token {
            name,
            power,
            toughness,
            types,
            colors,
            keywords,
            tapped,
            count,
            attach_to,
            enters_attacking,
            supertypes,
            static_abilities,
            enter_with_counters,
            ..
        } => Effect::Token {
            name,
            power,
            toughness,
            types,
            colors,
            keywords,
            tapped,
            count,
            owner: target_filter,
            attach_to,
            enters_attacking,
            supertypes,
            static_abilities,
            enter_with_counters,
        },
        // CR 701.23a + CR 115.2: "Target player searches their library."
        // `SearchLibrary.target_player` already exists on the variant —
        // re-bind it explicitly here.
        Effect::SearchLibrary {
            filter,
            count,
            reveal,
            selection_constraint,
            ..
        } => Effect::SearchLibrary {
            filter,
            count,
            reveal,
            target_player: Some(target_filter),
            selection_constraint,
        },
        // No player-target slot on this effect. Strict-fail so the
        // shape-mismatch surfaces in the report rather than silently
        // dropping the player binding (which would resolve to controller
        // by default — a category-(c) rules-correctness violation).
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "Effect::*::target_player",
                needed_variant: format!(
                    "no player-target slot on Effect::{}",
                    effect_variant_name(&other)
                ),
            });
        }
    })
}

/// Best-effort variant name for an `Effect`. Used in strict-fail reports
/// when an `Action::PlayerAction(non-You, ...)` wraps an effect with no
/// player-target slot.
fn effect_variant_name(e: &Effect) -> String {
    serde_json::to_value(e)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| {
            format!("{e:?}")
                .split(' ')
                .next()
                .unwrap_or("?")
                .trim_end_matches('{')
                .to_string()
        })
}

/// Extract the serde-tagged variant name from a `Player` for error reporting.
/// Falls back to `{:?}` truncation if serialization fails.
fn variant_name_player(p: &Player) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.get("_Player").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| {
            format!("{p:?}")
                .split('(')
                .next()
                .unwrap_or("?")
                .to_string()
        })
}

/// Extract the serde-tagged variant name from a `Players` for error reporting.
fn variant_name_players(p: &Players) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.get("_Players").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| {
            format!("{p:?}")
                .split('(')
                .next()
                .unwrap_or("?")
                .to_string()
        })
}

fn variant_tag(a: &Action) -> String {
    serde_json::to_value(a)
        .ok()
        .and_then(|v| v.get("_Action").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// CR 701.23 (Search) + CR 701.24 (Shuffle): Lower an mtgish
/// `Action::SearchLibrary(actions)` body — a sequence of `SearchLibraryAction`
/// procedure steps — into the engine's flat effect chain
/// `Effect::SearchLibrary { filter, count, up_to, reveal, ... }
///   → Effect::ChangeZone { Library → destination, ... }
///   → [Effect::Shuffle { Controller }]`.
///
/// The engine documents that `SearchLibrary`'s destination is handled by the
/// trailing `ChangeZone` (`ability.rs:3633-3635`); the native parser composes
/// the same chain for tutor effects (`oracle_effect/imperative.rs:2502-2547`).
///
/// Recognized procedure shapes (covering the top SearchLibraryAction patterns
/// by frequency — see action sequence histogram):
///
/// | First action                                          | Filter | Count | up_to | reveal | Dest        |
/// |-------------------------------------------------------|--------|-------|-------|--------|-------------|
/// | `PutAGenericCardIntoHand`                             | Any    | 1     | false | false  | Hand        |
/// | `PutAGenericCardIntoGraveyard`                        | Any    | 1     | false | false  | Graveyard   |
/// | `MayPutACardOntoTheBattlefield(c, repls)`             | c      | 1     | true  | false  | Battlefield |
/// | `MayPutAnyNumberOfCardsOntoTheBattlefield(c, repls)`  | c      | MAX   | true  | false  | Battlefield |
/// | `MayPutUptoNumberCardsOntoTheBattlefield(n, c, repls)`| c      | n     | true  | false  | Battlefield |
/// | `MayRevealACardOfTypeAndPutItIntoHand(c)`             | c      | 1     | true  | true   | Hand        |
/// | `MayRevealUptoNumberCardsOfTypeAndPutThemIntoHand`    | c      | n     | true  | true   | Hand        |
/// | `MayRevealAnyNumberOfCardsOfTypeAndPutThemIntoHand(c)`| c      | MAX   | true  | true   | Hand        |
/// | `MayPutACardOfTypeIntoGraveyard(c)`                   | c      | 1     | true  | false  | Graveyard   |
/// | `MayExileACardOfType(c)`                              | c      | 1     | true  | false  | Exile       |
/// | `MayExileAnyNumberOfCardsOfType(c)`                   | c      | MAX   | true  | false  | Exile       |
/// | `FindACardOfType(c)` + `PutFoundCardOntoBattlefield`  | c      | 1     | false | false  | Battlefield |
///
/// The trailing step is one of `Shuffle` (emit `Effect::Shuffle`),
/// `DontShuffle` (omit shuffle), or absent (omit shuffle). Any other
/// shape strict-fails so coverage of the long tail is reported, not silently
/// elided.
fn convert_search_library(actions: &[SearchLibraryAction]) -> ConvResult<Vec<Effect>> {
    use SearchLibraryAction as S;

    let (head, rest) = actions.split_first().ok_or(ConversionGap::MalformedIdiom {
        idiom: "Action::SearchLibrary",
        path: String::new(),
        detail: "empty SearchLibrary action list".into(),
    })?;

    // Helper: max-count sentinel (CR 107.1c "any number of"). Mirrors the
    // native parser's `i32::MAX` ceiling — the resolver caps against the
    // matching set's actual size.
    let any_count = || QuantityExpr::Fixed { value: i32::MAX };

    // Lower the first procedure step into (filter, count, up_to, reveal,
    // destination, enter_replacements, consumed_extra). The optional second
    // step (e.g. `PutFound...` for the bare `Find` head) is consumed via
    // `consumed_extra` so the `tail` check below only sees the trailing
    // Shuffle/DontShuffle.
    let no_repls = EnterReplacements::default();
    let (filter, count, _up_to, reveal, destination, enter_repls, consumed_extra) = match head {
        // CR 701.23 — bare-find tutor (Demonic Tutor / Diabolic Tutor).
        S::PutAGenericCardIntoHand => (
            TargetFilter::Any,
            QuantityExpr::Fixed { value: 1 },
            false,
            false,
            Zone::Hand,
            no_repls,
            false,
        ),
        S::PutAGenericCardIntoGraveyard => (
            TargetFilter::Any,
            QuantityExpr::Fixed { value: 1 },
            false,
            false,
            Zone::Graveyard,
            no_repls,
            false,
        ),
        // CR 701.23 — Rampant Growth class. Optional ("may put") encodes as
        // `up_to: true` (CR 107.1c). Accompanying replacements (tapped,
        // under-your-control, transformed, attacking) decode via
        // `extract_enter_replacements` (CR 614.12); unrecognized variants
        // strict-fail.
        S::MayPutACardOntoTheBattlefield(cards, repls) => (
            filter_mod::cards_to_filter(cards)?,
            QuantityExpr::Fixed { value: 1 },
            true,
            false,
            Zone::Battlefield,
            extract_enter_replacements(repls)?,
            false,
        ),
        S::MayPutAnyNumberOfCardsOntoTheBattlefield(cards, repls) => (
            filter_mod::cards_to_filter(cards)?,
            any_count(),
            true,
            false,
            Zone::Battlefield,
            extract_enter_replacements(repls)?,
            false,
        ),
        S::MayPutUptoNumberCardsOntoTheBattlefield(n, cards, repls) => (
            filter_mod::cards_to_filter(cards)?,
            quantity::convert(n)?,
            true,
            false,
            Zone::Battlefield,
            extract_enter_replacements(repls)?,
            false,
        ),
        // CR 701.23 + CR 701.20 — Worldly Tutor class (reveal + hand).
        S::MayRevealACardOfTypeAndPutItIntoHand(cards) => (
            filter_mod::cards_to_filter(cards)?,
            QuantityExpr::Fixed { value: 1 },
            true,
            true,
            Zone::Hand,
            no_repls,
            false,
        ),
        S::MayRevealUptoNumberCardsOfTypeAndPutThemIntoHand(n, cards)
        | S::MayRevealUptoNumberCardsOfTypeAndPutIntoHand(n, cards) => (
            filter_mod::cards_to_filter(cards)?,
            quantity::convert(n)?,
            true,
            true,
            Zone::Hand,
            no_repls,
            false,
        ),
        S::MayRevealAnyNumberOfCardsOfTypeAndPutThemIntoHand(cards) => (
            filter_mod::cards_to_filter(cards)?,
            any_count(),
            true,
            true,
            Zone::Hand,
            no_repls,
            false,
        ),
        // CR 701.23 + CR 701.7 (Mill) / CR 701.13 (Exile).
        S::MayPutACardOfTypeIntoGraveyard(cards) => (
            filter_mod::cards_to_filter(cards)?,
            QuantityExpr::Fixed { value: 1 },
            true,
            false,
            Zone::Graveyard,
            no_repls,
            false,
        ),
        // CR 701.23 + CR 701.7: "Search your library for any number of
        // [type] cards and put them into your graveyard" (Mesmeric Orb /
        // Stitcher's Supplier-class mill tutor). Multi-result variant of
        // `MayPutACardOfTypeIntoGraveyard`.
        S::MayPutAnyNumberOfCardsOfTypeIntoGraveyard(cards) => (
            filter_mod::cards_to_filter(cards)?,
            any_count(),
            true,
            false,
            Zone::Graveyard,
            no_repls,
            false,
        ),
        S::MayExileACardOfType(cards) => (
            filter_mod::cards_to_filter(cards)?,
            QuantityExpr::Fixed { value: 1 },
            true,
            false,
            Zone::Exile,
            no_repls,
            false,
        ),
        S::MayExileAnyNumberOfCardsOfType(cards) => (
            filter_mod::cards_to_filter(cards)?,
            any_count(),
            true,
            false,
            Zone::Exile,
            no_repls,
            false,
        ),
        // CR 701.23 + CR 701.20: Mystical Tutor / Worldly Tutor / Boggart
        // Harbinger class — search, reveal, put on top of library. The
        // "set aside" step is a UX detail; the engine's
        // `Effect::SearchLibrary { reveal: true } → ChangeZone(→ Library top)`
        // expresses the rules-relevant outcome.
        S::MaySetAsideAndRevealACardOfTypeToPutOnTopOfLibrary(cards) => (
            filter_mod::cards_to_filter(cards)?,
            QuantityExpr::Fixed { value: 1 },
            true,
            true,
            Zone::Library,
            no_repls,
            false,
        ),
        // CR 701.23 + CR 701.7 (Mill): Buried Alive class — bounded
        // up-to-N tutor that puts found cards into graveyard.
        S::MayPutUptoNumberOfCardsOfTypeIntoGraveyard(n, cards) => (
            filter_mod::cards_to_filter(cards)?,
            quantity::convert(n)?,
            true,
            false,
            Zone::Graveyard,
            no_repls,
            false,
        ),
        // CR 701.23 + CR 701.20 + CR 701.24: Mystical Tutor / Liliana Vess
        // class — "search your library for a card, reveal it, then shuffle
        // and put that card on top of your library". The expected sibling
        // sequence is `MayRevealACardOfType(cards)` followed by
        // `ShuffleAndPutRevealedCardOnTop` (the shuffle + top-of-library
        // single step). Engine encoding: `SearchLibrary { reveal: true } →
        // ChangeZone(→ Library) → Shuffle`. We consume both steps here and
        // emit the shuffle inline so the trailing tail is empty.
        S::MayRevealACardOfType(cards) => {
            let f = filter_mod::cards_to_filter(cards)?;
            match rest.first() {
                Some(S::ShuffleAndPutRevealedCardOnTop) => (
                    f,
                    QuantityExpr::Fixed { value: 1 },
                    true,
                    true,
                    Zone::Library,
                    no_repls,
                    true,
                ),
                _ => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "SearchLibrary/MayRevealACardOfType",
                        path: String::new(),
                        detail: format!(
                            "MayRevealACardOfType not followed by ShuffleAndPutRevealedCardOnTop ({} steps)",
                            rest.len()
                        ),
                    });
                }
            }
        }
        // CR 701.23 + CR 701.24: "Search your library for a card, then
        // shuffle and put that card on top" (Bringer of the Black Dawn /
        // Liliana Vess -2 / Insatiable Avarice class). The set-aside step
        // is procedural; the rules-relevant outcome is identical to the
        // Mystical Tutor pattern except the card is not revealed. Expected
        // sibling sequence: `SetAsideAGenericCard` followed by `Shuffle`
        // and `PutOnTopOfLibrary` — both are consumed here.
        S::SetAsideAGenericCard => match rest {
            [S::Shuffle, S::PutOnTopOfLibrary, ..] => {
                // Consume both extra steps; tail logic below will emit no
                // additional shuffle (we emit our own inline).
                (
                    TargetFilter::Any,
                    QuantityExpr::Fixed { value: 1 },
                    false,
                    false,
                    Zone::Library,
                    no_repls,
                    // sentinel: we consumed two — handled below by setting
                    // `consumed_extra` and forcing the tail length check.
                    true,
                )
            }
            _ => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "SearchLibrary/SetAsideAGenericCard",
                    path: String::new(),
                    detail: format!(
                        "SetAsideAGenericCard not followed by [Shuffle, PutOnTopOfLibrary] ({} steps)",
                        rest.len()
                    ),
                });
            }
        },
        // CR 701.23 — `Find` is the bare two-step form: the next action
        // names the destination (Battlefield via `PutFoundCardOntoBattlefield`).
        S::FindACardOfType(cards) => {
            let f = filter_mod::cards_to_filter(cards)?;
            // Inspect the second step.
            match rest.first() {
                Some(S::PutFoundCardOntoBattlefield(repls)) => (
                    f,
                    QuantityExpr::Fixed { value: 1 },
                    false,
                    false,
                    Zone::Battlefield,
                    extract_enter_replacements(repls)?,
                    true,
                ),
                _ => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "SearchLibrary/FindACardOfType",
                        path: String::new(),
                        detail: format!(
                            "FindACardOfType not followed by recognized destination ({} steps)",
                            rest.len()
                        ),
                    });
                }
            }
        }
        other => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "SearchLibrary/head",
                path: String::new(),
                detail: format!(
                    "unsupported SearchLibraryAction head: {}",
                    search_library_action_tag(other)
                ),
            });
        }
    };

    // Tail handling. After the head (and optional consumed extra), the
    // remaining sequence must be one of: [], [Shuffle], [DontShuffle].
    // Anything richer (PutTheRemainingCardsIntoHand, etc.) is the long-tail
    // class that strict-fails so it surfaces in the report.
    let tail = if consumed_extra { &rest[1..] } else { rest };
    let shuffle = match tail {
        [] => false,
        [S::Shuffle] => true,
        [S::DontShuffle] => false,
        _ => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "SearchLibrary/tail",
                path: String::new(),
                detail: format!(
                    "unsupported SearchLibrary tail length {} (expected [Shuffle] / [DontShuffle] / [])",
                    tail.len()
                ),
            });
        }
    };

    // Build the chain. CR 701.23a: search produces the typed
    // `Effect::SearchLibrary`; CR 701.23 + CR 701.18 (move): the engine
    // encodes destination via a follow-on `Effect::ChangeZone(Library →
    // dest)`; CR 701.24 (Shuffle): optional terminal `Effect::Shuffle`.
    let mut out = Vec::with_capacity(3);
    out.push(Effect::SearchLibrary {
        filter,
        count,
        reveal,
        target_player: None,
        selection_constraint: SearchSelectionConstraint::None,
    });
    out.push(Effect::ChangeZone {
        origin: Some(Zone::Library),
        destination,
        target: TargetFilter::Any,
        owner_library: false,
        enter_transformed: enter_repls.enter_transformed,
        under_your_control: enter_repls.under_your_control,
        enter_tapped: enter_repls.enter_tapped,
        enters_attacking: enter_repls.enters_attacking,
        up_to: false,
        enter_with_counters: vec![],
    });
    if shuffle {
        out.push(Effect::Shuffle {
            target: TargetFilter::Controller,
        });
    }
    Ok(out)
}

/// CR 614.12 (replacement effects on enter): the typed projection of an
/// `Effect::ChangeZone { destination: Battlefield, ... }`'s `enter_*` flags.
///
/// The mtgish AST lists each entry-modifying replacement as a distinct
/// `ReplacementActionWouldEnter` value; the engine collapses the
/// commonly-co-occurring set into named flags on `Effect::ChangeZone`. This
/// struct is the single converter-side authority that maps the AST list to
/// those flags so every "put onto battlefield" call site (SearchLibrary
/// heads, reanimate, etc.) shares one extractor.
#[derive(Debug, Default, Clone, Copy)]
struct EnterReplacements {
    /// CR 614.1: Object enters tapped.
    enter_tapped: bool,
    /// CR 110.2: Object enters under the ability controller's control
    /// (rather than its owner's).
    under_your_control: bool,
    /// CR 712.2: Object enters showing its back face.
    enter_transformed: bool,
    /// CR 508.4: Object enters tapped and attacking.
    enters_attacking: bool,
}

/// CR 614.12: Decode the `Vec<ReplacementActionWouldEnter>` accompanying a
/// "put onto battlefield" step into a typed flag set. Recognized variants:
///
/// | Variant                                  | Engine flag           |
/// |------------------------------------------|-----------------------|
/// | `EntersNormally`                         | (no-op marker)        |
/// | `EntersTapped`                           | `enter_tapped`        |
/// | `EntersUnderPlayersControl(Player::You)` | `under_your_control`  |
/// | `EntersUnderOwnersControl`               | (no-op — engine default) |
/// | `EntersTransformed`                      | `enter_transformed`   |
/// | `EntersAttacking`                        | `enters_attacking`    |
///
/// Unrecognized variants (counters, choices, layered effects, non-You
/// player-control bindings, …) strict-fail so the gap surfaces in the
/// report. Multi-entry lists are accepted as long as every entry decodes.
fn extract_enter_replacements(
    repls: &[ReplacementActionWouldEnter],
) -> ConvResult<EnterReplacements> {
    use ReplacementActionWouldEnter as R;
    let mut out = EnterReplacements::default();
    for r in repls {
        match r {
            R::EntersNormally | R::EntersUnderOwnersControl => {}
            R::EntersTapped => out.enter_tapped = true,
            R::EntersTransformed => out.enter_transformed = true,
            R::EntersAttacking => out.enters_attacking = true,
            R::EntersUnderPlayersControl(player) => match &**player {
                Player::You => out.under_your_control = true,
                other => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "SearchLibrary/replacements",
                        path: String::new(),
                        detail: format!(
                            "EntersUnderPlayersControl({other:?}) — only Player::You is supported"
                        ),
                    });
                }
            },
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "SearchLibrary/replacements",
                    path: String::new(),
                    detail: format!("unsupported ReplacementActionWouldEnter: {other:?}"),
                });
            }
        }
    }
    Ok(out)
}

/// CR 113.6 + CR 400.7: Convert a `CardInGraveyard` (a single graveyard-card
/// reference) into a `TargetFilter` for use as the `target` slot on
/// `Effect::ChangeZone { origin: Graveyard, ... }`. The reference variants
/// (`Ref_TargetGraveyardCard*`, `TheChosenGraveyardCard`,
/// `TheGraveyardCardChosenThisWay`, etc.) all collapse to `TargetFilter::Any`
/// because the underlying choice/target was constrained when the player was
/// prompted (CR 115.1) — the engine resolves the bound target via the
/// targeting layer at resolution time. Dynamic refs that pin a specific
/// player's graveyard or carry their own predicate strict-fail so the gap
/// surfaces in the report.
fn card_in_graveyard_to_filter(card: &CardInGraveyard) -> ConvResult<TargetFilter> {
    use CardInGraveyard as C;
    Ok(match card {
        C::Ref_TargetGraveyardCard
        | C::Ref_TargetGraveyardCard1
        | C::Ref_TargetGraveyardCard2
        | C::Ref_TargetGraveyardCard3
        | C::Ref_TargetGraveyardCard4
        | C::Ref_TargetGraveyardCard5
        | C::TheGraveyardCardChosenThisWay
        | C::TheChosenGraveyardCard
        | C::TheLastGraveyardCardChosenThisWay
        | C::Trigger_ThatGraveyardCard
        | C::ThisGraveyardCard => TargetFilter::Any,
        other => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "CardInGraveyard/convert",
                path: String::new(),
                detail: format!("unsupported CardInGraveyard ref: {other:?}"),
            });
        }
    })
}

fn search_library_action_tag(a: &SearchLibraryAction) -> String {
    serde_json::to_value(a)
        .ok()
        .and_then(|v| {
            v.get("_SearchLibraryAction")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// CR 400.7 + CR 701.13: Convert a `CardInExile` reference (the parameterless
/// context refs that appear in flicker-style "return that exiled card to the
/// battlefield" sentences) into a `TargetFilter`. Mirrors
/// `card_in_graveyard_to_filter` — every recognized variant is an anaphoric
/// pointer to a previously-bound exile context (the most recent exile by this
/// effect chain, the trigger event, etc.), so they collapse to
/// `TargetFilter::Any` and the engine resolves the bound target via the
/// targeting layer at resolution time. Variants we don't yet ground
/// (player-pile context, "the second card exiled this way" pair-pointers,
/// chosen-this-way runtime refs) strict-fail so the gap surfaces.
fn card_in_exile_to_filter(card: &CardInExile) -> ConvResult<TargetFilter> {
    use CardInExile as C;
    Ok(match card {
        C::TheLastExiledCard
        | C::Ref_TargetExiledCard
        | C::Ref_TargetExiledCard1
        | C::Ref_TargetExiledCard2
        | C::TheExiledCard
        | C::TheExiledCardChosenThisWay
        | C::TheChosenExiledCard
        | C::TheCardExiledThisWay
        | C::TheExiledCardFoundThisWay
        | C::TheFirstCardExiledThisWay
        | C::TheSecondCardExiledThisWay
        | C::TheSingleCardExiledThisWay
        | C::TheSinglePermanentExiledThisWay
        | C::TheSpecificCardExiledThisWay
        | C::ThisExiledCard
        | C::ThisExiledPermanentCard
        | C::TheExiledDeadPermanent
        | C::Trigger_ThatExiledCard
        | C::WhenAPermanentIsExiled_ThatExiledPermanent => TargetFilter::Any,
        other => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "CardInExile/convert",
                path: String::new(),
                detail: format!("unsupported CardInExile ref: {other:?}"),
            });
        }
    })
}

/// CR 111.5 + CR 614.12: Lower a `Vec<TokenFlag>` onto the existing
/// `Effect::Token` slots produced by `token::convert`. Each recognized flag
/// stamps one axis on the spec (`tapped`, `enters_attacking`, `attach_to`,
/// `enter_with_counters`); unknown flags strict-fail with an explicit
/// prerequisite so the report enumerates the remaining axes that need slots.
fn apply_token_flags(mut effect: Effect, flags: &[TokenFlag]) -> ConvResult<Effect> {
    let Effect::Token {
        tapped,
        enters_attacking,
        attach_to,
        enter_with_counters,
        ..
    } = &mut effect
    else {
        return Err(ConversionGap::MalformedIdiom {
            idiom: "Action::CreateTokensWithFlags",
            path: String::new(),
            detail: "inner spec did not produce Effect::Token".into(),
        });
    };

    for flag in flags {
        match flag {
            // CR 614.12 + CR 614.1: token enters tapped.
            TokenFlag::EntersTapped => *tapped = true,
            // CR 508.4: token enters tapped and attacking.
            TokenFlag::EntersAttacking => *enters_attacking = true,
            // CR 303.7 + CR 701.4: "create a token attached to <permanent>".
            // The single-permanent variant binds to the explicit target.
            TokenFlag::EntersAttachedToPermanent(p) => {
                *attach_to = Some(convert_permanent(p)?);
            }
            // CR 303.7 + CR 701.4: "create a token attached to a <filter>" —
            // multi-match filter variant.
            TokenFlag::EntersAttachedToAPermanent(filter) => {
                *attach_to = Some(convert_permanents(filter)?);
            }
            // CR 122.1 + CR 614.12: "the token enters with a <counter> counter
            // on it." Quantity defaults to 1 — the `EntersWithNumberCounters`
            // variant carries an explicit count.
            TokenFlag::EntersWithACounter(ct) => {
                enter_with_counters.push((counter_type_name(ct), QuantityExpr::Fixed { value: 1 }));
            }
            // CR 122.1 + CR 614.12: "the token enters with N <counter>
            // counters on it" — explicit count via `quantity::convert`.
            TokenFlag::EntersWithNumberCounters(n, ct) => {
                enter_with_counters.push((counter_type_name(ct), quantity::convert(n)?));
            }
            // Remaining flags need engine slots `Effect::Token` does not
            // expose today (a `blocking_attacker` axis, an
            // `attacking_player` redirect distinct from `enters_attacking`,
            // a per-token `Vec<PermanentRule>` until-expiration). Surface as
            // engine prerequisites so the work queue tracks them.
            other => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "Effect::Token",
                    needed_variant: format!("token-flag-slot:{}", token_flag_tag(other)),
                });
            }
        }
    }

    Ok(effect)
}

fn token_flag_tag(f: &TokenFlag) -> String {
    serde_json::to_value(f)
        .ok()
        .and_then(|v| {
            v.get("_TokenFlag")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// CR 700.3 + CR 400.1: Convert a `CardsInHand` filter (a multi-match
/// predicate over the controller's hand) into a `TargetFilter` for use as
/// the `target` slot on `Effect::ChangeZone { origin: Hand, ... }`.
///
/// Today only the unconstrained variant (`AnyCard`) and singular-card refs
/// (`SingleCardInHand`) lower cleanly — the engine's hand-origin
/// `ChangeZone` resolver evaluates the target against the controller's hand
/// at resolution time, so `TargetFilter::Any` is the correct
/// "any card from your hand" filter. Typed predicates (`IsCardtype`,
/// `IsCreatureType`, `IsLandType`, `IsColor`, `ManaValueIs`, etc.)
/// strict-fail until a full `cards_in_hand → TargetFilter` lowering is
/// built — they would compose onto `FilterProp` arms but the conversion is
/// non-trivial enough to belong in `convert/filter.rs` proper rather than
/// inline here.
fn cards_in_hand_to_filter(cards: &CardsInHand) -> ConvResult<TargetFilter> {
    use CardsInHand as C;
    Ok(match cards {
        C::AnyCard | C::SingleCardInHand(_) => TargetFilter::Any,
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TargetFilter",
                needed_variant: format!("cards_in_hand_to_filter/{}", cards_in_hand_tag(other)),
            });
        }
    })
}

fn cards_in_hand_tag(c: &CardsInHand) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| {
            v.get("_CardsInHand")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn quantity_tag(g: &GameNumber) -> String {
    serde_json::to_value(g)
        .ok()
        .and_then(|v| {
            v.get("_GameNumber")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// CR 106.4: Maps a `ManaUseModifier` (mtgish "spend this mana only to ..."
/// flag) to the engine's `ManaSpendRestriction` set. Engine restrictions are
/// AND-conjoined; the rare `ManaUseModifier::And` shape is flattened, while
/// `Or` lacks a clean engine slot (would compose disjunctively across
/// restriction kinds — engine prerequisite) so it strict-fails. Unsupported
/// modifier kinds also strict-fail with explicit engine-prerequisite
/// breadcrumbs so the report drives prioritization.
fn convert_mana_use_modifier(modifier: &ManaUseModifier) -> ConvResult<Vec<ManaSpendRestriction>> {
    fn one(r: ManaSpendRestriction) -> ConvResult<Vec<ManaSpendRestriction>> {
        Ok(vec![r])
    }
    match modifier {
        ManaUseModifier::CanOnlySpendOnXCost => one(ManaSpendRestriction::XCostOnly),
        ManaUseModifier::CanOnlySpendToActivateAbilities => one(ManaSpendRestriction::ActivateOnly),
        ManaUseModifier::CanOnlySpendOnSpells(spells) => {
            let card_type = spells_to_single_cardtype(spells)?;
            one(ManaSpendRestriction::SpellType(card_type_name(card_type)))
        }
        ManaUseModifier::And(modifiers) => {
            let mut out = Vec::with_capacity(modifiers.len());
            for m in modifiers {
                out.extend(convert_mana_use_modifier(m)?);
            }
            Ok(out)
        }
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ManaSpendRestriction",
            needed_variant: format!("ManaUseModifier::{}", mana_use_modifier_tag(other)),
        }),
    }
}

/// Extracts a single `CardType` from a `Spells` filter shape. Matches only
/// the unambiguous `IsCardtype` form; anything richer (combinator, multi-type
/// `Or`, color filter, conditional) returns a strict-failure prerequisite.
fn spells_to_single_cardtype(spells: &Spells) -> ConvResult<&CardType> {
    match spells {
        Spells::IsCardtype(ct) => Ok(ct),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ManaSpendRestriction::SpellType",
            needed_variant: format!("Spells filter beyond IsCardtype: {}", spells_tag(other)),
        }),
    }
}

fn card_type_name(ct: &CardType) -> String {
    match ct {
        CardType::Artifact => "Artifact",
        CardType::Battle => "Battle",
        CardType::Conspiracy => "Conspiracy",
        CardType::Creature => "Creature",
        CardType::Dungeon => "Dungeon",
        CardType::Enchantment => "Enchantment",
        CardType::Instant => "Instant",
        CardType::Kindred => "Kindred",
        CardType::Land => "Land",
        CardType::Phenomenon => "Phenomenon",
        CardType::Plane => "Plane",
        CardType::Planeswalker => "Planeswalker",
        CardType::Scheme => "Scheme",
        CardType::Sorcery => "Sorcery",
        CardType::Vanguard => "Vanguard",
    }
    .to_string()
}

fn mana_use_modifier_tag(m: &ManaUseModifier) -> String {
    serde_json::to_value(m)
        .ok()
        .and_then(|v| {
            v.get("_ManaUseModifier")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn spells_tag(s: &Spells) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.get("_Spells").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}
