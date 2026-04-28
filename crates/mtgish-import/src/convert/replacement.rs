//! mtgish `AsPermanentEnters` and friends → engine `ReplacementDefinition`
//! (Phase 9 narrow slice).
//!
//! Covers ETB-tapped and ETB-with-N-counters — the two highest-frequency
//! shapes. Other replacement events (damage, draw, gain-life, etc.) and
//! the "enter as a copy" / "enter transformed" / etc. variants land later.

use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ChoiceType, ControllerRef, DamageModification,
    DamageTargetFilter, Effect, QuantityExpr, QuantityModification, ReplacementCondition,
    ReplacementDefinition, ReplacementMode, TargetFilter,
};
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

use crate::convert::filter::{convert as convert_permanents, convert_permanent};
use crate::convert::mana;
use crate::convert::quantity;
use crate::convert::result::{ConvResult, ConversionGap};
use crate::schema::types::{
    Condition, CounterType, Expiration, FutureReplacableEventWouldDealDamage, GameNumber,
    Permanent, Permanents, Player, Players, ReplacableEventWouldDealDamage,
    ReplacableEventWouldDraw, ReplacableEventWouldEnter, ReplacableEventWouldGainLife,
    ReplacableEventWouldPutCounters, ReplacableEventWouldPutIntoGraveyard,
    ReplacementActionWouldDealDamage, ReplacementActionWouldDraw, ReplacementActionWouldEnter,
    ReplacementActionWouldEnterCost, ReplacementActionWouldGainLife,
    ReplacementActionWouldPutCounters, ReplacementActionWouldPutIntoGraveyard,
    SingleDamageRecipient,
};

/// CR 702.138: `Rule::AsPermanentEscapes(target, actions)` — Theros
/// Beyond Death "escape" mechanic. Structurally identical to
/// `AsPermanentEnters` (the actions are `ReplacementActionWouldEnter`),
/// but the replacement should fire only when the card enters via Escape
/// — not on every ETB. The engine has no Escape gating slot on
/// `ReplacementDefinition` today; strict-fail with EnginePrerequisiteMissing.
pub fn convert_as_escapes(
    _target: &Permanent,
    _actions: &[ReplacementActionWouldEnter],
) -> ConvResult<Vec<ReplacementDefinition>> {
    Err(ConversionGap::EnginePrerequisiteMissing {
        engine_type: "ReplacementDefinition",
        needed_variant: "Escape gating (CR 702.138) — fire only on cast-from-graveyard ETB".into(),
    })
}

/// CR 614.12: Build a `ReplacementDefinition` from `AsPermanentEnters(target,
/// actions)`. Each action becomes one definition (engine pairs one
/// ReplacementDefinition per replacement event source).
pub fn convert_as_enters(
    target: &Permanent,
    actions: &[ReplacementActionWouldEnter],
) -> ConvResult<Vec<ReplacementDefinition>> {
    let valid_card = convert_permanent(target)?;
    let mut out = Vec::new();
    let mut iter = actions.iter().peekable();
    while let Some(act) = iter.next() {
        if let Some(def) = try_build_may_cost_pair(act, iter.peek().copied(), &valid_card)? {
            iter.next();
            out.push(def);
            continue;
        }
        let (condition, mode, exec) = build_replacement_exec(act, &valid_card)?;
        out.push(ReplacementDefinition {
            expires_at_eot: false,
            event: ReplacementEvent::ChangeZone,
            execute: Some(Box::new(exec)),
            mode,
            valid_card: Some(valid_card.clone()),
            description: None,
            condition,
            destination_zone: Some(Zone::Battlefield),
            damage_modification: None,
            damage_source_filter: None,
            damage_target_filter: None,
            combat_scope: None,
            shield_kind: Default::default(),
            quantity_modification: None,
            token_owner_scope: None,
            valid_player: None,
            is_consumed: false,
            redirect_target: None,
            mana_modification: None,
            additional_token_spec: None,
            ensure_token_specs: None,
        });
    }
    Ok(out)
}

/// CR 614.12: Build replacement definitions from `ReplaceWouldEnter(event,
/// actions)`. The event names which permanents the replacement applies to;
/// `PermanentWouldEnterTheBattlefield` narrows to a single source-referent
/// (handled identically to `AsPermanentEnters`), while the
/// `APermanentWouldEnterTheBattlefield*` variants apply to *any* permanent
/// matching the filter (e.g., "permanents your opponents control enter
/// tapped").
pub fn convert_replace_would_enter(
    event: &ReplacableEventWouldEnter,
    actions: &[ReplacementActionWouldEnter],
) -> ConvResult<Vec<ReplacementDefinition>> {
    use ReplacableEventWouldEnter as E;
    let valid_card = match event {
        E::PermanentWouldEnterTheBattlefield(p) => convert_permanent(p)?,
        E::APermanentWouldEnterTheBattlefield(ps) => convert_permanents(ps)?,
        // CR 601.2: "wasn't cast" / "from exile or after being cast from exile"
        // and "under a player's control" carry extra event-side gating that the
        // engine's ReplacementDefinition doesn't expose as a structured hook
        // yet. Surface as a strict gap so the report tracks the engine work.
        E::PermanentWouldEnterTheBattlefieldAndWasntCastOrNoManaWasSpentToCast(_)
        | E::APermanentWouldEnterTheBattlefieldAndWasntCast(_)
        | E::APermanentWouldEnterTheBattlefieldFromExileOrAfterBeingCastFromExile(_)
        | E::APermanentWouldEnterTheBattlefieldUnderAPlayersControl(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: format!(
                    "ReplaceWouldEnter event gating ({})",
                    serde_json::to_value(event)
                        .ok()
                        .and_then(|v| v
                            .get("_ReplacableEventWouldEnter")
                            .and_then(|t| t.as_str())
                            .map(String::from))
                        .unwrap_or_else(|| "<unknown>".into())
                ),
            });
        }
    };

    let mut out = Vec::new();
    let mut iter = actions.iter().peekable();
    while let Some(act) = iter.next() {
        if let Some(def) = try_build_may_cost_pair(act, iter.peek().copied(), &valid_card)? {
            iter.next();
            out.push(def);
            continue;
        }
        let (condition, mode, exec) = build_replacement_exec(act, &valid_card)?;
        out.push(ReplacementDefinition {
            expires_at_eot: false,
            event: ReplacementEvent::ChangeZone,
            execute: Some(Box::new(exec)),
            mode,
            valid_card: Some(valid_card.clone()),
            description: None,
            condition,
            destination_zone: Some(Zone::Battlefield),
            damage_modification: None,
            damage_source_filter: None,
            damage_target_filter: None,
            combat_scope: None,
            shield_kind: Default::default(),
            quantity_modification: None,
            token_owner_scope: None,
            valid_player: None,
            is_consumed: false,
            redirect_target: None,
            mana_modification: None,
            additional_token_spec: None,
            ensure_token_specs: None,
        });
    }
    Ok(out)
}

/// CR 614.2 + CR 615.1: Build replacement definitions from
/// `ReplaceWouldDealDamage(event, actions)`. The event names which
/// damage events the replacement applies to (mapped to
/// `damage_source_filter` / `damage_target_filter` / `combat_scope`
/// slots on `ReplacementDefinition`); the actions are folded into the
/// replacement's `damage_modification` slot. Each action becomes one
/// definition (callers append).
///
/// Action coverage: prevention-family actions (PreventThatDamage /
/// CancelThatDamage / PreventSomeOfThatDamage) plus `DealDamageInstead`
/// (flat override via `DamageModification::SetTo`). Other actions
/// (DealToTargetInstead, redirection variants) strict-fail pending
/// further engine extensions.
pub fn convert_replace_would_deal_damage(
    event: &ReplacableEventWouldDealDamage,
    actions: &[ReplacementActionWouldDealDamage],
) -> ConvResult<Vec<ReplacementDefinition>> {
    // CR 614.x: "If [event-A] or [event-B]" — expand to one
    // ReplacementDefinition per inner event (engine has no event-OR slot;
    // multiple replacements are equivalent under CR 616 ordering).
    if let ReplacableEventWouldDealDamage::Or(inner) = event {
        let mut out = Vec::new();
        for sub in inner {
            out.extend(convert_replace_would_deal_damage(sub, actions)?);
        }
        return Ok(out);
    }
    let event_filters = event_to_damage_filters(event)?;
    let mut out = Vec::new();
    for act in actions {
        let modification = damage_action_to_modification(act)?;
        out.push(ReplacementDefinition {
            expires_at_eot: false,
            event: ReplacementEvent::DamageDone,
            execute: None,
            mode: Default::default(),
            valid_card: None,
            description: None,
            condition: None,
            destination_zone: None,
            damage_modification: Some(modification),
            damage_source_filter: event_filters.source_filter.clone(),
            damage_target_filter: event_filters.target_filter.clone(),
            combat_scope: event_filters.combat_scope.clone(),
            shield_kind: Default::default(),
            quantity_modification: None,
            token_owner_scope: None,
            valid_player: None,
            is_consumed: false,
            redirect_target: None,
            mana_modification: None,
            additional_token_spec: None,
            ensure_token_specs: None,
        });
    }
    Ok(out)
}

#[derive(Default, Clone)]
struct DamageEventFilters {
    source_filter: Option<TargetFilter>,
    target_filter: Option<DamageTargetFilter>,
    combat_scope: Option<engine::types::ability::CombatDamageScope>,
}

/// CR 614.2: Decompose a `ReplacableEventWouldDealDamage` into the three
/// engine-side filter slots. Only the high-frequency variants are
/// recognised — others strict-fail so the report tracks the work queue.
fn event_to_damage_filters(
    event: &ReplacableEventWouldDealDamage,
) -> ConvResult<DamageEventFilters> {
    use engine::types::ability::CombatDamageScope;
    use ReplacableEventWouldDealDamage as E;
    Ok(match event {
        // "Damage would be dealt to [recipient]" — recipient sets the
        // target filter; source is unrestricted.
        E::DamageWouldBeDealtToRecipient(r) => DamageEventFilters {
            source_filter: None,
            target_filter: recipient_to_damage_target_filter(r),
            combat_scope: None,
        },
        E::DamageWouldBeDealtToARecipient(_recipients) => DamageEventFilters {
            // Multi-recipient lists don't fit the single-target slot; leave
            // unfiltered so the runtime applies broadly. Refinement is a
            // future engine extension.
            source_filter: None,
            target_filter: None,
            combat_scope: None,
        },
        // Source-typed damage events. We don't yet decompose `DamageSources`
        // into a `TargetFilter`; emit unfiltered to keep the structural
        // shape valid (the engine will apply broadly until source filtering
        // lands).
        E::DamageWouldBeDealtByASourceToRecipient(_src, recipient) => DamageEventFilters {
            source_filter: None,
            target_filter: recipient_to_damage_target_filter(recipient),
            combat_scope: None,
        },
        E::DamageWouldBeDealtByASourceToARecipient(_src, _recipients) => {
            DamageEventFilters::default()
        }
        E::DamageWouldBeDealtByAPermanentToRecipient(_perm, recipient) => DamageEventFilters {
            source_filter: None,
            target_filter: recipient_to_damage_target_filter(recipient),
            combat_scope: None,
        },
        // CR 614.2: Permanent-source, multi-recipient variant — leave
        // both source and target unfiltered so the runtime applies
        // broadly. Source-permanent narrowing is a future engine
        // extension (mirrors `DamageWouldBeDealtByASourceToARecipient`).
        E::DamageWouldBeDealtByAPermanentToARecipient(_perms, _recipients) => {
            DamageEventFilters::default()
        }
        // CR 614.2 + CR 510.1a: Noncombat damage to a multi-recipient
        // list. Same handling as the single-recipient variant but with
        // an unfiltered target slot.
        E::NoncombatDamageWouldBeDealtToARecipient(_recipients) => DamageEventFilters {
            source_filter: None,
            target_filter: None,
            combat_scope: Some(CombatDamageScope::NoncombatOnly),
        },
        // Combat-only / noncombat-only restrictors set `combat_scope` per
        // CR 614.1a.
        E::CombatDamageWouldBeDealtByACreatureToRecipient(_perm, recipient) => DamageEventFilters {
            source_filter: None,
            target_filter: recipient_to_damage_target_filter(recipient),
            combat_scope: Some(CombatDamageScope::CombatOnly),
        },
        E::CombatDamageWouldBeDealtToRecipient(recipient) => DamageEventFilters {
            source_filter: None,
            target_filter: recipient_to_damage_target_filter(recipient),
            combat_scope: Some(CombatDamageScope::CombatOnly),
        },
        E::NoncombatDamageWouldBeDealtToRecipient(recipient) => DamageEventFilters {
            source_filter: None,
            target_filter: recipient_to_damage_target_filter(recipient),
            combat_scope: Some(CombatDamageScope::NoncombatOnly),
        },
        other => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: damage_event_tag(other),
            });
        }
    })
}

/// CR 614.2: Map a `SingleDamageRecipient` to the engine's
/// `DamageTargetFilter`. The engine enum is intentionally narrow — only
/// "creature", "player", and "opponent" kinds exist. Recipients that
/// don't match cleanly leave the slot `None` (broad match).
fn recipient_to_damage_target_filter(r: &SingleDamageRecipient) -> Option<DamageTargetFilter> {
    match r {
        SingleDamageRecipient::Player(p) => match &**p {
            // "to you" — no engine variant for self-only; broad match.
            Player::You => None,
            // All other player refs collapse to a generic "player" filter.
            // (`Player::Opponent` doesn't exist; opponent recipients in the
            // corpus are encoded via `Players::Opponent` paired with a
            // multi-recipient list, not a `SingleDamageRecipient::Player`.)
            _ => Some(DamageTargetFilter::PlayerOnly),
        },
        // Permanent recipient — narrow shapes use `CreatureOnly` (the
        // closest engine analogue for "to a permanent / creature").
        SingleDamageRecipient::Permanent(_) => Some(DamageTargetFilter::CreatureOnly),
        _ => None,
    }
}

/// CR 615.1 + CR 614.1a: Map a `ReplacementActionWouldDealDamage` to a
/// `DamageModification`. Only the prevention family is covered today;
/// other action shapes strict-fail.
fn damage_action_to_modification(
    act: &ReplacementActionWouldDealDamage,
) -> ConvResult<DamageModification> {
    match act {
        // CR 615.1: "Prevent that damage." / "If a source would deal damage
        // ... prevent that damage." Continuous prevent-all replacement encoded
        // as `Minus { value: u32::MAX }` — saturating-subtraction yields 0 for
        // any amount and the replacement is not consumed.
        ReplacementActionWouldDealDamage::PreventThatDamage
        | ReplacementActionWouldDealDamage::CancelThatDamage => {
            Ok(DamageModification::Minus { value: u32::MAX })
        }
        // "Prevent N of that damage."
        ReplacementActionWouldDealDamage::PreventSomeOfThatDamage(g) => {
            let qty = quantity::convert(g)?;
            match qty {
                QuantityExpr::Fixed { value } if (0..=u32::MAX as i32).contains(&value) => {
                    Ok(DamageModification::Minus {
                        value: value as u32,
                    })
                }
                // CR 615.1: Dynamic prevention amount ("prevent X damage,
                // where X is …") — engine `DamageModification::Minus`
                // takes only `u32`, not `QuantityExpr`.
                _ => Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "DamageModification",
                    needed_variant: "Minus { count: QuantityExpr }".into(),
                }),
            }
        }
        // CR 614.1a: "It deals N damage instead." Flat override of the
        // event's amount via the `SetTo` variant. The Twice/Thrice
        // multiplier idioms over `WouldDealDamage_ThatMuchDamage` (Furnace
        // of Rath / Fiery Emancipation / Angrath's Marauders) map onto
        // the engine's typed `Double` / `Triple` modifications. Other
        // dynamic shapes strict-fail.
        ReplacementActionWouldDealDamage::DealDamageInstead(g) => {
            // CR 614.1a: "[source] deals twice/thrice that much damage
            // instead" — typed multiplier replacement.
            if let Some(modification) = damage_multiplier_modification(g) {
                return Ok(modification);
            }
            let qty = quantity::convert(g)?;
            match qty {
                QuantityExpr::Fixed { value } if (0..=u32::MAX as i32).contains(&value) => {
                    Ok(DamageModification::SetTo {
                        value: value as u32,
                    })
                }
                _ => Err(ConversionGap::MalformedIdiom {
                    idiom: "DamageAction/DealDamageInstead",
                    path: String::new(),
                    detail: "non-fixed override amount needs dynamic SetTo".into(),
                }),
            }
        }
        // CR 615.x: Damage replacement actions that aren't a
        // straightforward `DamageModification` — they require an
        // `execute` body (counter manipulation, sacrifice, draw, life
        // loss, etc.) or post-replacement composition (Or/If/IfElse/
        // Unless/MayAction/MayActions/MustCost). The engine's
        // `damage_modification` slot only carries arithmetic; it has no
        // hook for arbitrary side-effects on the damage replacement.
        ReplacementActionWouldDealDamage::If(_, _)
        | ReplacementActionWouldDealDamage::IfElse(_, _, _)
        | ReplacementActionWouldDealDamage::Unless(_, _)
        | ReplacementActionWouldDealDamage::MayAction(_)
        | ReplacementActionWouldDealDamage::MayActions(_)
        | ReplacementActionWouldDealDamage::MustCost(_)
        | ReplacementActionWouldDealDamage::PlayerMayCost(_, _)
        | ReplacementActionWouldDealDamage::EachPlayerAction(_, _)
        | ReplacementActionWouldDealDamage::PlayerAction(_, _)
        | ReplacementActionWouldDealDamage::ChooseAPlayer(_) => {
            Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: format!(
                    "damage replacement gating action ({})",
                    damage_action_tag(act)
                ),
            })
        }
        // CR 615.x: Side-effect-bearing damage replacements
        // (counters, life-loss, draw, sacrifice, token-create, mill,
        // exile, destroy, gain-control, redirect-to-target). These
        // need an execute body the runtime currently doesn't read for
        // damage replacements.
        ReplacementActionWouldDealDamage::PutACounterOfTypeOnPermanent(_, _)
        | ReplacementActionWouldDealDamage::PutNumberCountersOfTypeOnPermanent(_, _, _)
        | ReplacementActionWouldDealDamage::RemoveACounterOfTypeFromPermanent(_, _)
        | ReplacementActionWouldDealDamage::RemoveNumberCountersOfTypeFromPermanent(_, _, _)
        | ReplacementActionWouldDealDamage::CreateTokens(_)
        | ReplacementActionWouldDealDamage::DestroyPermanent(_)
        | ReplacementActionWouldDealDamage::DrawNumberCards(_)
        | ReplacementActionWouldDealDamage::ExileNumberGraveyardCards(_, _)
        | ReplacementActionWouldDealDamage::ExileTheTopNumberCardsOfLibrary(_)
        | ReplacementActionWouldDealDamage::GainControlOfPermanent(_)
        | ReplacementActionWouldDealDamage::GainLife(_)
        | ReplacementActionWouldDealDamage::GetNumberRadCounters(_)
        | ReplacementActionWouldDealDamage::MillNumberCards(_)
        | ReplacementActionWouldDealDamage::SacrificeNumberPermanents(_, _)
        | ReplacementActionWouldDealDamage::ShufflePermanentIntoLibrary(_)
        | ReplacementActionWouldDealDamage::LoseTheGame
        | ReplacementActionWouldDealDamage::ContinueDealingDamage
        | ReplacementActionWouldDealDamage::DealDamageAsThoughItHadInfect
        | ReplacementActionWouldDealDamage::DealSomeDamageToRecipientInstead(_, _)
        | ReplacementActionWouldDealDamage::DealToAnyTargetInstead(_)
        | ReplacementActionWouldDealDamage::DealToCreatureOrPlaneswalkerInstead(_)
        | ReplacementActionWouldDealDamage::DealToPlayerInstead(_)
        | ReplacementActionWouldDealDamage::PreventAllButSomeOfThatDamage(_)
        | ReplacementActionWouldDealDamage::PermanentDealsDamage(_, _, _)
        | ReplacementActionWouldDealDamage::SpellDealsDamage(_, _, _)
        | ReplacementActionWouldDealDamage::HaveSpellDealDamage(_, _, _)
        | ReplacementActionWouldDealDamage::VanguardDealsDamage(_, _, _)
        | ReplacementActionWouldDealDamage::CreateFutureTrigger(_, _)
        | ReplacementActionWouldDealDamage::ReflexiveTrigger(_) => {
            Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: format!(
                    "damage replacement side-effect ({})",
                    damage_action_tag(act)
                ),
            })
        }
    }
}

/// CR 614.1a: Recognise "twice / thrice the damage that would be dealt"
/// idioms over `WouldDealDamage_ThatMuchDamage` and map them onto the
/// engine's typed multiplicative `DamageModification` variants
/// (Furnace of Rath / Fiery Emancipation / Angrath's Marauders class).
/// Returns `None` for any other shape so the caller falls through to the
/// generic SetTo path.
fn damage_multiplier_modification(g: &GameNumber) -> Option<DamageModification> {
    match g {
        GameNumber::Twice(inner)
            if matches!(**inner, GameNumber::WouldDealDamage_ThatMuchDamage) =>
        {
            Some(DamageModification::Double)
        }
        GameNumber::Thrice(inner)
            if matches!(**inner, GameNumber::WouldDealDamage_ThatMuchDamage) =>
        {
            Some(DamageModification::Triple)
        }
        _ => None,
    }
}

fn damage_event_tag(e: &ReplacableEventWouldDealDamage) -> String {
    serde_json::to_value(e)
        .ok()
        .and_then(|v| {
            v.get("_ReplacableEventWouldDealDamage")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn damage_action_tag(a: &ReplacementActionWouldDealDamage) -> String {
    serde_json::to_value(a)
        .ok()
        .and_then(|v| {
            v.get("_ReplacementActionWouldDealDamage")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// CR 614.11: Build replacement definitions from `ReplaceWouldDraw(event,
/// actions)`. The runtime `draw_applier` reads the count from the
/// `execute` body's `Effect::Draw { count, .. }` and substitutes it for
/// the original event's draw count. So:
///
/// - `DrawACard` → `count = 1`
/// - `DrawNumberCards(N)` → `count = N`
/// - `SkipThatDraw` → `count = 0` (the draw is replaced with no cards)
///
/// Other actions (PlayerAction / ChooseAnAction / If / Unless / IfElse /
/// LookAtTheTopNumberCardsOfLibrary / etc.) require execute bodies the
/// runtime doesn't yet read; they strict-fail.
pub fn convert_replace_would_draw(
    event: &ReplacableEventWouldDraw,
    actions: &[ReplacementActionWouldDraw],
) -> ConvResult<Vec<ReplacementDefinition>> {
    let valid_player = draw_event_to_valid_player(event)?;
    let mut out = Vec::new();
    for act in actions {
        let count = draw_action_to_count(act)?;
        let exec = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count,
                target: TargetFilter::Controller,
            },
        );
        out.push(ReplacementDefinition {
            expires_at_eot: false,
            event: ReplacementEvent::Draw,
            execute: Some(Box::new(exec)),
            mode: Default::default(),
            valid_card: None,
            description: None,
            condition: None,
            destination_zone: None,
            damage_modification: None,
            damage_source_filter: None,
            damage_target_filter: None,
            combat_scope: None,
            shield_kind: Default::default(),
            quantity_modification: None,
            token_owner_scope: None,
            valid_player: valid_player.clone(),
            is_consumed: false,
            redirect_target: None,
            mana_modification: None,
            additional_token_spec: None,
            ensure_token_specs: None,
        });
    }
    Ok(out)
}

/// CR 614.11: Map the schema event variant to a `valid_player` filter.
/// "A player would draw a card" → `None` (any player); "you would draw"
/// → `Some(You)`. Future variants (extra-draw / multi-draw events) need
/// per-event runtime support and strict-fail today.
fn draw_event_to_valid_player(
    event: &ReplacableEventWouldDraw,
) -> ConvResult<Option<engine::types::ability::ControllerRef>> {
    use engine::types::ability::ControllerRef;
    use ReplacableEventWouldDraw as E;
    match event {
        E::APlayerWouldDrawACard(_)
        | E::APlayerWouldDrawOneOrMoreCards(_)
        | E::APlayerWouldDrawTwoOrMoreCards(_) => Ok(None),
        E::PlayerWouldDrawDuringTheirDrawStep(p) => match &**p {
            Player::You => Ok(Some(ControllerRef::You)),
            _ => Ok(None),
        },
        other => Err(ConversionGap::UnknownVariant {
            path: String::new(),
            repr: serde_json::to_value(other)
                .ok()
                .and_then(|v| {
                    v.get("_ReplacableEventWouldDraw")
                        .and_then(|t| t.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| "<unknown>".to_string()),
        }),
    }
}

/// CR 614.11: Map a draw replacement action to the `count` quantity that
/// the engine's `draw_applier` will substitute for the original event.
fn draw_action_to_count(act: &ReplacementActionWouldDraw) -> ConvResult<QuantityExpr> {
    match act {
        ReplacementActionWouldDraw::DrawACard => Ok(QuantityExpr::Fixed { value: 1 }),
        ReplacementActionWouldDraw::DrawNumberCards(g) => quantity::convert(g),
        ReplacementActionWouldDraw::SkipThatDraw => Ok(QuantityExpr::Fixed { value: 0 }),
        other => Err(ConversionGap::UnknownVariant {
            path: String::new(),
            repr: serde_json::to_value(other)
                .ok()
                .and_then(|v| {
                    v.get("_ReplacementActionWouldDraw")
                        .and_then(|t| t.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| "<unknown>".to_string()),
        }),
    }
}

/// CR 614.6 + CR 614.12: Build replacement definitions from
/// `ReplaceWouldPutIntoGraveyard(event, actions)`. The engine encodes
/// zone-redirect on death by emitting a `ReplacementEvent::Moved`
/// definition whose `destination_zone = Some(Zone::Graveyard)` (matches
/// the in-flight "would be put into graveyard" event) and whose
/// `execute` body is `Effect::ChangeZone { destination: <new zone> }`.
/// The replacement runtime computes `redirect_zone` from the execute
/// body's destination and rewrites the ProposedEvent in place.
///
/// Action coverage: the four "instead" zone-redirect shapes
/// (ExileItInstead / PutItInOwnersHandInstead / PutItOnTopOfOwners
/// LibraryInstead / PutItOnBottomOfOwnersLibraryInstead). Counter-on-
/// exile and conditional shapes strict-fail.
pub fn convert_replace_would_put_into_graveyard(
    event: &ReplacableEventWouldPutIntoGraveyard,
    actions: &[ReplacementActionWouldPutIntoGraveyard],
) -> ConvResult<Vec<ReplacementDefinition>> {
    let valid_card = graveyard_event_to_valid_card(event)?;
    let mut out = Vec::new();
    for act in actions {
        let destination = graveyard_action_to_destination(act)?;
        let exec = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: Vec::new(),
            },
        );
        out.push(ReplacementDefinition {
            expires_at_eot: false,
            event: ReplacementEvent::Moved,
            execute: Some(Box::new(exec)),
            mode: Default::default(),
            valid_card: valid_card.clone(),
            description: None,
            condition: None,
            destination_zone: Some(Zone::Graveyard),
            damage_modification: None,
            damage_source_filter: None,
            damage_target_filter: None,
            combat_scope: None,
            shield_kind: Default::default(),
            quantity_modification: None,
            token_owner_scope: None,
            valid_player: None,
            is_consumed: false,
            redirect_target: None,
            mana_modification: None,
            additional_token_spec: None,
            ensure_token_specs: None,
        });
    }
    Ok(out)
}

/// CR 614.6: Decompose a `ReplacableEventWouldPutIntoGraveyard` event
/// into a `valid_card` filter scoping which permanents the replacement
/// applies to. Only the `APermanentWouldDie(Permanents)` shape is
/// covered today (33 of the 51 corpus occurrences); the multi-zone /
/// player-scoped variants need additional engine plumbing.
fn graveyard_event_to_valid_card(
    event: &ReplacableEventWouldPutIntoGraveyard,
) -> ConvResult<Option<TargetFilter>> {
    use ReplacableEventWouldPutIntoGraveyard as E;
    match event {
        E::APermanentWouldDie(perms) | E::APermanentWouldBePutIntoAGraveyard(perms) => {
            Ok(Some(convert_permanents(perms)?))
        }
        other => Err(ConversionGap::UnknownVariant {
            path: String::new(),
            repr: serde_json::to_value(other)
                .ok()
                .and_then(|v| {
                    v.get("_ReplacableEventWouldPutIntoGraveyard")
                        .and_then(|t| t.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| "<unknown>".to_string()),
        }),
    }
}

/// CR 614.6: Build replacement definitions for `Rule::AsPutIntoA
/// GraveyardFromAnywhere(SingleCard, Vec<PutIntoGraveyardAction>)` —
/// the Rest in Peace / Necropotence "if [this] would be put into a
/// graveyard from anywhere, [redirect]" pattern. Differs from
/// `convert_replace_would_put_into_graveyard` in two ways:
///
/// 1. `valid_card = Some(SelfRef)` (the rule is keyed on self, not a
///    permanents filter).
/// 2. The Effect::ChangeZone has no `origin` constraint (None), since
///    the rule fires on graveyard-from-anywhere — battlefield, hand,
///    library, exile, even stack.
///
/// Action coverage: `ExileItInstead` → `Zone::Exile`. Other variants
/// (RevealItAndShuffleItIntoLibraryInstead) need extra Effect::ChangeZone
/// shape that doesn't exist today (no shuffle-after-redirect slot) and
/// strict-fail.
pub fn convert_as_put_into_graveyard_from_anywhere(
    actions: &[crate::schema::types::PutIntoGraveyardAction],
) -> ConvResult<Vec<ReplacementDefinition>> {
    use crate::schema::types::PutIntoGraveyardAction as A;
    let mut out = Vec::new();
    for act in actions {
        let destination = match act {
            A::ExileItInstead => Zone::Exile,
            other => {
                return Err(ConversionGap::UnknownVariant {
                    path: String::new(),
                    repr: serde_json::to_value(other)
                        .ok()
                        .and_then(|v| {
                            v.get("_PutIntoGraveyardAction")
                                .and_then(|t| t.as_str())
                                .map(String::from)
                        })
                        .unwrap_or_else(|| "<unknown>".to_string()),
                });
            }
        };
        let exec = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: Vec::new(),
            },
        );
        out.push(ReplacementDefinition {
            expires_at_eot: false,
            event: ReplacementEvent::Moved,
            execute: Some(Box::new(exec)),
            mode: Default::default(),
            valid_card: Some(TargetFilter::SelfRef),
            description: None,
            condition: None,
            destination_zone: Some(Zone::Graveyard),
            damage_modification: None,
            damage_source_filter: None,
            damage_target_filter: None,
            combat_scope: None,
            shield_kind: Default::default(),
            quantity_modification: None,
            token_owner_scope: None,
            valid_player: None,
            is_consumed: false,
            redirect_target: None,
            mana_modification: None,
            additional_token_spec: None,
            ensure_token_specs: None,
        });
    }
    Ok(out)
}

/// CR 614.6: Map a `ReplacementActionWouldPutIntoGraveyard` to the new
/// destination zone. Only the four pure-redirect shapes map cleanly;
/// other actions (counter-on-exile, conditional, may-cost) need
/// dedicated handling.
fn graveyard_action_to_destination(
    act: &ReplacementActionWouldPutIntoGraveyard,
) -> ConvResult<Zone> {
    match act {
        ReplacementActionWouldPutIntoGraveyard::ExileItInstead => Ok(Zone::Exile),
        ReplacementActionWouldPutIntoGraveyard::PutItInOwnersHandInstead => Ok(Zone::Hand),
        ReplacementActionWouldPutIntoGraveyard::PutItOnTopOfOwnersLibraryInstead
        | ReplacementActionWouldPutIntoGraveyard::PutItOnBottomOfOwnersLibraryInstead
        | ReplacementActionWouldPutIntoGraveyard::ShuffleItIntoLibraryInstead => Ok(Zone::Library),
        other => Err(ConversionGap::UnknownVariant {
            path: String::new(),
            repr: serde_json::to_value(other)
                .ok()
                .and_then(|v| {
                    v.get("_ReplacementActionWouldPutIntoGraveyard")
                        .and_then(|t| t.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| "<unknown>".to_string()),
        }),
    }
}

/// CR 614.1a: Build replacement definitions from
/// `ReplaceWouldPutCounters(event, actions)`. The engine's
/// `quantity_modification: Option<QuantityModification>` slot supports
/// the Hardened Scales (+N) and Doubling Season (×2) families. mtgish
/// encodes both via a `GameNumber` expression over
/// `WouldPutCounters_NumberOfCounters` (the original event's count):
///
/// - `Plus(WouldPutCounters_NumberOfCounters, Integer(n))` →
///   `QuantityModification::Plus { value: n }`
/// - `Twice(WouldPutCounters_NumberOfCounters)` →
///   `QuantityModification::Double`
///
/// Other quantity expressions (multipliers other than 2, references to
/// other game state) strict-fail until `QuantityModification` grows
/// additional axes.
pub fn convert_replace_would_put_counters(
    event: &ReplacableEventWouldPutCounters,
    actions: &[ReplacementActionWouldPutCounters],
) -> ConvResult<Vec<ReplacementDefinition>> {
    let valid_card = counter_event_to_valid_card(event)?;
    let mut out = Vec::new();
    for act in actions {
        let modification = counter_action_to_modification(act)?;
        out.push(ReplacementDefinition {
            expires_at_eot: false,
            event: ReplacementEvent::AddCounter,
            execute: None,
            mode: Default::default(),
            valid_card: valid_card.clone(),
            description: None,
            condition: None,
            destination_zone: None,
            damage_modification: None,
            damage_source_filter: None,
            damage_target_filter: None,
            combat_scope: None,
            shield_kind: Default::default(),
            quantity_modification: Some(modification.clone()),
            token_owner_scope: None,
            valid_player: None,
            is_consumed: false,
            redirect_target: None,
            mana_modification: None,
            additional_token_spec: None,
            ensure_token_specs: None,
        });
    }
    Ok(out)
}

/// CR 614.1a: Decompose a `ReplacableEventWouldPutCounters` event into a
/// `valid_card` filter scoping which permanents the replacement applies
/// to. Covers the dominant `CountersOfTypeWouldBePointOnAPermanent`
/// shape (16 of 27 corpus occurrences); other shapes strict-fail.
fn counter_event_to_valid_card(
    event: &ReplacableEventWouldPutCounters,
) -> ConvResult<Option<TargetFilter>> {
    use ReplacableEventWouldPutCounters as E;
    match event {
        E::CountersOfTypeWouldBePointOnAPermanent(_counter, perms) => {
            Ok(Some(convert_permanents(perms)?))
        }
        E::CountersWouldBePutOnAPermanent(perms)
        | E::AnEffectWouldPutCountersOnAPermanent(perms) => Ok(Some(convert_permanents(perms)?)),
        other => Err(ConversionGap::UnknownVariant {
            path: String::new(),
            repr: serde_json::to_value(other)
                .ok()
                .and_then(|v| {
                    v.get("_ReplacableEventWouldPutCounters")
                        .and_then(|t| t.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| "<unknown>".to_string()),
        }),
    }
}

/// CR 614.1a: Map a `ReplacementActionWouldPutCounters` to the engine's
/// `QuantityModification`. Recognises the
/// `Plus(NumberOfCounters, Integer(n))` and `Twice(NumberOfCounters)`
/// quantity expressions; other shapes strict-fail.
fn counter_action_to_modification(
    act: &ReplacementActionWouldPutCounters,
) -> ConvResult<QuantityModification> {
    match act {
        ReplacementActionWouldPutCounters::PutNewAmount(g)
        | ReplacementActionWouldPutCounters::PutNewAmountOfType(g, _) => {
            game_number_to_modification(
                g,
                |gn| matches!(gn, GameNumber::WouldPutCounters_NumberOfCounters),
                "ReplaceWouldPutCounters/quantity_shape",
            )
        }
        other => Err(ConversionGap::UnknownVariant {
            path: String::new(),
            repr: serde_json::to_value(other)
                .ok()
                .and_then(|v| {
                    v.get("_ReplacementActionWouldPutCounters")
                        .and_then(|t| t.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| "<unknown>".to_string()),
        }),
    }
}

/// CR 614.1a: Decompose the new-amount quantity expression into a
/// `QuantityModification`. Pattern-matches the two structural shapes
/// the corpus uses (`Plus` over the event self-reference + a constant,
/// and `Twice` over the event self-reference); anything else strict-fails.
///
/// `is_self_ref` identifies the event-specific self-reference
/// `GameNumber` variant (e.g., `WouldPutCounters_NumberOfCounters` for
/// counter replacements, `WouldGainLife_LifeAmount` for life-gain
/// replacements). The shape is identical across both events.
fn game_number_to_modification(
    g: &GameNumber,
    is_self_ref: impl Fn(&GameNumber) -> bool,
    idiom: &'static str,
) -> ConvResult<QuantityModification> {
    match g {
        GameNumber::Twice(inner) if is_self_ref(inner) => Ok(QuantityModification::Double),
        GameNumber::Plus(a, b) if is_self_ref(a) || is_self_ref(b) => {
            let n_node = if is_self_ref(a) { &**b } else { &**a };
            match n_node {
                GameNumber::Integer(n) => {
                    let value = u32::try_from(*n).map_err(|_| ConversionGap::MalformedIdiom {
                        idiom,
                        path: String::new(),
                        detail: format!("expected non-negative add value, got {n}"),
                    })?;
                    Ok(QuantityModification::Plus { value })
                }
                _ => Err(ConversionGap::MalformedIdiom {
                    idiom,
                    path: String::new(),
                    detail: "expected Plus(<self-ref>, Integer(n))".into(),
                }),
            }
        }
        other => Err(ConversionGap::MalformedIdiom {
            idiom,
            path: String::new(),
            detail: format!(
                "unsupported quantity: {}",
                serde_json::to_value(other)
                    .ok()
                    .and_then(|v| v
                        .get("_GameNumber")
                        .and_then(|t| t.as_str())
                        .map(String::from))
                    .unwrap_or_else(|| "<unknown>".into())
            ),
        }),
    }
}

/// CR 614.1a: Build replacement definitions from `Rule::ReplaceWouldGainLife
/// (event, actions)`. Mirrors the round-5 counter-replacement structure:
/// the runtime's `gain_life_applier` consumes `quantity_modification`
/// (extended in this round to also apply on `LifeGain` events). Action
/// coverage:
///
/// - `GainLife(Plus(LifeAmount, Integer(N)))` →
///   `QuantityModification::Plus { value: N }` (Hardened-Heart pattern).
/// - `GainLife(Twice(LifeAmount))` → `QuantityModification::Double`
///   (Boon Reflection / Rhox Faithmender).
///
/// Other actions (DrawNumberCards, GainNoLifeInstead, LoseLife,
/// PlayerAction wrappers) strict-fail.
pub fn convert_replace_would_gain_life(
    event: &ReplacableEventWouldGainLife,
    actions: &[ReplacementActionWouldGainLife],
) -> ConvResult<Vec<ReplacementDefinition>> {
    let valid_player = gain_life_event_to_valid_player(event)?;
    let mut out = Vec::new();
    for act in actions {
        let modification = gain_life_action_to_modification(act)?;
        out.push(ReplacementDefinition {
            expires_at_eot: false,
            event: ReplacementEvent::GainLife,
            execute: None,
            mode: Default::default(),
            valid_card: None,
            description: None,
            condition: None,
            destination_zone: None,
            damage_modification: None,
            damage_source_filter: None,
            damage_target_filter: None,
            combat_scope: None,
            shield_kind: Default::default(),
            quantity_modification: Some(modification.clone()),
            token_owner_scope: None,
            valid_player: valid_player.clone(),
            is_consumed: false,
            redirect_target: None,
            mana_modification: None,
            additional_token_spec: None,
            ensure_token_specs: None,
        });
    }
    Ok(out)
}

/// CR 614.1a: Map the schema event variant to a `valid_player` filter.
/// `APlayerWouldGainLife` → broad (None); `PlayerWouldGainLife(You)` →
/// `Some(You)`. Spell/ability-caused life gain strict-fails (event
/// distinction the engine matcher doesn't yet expose).
fn gain_life_event_to_valid_player(
    event: &ReplacableEventWouldGainLife,
) -> ConvResult<Option<engine::types::ability::ControllerRef>> {
    use engine::types::ability::ControllerRef;
    use ReplacableEventWouldGainLife as E;
    match event {
        E::APlayerWouldGainLife(_) => Ok(None),
        E::PlayerWouldGainLife(p) => match &**p {
            Player::You => Ok(Some(ControllerRef::You)),
            _ => Ok(None),
        },
        E::ASpellOrAbilityWouldCauseItsControllerToGainLife(_) => {
            Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: "valid_player gating on spell-or-ability source".into(),
            })
        }
    }
}

/// CR 614.1a: Map a `ReplacementActionWouldGainLife` to the engine's
/// `QuantityModification`.
fn gain_life_action_to_modification(
    act: &ReplacementActionWouldGainLife,
) -> ConvResult<QuantityModification> {
    match act {
        ReplacementActionWouldGainLife::GainLife(g) => game_number_to_modification(
            g,
            |gn| matches!(gn, GameNumber::WouldGainLife_LifeAmount),
            "ReplaceWouldGainLife/quantity_shape",
        ),
        ReplacementActionWouldGainLife::GainNoLifeInstead => {
            // "Gain no life instead" — express as a multiplicative wipe.
            // The engine's `QuantityModification::Minus { value: u32::MAX }`
            // saturates to 0, which is the correct semantic.
            Ok(QuantityModification::Minus { value: u32::MAX })
        }
        other => Err(ConversionGap::UnknownVariant {
            path: String::new(),
            repr: serde_json::to_value(other)
                .ok()
                .and_then(|v| {
                    v.get("_ReplacementActionWouldGainLife")
                        .and_then(|t| t.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| "<unknown>".to_string()),
        }),
    }
}

fn try_build_may_cost_pair(
    act: &ReplacementActionWouldEnter,
    next: Option<&ReplacementActionWouldEnter>,
    target: &TargetFilter,
) -> ConvResult<Option<ReplacementDefinition>> {
    let ReplacementActionWouldEnter::MayCost(cost) = act else {
        return Ok(None);
    };
    let Some(next) = next else {
        return Ok(None);
    };

    let (execute, decline) = match next {
        ReplacementActionWouldEnter::If(Condition::CostWasPaid, body) => {
            (Some(may_cost_body_ability(body, target)?), None)
        }
        ReplacementActionWouldEnter::Unless(Condition::CostWasPaid, body) => {
            (None, Some(Box::new(may_cost_body_ability(body, target)?)))
        }
        ReplacementActionWouldEnter::IfElse(cond, then_body, else_body)
            if matches!(&**cond, Condition::CostWasPaid) =>
        {
            (
                Some(may_cost_body_ability(then_body, target)?),
                Some(Box::new(may_cost_body_ability(else_body, target)?)),
            )
        }
        _ => return Ok(None),
    };

    Ok(Some(ReplacementDefinition {
        expires_at_eot: false,
        event: ReplacementEvent::ChangeZone,
        execute: execute.map(Box::new),
        mode: ReplacementMode::MayCost {
            cost: convert_enter_cost(cost)?,
            decline,
        },
        valid_card: Some(target.clone()),
        description: None,
        condition: None,
        destination_zone: Some(Zone::Battlefield),
        damage_modification: None,
        damage_source_filter: None,
        damage_target_filter: None,
        combat_scope: None,
        shield_kind: Default::default(),
        quantity_modification: None,
        token_owner_scope: None,
        valid_player: None,
        is_consumed: false,
        redirect_target: None,
        mana_modification: None,
        additional_token_spec: None,
        ensure_token_specs: None,
    }))
}

fn may_cost_body_ability(
    body: &[ReplacementActionWouldEnter],
    target: &TargetFilter,
) -> ConvResult<AbilityDefinition> {
    let [inner] = body else {
        return Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ReplacementDefinition",
            needed_variant: format!("ETB MayCost CostWasPaid body with {} actions", body.len()),
        });
    };
    let (condition, mode, exec) = build_replacement_exec(inner, target)?;
    if condition.is_some() || !matches!(mode, ReplacementMode::Mandatory) {
        return Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ReplacementDefinition",
            needed_variant: "ETB MayCost nested conditional/optional body".into(),
        });
    }
    Ok(exec)
}

fn convert_enter_cost(cost: &ReplacementActionWouldEnterCost) -> ConvResult<AbilityCost> {
    Ok(match cost {
        ReplacementActionWouldEnterCost::PayLife(amount) => AbilityCost::PayLife {
            amount: quantity::convert(amount)?,
        },
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "AbilityCost",
                needed_variant: format!("ETB MayCost payment ({})", enter_cost_variant_tag(other)),
            });
        }
    })
}

fn enter_cost_variant_tag(cost: &ReplacementActionWouldEnterCost) -> String {
    serde_json::to_value(cost)
        .ok()
        .and_then(|v| {
            v.get("_ReplacementActionWouldEnterCost")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// CR 614.12 + CR 614.1d: Build the `execute` body and (optional) gating
/// `ReplacementCondition` for one ETB replacement action. The condition slot
/// is populated when the action is `Unless(cond, body)` and `cond` maps to
/// a typed `ReplacementCondition`; otherwise `None` (the replacement always
/// fires when the event matches `valid_card`).
fn build_replacement_exec(
    act: &ReplacementActionWouldEnter,
    target: &TargetFilter,
) -> ConvResult<(
    Option<ReplacementCondition>,
    ReplacementMode,
    AbilityDefinition,
)> {
    use ReplacementActionWouldEnter as A;
    // CR 614.1d: `Unless(cond, body)` — the body is a vector of inner
    // `ReplacementActionWouldEnter` actions whose effect should be gated
    // by the engine's `ReplacementCondition`. We support the dominant
    // single-action body shape (`[EntersTapped]` and friends — 153/155
    // corpus occurrences) by recursing into `build_replacement_exec` on
    // the lone inner action and lifting the condition. Multi-action and
    // nested-Unless bodies strict-fail until the engine grows a
    // composition primitive.
    if let A::Unless(cond, body) = act {
        let [inner] = body.as_slice() else {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: format!(
                    "ETB conditional/gate action (Unless multi-action body, {} actions)",
                    body.len()
                ),
            });
        };
        let (inner_cond, inner_mode, exec) = build_replacement_exec(inner, target)?;
        if inner_cond.is_some() {
            // Nested Unless: the engine's `condition` slot is single-valued.
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: "ETB conditional/gate action (Unless nested in Unless)".into(),
            });
        }
        if !matches!(inner_mode, ReplacementMode::Mandatory) {
            // CR 614.1d + CR 614.12: Optional inner inside Unless would
            // require composing two distinct decision points (the gate +
            // the optional accept/decline). Engine's single ReplacementMode
            // slot can't express it.
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: "ETB conditional/gate action (Unless wrapping Optional body)"
                    .into(),
            });
        }
        let condition = convert_etb_unless_condition(cond)?;
        return Ok((Some(condition), ReplacementMode::Mandatory, exec));
    }
    // CR 614.1d + CR 614.12: `If(cond, body)` — positive-form ETB gate.
    // Symmetric to `Unless(cond, body)`: applies the body's replacement only
    // when the condition holds at replacement check time. Lowers to
    // `ReplacementCondition::OnlyIfQuantity` (the positive-form analog to
    // `UnlessQuantity` — both reuse `(lhs, comparator, rhs)`). Same
    // single-inner-action constraint as Unless: the engine's `condition`
    // slot is single-valued, multi-action / nested bodies strict-fail.
    if let A::If(cond, body) = act {
        let [inner] = body.as_slice() else {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: format!(
                    "ETB conditional/gate action (If multi-action body, {} actions)",
                    body.len()
                ),
            });
        };
        let (inner_cond, inner_mode, exec) = build_replacement_exec(inner, target)?;
        if inner_cond.is_some() {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: "ETB conditional/gate action (If nested in If/Unless)".into(),
            });
        }
        if !matches!(inner_mode, ReplacementMode::Mandatory) {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: "ETB conditional/gate action (If wrapping Optional body)".into(),
            });
        }
        let condition = convert_etb_if_condition(cond)?;
        return Ok((Some(condition), ReplacementMode::Mandatory, exec));
    }
    // CR 614.12 + CR 702.33d: mtgish's `IfPassesFilter` is the source-
    // predicate sibling of `If(cond, body)`: apply the replacement body only
    // if the entering permanent itself passes `pred`. The high-frequency
    // kicked-entry shape maps directly to the engine's existing
    // `CastViaKicker` replacement gate.
    if let A::IfPassesFilter(pred, body) = act {
        let [inner] = body.as_slice() else {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: format!(
                    "ETB conditional/gate action (IfPassesFilter multi-action body, {} actions)",
                    body.len()
                ),
            });
        };
        let (inner_cond, inner_mode, exec) = build_replacement_exec(inner, target)?;
        if inner_cond.is_some() {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: "ETB conditional/gate action (IfPassesFilter nested condition)"
                    .into(),
            });
        }
        if !matches!(inner_mode, ReplacementMode::Mandatory) {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant:
                    "ETB conditional/gate action (IfPassesFilter wrapping Optional body)".into(),
            });
        }
        let condition = convert_etb_if_passes_filter_condition(pred)?;
        return Ok((Some(condition), ReplacementMode::Mandatory, exec));
    }
    // CR 614.12 + CR 117.4: `MayActions(body)` — optional ETB action(s).
    // The body is one or more inner ETB actions; the player may choose to
    // accept (run the body) or decline (no replacement effect applied).
    // Mirrors the `parse_clone_replacement` shape: `Optional { decline: None }`.
    // Single-inner-action only — multi-action bodies would need a sub_ability
    // chain whose component conditions/modes can't all live in the single
    // ReplacementDefinition slot. Strict-fail those.
    if let A::MayActions(body) = act {
        let [inner] = body.as_slice() else {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: format!(
                    "ETB optional action (MayActions multi-action body, {} actions)",
                    body.len()
                ),
            });
        };
        let (inner_cond, inner_mode, exec) = build_replacement_exec(inner, target)?;
        if inner_cond.is_some() {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: "ETB optional action (MayActions wrapping Unless)".into(),
            });
        }
        if !matches!(inner_mode, ReplacementMode::Mandatory) {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: "ETB optional action (MayActions nested in MayActions)".into(),
            });
        }
        return Ok((None, ReplacementMode::Optional { decline: None }, exec));
    }
    let effect = match act {
        // CR 614.12 + CR 121.6: Enters tapped — direct Effect::Tap.
        A::EntersTapped => Effect::Tap {
            target: target.clone(),
        },
        // CR 614.12 + CR 122.1: Enters with a counter (default 1) /
        // enters with N counters of a typed kind.
        A::EntersWithACounter(ct) => Effect::AddCounter {
            counter_type: counter_type_name(ct),
            count: QuantityExpr::Fixed { value: 1 },
            target: target.clone(),
        },
        A::EntersWithNumberCounters(g, ct) => Effect::AddCounter {
            counter_type: counter_type_name(ct),
            count: quantity::convert(g)?,
            target: target.clone(),
        },
        // CR 614.12 + CR 110.2: "Enters under [opponent / a player]'s
        // control." `Effect::ChangeZone` carries `under_your_control`,
        // but the engine has no slot for "under SOME OTHER player's
        // control" inside the as-enters replacement frame, and the
        // replacement runtime never reads ChangeZone for an ETB
        // replacement (it reads scalar effects). Strict-fail.
        A::EntersUnderAPlayersControl(_) | A::EntersUnderPlayersControl(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: "ETB action: enters under another player's control".into(),
            });
        }
        // CR 122.1 (counter-of-choice / EntersPrepared / per-each /
        // for-each-kind / different-counters / etc.) — these need new
        // engine ETB action shapes (player picks counter type, "ready"
        // counter primitive, dynamic per-each-quantity, etc.).
        A::EntersWithACounterOfChoice(_)
        | A::EntersWithNumberDifferentCountersOfChoice(_, _)
        | A::EntersWithNumberCombinationCountersOfChoice(_, _)
        | A::EntersWithACounterOfTypeForEachKindOfCounterOnPermanent(_)
        | A::EntersWithAnAbilityCounterForEachAbilityOnACardDiscardedThisWay(_)
        | A::EntersWithNotedCounters
        | A::EntersWithNumberCountersForEach(_, _, _)
        | A::EntersPrepared => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "Effect",
                needed_variant: format!("ETB counter-action shape ({})", variant_tag(act)),
            });
        }
        // CR 614.12: Untapped-instead replacement — needs an engine
        // "force-untapped" override since `enter_tapped: false` is the
        // default (no replacement fires for the default).
        A::EntersUntapped => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: "ETB action: enters untapped (override default-tapped)".into(),
            });
        }
        // CR 614.12 + CR 614.12a: As-enters choice gates — the player
        // makes a named choice "before the permanent enters the
        // battlefield." Each wireable arm emits `Effect::Choose { ..,
        // persist: true }`, which the replacement runtime uses to set
        // `WaitingFor::NamedChoice` and persist the selection on the
        // entering object's `chosen_attributes` (so downstream "the
        // chosen color/type/..." references can read it). This mirrors
        // the native parser shape in `oracle_replacement.rs`
        // (`parse_as_enters_choose`).
        A::ChooseACreatureType => Effect::Choose {
            choice_type: ChoiceType::CreatureType,
            persist: true,
        },
        A::ChooseAColor(_) => Effect::Choose {
            choice_type: ChoiceType::Color,
            persist: true,
        },
        A::ChooseACardName(_) => Effect::Choose {
            choice_type: ChoiceType::CardName,
            persist: true,
        },
        A::ChooseACardtype => Effect::Choose {
            choice_type: ChoiceType::CardType,
            persist: true,
        },
        A::ChooseABasicLandType => Effect::Choose {
            choice_type: ChoiceType::BasicLandType,
            persist: true,
        },
        // CR 305.7: "land type" includes basic + nonbasic. Both
        // unparameterized (ChooseALandType) and parameterized
        // (ChooseLandType(opts)) collapse to the engine's bounded
        // LandType choice — the engine resolves the option set at
        // runtime.
        A::ChooseALandType | A::ChooseLandType(_) => Effect::Choose {
            choice_type: ChoiceType::LandType,
            persist: true,
        },
        // CR 800.4a: opponent-scoped player choice when the schema
        // filter narrows to opponents; broader player choice
        // otherwise. Re-uses the existing `players_to_controller`
        // bridge for opponent detection.
        A::ChooseAPlayer(players) => {
            let choice_type = match crate::convert::filter::players_to_controller(players.as_ref())
            {
                Ok(ControllerRef::Opponent) => ChoiceType::Opponent,
                _ => ChoiceType::Player,
            };
            Effect::Choose {
                choice_type,
                persist: true,
            }
        }
        // CR 614.12a: "Choose a number between X and Y" — engine's
        // `NumberRange` carries u8 bounds. Strict-fail if the schema
        // values are out of range or inverted (defensive — the engine
        // would generate a degenerate option list).
        A::ChooseANumberBetween(min, max) => {
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
        A::ChooseEvenOrOdd => Effect::Choose {
            choice_type: ChoiceType::OddOrEven,
            persist: true,
        },
        A::ChooseTwoColors => Effect::Choose {
            choice_type: ChoiceType::TwoColors,
            persist: true,
        },
        // CR 614.12a + CR 701.x voting: enumerated option lists become
        // `ChoiceType::Labeled`. Each variant supplies its own option
        // source.
        A::ChooseADirection => Effect::Choose {
            choice_type: ChoiceType::Labeled {
                options: vec!["Left".to_string(), "Right".to_string()],
            },
            persist: true,
        },
        A::ChooseACreatureTypeFromList(opts) => {
            if opts.is_empty() {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "ChoiceType::Labeled",
                    needed_variant: "ChooseACreatureTypeFromList with empty option list".into(),
                });
            }
            Effect::Choose {
                choice_type: ChoiceType::Labeled {
                    options: opts.iter().map(|c| format!("{c:?}")).collect(),
                },
                persist: true,
            }
        }
        A::ChooseACardtypeFromList(opts) => {
            if opts.is_empty() {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "ChoiceType::Labeled",
                    needed_variant: "ChooseACardtypeFromList with empty option list".into(),
                });
            }
            Effect::Choose {
                choice_type: ChoiceType::Labeled {
                    options: opts.iter().map(|c| format!("{c:?}")).collect(),
                },
                persist: true,
            }
        }
        A::ChooseWord(opts) => {
            if opts.is_empty() {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "ChoiceType::Labeled",
                    needed_variant: "ChooseWord with empty option list".into(),
                });
            }
            Effect::Choose {
                choice_type: ChoiceType::Labeled {
                    options: opts.clone(),
                },
                persist: true,
            }
        }
        // CR 614.12a strict-fails — each gets its own refined tag so
        // the report attributes the missing engine prerequisite to the
        // exact schema variant.
        // CR 614.12a: pair-choice carries explicit allowed (color,
        // creature-type) pairs from the schema. Splitting into two
        // independent `Choose` steps via `sub_ability` would allow
        // disallowed combinations, so strict-fail until the engine
        // grows a paired/labelled-tuple choice slot.
        A::ChooseAColorAndCreatureType(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "paired (color, creature-type) choice (CR 614.12a)".into(),
            });
        }
        // CR 205.2: "every card type except those in the list" — the
        // complement is well-defined against the bounded card-type
        // set, but the engine has no slot to express the
        // "except this list" reduction without leaking the full set
        // here (which would drift as new types are added). Strict-fail.
        A::ChooseACardtypeExceptFromList(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "card-type choice with except-list complement (CR 205.2)".into(),
            });
        }
        // CR 205.3j: planeswalker types are an authoritative bounded
        // list, but the engine does not currently expose that list as
        // a `ChoiceType` variant or a constant the converter can read.
        // Strict-fail until the engine adds a `PlaneswalkerType`
        // choice variant rather than enumerating the ~100-name list
        // here (which would need to track CR errata).
        A::ChooseAPlaneswalkerType => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "planeswalker-type choice (CR 205.3j)".into(),
            });
        }
        // CR 614.12a / CR 701.x — remaining choice gates without a
        // 1:1 engine `ChoiceType` mapping. Each strict-fails with its
        // own refined tag.
        A::ChooseACardtypeSharedAmongExiledCards(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "card-type-shared-among-exiled choice".into(),
            });
        }
        A::ChooseAPermanent(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "permanent choice (target-style, not named-choice)".into(),
            });
        }
        A::ChooseANumberFromAmongAtRandom(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "number-from-list-at-random choice".into(),
            });
        }
        A::ChooseANumberGreaterThanNumber(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "unbounded-number-greater-than choice".into(),
            });
        }
        A::ChooseTwoBasicLandTypes => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "two-basic-land-types choice".into(),
            });
        }
        A::ChooseTwoPlayers(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "two-players choice".into(),
            });
        }
        A::ChooseUptoNumberColors(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "up-to-N-colors choice".into(),
            });
        }
        A::ChooseNumberAbilities(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "N-from-ability-list choice".into(),
            });
        }
        A::SecretlyChooseANumberBetween(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "secret number-range choice".into(),
            });
        }
        A::SecretlyChooseAPlayer(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ChoiceType",
                needed_variant: "secret player choice".into(),
            });
        }
        // CR 118.5 / CR 614.12: Optional / mandatory cost gates on ETB
        // ("As ~ enters, you may pay {2}", "As ~ enters, sacrifice a
        // creature"). Need an engine ETB-side cost-gate primitive.
        A::MayCost(_) | A::MustCost(_) | A::PayAnyAmountOfLife | A::PayAnyAmountOfLifeUpto(_) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: format!("ETB cost gate ({})", variant_tag(act)),
            });
        }
        // CR 614.12: Conditional ETB shapes — If/IfElse/Unless/
        // IfPassesFilter/MayActions/FlipACoin gating around ETB
        // replacement actions. Engine needs a condition slot inside
        // the ETB replacement frame.
        A::IfElse(_, _, _)
        | A::IfElsePassesFilter(_, _, _)
        | A::IfPassesFilter(_, _)
        | A::IfCardPassesFilter(_, _)
        | A::FlipACoin_OnHeadAndOnTails(_, _)
        | A::APlayerAction(_, _)
        | A::EachPlayerAction(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: format!("ETB conditional/gate action ({})", variant_tag(act)),
            });
        }
        // `Unless` and `MayActions` are handled by the early-return guards above.
        A::Unless(_, _) => unreachable!("Unless handled by early-return guard"),
        A::MayActions(_) => unreachable!("MayActions handled by early-return guard"),
        // CR 707.x: "Enters as a copy" / face-down / transformed /
        // attached / attacking / blocking — bespoke ETB modifier shapes
        // that need dedicated engine primitives.
        A::EnterAsACopyOfACardInAPlayersGraveyard(_, _, _)
        | A::EnterAsACopyOfACardInExile(_, _)
        | A::EnterAsACopyOfAPermanent(_, _)
        | A::EnterAsACopyOfAPermanentUntil(_, _, _)
        | A::EnterAsACopyOfPermanent(_, _)
        | A::EnterAsCopyOfExiled(_, _)
        | A::EntersAsFaceDownArtifactCreature(_, _)
        | A::EntersAsFaceDownCreatureWithAbilitiesAndNotedName(_, _, _)
        | A::EntersAsFaceDownLand(_)
        | A::EntersAsNonAuraEnchantment
        | A::EntersAttachedToAPermanent(_)
        | A::EntersAttachedToPermanent(_)
        | A::EntersAttachedToPlayer(_)
        | A::EntersAttacking
        | A::EntersAttackingPlayer(_)
        | A::EntersAttackingPlayerOrPlaneswalkerControlledBy(_)
        | A::EntersBlockingAttacker(_)
        | A::EntersConverted
        | A::EntersFaceDown
        | A::EntersFlipped
        | A::EntersTransformed
        | A::EntersWithLayerEffect(_)
        | A::EntersWithLayerEffectOfChoice(_)
        | A::EntersWithLayerEffectUntil(_, _)
        | A::EntersWithPerpetualEffect(_)
        | A::EntersUnderOwnersControl
        | A::EntersNormally => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: format!("ETB modifier shape ({})", variant_tag(act)),
            });
        }
        // CR 614.12: "Instead" actions that aren't ETB at all —
        // exile-instead / shuffle-into-library-instead / put-into-
        // graveyard-instead operate on the entering object but redirect
        // it to a different zone. Need an ETB→zone-redirect primitive.
        A::ExileItInstead | A::ShuffleItIntoLibraryInstead | A::PutIntoGraveyardInstead => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: format!("ETB redirect-to-zone ({})", variant_tag(act)),
            });
        }
        // Catch-all for residual variants — strict-fail with engine
        // prerequisite (genuinely missing) rather than UnknownVariant
        // (mis-identified as a parser issue).
        _ => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: format!("ETB action ({})", variant_tag(act)),
            });
        }
    };
    Ok((
        None,
        ReplacementMode::Mandatory,
        AbilityDefinition::new(AbilityKind::Spell, effect),
    ))
}

/// CR 614.1d: Map an mtgish `Condition` (the "unless X" gate of an
/// `Unless(cond, body)` ETB action) to the engine's `ReplacementCondition`.
///
/// This is the replacement-context bridge — separate from the ability /
/// trigger / static condition bridges in `convert/condition.rs` because
/// the engine models replacement gating as a closed catalog of typed
/// shapes (`UnlessControlsMatching`, `UnlessControlsCountMatching`,
/// `UnlessPlayerLifeAtMost`, `UnlessQuantity`, `CastViaKicker`, etc.)
/// rather than a generic `AbilityCondition`. Per the project hard rule,
/// unrecognized shapes strict-fail rather than dropping the gate to
/// `None` (which would convert "enters tapped unless X" into "always
/// enters tapped").
///
/// Coverage today: `PlayerPassesFilter(You, ControlsA(<perms>))` — the
/// dominant shape (89/155 corpus occurrences), covering check lands,
/// basic-land lookup, and "unless you control a [type]" patterns. Other
/// shapes (`CostWasPaid` shock-land family, `APlayerPassesFilter`
/// life-total bond lands, `NumPermanentsIs`, `IsPlayersNthTurn`) need
/// either dedicated `ReplacementCondition` arms or a shared
/// `Condition → ReplacementCondition` bridge that lifts the existing
/// quantity-comparison work in `convert::condition`. They strict-fail.
fn convert_etb_unless_condition(cond: &Condition) -> ConvResult<ReplacementCondition> {
    match cond {
        Condition::PlayerPassesFilter(player, predicate) => {
            // CR 614.1d: only the controller-as-You shape is meaningful for
            // ETB replacements ("unless you control a [type]"); other player
            // refs need separate engine slots.
            if !matches!(&**player, Player::You) {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "ReplacementCondition",
                    needed_variant: format!(
                        "ETB Unless condition (PlayerPassesFilter non-You: {:?})",
                        player
                    ),
                });
            }
            convert_etb_unless_you_predicate(predicate)
        }
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ReplacementCondition",
            needed_variant: format!(
                "ETB Unless condition ({})",
                serde_json::to_value(other)
                    .ok()
                    .and_then(|v| v
                        .get("_Condition")
                        .and_then(|t| t.as_str())
                        .map(String::from))
                    .unwrap_or_else(|| "<unknown>".into())
            ),
        }),
    }
}

/// CR 614.1d: Convert the `Players` predicate of `PlayerPassesFilter(You, ...)`
/// into a `ReplacementCondition`. Split out from `convert_etb_unless_condition`
/// so the dispatch table for the predicate axis (ControlsA / ControlsNum / Or)
/// stays composable. Each predicate shape maps to a typed slot:
///
/// - `ControlsA(perms)` → `UnlessControlsMatching` (existing class).
/// - `ControlsNum(GreaterThanOrEqualTo(N), perms)` →
///   `UnlessControlsCountMatching { minimum: N, filter }`.
/// - `ControlsNum(LessThanOrEqualTo(N), perms)` → `UnlessControlsOtherLeq`,
///   but only when `convert_permanents` produces a `TargetFilter::Typed(tf)`
///   leaf (the engine's `UnlessControlsOtherLeq.filter` is `TypedFilter`,
///   not the wrapped sum type — compound `Or`/`And` filters strict-fail).
///   `FilterProp::Another` is stamped on so the entering permanent itself
///   isn't counted (per the variant doc).
/// - `Or(subs)` → recursive map of each sub-`Players` to a `TargetFilter`,
///   combined via `TargetFilter::Or`, emitted as `UnlessControlsMatching`.
///   Today only `ControlsA` is supported inside `Or`; future rounds widen.
///
/// Other comparator shapes (GreaterThan, LessThan, EqualTo, NotEqualTo, etc.)
/// strict-fail with a refined `needed_variant` naming the specific shape.
fn convert_etb_unless_you_predicate(predicate: &Players) -> ConvResult<ReplacementCondition> {
    use crate::schema::types::Comparison;
    match predicate {
        Players::ControlsA(perms) => {
            let filter = bind_filter_controller_you(convert_permanents(perms)?);
            Ok(ReplacementCondition::UnlessControlsMatching { filter })
        }
        // CR 614.1d: "unless you control N or more [type]" / "unless you
        // control N or fewer other [type]" — quantity-gated forms.
        Players::ControlsNum(comparison, perms) => match &**comparison {
            Comparison::GreaterThanOrEqualTo(n) => {
                let minimum = fixed_u32_or_engine_gap(
                    n,
                    "UnlessControlsCountMatching.minimum (non-fixed / negative)",
                )?;
                let filter = bind_filter_controller_you(convert_permanents(perms)?);
                Ok(ReplacementCondition::UnlessControlsCountMatching { minimum, filter })
            }
            Comparison::LessThanOrEqualTo(n) => {
                let count = fixed_u32_or_engine_gap(
                    n,
                    "UnlessControlsOtherLeq.count (non-fixed / negative)",
                )?;
                // CR 614.1d: `UnlessControlsOtherLeq.filter` is `TypedFilter`,
                // not `TargetFilter`, so any compound `Or`/`And`/`Not` shape
                // can't fit. Strict-fail with a refined tag instead of
                // silently flattening.
                let target_filter = bind_filter_controller_you(convert_permanents(perms)?);
                let mut tf = match target_filter {
                    TargetFilter::Typed(tf) => tf,
                    other => {
                        return Err(ConversionGap::EnginePrerequisiteMissing {
                            engine_type: "ReplacementCondition::UnlessControlsOtherLeq",
                            needed_variant: format!(
                                "filter: TypedFilter (got compound TargetFilter shape: {})",
                                target_filter_tag(&other)
                            ),
                        });
                    }
                };
                // Per the variant doc: `FilterProp::Another` ensures the
                // entering permanent itself isn't counted. The native parser
                // pre-stamps it; mirror that here.
                use engine::types::ability::FilterProp;
                if !tf
                    .properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::Another))
                {
                    tf.properties.push(FilterProp::Another);
                }
                Ok(ReplacementCondition::UnlessControlsOtherLeq { count, filter: tf })
            }
            other => Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementCondition",
                needed_variant: format!(
                    "ETB Unless condition (ControlsNum comparator: {})",
                    comparison_tag(other)
                ),
            }),
        },
        // CR 614.1d: "unless you control a [type-A] or a [type-B]" — disjunction
        // over multiple ControlsA predicates. Combine into one TargetFilter::Or
        // and emit a single UnlessControlsMatching. Only `ControlsA` is
        // currently supported as a sub-arm; mixing in `ControlsNum` would need
        // boolean composition over heterogeneous count + filter arms.
        Players::Or(subs) => {
            let mut filters = Vec::with_capacity(subs.len());
            for sub in subs {
                match sub {
                    Players::ControlsA(perms) => {
                        filters.push(bind_filter_controller_you(convert_permanents(perms)?));
                    }
                    other => {
                        return Err(ConversionGap::EnginePrerequisiteMissing {
                            engine_type: "ReplacementCondition",
                            needed_variant: format!(
                                "ETB Unless condition (Or sub-arm not ControlsA: {})",
                                players_tag(other)
                            ),
                        });
                    }
                }
            }
            Ok(ReplacementCondition::UnlessControlsMatching {
                filter: TargetFilter::Or { filters },
            })
        }
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ReplacementCondition",
            needed_variant: format!(
                "ETB Unless condition (PlayerPassesFilter predicate: {})",
                players_tag(other)
            ),
        }),
    }
}

/// CR 614.1d: Map an mtgish `Condition` (the positive `If(cond, body)` ETB
/// action) to the engine's `ReplacementCondition`. Symmetric to
/// `convert_etb_unless_condition` but emits the positive-form `OnlyIfQuantity`
/// gate instead of the dedicated `UnlessControlsMatching` family — both
/// share the same `(lhs, comparator, rhs)` quantity-comparison shape.
///
/// Today covers the same dominant predicate axis as the Unless side:
/// `PlayerPassesFilter(You, ControlsA(<perms>))` and the count / Or
/// variants. Lowering uses `QuantityRef::ObjectCount { filter }` for `lhs`
/// and `QuantityExpr::Fixed` for `rhs`. `FilterProp::Another` is stamped
/// onto the filter so the entering permanent itself isn't counted —
/// mirroring the `UnlessControlsOtherLeq` pattern and aligning with the
/// CR 614.13 "would-enter" perspective (the source isn't yet on the
/// battlefield, but stamping `Another` is defensive against any
/// tracker that places it pre-replacement-evaluation).
fn convert_etb_if_condition(cond: &Condition) -> ConvResult<ReplacementCondition> {
    match cond {
        Condition::PlayerPassesFilter(player, predicate) => {
            if !matches!(&**player, Player::You) {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "ReplacementCondition",
                    needed_variant: format!(
                        "ETB If condition (PlayerPassesFilter non-You: {:?})",
                        player
                    ),
                });
            }
            convert_etb_if_you_predicate(predicate)
        }
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ReplacementCondition",
            needed_variant: format!(
                "ETB If condition ({})",
                serde_json::to_value(other)
                    .ok()
                    .and_then(|v| v
                        .get("_Condition")
                        .and_then(|t| t.as_str())
                        .map(String::from))
                    .unwrap_or_else(|| "<unknown>".into())
            ),
        }),
    }
}

/// CR 614.12 + CR 702.33d: Convert an ETB source predicate from
/// `ReplacementActionWouldEnter::IfPassesFilter` into an engine replacement
/// gate. This is intentionally narrower than `filter::convert`: a filter that
/// can target objects generally is not automatically valid as a replacement
/// applicability condition on the entering source.
fn convert_etb_if_passes_filter_condition(pred: &Permanents) -> ConvResult<ReplacementCondition> {
    Ok(match pred {
        Permanents::WasKicked => ReplacementCondition::CastViaKicker {
            variant: None,
            kicker_cost: None,
        },
        Permanents::WasKickedWithKicker(cost) => ReplacementCondition::CastViaKicker {
            variant: None,
            kicker_cost: Some(mana::convert(cost)?),
        },
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementCondition",
                needed_variant: format!(
                    "ETB IfPassesFilter source predicate ({})",
                    permanents_tag(other)
                ),
            });
        }
    })
}

/// CR 614.1d: Convert the `Players` predicate of `PlayerPassesFilter(You, ...)`
/// in positive `If` context into a `ReplacementCondition::OnlyIfQuantity`
/// over `ObjectCount { filter }`. The four predicate shapes collapse to one
/// unified gate (vs the four parallel `UnlessControls*` arms on the
/// negative side) because `OnlyIfQuantity` already parameterizes
/// comparator+rhs.
///
/// - `ControlsA(perms)` → `ObjectCount{filter} GE 1`
/// - `ControlsNum(GE(N), perms)` → `ObjectCount{filter} GE N`
/// - `ControlsNum(LE(N), perms)` → `ObjectCount{filter} LE N` (with `Another` stamped)
/// - `Or(subs)` → `ObjectCount{filter: Or{subs}} GE 1`
///
/// Other comparator shapes strict-fail with a refined `needed_variant`.
fn convert_etb_if_you_predicate(predicate: &Players) -> ConvResult<ReplacementCondition> {
    use crate::schema::types::Comparison;
    use engine::types::ability::{Comparator, FilterProp, QuantityRef};

    let make_gate = |filter: TargetFilter, comparator: Comparator, rhs: u32| {
        ReplacementCondition::OnlyIfQuantity {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            },
            comparator,
            rhs: QuantityExpr::Fixed { value: rhs as i32 },
            active_player_req: None,
        }
    };

    let stamp_another = |mut tf: TargetFilter| {
        if let TargetFilter::Typed(ref mut t) = tf {
            if !t
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::Another))
            {
                t.properties.push(FilterProp::Another);
            }
        }
        tf
    };

    match predicate {
        Players::ControlsA(perms) => {
            let filter = stamp_another(bind_filter_controller_you(convert_permanents(perms)?));
            Ok(make_gate(filter, Comparator::GE, 1))
        }
        Players::ControlsNum(comparison, perms) => match &**comparison {
            Comparison::GreaterThanOrEqualTo(n) => {
                let minimum = fixed_u32_or_engine_gap(
                    n,
                    "OnlyIfQuantity.rhs (non-fixed / negative for ControlsNum GE)",
                )?;
                let filter = stamp_another(bind_filter_controller_you(convert_permanents(perms)?));
                Ok(make_gate(filter, Comparator::GE, minimum))
            }
            Comparison::LessThanOrEqualTo(n) => {
                let count = fixed_u32_or_engine_gap(
                    n,
                    "OnlyIfQuantity.rhs (non-fixed / negative for ControlsNum LE)",
                )?;
                let filter = stamp_another(bind_filter_controller_you(convert_permanents(perms)?));
                Ok(make_gate(filter, Comparator::LE, count))
            }
            other => Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementCondition",
                needed_variant: format!(
                    "ETB If condition (ControlsNum comparator: {})",
                    comparison_tag(other)
                ),
            }),
        },
        Players::Or(subs) => {
            let mut filters = Vec::with_capacity(subs.len());
            for sub in subs {
                match sub {
                    Players::ControlsA(perms) => {
                        filters.push(bind_filter_controller_you(convert_permanents(perms)?));
                    }
                    other => {
                        return Err(ConversionGap::EnginePrerequisiteMissing {
                            engine_type: "ReplacementCondition",
                            needed_variant: format!(
                                "ETB If condition (Or sub-arm not ControlsA: {})",
                                players_tag(other)
                            ),
                        });
                    }
                }
            }
            let or_filter = stamp_another(TargetFilter::Or { filters });
            Ok(make_gate(or_filter, Comparator::GE, 1))
        }
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ReplacementCondition",
            needed_variant: format!(
                "ETB If condition (PlayerPassesFilter predicate: {})",
                players_tag(other)
            ),
        }),
    }
}

/// CR 614.1d: Coerce a `GameNumber` into a `u32` for `ReplacementCondition`
/// quantity slots that are typed `u32` (not `QuantityExpr`). Mirrors the
/// `fixed_count_or_engine_gap` helper in `cost.rs`. Strict-fails on dynamic
/// or negative values.
fn fixed_u32_or_engine_gap(n: &GameNumber, needed_variant: &str) -> ConvResult<u32> {
    match quantity::convert(n)? {
        QuantityExpr::Fixed { value } if value >= 0 => Ok(value as u32),
        _ => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ReplacementCondition",
            needed_variant: needed_variant.to_string(),
        }),
    }
}

fn comparison_tag(c: &crate::schema::types::Comparison) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| {
            v.get("_Comparison")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// Diagnostic-only short-tag for a `TargetFilter` discriminant. Used
/// exclusively to format the `needed_variant` string when an
/// `UnlessControlsOtherLeq` shape can't be expressed because the engine
/// requires a single `TypedFilter` rather than an arbitrary `TargetFilter`.
/// Format derived from `Debug`; the variant header only (no payload), so
/// error messages stay scannable in the gap report.
fn target_filter_tag(f: &TargetFilter) -> String {
    let dbg = format!("{f:?}");
    dbg.split(['(', ' ', '{'])
        .next()
        .unwrap_or(&dbg)
        .to_string()
}

/// CR 614.1d: Stamp `ControllerRef::You` onto every `Typed` leaf of the
/// converted filter, mirroring the post-processing the native parser
/// performs in `parse_controls_typed_condition` (`oracle_replacement.rs`)
/// before constructing `ReplacementCondition::UnlessControlsMatching`.
/// The runtime matcher does not separately enforce a controller equality
/// check; the filter must encode it.
///
/// Local copy of `convert::condition::bind_filter_controller_you` (private
/// in that module) — keeping the helper inline preserves multi-agent file
/// boundaries. Both copies share the same recursive shape over `Or`/`And`/
/// `Not`/`Typed`; if the canonical helper is ever made `pub(crate)`, this
/// duplicate should be removed.
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

fn players_tag(p: &Players) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.get("_Players").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn permanents_tag(p: &Permanents) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| {
            v.get("_Permanents")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn counter_type_name(ct: &CounterType) -> String {
    if let CounterType::PTCounter(p, t) = ct {
        return format!("{p:+}/{t:+}");
    }
    format!("{ct:?}")
        .strip_suffix("Counter")
        .map(str::to_string)
        .unwrap_or_else(|| format!("{ct:?}"))
}

fn variant_tag(a: &ReplacementActionWouldEnter) -> String {
    serde_json::to_value(a)
        .ok()
        .and_then(|v| {
            v.get("_ReplacementActionWouldEnter")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// CR 615.1 + CR 514.2: Build an `Effect::PreventDamage` from
/// `Action::CreateReplaceWouldDealDamageUntil(event, actions, expiration)`.
///
/// The engine's `prevent_damage::resolve` is the single authority for
/// "until end of turn, prevent damage" replacements: it constructs a
/// `ReplacementDefinition` with `ShieldKind::Prevention` and places it
/// either on the bound target object (when the outer `Action::Targeted`
/// supplies one), on the source permanent (for activated abilities), or
/// in `state.pending_damage_prevention` (for instants/sorceries with no
/// target). Either way the shield is cleaned up at end of turn per
/// CR 514.2 + CR 615.
///
/// Action coverage: `PreventThatDamage` / `CancelThatDamage` (→
/// `PreventionAmount::All`) and `PreventSomeOfThatDamage(Integer(N))` (→
/// `PreventionAmount::Next(N)`). All other actions strict-fail since
/// `Effect::PreventDamage` only encodes prevention; redirection,
/// doubling, and gain-life riders need different effect shapes.
///
/// Expiration coverage: `UntilEndOfTurn` only. The shield mechanism
/// inherently expires at cleanup; non-EOT expirations would require a
/// duration-bounded replacement primitive that does not exist today.
pub fn convert_create_replace_would_deal_damage_until(
    event: &ReplacableEventWouldDealDamage,
    actions: &[ReplacementActionWouldDealDamage],
    expiration: &Expiration,
) -> ConvResult<engine::types::ability::Effect> {
    if !matches!(expiration, Expiration::UntilEndOfTurn) {
        return Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "Effect",
            needed_variant: format!(
                "PreventDamage with non-EOT expiration ({})",
                expiration_tag(expiration)
            ),
        });
    }
    let amount = single_prevent_amount(actions)?;
    let (scope, source_filter) = damage_event_to_prevent_scope(event)?;
    Ok(engine::types::ability::Effect::PreventDamage {
        amount,
        target: engine::types::ability::TargetFilter::Any,
        scope,
        damage_source_filter: source_filter,
    })
}

/// CR 615.1 + CR 514.2: Build an `Effect::PreventDamage` from
/// `Action::CreateFutureReplaceWouldDealDamage(event, actions)`.
///
/// "Future" damage replacements scope to the *next* damage event of a
/// described shape (Bandage: "the next 1 damage that would be dealt to
/// any target this turn"; Awe Strike: "the next time target creature
/// would deal damage this turn"). They map onto the same prevention-
/// shield primitive — the shield's `PreventionAmount::Next(N)` absorbs N
/// damage and is then consumed; cleanup at end of turn handles unfired
/// shields.
///
/// Event coverage: variants carrying an explicit `Integer(N)` amount
/// produce `Next(N)`. "NextTime..." variants without an amount produce
/// `Next(u32::MAX)` (saturating-absorption — one damage event of any
/// magnitude is fully prevented and the shield is then consumed).
///
/// Action coverage: only `PreventThatDamage` / `CancelThatDamage`. Other
/// actions (DealToTargetInstead, GainLife riders, etc.) need richer
/// replacement primitives and strict-fail.
pub fn convert_create_future_replace_would_deal_damage(
    event: &FutureReplacableEventWouldDealDamage,
    actions: &[ReplacementActionWouldDealDamage],
) -> ConvResult<engine::types::ability::Effect> {
    // Only the prevention-action shape is supported; reject other actions
    // before computing the event-side amount/scope.
    require_prevention_only(actions)?;
    let (amount, scope, source_filter) = future_damage_event_to_prevent_params(event)?;
    Ok(engine::types::ability::Effect::PreventDamage {
        amount,
        target: engine::types::ability::TargetFilter::Any,
        scope,
        damage_source_filter: source_filter,
    })
}

/// CR 615.1: Recognise the prevention-only single-action shape and map
/// to `PreventionAmount`. `PreventThatDamage` / `CancelThatDamage`
/// (block-all) → `All`; `PreventSomeOfThatDamage(Integer(N))` →
/// `Next(N)`. Any other action shape (or multi-action list) strict-fails.
fn single_prevent_amount(
    actions: &[ReplacementActionWouldDealDamage],
) -> ConvResult<engine::types::ability::PreventionAmount> {
    use engine::types::ability::PreventionAmount;
    let [act] = actions else {
        return Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "Effect",
            needed_variant: format!(
                "PreventDamage multi-action list ({} actions)",
                actions.len()
            ),
        });
    };
    match act {
        ReplacementActionWouldDealDamage::PreventThatDamage
        | ReplacementActionWouldDealDamage::CancelThatDamage => Ok(PreventionAmount::All),
        ReplacementActionWouldDealDamage::PreventSomeOfThatDamage(g) => match &**g {
            GameNumber::Integer(n) => {
                let value = u32::try_from(*n).map_err(|_| ConversionGap::MalformedIdiom {
                    idiom: "PreventDamage/PreventSomeOfThatDamage",
                    path: String::new(),
                    detail: format!("expected non-negative amount, got {n}"),
                })?;
                Ok(PreventionAmount::Next(value))
            }
            _ => Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "PreventionAmount",
                needed_variant: "Next { count: QuantityExpr } (dynamic prevention amount)".into(),
            }),
        },
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "Effect",
            needed_variant: format!(
                "PreventDamage non-prevention action ({})",
                damage_action_tag(other)
            ),
        }),
    }
}

/// CR 615.1: Reject any action list that isn't a single prevention
/// action. Used by the future-event path which packs the amount into
/// the event side rather than the action side.
fn require_prevention_only(actions: &[ReplacementActionWouldDealDamage]) -> ConvResult<()> {
    let [act] = actions else {
        return Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "Effect",
            needed_variant: format!(
                "PreventDamage multi-action list ({} actions)",
                actions.len()
            ),
        });
    };
    match act {
        ReplacementActionWouldDealDamage::PreventThatDamage
        | ReplacementActionWouldDealDamage::CancelThatDamage => Ok(()),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "Effect",
            needed_variant: format!(
                "PreventDamage non-prevention action ({})",
                damage_action_tag(other)
            ),
        }),
    }
}

/// CR 615 + CR 614.1a: Decompose a `ReplacableEventWouldDealDamage` into
/// the `(scope, damage_source_filter)` tuple expected by
/// `Effect::PreventDamage`. Combat-prefixed variants set
/// `PreventionScope::CombatDamage`; others remain `AllDamage`. When the
/// event names a typed source (`...ByACreature(perms)` /
/// `...ByAPermanent(perms)`), convert via `convert_permanents` and use
/// it as the source filter; otherwise leave the source slot `None`.
fn damage_event_to_prevent_scope(
    event: &ReplacableEventWouldDealDamage,
) -> ConvResult<(
    engine::types::ability::PreventionScope,
    Option<engine::types::ability::TargetFilter>,
)> {
    use engine::types::ability::PreventionScope;
    use ReplacableEventWouldDealDamage as E;
    Ok(match event {
        E::CombatDamageWouldBeDealt
        | E::CombatDamageWouldBeDealtToARecipient(_)
        | E::CombatDamageWouldBeDealtToRecipient(_) => (PreventionScope::CombatDamage, None),
        E::CombatDamageWouldBeDealtByACreature(perms)
        | E::CombatDamageWouldBeDealtByACreatureToARecipient(perms, _)
        | E::CombatDamageWouldBeDealtByACreatureToASetOfRecipients(perms, _)
        | E::CombatDamageWouldBeDealtByACreatureToRecipient(perms, _) => (
            PreventionScope::CombatDamage,
            Some(convert_permanents(perms)?),
        ),
        E::CombatDamageWouldBeDealtByCreature(perm)
        | E::CombatDamageWouldBeDealtByCreatureToARecipient(perm, _)
        | E::CombatDamageWouldBeDealtByCreatureToRecipient(perm, _) => (
            PreventionScope::CombatDamage,
            Some(convert_permanent(perm)?),
        ),
        E::DamageWouldBeDealtByAPermanent(perms)
        | E::DamageWouldBeDealtByAPermanentToARecipient(perms, _)
        | E::DamageWouldBeDealtByAPermanentToRecipient(perms, _) => {
            (PreventionScope::AllDamage, Some(convert_permanents(perms)?))
        }
        // CR 614.x: `Or` over a list of inner events — the engine has no
        // OR slot on `Effect::PreventDamage`. Strict-fail (rather than
        // expanding into multiple effects, which only the action-list
        // builder can do).
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "Effect",
                needed_variant: format!(
                    "PreventDamage event variant ({})",
                    damage_event_tag(other)
                ),
            });
        }
    })
}

/// CR 615 + CR 614.1a: Decompose a `FutureReplacableEventWouldDealDamage`
/// into `(amount, scope, damage_source_filter)`. `NextAmount...(N, ...)`
/// variants pack the damage cap into the event; `NextTime...` variants
/// have no cap and use `Next(u32::MAX)` (saturating-absorb-once).
fn future_damage_event_to_prevent_params(
    event: &FutureReplacableEventWouldDealDamage,
) -> ConvResult<(
    engine::types::ability::PreventionAmount,
    engine::types::ability::PreventionScope,
    Option<engine::types::ability::TargetFilter>,
)> {
    use engine::types::ability::{PreventionAmount, PreventionScope};
    use FutureReplacableEventWouldDealDamage as E;
    let amount_from = |g: &GameNumber| -> ConvResult<PreventionAmount> {
        match g {
            GameNumber::Integer(n) => {
                let value = u32::try_from(*n).map_err(|_| ConversionGap::MalformedIdiom {
                    idiom: "FuturePreventDamage/Integer",
                    path: String::new(),
                    detail: format!("expected non-negative amount, got {n}"),
                })?;
                Ok(PreventionAmount::Next(value))
            }
            _ => Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "PreventionAmount",
                needed_variant: "Next { count: QuantityExpr } (dynamic future-damage amount)"
                    .into(),
            }),
        }
    };
    Ok(match event {
        // Amount packed into the event — read it.
        E::NextAmountOfDamageThatWouldBeDealtThisTurnByPermanent(g, perm) => (
            amount_from(g)?,
            PreventionScope::AllDamage,
            Some(convert_permanent(perm)?),
        ),
        E::NextAmountOfDamageThatWouldBeDealtThisTurnToARecipient(g, _)
        | E::NextAmountOfDamageThatWouldBeDealtThisTurnToEachRecipient(g, _)
        | E::NextAmountOfDamageThatWouldBeDealtThisTurnToRecipient(g, _) => {
            (amount_from(g)?, PreventionScope::AllDamage, None)
        }
        E::NextAmountOfDamageThatWouldBeDealtThisTurnBySourceToARecipient(g, _src, _)
        | E::NextAmountOfDamageThatWouldBeDealtThisTurnBySourceToRecipient(g, _src, _) => {
            // CR 614.1a: typed-source filter not yet decomposed (no
            // `DamageSources` → `TargetFilter` bridge); leave unfiltered.
            (amount_from(g)?, PreventionScope::AllDamage, None)
        }
        E::NextAmountOfDamageThatWouldBeDealtThisTurnBySpellToRecipient(g, _spell, _) => {
            // CR 614.1a: spell-source filter not yet decomposed; leave
            // unfiltered. Source narrowing is a future extension.
            (amount_from(g)?, PreventionScope::AllDamage, None)
        }
        // No-amount "next time" variants — saturate at u32::MAX so the
        // shield prevents whatever single damage event matches first
        // and is then consumed. CR 615.1 covers single-event shields.
        E::NextTimeCombatDamageWouldBeDealtThisTurnByCreature(perm)
        | E::NextTimeCombatDamageWouldBeDealtThisTurnByCreatureToAnyNumberOfRecipients(perm, _)
        | E::NextTimeCombatDamageWouldBeDealtThisTurnByCreatureToRecipient(perm, _) => (
            PreventionAmount::Next(u32::MAX),
            PreventionScope::CombatDamage,
            Some(convert_permanent(perm)?),
        ),
        E::NextTimeDamageWouldBeDealtThisTurnByPermanent(perm)
        | E::NextTimeDamageWouldBeDealtThisTurnByPermanentToARecipient(perm, _)
        | E::NextTimeDamageWouldBeDealtThisTurnByPermanentToRecipient(perm, _) => (
            PreventionAmount::Next(u32::MAX),
            PreventionScope::AllDamage,
            Some(convert_permanent(perm)?),
        ),
        E::NextTimeDamageWouldBeDealtThisTurnByAPermanentToRecipient(perms, _) => (
            PreventionAmount::Next(u32::MAX),
            PreventionScope::AllDamage,
            Some(convert_permanents(perms)?),
        ),
        E::NextTimeDamageWouldBeDealtThisTurnToARecipient(_)
        | E::NextTimeDamageWouldBeDealtThisTurnToRecipient(_)
        | E::NextTimeDamageWouldBeDealtToRecipient(_) => (
            PreventionAmount::Next(u32::MAX),
            PreventionScope::AllDamage,
            None,
        ),
        // Source-by-spell / source-by-anonymous-source — engine has no
        // typed bridge from `Spells`/`SingleDamageSource` to a
        // `TargetFilter` yet; leave unfiltered.
        E::NextTimeDamageWouldBeDealtThisTurnByASpellToRecipient(_, _)
        | E::NextTimeDamageWouldBeDealtThisTurnBySource(_)
        | E::NextTimeDamageWouldBeDealtThisTurnBySourceToARecipient(_, _)
        | E::NextTimeDamageWouldBeDealtThisTurnBySourceToRecipient(_, _) => (
            PreventionAmount::Next(u32::MAX),
            PreventionScope::AllDamage,
            None,
        ),
        E::NextDistributedDamageThisTurn => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "Effect",
                needed_variant: "PreventDamage on distributed-damage events".into(),
            });
        }
    })
}

fn expiration_tag(e: &Expiration) -> String {
    serde_json::to_value(e)
        .ok()
        .and_then(|v| {
            v.get("_Expiration")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

#[cfg(test)]
mod tests {
    use engine::types::ability::{
        AbilityCost, Effect, QuantityExpr, ReplacementMode, TargetFilter,
    };

    use super::*;
    use crate::schema::types::{Condition, GameNumber, Permanent, ReplacementActionWouldEnter};

    #[test]
    fn as_enters_may_pay_life_unless_tapped_lowers_to_single_cost_gate() {
        let defs = convert_as_enters(
            &Permanent::ThisPermanent,
            &[
                ReplacementActionWouldEnter::MayCost(ReplacementActionWouldEnterCost::PayLife(
                    Box::new(GameNumber::Integer(2)),
                )),
                ReplacementActionWouldEnter::Unless(
                    Condition::CostWasPaid,
                    vec![ReplacementActionWouldEnter::EntersTapped],
                ),
            ],
        )
        .unwrap();

        assert_eq!(defs.len(), 1);
        assert!(defs[0].execute.is_none());
        assert_eq!(defs[0].valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(
            &defs[0].mode,
            ReplacementMode::MayCost {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 2 }
                },
                decline: Some(decline),
            } if matches!(&*decline.effect, Effect::Tap { target } if *target == TargetFilter::SelfRef)
        ));
    }

    #[test]
    fn standalone_as_enters_may_cost_remains_a_precise_engine_gap() {
        let err = convert_as_enters(
            &Permanent::ThisPermanent,
            &[ReplacementActionWouldEnter::MayCost(
                ReplacementActionWouldEnterCost::PayLife(Box::new(GameNumber::Integer(2))),
            )],
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                ..
            }
        ));
    }
}
