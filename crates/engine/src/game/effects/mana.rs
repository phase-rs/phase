use crate::game::quantity::{resolve_quantity, resolve_quantity_with_targets};
use crate::game::{mana_payment, mana_sources};
#[cfg(test)]
use crate::types::ability::ManaContribution;
use crate::types::ability::{
    ChoiceValue, Effect, EffectError, EffectKind, LinkedExileScope, ManaProduction,
    ManaSpendRestriction, ManaTargetRole, ManaTargetSlot, ObjectScope, ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, ManaChoice, ManaChoiceContext, ManaChoicePrompt, MayTriggerOrigin, WaitingFor,
};
use crate::types::mana::{ManaColor, ManaRestriction, ManaType};
use crate::types::player::PlayerId;

/// CR 601.2c + CR 608.2b: A view of `ability` whose `targets` contain only the
/// target chosen for `slot`, re-validated against that slot's own filter.
/// Shared quantity resolution (`QuantityRef::TargetZoneCardCount`,
/// `LifeTotal { player: Target }`) reads "the first player target", so each role
/// must be resolved against a view holding only its own — otherwise the
/// recipient at index 0 would be read as the count source. Scoping HERE keeps
/// `game/quantity.rs` slot-agnostic, because `TargetZoneCardCount` serves many
/// non-mana cards.
///
/// Returns `None` when the role declares no filter for `slot`, when that filter
/// is a context-ref (no slot surfaced), when the index is out of range, or when
/// the chosen target is no longer legal (CR 608.2b) — callers distinguish
/// "no such role" from "role present but target illegal" via `role.filter_for`.
fn ability_scoped_to_slot(
    state: &GameState,
    ability: &ResolvedAbility,
    role: &ManaTargetRole,
    slot: ManaTargetSlot,
) -> Option<ResolvedAbility> {
    let index = role.slot_index(slot)?;
    let filter = role.filter_for(slot)?;
    let chosen = ability.targets.get(index)?;
    let legal = crate::game::targeting::validate_targets_for_ability(
        state,
        std::slice::from_ref(chosen),
        filter,
        ability,
    );
    let _kept = legal.into_iter().next()?;
    let mut scoped = ability.clone();
    scoped.targets = retain_only_player_at(ability, Some(index));
    Some(scoped)
}

/// CR 601.2c: Narrow `ability.targets` to ONE player target — the entry at
/// `keep`, or none at all when `keep` is `None` — while leaving every NON-player
/// target in place at its original position.
///
/// Scoping must be confined to the axis the shared quantity resolvers actually
/// read. `QuantityRef::TargetZoneCardCount` and `LifeTotal { player: Target }`
/// scan for the FIRST `TargetRef::Player`, so leaving two players visible is
/// what let a count read the recipient. Object-scoped production
/// (`ManaProduction::AnyCombinationOfObjectColors { scope: Target }` via
/// `object_colors_for_scope`) reads an OBJECT target from the same vec and
/// requires nothing about the player roles — clearing the whole vec would make
/// that half of the production silently produce no colors, which CR 608.2b does
/// not license: only the part that "requires information about an illegal
/// target" fails.
fn retain_only_player_at(ability: &ResolvedAbility, keep: Option<usize>) -> Vec<TargetRef> {
    ability
        .targets
        .iter()
        .enumerate()
        .filter(|(i, t)| !matches!(t, TargetRef::Player(_)) || Some(*i) == keep)
        .map(|(_, t)| t.clone())
        .collect()
}

/// CR 601.2c + CR 608.2b: The ability view the production COUNT resolves
/// against. FOUR distinct cases, which must not be collapsed:
///
/// 1. No count-source role declared (no role at all, or `Recipient`-only —
///    Jetfire, and every single-role mana). The count reads nothing
///    target-derived, so the unscoped ability is correct and this preserves
///    today's behavior exactly.
/// 2. The count source is a CONTEXT REF (`ScopedPlayer`, `TriggeringPlayer`,
///    …). It surfaces no target slot, so there is no chosen target to be
///    illegal — the player comes from context, exactly as the recipient path
///    does. Resolving it through the CR 608.2b branch instead would silently
///    yield 0 under a rule that does not apply, because nothing here is an
///    illegal target.
/// 3. A count source is declared, surfaces a slot, and its chosen target is
///    still legal. Narrow to that ONE player, so shared "first player target"
///    quantity resolution (`QuantityRef::TargetZoneCardCount`,
///    `LifeTotal { player: Target }`) cannot read the recipient instead.
/// 4. A count source is declared but its chosen target is no longer legal.
///    CR 608.2b: the effect "fails to determine any such information" about an
///    illegal target — so expose NO player, resolving the count to 0. Falling
///    back to the unscoped ability here would be a BUG: it still holds the
///    (legal) recipient, so the count would read the RECIPIENT's hand instead
///    of failing.
///
/// In cases 3 and 4 only the PLAYER axis is narrowed; non-player targets stay
/// put (see `retain_only_player_at`).
fn count_scoped_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    role: Option<&ManaTargetRole>,
) -> ResolvedAbility {
    // Case 1: nothing target-derived to scope.
    let Some(role) = role.filter(|r| r.count_source().is_some()) else {
        return ability.clone();
    };
    // Case 2: context-ref count source — read the player from context, mirroring
    // `mana_effect_recipient`, and expose it as the sole player target.
    if let Some(filter) = role.count_source().filter(|filter| filter.is_context_ref()) {
        let from_context = super::resolve_player_for_context_ref(state, ability, filter);
        let mut scoped = ability.clone();
        scoped.targets = retain_only_player_at(ability, None);
        scoped.targets.push(TargetRef::Player(from_context));
        return scoped;
    }
    // Case 3, else case 4.
    ability_scoped_to_slot(state, ability, role, ManaTargetSlot::CountSource).unwrap_or_else(|| {
        let mut scoped = ability.clone();
        scoped.targets = retain_only_player_at(ability, None);
        scoped
    })
}

/// CR 106.4 + CR 608.2b: Which player's mana pool receives the mana.
/// `None` means the effect declares a recipient whose chosen target is no longer
/// legal — the mana is not added to any pool ("illegal targets won't be affected
/// by parts of the effect for which they're illegal"). This is DISTINCT from a
/// role with no recipient at all (Jeska's Will, Carpet of Flowers), which
/// correctly deposits into `ability.controller`.
///
/// Shared by the immediate `resolve` path, the color-choice prompt, and the
/// prompt-completion path so all three agree on the recipient. No quantity is
/// inspected anywhere — the role states the answer.
fn mana_effect_recipient(
    state: &GameState,
    ability: &ResolvedAbility,
    role: Option<&ManaTargetRole>,
) -> Option<PlayerId> {
    let Some(filter) = role.and_then(ManaTargetRole::recipient) else {
        // CR 106.4: no recipient role declared — the controller adds the mana.
        return Some(ability.controller);
    };
    // CR 106.4: context-ref recipients (ScopedPlayer, TriggeringPlayer,
    // ParentTargetController, chosen player) resolve via the context, not
    // `ability.targets`.
    if filter.is_context_ref() {
        return Some(super::resolve_player_for_context_ref(
            state, ability, filter,
        ));
    }
    // CR 115.1 + CR 106.4: "target player adds …" (Jetfire, Ingenious
    // Scientist) — the targeted player was chosen at announcement and lives in
    // this role's OWN slot. An illegal chosen target yields `None`: the mana is
    // not deposited anywhere (CR 608.2b). Never fall back to the controller —
    // that would hand a targeted player's mana to the caster.
    let scoped = ability_scoped_to_slot(state, ability, role?, ManaTargetSlot::Recipient)?;
    scoped.targets.iter().find_map(|t| match t {
        TargetRef::Player(player_id) => Some(*player_id),
        TargetRef::Object(_) => None,
    })
}

/// Mana effect: adds mana to the recipient's mana pool (CR 106.4).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (produced, restrictions, grants, expiry, mana_role) = match &ability.effect {
        Effect::Mana {
            produced,
            restrictions,
            grants,
            expiry,
            // CR 601.2c: `target` is a ROLE. The RECIPIENT names the player
            // whose mana pool receives the mana (CR 106.4); the COUNT SOURCE
            // names the player a `TargetZoneCardCount` / `LifeTotal` quantity
            // inside `produced` reads. Each is resolved below from its OWN
            // target slot — no quantity shape is inspected to tell them apart.
            target,
        } => (produced, restrictions, grants, *expiry, target.clone()),
        _ => return Err(EffectError::MissingParam("Produced".to_string())),
    };
    // CR 601.2c + CR 608.2b: resolve the production count against a view of the
    // ability holding ONLY the count source's own chosen target, so shared
    // "first player target" quantity resolution cannot read the recipient.
    let count_ability = count_scoped_ability(state, ability, mana_role.as_ref());
    let count_ability = &count_ability;
    let is_triggered_mana_inline = crate::game::mana_abilities::is_triggered_mana_ability(
        ability,
        state.current_trigger_event.as_ref(),
    );
    let mana_choice = (!is_triggered_mana_inline)
        .then(|| {
            crate::game::mana_abilities::mana_choice_prompt(
                &ability.effect,
                state,
                ability.source_id,
                Some(ability),
            )
        })
        .flatten();
    if let Some(choice) = mana_choice {
        // CR 106.4: the player who *chooses* the color is the effect's named
        // recipient — for "that player adds one mana of any color they choose"
        // (Spectral Searchlight, Stadium Vendors) that is the chosen player, not
        // the controller. Resolve it here so the prompt is directed correctly;
        // `handle_choose_mana_effect` re-derives the same recipient for deposit.
        //
        // CR 608.2b: when the declared recipient's chosen target is no longer
        // legal, no player adds this mana, so there is no color for anyone to
        // choose — the mana part of the effect simply does nothing.
        let Some(prompt_player) = mana_effect_recipient(state, ability, mana_role.as_ref()) else {
            return Ok(());
        };
        state.waiting_for = WaitingFor::ChooseManaColor {
            player: prompt_player,
            choice,
            context: ManaChoiceContext::ResolvingEffect(Box::new(ability.clone())),
        };
        return Ok(());
    }

    // CR 106.3: Mana is produced by the effects of mana abilities, spells, and
    // abilities that aren't mana abilities. The source of produced mana is the
    // source of the ability or spell.
    // CR 107.1b: When X is part of a mana production quantity (rare — e.g., an
    // effect on the stack that resolved via `ResolvedAbility` and produces X mana),
    // `resolve_quantity_with_targets` threads `ability.chosen_x` through to the
    // `Variable { name: "X" }` branch of `resolve_ref`. Non-X mana production
    // (Fixed, ObjectCount, etc.) is unaffected.
    //
    // CR 605.1b + CR 106.12a: For inline `TapsForMana` triggered mana abilities
    // (Fertile Ground, Utopia Sprawl AnyOneColor), the auto-tap planner may have
    // stored a `current_triggered_mana_override` so the resolver produces the
    // planned color rather than defaulting to the first listed option.
    let mana_types = if is_triggered_mana_inline {
        match state.current_triggered_mana_override.clone() {
            Some(crate::types::game_state::ProductionOverride::SingleColor(color)) => {
                // Resolve the count from the production descriptor, then produce
                // that many units of the override color — mirrors the behavior of
                // `resolve_single_color_override` in `mana_abilities.rs`.
                let count = resolve_mana_types_with_ability(produced, &*state, count_ability).len();
                vec![color; count]
            }
            Some(crate::types::game_state::ProductionOverride::Combination(types)) => types,
            None => resolve_mana_types_with_ability(produced, &*state, count_ability),
        }
    } else {
        resolve_mana_types_with_ability(produced, &*state, count_ability)
    };
    let source_could_produce_two_or_more_colors =
        mana_sources::mana_production_could_produce_two_or_more_colors(
            state,
            ability.controller,
            ability.source_id,
            produced,
        );

    // Resolve restriction templates into concrete restrictions
    let concrete_restrictions = resolve_restrictions(restrictions, state, ability.source_id);

    let recipient = match produced {
        // CR 106.3 + CR 109.5: "add one mana of any type that land produced" —
        // the bonus mana goes to the player who tapped the land (the
        // `TappedForMana` event's `player_id`), not the trigger's controller.
        ManaProduction::TriggerEventManaType => state
            .current_trigger_event
            .as_ref()
            .and_then(|event| match event {
                GameEvent::TappedForMana { player_id, .. } => Some(*player_id),
                _ => None,
            })
            .unwrap_or(ability.controller),
        // CR 106.4: A subject-led mana clause routes the mana to the named
        // player ("the active player adds {C}{C} …" on a Phase trigger, "that
        // player adds one mana of any color" on Spectral Searchlight).
        _ => match mana_effect_recipient(state, ability, mana_role.as_ref()) {
            Some(player) => player,
            // CR 608.2b: "Illegal targets won't be affected by parts of the
            // effect for which they're illegal." A declared recipient whose
            // chosen target is no longer legal receives nothing, and the mana is
            // NOT redirected to the controller. The count source's half of the
            // sentence already resolved above and is unaffected.
            None => {
                events.push(GameEvent::EffectResolved {
                    kind: EffectKind::from(&ability.effect),
                    source_id: ability.source_id,
                    subject: None,
                });
                return Ok(());
            }
        },
    };

    // CR 106.4: When an effect instructs a player to add mana, that mana goes
    // into that player's mana pool.
    let produced_mana = !mana_types.is_empty();
    for mana_type in mana_types {
        mana_payment::produce_mana_with_attributes_from_source_quality(
            state,
            ability.source_id,
            mana_type,
            recipient,
            false,
            source_could_produce_two_or_more_colors,
            &concrete_restrictions,
            grants,
            expiry,
            events,
        );
    }
    record_firebending_if_marked(state, ability, produced_mana, events);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 106.3 + CR 608.2d: Complete a mana-choice prompt created while a spell
/// or non-mana ability effect is resolving.
pub fn handle_choose_mana_effect(
    state: &mut GameState,
    ability: &ResolvedAbility,
    prompt: &ManaChoicePrompt,
    chosen: ManaChoice,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, crate::game::engine::EngineError> {
    let Effect::Mana {
        produced,
        restrictions,
        grants,
        expiry,
        target,
    } = &ability.effect
    else {
        return Err(crate::game::engine::EngineError::InvalidAction(
            "Pending mana choice is not a mana effect".to_string(),
        ));
    };

    // CR 601.2c + CR 608.2b: the prompt-completion path derives the COUNT too
    // (`SingleColor` multiplies the chosen color by it), so it must read the
    // count source's own slot for exactly the same reason `resolve` does —
    // otherwise a `Both` role with a color choice counts the RECIPIENT.
    let count_ability = count_scoped_ability(state, ability, target.as_ref());
    let mana_types = chosen_mana_types_for_prompt(state, &count_ability, produced, prompt, chosen)?;
    let source_could_produce_two_or_more_colors =
        mana_sources::mana_production_could_produce_two_or_more_colors(
            state,
            ability.controller,
            ability.source_id,
            produced,
        );
    let concrete_restrictions = resolve_restrictions(restrictions, state, ability.source_id);
    // CR 106.4: deposit the mana into the effect's named recipient's pool (the
    // same player the color prompt was directed to in `resolve`), not the
    // controller. Priority still returns to the controller below — only the mana
    // is redirected.
    //
    // CR 608.2b: `None` means the declared recipient's chosen target is no
    // longer legal — no player adds this mana, and it is NOT redirected to the
    // controller.
    let recipient = mana_effect_recipient(state, ability, target.as_ref());
    let produced_mana = recipient.is_some() && !mana_types.is_empty();
    if let Some(recipient) = recipient {
        for mana_type in mana_types {
            mana_payment::produce_mana_with_attributes_from_source_quality(
                state,
                ability.source_id,
                mana_type,
                recipient,
                false,
                source_could_produce_two_or_more_colors,
                &concrete_restrictions,
                grants,
                *expiry,
                events,
            );
        }
    }
    record_firebending_if_marked(state, ability, produced_mana, events);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    // Priority is restored to the ability's controller exactly as before this
    // change (the recipient computed above governs only where the mana lands, a
    // player receiving mana does not thereby gain priority). Kept as prior
    // behavior rather than migrated to the active player (CR 117.3b) to stay
    // scoped to the mana-recipient fix.
    state.waiting_for = WaitingFor::Priority {
        player: ability.controller,
    };
    state.priority_player = ability.controller;
    super::drain_pending_continuation(state, events);
    Ok(state.waiting_for.clone())
}

fn record_firebending_if_marked(
    state: &mut GameState,
    ability: &ResolvedAbility,
    produced_mana: bool,
    events: &mut Vec<GameEvent>,
) {
    if !produced_mana {
        return;
    }
    let Some(MayTriggerOrigin::Keyword {
        keyword: crate::types::keywords::KeywordKind::Firebending,
    }) = ability.may_trigger_origin
    else {
        return;
    };
    crate::game::bending::record_bending(
        state,
        events,
        crate::types::events::BendingType::Fire,
        ability.source_id,
        ability.controller,
    );
}

fn chosen_mana_types_for_prompt(
    state: &GameState,
    ability: &ResolvedAbility,
    produced: &ManaProduction,
    prompt: &ManaChoicePrompt,
    chosen: ManaChoice,
) -> Result<Vec<ManaType>, crate::game::engine::EngineError> {
    match (prompt, chosen) {
        (ManaChoicePrompt::SingleColor { options }, ManaChoice::SingleColor(color)) => {
            if !options.contains(&color) {
                return Err(crate::game::engine::EngineError::InvalidAction(
                    "Chosen color is not among the legal options".to_string(),
                ));
            }
            let count = resolve_mana_types_for_ability(produced, state, ability).len();
            Ok(vec![color; count])
        }
        (ManaChoicePrompt::Combination { options }, ManaChoice::Combination(combo)) => {
            if !options.iter().any(|option| option == &combo) {
                return Err(crate::game::engine::EngineError::InvalidAction(
                    "Chosen combination is not among the legal options".to_string(),
                ));
            }
            Ok(combo)
        }
        (ManaChoicePrompt::AnyCombination { count, options }, ManaChoice::Combination(combo)) => {
            if combo.len() != *count || combo.iter().any(|color| !options.contains(color)) {
                return Err(crate::game::engine::EngineError::InvalidAction(
                    "Chosen mana combination is not legal for this prompt".to_string(),
                ));
            }
            Ok(combo)
        }
        _ => Err(crate::game::engine::EngineError::InvalidAction(
            "Mana choice shape does not match the active prompt".to_string(),
        )),
    }
}

/// Resolve parse-time restriction templates into concrete `ManaRestriction` values.
/// CR 106.6: Some spells or abilities that produce mana restrict how that mana can be spent.
pub(crate) fn resolve_restrictions(
    templates: &[ManaSpendRestriction],
    state: &GameState,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<ManaRestriction> {
    templates
        .iter()
        .filter_map(|template| match template {
            ManaSpendRestriction::SpellOnly => Some(ManaRestriction::OnlyForSpell),
            ManaSpendRestriction::SpellType(t) => {
                Some(ManaRestriction::OnlyForSpellType(t.clone()))
            }
            ManaSpendRestriction::ChosenCreatureType => state
                .objects
                .get(&source_id)
                .and_then(|obj| obj.chosen_creature_type())
                .map(|ct| ManaRestriction::OnlyForCreatureType(ct.to_string())),
            // CR 106.6: Combined spell type + ability activation restriction.
            ManaSpendRestriction::SpellTypeOrAbilityActivation {
                spell_type,
                ability,
            } => Some(ManaRestriction::OnlyForTypeSpellsOrAbilities {
                spell_type: spell_type.clone(),
                ability: *ability,
            }),
            ManaSpendRestriction::ActivateOnly => Some(ManaRestriction::OnlyForActivation),
            ManaSpendRestriction::ActivateTagged(tag) => {
                Some(ManaRestriction::OnlyForTaggedActivation(*tag))
            }
            ManaSpendRestriction::XCostOnly => Some(ManaRestriction::OnlyForXCosts),
            ManaSpendRestriction::SpellWithKeywordKind(kind) => {
                Some(ManaRestriction::OnlyForSpellWithKeywordKind(*kind))
            }
            ManaSpendRestriction::SpellWithKeywordKindFromZone { kind, zone } => Some(
                ManaRestriction::OnlyForSpellWithKeywordKindFromZone(*kind, *zone),
            ),
            ManaSpendRestriction::SpellWithManaValue { comparator, value } => {
                Some(ManaRestriction::OnlyForSpellWithManaValue {
                    comparator: *comparator,
                    value: *value,
                })
            }
            // CR 106.6 + CR 107.3 + CR 202.3: Lower the disjunctive MV/X cost
            // criteria (with optional type narrowing) into the runtime gate
            // checked against `SpellMeta` by `allows_spell`.
            ManaSpendRestriction::SpellMatchingCostCriteria {
                spell_type,
                criteria,
            } => Some(ManaRestriction::OnlyForSpellMatchingCostCriteria {
                spell_type: spell_type.clone(),
                criteria: criteria.clone(),
            }),
            // CR 105.2 + CR 106.6: Lower color-count spend restrictions into the
            // runtime gate checked against `SpellMeta.color_count`.
            ManaSpendRestriction::SpellWithColorCount { comparator, count } => {
                Some(ManaRestriction::OnlyForSpellWithColorCount {
                    comparator: *comparator,
                    count: *count,
                })
            }
            ManaSpendRestriction::SpellFromZone(zs) => {
                Some(ManaRestriction::OnlyForSpellFromZone(*zs))
            }
            // CR 106.6 + CR 116.2m + CR 709.5e: Lower the door-unlock special-action
            // leaf into the runtime gate checked by `allows_special_action` when a
            // Room's unlock cost is paid through `PaymentContext::SpecialAction`.
            ManaSpendRestriction::UnlockDoor => Some(ManaRestriction::OnlyForSpecialAction(
                crate::types::mana::SpecialAction::UnlockDoor,
            )),
            // CR 106.6 + CR 708.4: Lower the face-down-cast leaf into the runtime
            // gate checked against `SpellMeta.is_face_down` by `allows_spell`. The
            // gate reads cast face-down intent (not `obj.face_down`), so it
            // correctly rejects exile-concealment casts (foretell/hideaway, whose
            // `obj.face_down = true` but which are cast face up, CR 702.143c). It is
            // fail-closed: no production path casts a spell face down, so the gate
            // never over-permits.
            ManaSpendRestriction::FaceDownSpell => Some(ManaRestriction::OnlyForFaceDownSpell),
            // CR 106.6 + CR 116.2b + CR 702.37e / CR 702.168d / CR 701.40b: Lower
            // the turn-face-up special-action leaf into the runtime gate checked by
            // `allows_special_action`. The `GameAction::TurnFaceUp` handler pays the
            // morph/disguise/manifest cost through
            // `PaymentContext::SpecialAction(TurnFaceUp)`, so this gate is live —
            // spendable for a turn-up and rejected for any other context.
            ManaSpendRestriction::TurnPermanentFaceUp => {
                Some(ManaRestriction::OnlyForSpecialAction(
                    crate::types::mana::SpecialAction::TurnFaceUp,
                ))
            }
            // CR 106.6: Disjunction — recursively lower each branch. If every branch
            // dropped (e.g. an unresolvable `ChosenCreatureType` with no chosen type),
            // the disjunction has no payable cases, so drop it too.
            ManaSpendRestriction::Any(subs) => {
                let inner = resolve_restrictions(subs, state, source_id);
                (!inner.is_empty()).then_some(ManaRestriction::OnlyForAny(inner))
            }
        })
        .collect()
}

/// Resolve a typed mana production descriptor into concrete mana units.
///
/// CR 605.3a: Mana abilities don't use the stack, so they have no `ResolvedAbility`
/// and thus no `chosen_x` — this entry point used to be the legacy path for
/// `mana_abilities::resolve_mana_ability`. The inline mana-ability resolver now
/// always routes through `resolve_mana_types_for_ability` so the cost-paid
/// object snapshot (Food Chain class) and `chosen_x` are visible. Kept as a
/// minimal building block for callers that have neither.
#[allow(dead_code)]
pub(crate) fn resolve_mana_types(
    produced: &ManaProduction,
    state: &GameState,
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<ManaType> {
    resolve_mana_types_impl(produced, state, None, controller, source_id)
}

/// Variant of `resolve_mana_types` that threads the resolving ability's context
/// (including `chosen_x`) into quantity resolution. Use this from stack-resolving
/// effect handlers (`effects::mana::resolve`).
fn resolve_mana_types_with_ability(
    produced: &ManaProduction,
    state: &GameState,
    ability: &ResolvedAbility,
) -> Vec<ManaType> {
    resolve_mana_types_impl(
        produced,
        state,
        Some(ability),
        ability.controller,
        ability.source_id,
    )
}

/// CR 117.1 + CR 202.3: Public-crate wrapper for `resolve_mana_types_with_ability`.
/// Used by the inline mana-ability resolver in `mana_abilities.rs` to thread a
/// `ResolvedAbility` carrying `cost_paid_object` (Food Chain class)
/// and `chosen_x` into the production-count resolution.
pub(crate) fn resolve_mana_types_for_ability(
    produced: &ManaProduction,
    state: &GameState,
    ability: &ResolvedAbility,
) -> Vec<ManaType> {
    resolve_mana_types_with_ability(produced, state, ability)
}

fn resolve_count(
    count: &crate::types::ability::QuantityExpr,
    state: &GameState,
    ability: Option<&ResolvedAbility>,
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
) -> usize {
    let raw = match ability {
        Some(a) => resolve_quantity_with_targets(state, count, a),
        None => resolve_quantity(state, count, controller, source_id),
    };
    raw.max(0) as usize
}

fn resolve_mana_types_impl(
    produced: &ManaProduction,
    state: &GameState,
    ability: Option<&ResolvedAbility>,
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<ManaType> {
    match produced {
        // CR 106.1a: Colored mana is produced in the five standard colors.
        ManaProduction::Fixed { colors, .. } => colors.iter().map(mana_color_to_type).collect(),
        // CR 106.1b: Colorless mana is a type of mana distinct from colored mana.
        ManaProduction::Colorless { count } => {
            vec![ManaType::Colorless; resolve_count(count, state, ability, controller, source_id)]
        }
        // CR 106.5: If an ability would produce one or more mana of an undefined type,
        // it produces no mana instead.
        ManaProduction::AnyOneColor {
            count,
            color_options,
            ..
        } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let Some(mana_type) = color_options.first().map(mana_color_to_type) else {
                return Vec::new();
            };
            vec![mana_type; amount]
        }
        ManaProduction::AnyCombination {
            count,
            color_options,
        } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            if color_options.is_empty() {
                return Vec::new();
            }
            (0..amount)
                .map(|index| mana_color_to_type(&color_options[index % color_options.len()]))
                .collect()
        }
        ManaProduction::ChosenColor {
            count,
            fixed_alternative,
            ..
        } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            match (chosen_color_for_mana(state, source_id), fixed_alternative) {
                // A color was chosen — produce that color.
                (Some(color), _) => vec![mana_color_to_type(&color); amount],
                // CR 106.1: count derivation must be independent of color
                // resolvability — the SingleColor choice supplies the actual
                // color. When a fixed alternative exists, the no-prompt default
                // path (auto-tap / AI direct activation) produces the fixed
                // color deterministically; the count-derivation path
                // (`chosen_mana_types_for_prompt`) overrides the color with the
                // player's `SingleColor` choice, so the length is what matters.
                (None, Some(fixed)) => vec![mana_color_to_type(fixed); amount],
                // CR 106.5: pure chosen-color production with no color chosen
                // produces no mana (undefined type).
                (None, None) => Vec::new(),
            }
        }
        // CR 106.7: Produce mana of any color that a land an opponent controls could produce.
        // Delegates to mana_sources::opponent_land_color_options for the shared computation.
        ManaProduction::OpponentLandColors { count } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let color_options = mana_sources::opponent_land_color_options(state, controller);
            // CR 106.5: If no color can be defined, produce no mana.
            let Some(first) = color_options.first().copied() else {
                return Vec::new();
            };
            vec![first; amount]
        }
        // CR 106.1 + CR 106.5 + CR 202.2c: Omnath, Locus of All — "add three mana
        // in any combination of its colors." The color set is the scoped object's
        // colors (dynamic, mirroring AnyOneColorAmongPermanents), not a static
        // option list. This is the no-override default path; the per-unit free
        // choice is surfaced by `mana_choice_prompt` (ManaChoicePrompt::AnyCombination)
        // when the object has more than one color. Without an override the colors
        // are cycled, mirroring the static AnyCombination default. CR 106.5: a
        // colorless / unbound object produces no mana.
        ManaProduction::AnyCombinationOfObjectColors { count, scope } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let color_options = object_colors_for_scope(state, ability, *scope);
            if color_options.is_empty() {
                return Vec::new();
            }
            (0..amount)
                .map(|index| mana_color_to_type(&color_options[index % color_options.len()]))
                .collect()
        }
        // CR 106.7 + CR 106.1b: Reflecting Pool class — produce N mana of any
        // type (W/U/B/R/G/C) that a land matching `land_filter` could produce.
        // Without an explicit choice override (auto-tap during cost payment, or
        // direct activation without prompt), the first listed type is produced
        // mirroring the `OpponentLandColors` / `AnyOneColor` precedent. The
        // per-type choice prompt is surfaced by `mana_choice_prompt` when the
        // option set has more than one type. CR 106.5: an empty option set
        // (no matching lands, or only mutually-recursive producers) produces
        // no mana.
        ManaProduction::AnyTypeProduceableBy { count, land_filter } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let type_options = mana_sources::produceable_mana_types_by_filter(
                state,
                land_filter,
                controller,
                source_id,
            );
            let Some(first) = type_options.first().copied() else {
                return Vec::new();
            };
            vec![first; amount]
        }
        // CR 605.1a + CR 406.1 + CR 610.3: One mana of any of the colors among the
        // cards exiled-with this source (Pit of Offerings). Reads `state.exile_links`
        // for the relation; the per-color choice is selected by the caller via
        // `color_override` (auto-tap during cost payment, or AI/UI on direct activation),
        // exactly like `AnyOneColor`. Without an override the first listed color is
        // produced. CR 106.5: undefined mana type → produce no mana.
        ManaProduction::ChoiceAmongExiledColors { source } => {
            let color_options = exiled_color_options(state, *source, source_id);
            let Some(first) = color_options.first().copied() else {
                return Vec::new();
            };
            vec![first]
        }
        // CR 605.3b + CR 106.1a: Filter-land combinations. When no override is
        // supplied (stack-resolving paths or direct activation without choice),
        // fall back to the first listed combination — mirrors the
        // `ChoiceAmongExiledColors` precedent. `produce_mana_from_ability`
        // selects the combination via `ProductionOverride::Combination`, so
        // this branch is only hit on the "no override at all" path.
        ManaProduction::ChoiceAmongCombinations { options } => options
            .first()
            .map(|combo| combo.iter().map(mana_color_to_type).collect())
            .unwrap_or_default(),
        // CR 106.1: Mixed colorless + colored production (e.g. {C}{W}, {C}{C}{R}).
        ManaProduction::Mixed {
            colorless_count,
            colors,
        } => {
            let mut mana = vec![ManaType::Colorless; *colorless_count as usize];
            mana.extend(colors.iter().map(mana_color_to_type));
            mana
        }
        // CR 903.4 + CR 903.4f + CR 106.5: Produce mana of one color from the
        // activator's commander color identity. Without a color_override
        // (auto-tap, or no choice needed) this picks the first listed color,
        // mirroring the `ChoiceAmongExiledColors` / `AnyOneColor` precedent.
        // The color-choice prompt is driven by `mana_choice_prompt` when
        // identity.len() > 1. If the identity is empty — no commander or an
        // undefined identity per CR 903.4f — the ability produces no mana.
        ManaProduction::AnyInCommandersColorIdentity { count, .. } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let identity = super::super::commander::commander_color_identity(state, controller);
            let Some(first) = identity.first() else {
                return Vec::new();
            };
            vec![mana_color_to_type(first); amount]
        }
        // CR 106.1 + CR 109.1: Produce one mana of each distinct color (W/U/B/R/G)
        // found among permanents matching `filter`. Used by Faeburrow Elder.
        // Returns empty when no colored permanent matches (CR 106.5).
        ManaProduction::DistinctColorsAmongPermanents { filter } => {
            distinct_colors_among_permanents(state, ability, source_id, filter)
                .into_iter()
                .map(|c| mana_color_to_type(&c))
                .collect()
        }
        // CR 106.1 + CR 109.1: Mox Amber — one chosen color from among matching
        // permanents. Without a color_override, produce the first listed color
        // (mirrors ChoiceAmongExiledColors / AnyOneColor). CR 106.5: empty set
        // → no mana.
        ManaProduction::AnyOneColorAmongPermanents { count, filter, .. } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let color_options = distinct_colors_among_permanents(state, ability, source_id, filter);
            let Some(first) = color_options.first().copied() else {
                return Vec::new();
            };
            vec![mana_color_to_type(&first); amount]
        }
        // CR 603.7c + CR 106.3 + CR 106.5 + CR 106.12a: Vorinclex / Dictate of
        // Karametra — "add one mana of any type that land produced." The set of
        // produced types is read from the triggering `TappedForMana` event
        // carried in `state.current_trigger_event` at resolution time. The
        // `TapsForMana` trigger fires once per mana-ability resolution
        // (CR 106.12a), so this branch sees the full produced set, not a single
        // unit. If the current event is absent (off-stack resolution) or not a
        // `TappedForMana` event, this produces no mana (CR 106.5 — undefined
        // mana type).
        //
        // For every land the engine models, a single resolution produces mana
        // of one uniform color (basics → one type; Nykthos → all green), so
        // emitting one unit per *distinct* color yields exactly one mana — the
        // CR-correct "any type that land produced" with no choice to make.
        //
        // If a future card requires the player to *choose* among multiple
        // produced types in a single resolution ("any one type that land
        // produced"), the resolver must be extended to emit a player choice.
        // Add a separate `ManaProduction::TriggerEventManaTypeChoice` variant
        // before reusing this branch — silently expanding the vec here would
        // skip the choice.
        ManaProduction::TriggerEventManaType => {
            use crate::types::events::GameEvent;
            match &state.current_trigger_event {
                Some(GameEvent::TappedForMana { produced, .. }) => {
                    let distinct: std::collections::HashSet<_> = produced.iter().copied().collect();
                    distinct.into_iter().collect()
                }
                _ => Vec::new(),
            }
        }
    }
}

/// CR 106.1 + CR 109.1: Shared helper returning the distinct colors (W/U/B/R/G)
/// present among permanents matching `filter`. Colorless permanents contribute
/// nothing. Used by both the mana ability resolver and `mana_sources` so that
/// cost-payment and direct activation see the same option set.
/// CR 202.2c + CR 106.5: Colors of the object identified by `scope`, for an
/// `AnyCombinationOfObjectColors` mana production (Omnath, Locus of All). Reads
/// the object's current `color` (zone-independent — correct after the revealed
/// card is put into hand, per CR 400.7j), returned in stable WUBRG order. Empty
/// when the scope binds no object or the object is colorless (CR 106.5). Only
/// `ObjectScope::Target` has a printing today; other scopes bind no object.
pub(crate) fn object_colors_for_scope(
    state: &GameState,
    ability: Option<&ResolvedAbility>,
    scope: ObjectScope,
) -> Vec<ManaColor> {
    let obj_id = match scope {
        ObjectScope::Target => ability.and_then(|a| {
            a.targets.iter().find_map(|t| match t {
                TargetRef::Object(id) => Some(*id),
                _ => None,
            })
        }),
        _ => None,
    };
    let Some(obj) = obj_id.and_then(|id| state.objects.get(&id)) else {
        return Vec::new();
    };
    [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ]
    .into_iter()
    .filter(|c| obj.color.contains(c))
    .collect()
}

pub(crate) fn distinct_colors_among_permanents(
    state: &GameState,
    ability: Option<&ResolvedAbility>,
    source_id: crate::types::identifiers::ObjectId,
    filter: &crate::types::ability::TargetFilter,
) -> Vec<crate::types::mana::ManaColor> {
    use crate::game::filter::{matches_target_filter, FilterContext};
    let filter_ctx = match ability {
        Some(a) => FilterContext::from_ability(a),
        None => FilterContext::from_source(state, source_id),
    };
    let zone = filter
        .extract_in_zone()
        .unwrap_or(crate::types::zones::Zone::Battlefield);
    let mut seen: std::collections::HashSet<crate::types::mana::ManaColor> =
        std::collections::HashSet::new();
    for &id in crate::game::targeting::zone_object_ids(state, zone).iter() {
        if !matches_target_filter(state, id, filter, &filter_ctx) {
            continue;
        }
        if let Some(obj) = state.objects.get(&id) {
            for color in &obj.color {
                seen.insert(*color);
            }
        }
    }
    // Stable order for determinism (WUBRG).
    use crate::types::mana::ManaColor;
    [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ]
    .into_iter()
    .filter(|c| seen.contains(c))
    .collect()
}

/// CR 605.1a + CR 406.1 + CR 610.3: Resolve the legal `ManaType` set for a
/// `ChoiceAmongExiledColors` mana ability. Reads `state.exile_links` keyed to the
/// scope, collects the printed colors of every still-exiled linked object, and
/// drops colorless cards (CR 106.5). Shared by the resolver here and by
/// `mana_sources::mana_options_from_production` so cost-payment and direct
/// activation see the same legal set.
pub(crate) fn exiled_color_options(
    state: &GameState,
    scope: LinkedExileScope,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<ManaType> {
    let mut options: Vec<ManaType> = Vec::new();
    for link in &state.exile_links {
        let host_id = match scope {
            LinkedExileScope::ThisObject => source_id,
        };
        if link.source_id != host_id {
            continue;
        }
        let Some(exiled) = state.objects.get(&link.exiled_id) else {
            continue;
        };
        // CR 400.7: Only consider linked cards still in exile (links are pruned
        // from `state.exile_links` when the exiled card leaves exile, but guard
        // defensively in case ordering interleaves).
        if exiled.zone != crate::types::zones::Zone::Exile {
            continue;
        }
        // CR 202.3d + CR 709.4b: a linked exiled card is off the stack, so a split
        // card exposes the combined colors of both halves, not just its front half.
        for color in exiled.effective_colors() {
            let mana_type = mana_color_to_type(&color);
            if !options.contains(&mana_type) {
                options.push(mana_type);
            }
        }
    }
    options
}

pub(crate) fn chosen_color_for_mana(
    state: &GameState,
    source_id: crate::types::identifiers::ObjectId,
) -> Option<ManaColor> {
    state
        .objects
        .get(&source_id)
        .and_then(|obj| obj.chosen_color())
        .or_else(|| {
            state
                .last_named_choice
                .as_ref()
                .and_then(|choice| match choice {
                    ChoiceValue::Color(color) => Some(*color),
                    _ => None,
                })
        })
}

/// Convert a ManaColor to the runtime ManaType.
/// CR 106.1a: There are five colors of mana: white, blue, black, red, and green.
/// CR 106.1b: There are six types of mana: white, blue, black, red, green, and colorless.
fn mana_color_to_type(color: &ManaColor) -> ManaType {
    match color {
        ManaColor::White => ManaType::White,
        ManaColor::Blue => ManaType::Blue,
        ManaColor::Black => ManaType::Black,
        ManaColor::Red => ManaType::Red,
        ManaColor::Green => ManaType::Green,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ChoiceValue, DevotionColors, QuantityExpr,
        QuantityRef, TargetFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_mana_ability(produced: ManaProduction) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Mana {
                produced,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    /// CR 202.3d + CR 709.4b: "add mana of any of the exiled card's colors"
    /// (`ChoiceAmongExiledColors` → `exiled_color_options`) reads a linked exiled
    /// split card's COMBINED colors. Assault // Battery is {R} (front, Red) +
    /// {3}{G} (Green) → colors {Red, Green} off the stack.
    ///
    /// Revert-failing discriminator: reading the front-only `exiled.color` (Red)
    /// omits Green from the options, so the `contains(Green)` assertion fails.
    #[test]
    fn exiled_color_options_use_combined_split_colors() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::game::scenario_db::GameScenarioDbExt;
        use crate::types::game_state::{ExileLink, ExileLinkKind};

        let db = crate::test_support::shared_card_db();
        let mut sc = GameScenario::new();
        let source = sc.add_real_card(P0, "Gray Ogre", Zone::Battlefield, db);
        let exiled = sc.add_real_card(P0, "Assault", Zone::Exile, db);
        let mut state = sc.state;
        state.exile_links.push(ExileLink {
            exiled_id: exiled,
            source_id: source,
            kind: ExileLinkKind::TrackedBySource,
        });

        let options = exiled_color_options(&state, LinkedExileScope::ThisObject, source);
        assert!(
            options.contains(&ManaType::Red) && options.contains(&ManaType::Green),
            "a linked exiled Assault // Battery must expose BOTH Red and Green (its \
             combined split colors); the front-only read would omit Green — got {options:?}"
        );
    }

    #[test]
    fn produce_single_red_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    /// CR 106.4: for "Choose a player. That player adds one mana of any color
    /// they choose" (Spectral Searchlight, Stadium Vendors) the CHOSEN player —
    /// not the controller — is both prompted to pick the color and receives the
    /// mana. Driven through the production `resolve` path (which publishes the
    /// color prompt) into the `handle_choose_mana_effect` completion, so both the
    /// prompted player and the deposit are asserted. Revert-probe: without the
    /// recipient derivation the prompt is directed to P0 and the mana lands in
    /// P0's pool.
    #[test]
    fn chosen_player_mana_prompt_and_deposit_go_to_the_recipient() {
        let mut state = GameState::new_two_player(42);
        // The mana source on the battlefield, controlled by P0.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Spectral Searchlight".to_string(),
            Zone::Battlefield,
        );

        // P0 controls the effect and chose opponent P1 as the recipient.
        let mut ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: Some(ManaTargetRole::Recipient {
                    recipient: TargetFilter::ScopedPlayer,
                }),
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.scoped_player = Some(PlayerId(1));

        // Production path: `resolve` publishes the color prompt to the CHOSEN player.
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let (prompt, ctx_ability) = match &state.waiting_for {
            WaitingFor::ChooseManaColor {
                player,
                choice,
                context,
            } => {
                assert_eq!(
                    *player,
                    PlayerId(1),
                    "the chosen player (not the controller) must be prompted to pick the color"
                );
                let ctx = match context {
                    ManaChoiceContext::ResolvingEffect(a) => (**a).clone(),
                    other => panic!("expected ResolvingEffect context, got {other:?}"),
                };
                (choice.clone(), ctx)
            }
            other => panic!("expected a ChooseManaColor prompt, got {other:?}"),
        };

        // Completion: P1 picks Blue; the mana lands in P1's pool, not P0's.
        let mut events2 = Vec::new();
        handle_choose_mana_effect(
            &mut state,
            &ctx_ability,
            &prompt,
            ManaChoice::SingleColor(ManaType::Blue),
            &mut events2,
        )
        .unwrap();
        assert_eq!(
            state.players[1].mana_pool.count_color(ManaType::Blue),
            1,
            "chosen recipient (P1) must receive the mana"
        );
        assert_eq!(state.players[1].mana_pool.total(), 1);
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "controller (P0) must NOT receive the chosen player's mana"
        );
    }

    /// CR 115.1 + CR 106.4: "Target player adds that much {C}" (Jetfire) — a
    /// genuine `TargetFilter::Player` recipient whose count is NOT target-derived
    /// deposits into the chosen target player (`ability.targets`), not the
    /// controller. Revert-probe: before the `TargetFilter::Player` arm in
    /// `mana_effect_recipient`, the mana lands in P0's pool.
    #[test]
    fn target_player_recipient_deposits_into_the_target_not_controller() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jetfire".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 3 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: Some(ManaTargetRole::Recipient {
                    recipient: TargetFilter::Player,
                }),
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[1].mana_pool.count_color(ManaType::Colorless),
            3,
            "the targeted player (P1) must receive the mana"
        );
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "controller (P0) must NOT receive a targeted recipient's mana"
        );
    }

    /// Matrix row 5 (runtime half) — a CONTEXT-REF recipient plus a REAL count
    /// source. The recipient occupies NO surfaced slot, so the count source
    /// lands at surfaced index 0; naive "recipient == targets[0]" index math
    /// breaks exactly here.
    ///
    /// This is the subject-predicate shape ("That player adds {R} for each card
    /// in target opponent's hand", Blinkmoth Urn's route). CR 106.4: the mana
    /// goes to the scoped player. CR 115.1: the amount is read from the chosen
    /// count-source player's hand — NOT the scoped player's, and NOT the
    /// controller's.
    #[test]
    fn context_ref_recipient_with_real_count_source_reads_the_count_slot() {
        use crate::types::ability::{ManaProduction, ManaTargetRole, ZoneRef};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Subject Led Count Source".to_string(),
            Zone::Battlefield,
        );

        // P1 is BOTH the scoped recipient and the chosen count source here only
        // by construction of the two-player fixture; the discriminating fact is
        // that the count must come from the TARGET slot, so give P1 a hand of a
        // size the controller does not share.
        for i in 0..4 {
            create_object(
                &mut state,
                CardId(50 + i),
                PlayerId(1),
                format!("Count Card {i}"),
                Zone::Hand,
            );
        }

        let mut ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::TargetZoneCardCount {
                            zone: ZoneRef::Hand,
                        },
                    },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: Some(ManaTargetRole::Both {
                    recipient: TargetFilter::ScopedPlayer,
                    count_source: TargetFilter::Player,
                }),
            },
            // ONE target: the count source, at surfaced index 0. The context-ref
            // recipient surfaces no slot and so consumes no target.
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );
        ability.scoped_player = Some(PlayerId(1));

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("subject-led count-source mana resolves");

        assert_eq!(
            state.players[1].mana_pool.count_color(ManaType::Colorless),
            4,
            "CR 106.4 + CR 115.1: the scoped recipient receives mana equal to the \
             COUNT SOURCE slot's hand size"
        );
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "the controller receives nothing"
        );
    }

    /// Matrix row 10a — CR 608.2b clause (1): an illegal RECIPIENT means "that
    /// part of the effect does nothing", and the mana is NOT quietly redirected
    /// to the controller (which would hand a targeted opponent's mana to the
    /// caster). The count source's half is unaffected, and positions are stable.
    ///
    /// The chosen recipient here is the CONTROLLER while the recipient filter
    /// demands an OPPONENT, so the target is illegal at resolution. A
    /// `mana_effect_recipient` that skipped per-slot re-validation would find
    /// P0 and deposit into it — which is exactly what this asserts must not
    /// happen. Reach guard: the paired legal case below deposits a non-zero
    /// amount through the same code path.
    #[test]
    fn illegal_recipient_target_deposits_no_mana_anywhere() {
        use crate::types::ability::{ControllerRef, ManaProduction, ManaTargetRole, TypedFilter};

        let opponent_only =
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));

        let build = |chosen: PlayerId| {
            ResolvedAbility::new(
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 3 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: Some(ManaTargetRole::Recipient {
                        recipient: opponent_only.clone(),
                    }),
                },
                vec![TargetRef::Player(chosen)],
                ObjectId(100),
                PlayerId(0),
            )
        };

        // Reach guard / positive: a LEGAL opponent recipient does receive mana,
        // proving the negative below is not a vacuous "nothing ever resolves".
        let mut state = GameState::new_two_player(42);
        let legal = build(PlayerId(1));
        let mut events = Vec::new();
        crate::game::effects::mana::resolve(&mut state, &legal, &mut events)
            .expect("a legal opponent recipient resolves");
        assert_eq!(
            state.players[1].mana_pool.total(),
            3,
            "reach guard: the legal recipient must actually receive the mana"
        );
        assert_eq!(state.players[0].mana_pool.total(), 0);

        // Negative: the CONTROLLER is not a legal "target opponent".
        let mut state = GameState::new_two_player(42);
        let illegal = build(PlayerId(0));
        let mut events = Vec::new();
        crate::game::effects::mana::resolve(&mut state, &illegal, &mut events)
            .expect("an illegal recipient still resolves the effect, adding nothing");
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "CR 608.2b: an illegal recipient receives nothing and the mana is NOT \
             redirected to the controller"
        );
        assert_eq!(state.players[1].mana_pool.total(), 0);
    }

    /// CR 601.2c + CR 106.4: multi-authority provenance fixture for the two mana
    /// roles. The identical `TargetFilter::Player` is emitted both as a RECIPIENT
    /// (subject-led "target player adds", count target-independent) and as a
    /// COUNT SOURCE ("Add {U} for each card in target player's hand", count =
    /// `TargetZoneCardCount`). The ROLE now STATES which is which — the previous
    /// `mana_count_reads_targets` quantity-shape inference that had to GUESS is
    /// deleted, so its two assertions are deliberately NOT ported. Only the
    /// recipient role redirects the pool; a count-source-only role (and Jeska's
    /// Will's `Typed(Opponent)`) leaves the recipient on the controller. Tested at
    /// the `mana_effect_recipient` seam to avoid coupling to count resolution;
    /// the end-to-end runtime counterparts are
    /// `target_player_recipient_deposits_into_the_target_not_controller` and the
    /// `mana_target_recipient_and_count_source` integration test.
    #[test]
    fn mana_role_separates_recipient_from_count_source() {
        use crate::types::ability::{ControllerRef, TypedFilter, ZoneRef};

        let state = GameState::new_two_player(42);
        let src = ObjectId(100);
        let targets = vec![TargetRef::Player(PlayerId(1))];

        // Count-source production: the count reads a player target.
        let count_source = ManaProduction::AnyOneColor {
            count: QuantityExpr::Ref {
                qty: QuantityRef::TargetZoneCardCount {
                    zone: ZoneRef::Hand,
                },
            },
            color_options: vec![ManaColor::Blue],
            contribution: ManaContribution::Base,
        };

        // Recipient production: a target-independent count ("that much" →
        // EventContextAmount).
        let recipient_prod = ManaProduction::Colorless {
            count: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
        };

        let mk = |produced: ManaProduction, role: ManaTargetRole| {
            ResolvedAbility::new(
                Effect::Mana {
                    produced,
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: Some(role),
                },
                targets.clone(),
                src,
                PlayerId(0),
            )
        };

        // (a) Recipient role, target-independent count → the target (P1).
        let recipient_role = ManaTargetRole::Recipient {
            recipient: TargetFilter::Player,
        };
        let recip = mk(recipient_prod.clone(), recipient_role.clone());
        assert_eq!(
            mana_effect_recipient(&state, &recip, Some(&recipient_role)),
            Some(PlayerId(1)),
            "a Recipient role resolves to its own chosen target"
        );

        // (b) The SAME `TargetFilter::Player`, now declared as a COUNT SOURCE →
        // the controller (P0). This is the case the deleted quantity-shape
        // inference used to guess; the role states it.
        let count_role = ManaTargetRole::CountSource {
            count_source: TargetFilter::Player,
        };
        let cs = mk(count_source.clone(), count_role.clone());
        assert_eq!(
            mana_effect_recipient(&state, &cs, Some(&count_role)),
            Some(PlayerId(0)),
            "a CountSource role declares no recipient → the controller adds the mana"
        );

        // (c) Jeska's Will `Typed(Opponent)` count source → controller (P0).
        let opp = TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
        let jeska_role = ManaTargetRole::CountSource {
            count_source: opp.clone(),
        };
        let jeska = mk(count_source.clone(), jeska_role.clone());
        assert_eq!(
            mana_effect_recipient(&state, &jeska, Some(&jeska_role)),
            Some(PlayerId(0)),
            "Jeska's Will Typed(Opponent) count source → controller (unchanged)"
        );

        // (d) No role at all (Cabal Coffers) → the controller.
        let bare = ResolvedAbility::new(
            Effect::Mana {
                produced: recipient_prod.clone(),
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            src,
            PlayerId(0),
        );
        assert_eq!(
            mana_effect_recipient(&state, &bare, None),
            Some(PlayerId(0)),
            "no declared role → the controller adds the mana"
        );
    }

    /// CR 106.6 + CR 115.1: Jetfire's produced {C} carries the negative spend
    /// restriction "this mana can't be spent to cast nonartifact spells"
    /// (`SpellTypeOrAbilityActivation{ Artifact, Any }`), and it is deposited on
    /// the targeted player's mana units.
    #[test]
    fn target_player_recipient_mana_carries_spend_restriction() {
        use crate::types::mana::{AbilityActivationScope, ManaRestriction};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jetfire".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 2 },
                },
                restrictions: vec![ManaSpendRestriction::SpellTypeOrAbilityActivation {
                    spell_type: "Artifact".to_string(),
                    ability: AbilityActivationScope::Any,
                }],
                grants: vec![],
                expiry: None,
                target: Some(ManaTargetRole::Recipient {
                    recipient: TargetFilter::Player,
                }),
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].mana_pool.total(), 2);
        assert!(
            state.players[1].mana_pool.mana.iter().all(|unit| {
                unit.restrictions
                    .contains(&ManaRestriction::OnlyForTypeSpellsOrAbilities {
                        spell_type: "Artifact".to_string(),
                        ability: AbilityActivationScope::Any,
                    })
            }),
            "each produced {{C}} must carry the artifact-spell spend restriction"
        );
    }

    /// CR 106.1 + CR 106.5 + CR 202.2c: `AnyCombinationOfObjectColors` (Omnath,
    /// Locus of All) draws its colors from the target object. A monocolored
    /// target needs no prompt — `resolve` produces `count` mana of that color
    /// directly; a colorless target produces no mana (CR 106.5). (The multicolor
    /// prompt→produce flow is covered by the `omnath_tests` runtime suite.)
    #[test]
    fn any_combination_of_object_colors_uses_target_colors_and_empty_when_colorless() {
        let mk_ability = |target: ObjectId| {
            ResolvedAbility::new(
                Effect::Mana {
                    produced: ManaProduction::AnyCombinationOfObjectColors {
                        count: QuantityExpr::Fixed { value: 3 },
                        scope: crate::types::ability::ObjectScope::Target,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
                vec![TargetRef::Object(target)],
                ObjectId(100),
                PlayerId(0),
            )
        };

        // Monocolored (Black) target → three black mana, no prompt.
        let mut state = GameState::new_two_player(42);
        let mono = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "B".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&mono).unwrap().color = vec![ManaColor::Black];
        let mut events = Vec::new();
        resolve(&mut state, &mk_ability(mono), &mut events).unwrap();
        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseManaColor { .. }),
            "a single-color object needs no prompt"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 3);
        assert_eq!(state.players[0].mana_pool.total(), 3);

        // CR 106.5: colorless target → no prompt, no mana.
        let mut state2 = GameState::new_two_player(42);
        let colorless = create_object(
            &mut state2,
            CardId(2),
            PlayerId(0),
            "CL".to_string(),
            Zone::Hand,
        );
        state2.objects.get_mut(&colorless).unwrap().color = vec![];
        let mut events2 = Vec::new();
        resolve(&mut state2, &mk_ability(colorless), &mut events2).unwrap();
        assert_eq!(state2.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn firebending_marker_records_firebend_when_mana_is_produced() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Firebender".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().power = Some(4);
        let mut ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: crate::types::ability::ObjectScope::Source,
                        },
                    },
                    color_options: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: Some(crate::types::mana::ManaExpiry::EndOfCombat),
                target: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.may_trigger_origin = Some(MayTriggerOrigin::Keyword {
            keyword: crate::types::keywords::KeywordKind::Firebending,
        });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 4);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::Firebend {
                source_id,
                controller: PlayerId(0)
            } if *source_id == source
        )));
        assert!(state.players[0]
            .bending_types_this_turn
            .contains(&crate::types::events::BendingType::Fire));
    }

    #[test]
    fn firebending_marker_does_not_record_firebend_for_zero_mana() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Firebender".to_string(),
            Zone::Battlefield,
        );
        let mut ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 0 },
                    color_options: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: Some(crate::types::mana::ManaExpiry::EndOfCombat),
                target: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.may_trigger_origin = Some(MayTriggerOrigin::Keyword {
            keyword: crate::types::keywords::KeywordKind::Firebending,
        });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::Firebend { .. })));
    }

    #[test]
    fn produce_multiple_of_same_color() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green, ManaColor::Green, ManaColor::Green],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 3);
    }

    #[test]
    fn produce_event_context_amount_of_one_color() {
        let mut state = GameState::new_two_player(42);
        state.last_effect_count = Some(4);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyOneColor {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                color_options: vec![ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 4);
    }

    #[test]
    fn produce_empty_is_noop() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn produce_multi_color_fixed() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::White, ManaColor::Blue],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn emits_mana_added_per_unit() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Red, ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        let mana_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, GameEvent::ManaAdded { .. }))
            .collect();
        assert_eq!(mana_events.len(), 2);
    }

    #[test]
    fn emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Mana,
                ..
            }
        )));
    }

    #[test]
    fn empty_produced_adds_no_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn mana_units_track_source() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        let unit = &state.players[0].mana_pool.mana[0];
        assert_eq!(unit.source_id, ObjectId(100));
    }

    #[test]
    fn produce_colorless_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 2 },
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            2
        );
    }

    #[test]
    fn any_one_color_effect_prompts_when_multiple_options() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 2 },
                color_options: vec![ManaColor::Blue, ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::SingleColor { options },
                context: ManaChoiceContext::ResolvingEffect(_),
                ..
            } => assert_eq!(options, &[ManaType::Blue, ManaType::Red]),
            other => panic!("expected SingleColor mana choice, got {other:?}"),
        }
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn any_combination_effect_prompts_and_resumes_sub_ability() {
        let mut state = GameState::new_two_player(42);
        let drawn = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Drawn Card".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let mut ability = make_mana_ability(ManaProduction::AnyCombination {
            count: QuantityExpr::Fixed { value: 2 },
            color_options: ManaColor::ALL.to_vec(),
        });
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )));

        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let (choice, pending_effect) = match state.waiting_for.clone() {
            WaitingFor::ChooseManaColor {
                player,
                choice: ManaChoicePrompt::AnyCombination { count, options },
                context: ManaChoiceContext::ResolvingEffect(pending_effect),
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 2);
                assert_eq!(
                    options,
                    vec![
                        ManaType::White,
                        ManaType::Blue,
                        ManaType::Black,
                        ManaType::Red,
                        ManaType::Green,
                    ]
                );
                (
                    ManaChoicePrompt::AnyCombination { count, options },
                    pending_effect,
                )
            }
            other => panic!("expected AnyCombination mana choice, got {other:?}"),
        };
        assert!(state.active_ability_continuation().is_some());

        handle_choose_mana_effect(
            &mut state,
            &pending_effect,
            &choice,
            ManaChoice::Combination(vec![ManaType::Red, ManaType::Green]),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
        assert!(state.players[0].hand.contains(&drawn));
        assert!(state.active_ability_continuation().is_none());
    }

    #[test]
    fn any_combination_effect_rejects_wrong_choice_count() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        let ability = make_mana_ability(ManaProduction::AnyCombination {
            count: QuantityExpr::Fixed { value: 3 },
            color_options: vec![ManaColor::Black, ManaColor::Green],
        });

        resolve(&mut state, &ability, &mut events).unwrap();
        let (choice, pending_effect) = match state.waiting_for.clone() {
            WaitingFor::ChooseManaColor {
                choice,
                context: ManaChoiceContext::ResolvingEffect(pending_effect),
                ..
            } => (choice, pending_effect),
            other => panic!("expected ChooseManaColor, got {other:?}"),
        };

        let result = handle_choose_mana_effect(
            &mut state,
            &pending_effect,
            &choice,
            ManaChoice::Combination(vec![ManaType::Black, ManaType::Green]),
            &mut events,
        );
        assert!(result.is_err());
    }

    #[test]
    fn chosen_color_resolves_from_object_attribute() {
        use crate::types::ability::ChosenAttribute;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let obj_id = ObjectId(100);
        let mut obj = crate::game::game_object::GameObject::new(
            obj_id,
            CardId(1),
            PlayerId(0),
            "Captivating Crossroads".to_string(),
            Zone::Battlefield,
        );
        obj.chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Green));
        state.objects.insert(obj_id, obj);

        let mut events = Vec::new();
        let ability = make_mana_ability(ManaProduction::ChosenColor {
            count: QuantityExpr::Fixed { value: 1 },
            contribution: ManaContribution::Base,
            fixed_alternative: None,
        });
        // Override source_id to match our object
        let ability = ResolvedAbility {
            source_id: obj_id,
            ..ability
        };

        resolve(&mut state, &ability, &mut events).unwrap();

        let player = state.players.iter().find(|p| p.id == PlayerId(0)).unwrap();
        assert_eq!(player.mana_pool.count_color(ManaType::Green), 1);
    }

    #[test]
    fn chosen_color_dynamic_count_reads_current_named_choice() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;
        use crate::types::mana::{ManaCost, ManaCostShard};
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Nyx Lotus".to_string(),
            Zone::Battlefield,
        );
        let permanent = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Green Permanent".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&permanent).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 1,
        };
        state.last_named_choice = Some(ChoiceValue::Color(ManaColor::Green));

        let mut events = Vec::new();
        let ability = ResolvedAbility {
            source_id,
            ..make_mana_ability(ManaProduction::ChosenColor {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Devotion {
                        colors: DevotionColors::ChosenColor,
                    },
                },
                contribution: ManaContribution::Base,
                fixed_alternative: None,
            })
        };

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 2);
    }

    #[test]
    fn chosen_color_unresolved_is_noop() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::ChosenColor {
                count: QuantityExpr::Fixed { value: 1 },
                contribution: ManaContribution::Base,
                fixed_alternative: None,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn chosen_color_count_derivation_independent_of_color() {
        // Issue #482 Defect B: a `ChosenColor` with `fixed_alternative: Some(_)`
        // and no chosen color must still derive `count == 1` — the count
        // derivation cannot depend on a color being resolvable.
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Manor Gate".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility {
            source_id,
            ..make_mana_ability(ManaProduction::ChosenColor {
                count: QuantityExpr::Fixed { value: 1 },
                contribution: ManaContribution::Base,
                fixed_alternative: Some(ManaColor::Green),
            })
        };
        let produced = ManaProduction::ChosenColor {
            count: QuantityExpr::Fixed { value: 1 },
            contribution: ManaContribution::Base,
            fixed_alternative: Some(ManaColor::Green),
        };
        let types = resolve_mana_types_for_ability(&produced, &state, &ability);
        assert_eq!(
            types.len(),
            1,
            "count must derive to 1 even with no chosen color"
        );
        // No-prompt default path produces the fixed color deterministically.
        assert_eq!(types[0], ManaType::Green);
    }

    #[test]
    fn gate_land_mana_ability_offers_fixed_or_chosen() {
        // Issue #482 Defect B: Manor Gate's "{T}: Add {G} or one mana of the
        // chosen color" — once a color (Red) is chosen, the resolver supplied a
        // SingleColor choice must produce exactly the selected color, exactly
        // once, for either option.
        use crate::game::zones::create_object;
        use crate::types::ability::ChosenAttribute;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Manor Gate".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Red));

        let produced = ManaProduction::ChosenColor {
            count: QuantityExpr::Fixed { value: 1 },
            contribution: ManaContribution::Base,
            fixed_alternative: Some(ManaColor::Green),
        };
        let ability = ResolvedAbility {
            source_id,
            ..make_mana_ability(produced.clone())
        };

        // Each choice in the SingleColor prompt yields exactly that color once.
        for chosen in [ManaType::Green, ManaType::Red] {
            let prompt = ManaChoicePrompt::SingleColor {
                options: vec![ManaType::Green, ManaType::Red],
            };
            let types = chosen_mana_types_for_prompt(
                &state,
                &ability,
                &produced,
                &prompt,
                ManaChoice::SingleColor(chosen),
            )
            .unwrap();
            assert_eq!(types, vec![chosen], "chosen color produced exactly once");
        }
    }

    #[test]
    fn opponent_land_colors_produces_from_opponent_lands() {
        // CR 106.7: Mana of any color that a land an opponent controls could produce.
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind};
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // Opponent (PlayerId(1)) has a Mountain on the battlefield with a red mana ability.
        let mountain = create_object(
            &mut state,
            CardId(201),
            PlayerId(1),
            "Mountain".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&mountain).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Mountain".to_string());
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Red],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            }),
            &mut events,
        )
        .unwrap();

        // Should produce red mana (from opponent's Mountain).
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    /// CR 106.7 (issue #1556): Exotic Orchard — "Add one mana of any color that a
    /// land an opponent controls could produce." When the opponent's lands could
    /// produce more than one color, the activator must be prompted to choose,
    /// not silently handed the first color. Mirrors `AnyTypeProduceableBy`
    /// (Reflecting Pool) prompt behavior.
    #[test]
    fn opponent_land_colors_prompts_choice_when_multiple_colors_available() {
        let mut state = GameState::new_two_player(42);

        // Player 0 controls the Exotic Orchard — the prompt reads the source's controller.
        let orchard = create_object(
            &mut state,
            CardId(401),
            PlayerId(0),
            "Exotic Orchard".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&orchard)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        // Opponent (PlayerId(1)) controls a Mountain (red) and a Forest (green).
        for (cid, name, color, sub) in [
            (402u64, "Mountain", ManaColor::Red, "Mountain"),
            (403u64, "Forest", ManaColor::Green, "Forest"),
        ] {
            let land = create_object(
                &mut state,
                CardId(cid),
                PlayerId(1),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push(sub.to_string());
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![color],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::OpponentLandColors {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            orchard,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The activator must be asked which color — not handed the first one.
        match &state.waiting_for {
            crate::types::game_state::WaitingFor::ChooseManaColor {
                choice: crate::types::game_state::ManaChoicePrompt::SingleColor { options },
                ..
            } => {
                assert!(options.contains(&ManaType::Red), "red should be offered");
                assert!(
                    options.contains(&ManaType::Green),
                    "green should be offered"
                );
                assert_eq!(options.len(), 2);
            }
            other => panic!("expected a ChooseManaColor SingleColor prompt, got {other:?}"),
        }
        // No mana enters the pool until the choice is made.
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn opponent_land_colors_no_opponent_lands_produces_nothing() {
        // CR 106.5 + CR 106.7: If no color can be defined, produce no mana.
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn opponent_land_colors_mirror_exotic_orchard_no_recursion() {
        // CR 106.7: Two opposing Exotic Orchards with no other lands —
        // neither can define a color, so both produce no mana (no infinite recursion).
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind};
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // Opponent (PlayerId(1)) has an Exotic Orchard (OpponentLandColors ability).
        let opp_orchard = create_object(
            &mut state,
            CardId(301),
            PlayerId(1),
            "Exotic Orchard".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&opp_orchard).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::OpponentLandColors {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        // Player 0 activates their own OpponentLandColors ability.
        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            }),
            &mut events,
        )
        .unwrap();

        // No recursion; opponent's Exotic Orchard is skipped, so no colors available.
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn restriction_spell_type_attaches_to_produced_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![ManaSpendRestriction::SpellType("Creature".to_string())],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let unit = &state.players[0].mana_pool.mana[0];
        assert_eq!(unit.restrictions.len(), 1);
        assert_eq!(
            unit.restrictions[0],
            ManaRestriction::OnlyForSpellType("Creature".to_string())
        );
    }

    #[test]
    fn restriction_chosen_creature_type_resolves_from_source() {
        use crate::types::ability::ChosenAttribute;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let obj_id = ObjectId(200);
        let mut obj = crate::game::game_object::GameObject::new(
            obj_id,
            CardId(2),
            PlayerId(0),
            "Cavern of Souls".to_string(),
            Zone::Battlefield,
        );
        obj.chosen_attributes
            .push(ChosenAttribute::CreatureType("Elf".to_string()));
        state.objects.insert(obj_id, obj);

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![ManaSpendRestriction::ChosenCreatureType],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let unit = &state.players[0].mana_pool.mana[0];
        assert_eq!(unit.restrictions.len(), 1);
        assert_eq!(
            unit.restrictions[0],
            ManaRestriction::OnlyForCreatureType("Elf".to_string())
        );
    }

    #[test]
    fn restriction_chosen_creature_type_drops_when_no_choice() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![ManaSpendRestriction::ChosenCreatureType],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // No source object → restriction can't resolve → mana is unrestricted
        let unit = &state.players[0].mana_pool.mana[0];
        assert!(unit.restrictions.is_empty());
    }

    #[test]
    fn grants_flow_through_to_mana_unit() {
        use crate::types::mana::ManaSpellGrant;

        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![ManaSpellGrant::CantBeCountered],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let unit = &state.players[0].mana_pool.mana[0];
        assert_eq!(unit.grants, vec![ManaSpellGrant::CantBeCountered]);
    }

    /// CR 106.7 + CR 106.1b: Reflecting Pool — produces one mana of any type
    /// that a land you control could produce. With a Plains and a Swamp on the
    /// battlefield, the type union is {W, B}; the resolver picks the first
    /// listed type when no choice override is supplied (mirrors `AnyOneColor`).
    #[test]
    fn any_type_produceable_by_you_control_unions_types() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // Player 0 controls a Plains and a Swamp.
        for (card_id, name, color, subtype) in [
            (CardId(401), "Plains", ManaColor::White, "Plains"),
            (CardId(402), "Swamp", ManaColor::Black, "Swamp"),
        ] {
            let id = create_object(
                &mut state,
                card_id,
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push(subtype.to_string());
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![color],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        let land_filter = TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));
        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyTypeProduceableBy {
                count: QuantityExpr::Fixed { value: 1 },
                land_filter,
            }),
            &mut events,
        )
        .unwrap();

        // CR 106.7: Per-unit `first()` selection out of the type union — the
        // union is order-dependent on object iteration, so we assert that the
        // produced mana is one of the two valid contributing types (W or B)
        // rather than pinning to a single iteration order.
        assert_eq!(state.players[0].mana_pool.total(), 1);
        let white = state.players[0].mana_pool.count_color(ManaType::White);
        let black = state.players[0].mana_pool.count_color(ManaType::Black);
        assert_eq!(
            white + black,
            1,
            "produced mana must come from the {{W,B}} type union (got W={white}, B={black})"
        );

        // The full type union (helper-level) must include both colors.
        let options = crate::game::mana_sources::produceable_mana_types_by_filter(
            &state,
            &TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You)),
            PlayerId(0),
            ObjectId(100),
        );
        assert!(options.contains(&ManaType::White), "union must include W");
        assert!(options.contains(&ManaType::Black), "union must include B");
    }

    /// CR 106.5 + CR 106.7: When no land matches the filter, the type union is
    /// empty, so the ability produces no mana.
    #[test]
    fn any_type_produceable_by_empty_union_produces_nothing() {
        use crate::types::ability::{ControllerRef, TargetFilter, TypedFilter};

        let mut state = GameState::new_two_player(42);
        let land_filter = TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyTypeProduceableBy {
                count: QuantityExpr::Fixed { value: 1 },
                land_filter,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    /// CR 106.7: Two Reflecting Pools facing each other (no other lands) — the
    /// recursive `AnyTypeProduceableBy` skip prevents infinite recursion and
    /// the union collapses to empty (CR 106.5 — no mana).
    #[test]
    fn any_type_produceable_by_recursive_yields_empty() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let recursive_filter =
            TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));

        // Player 0 has a Reflecting Pool already on the battlefield.
        let pool = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Reflecting Pool".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&pool).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyTypeProduceableBy {
                        count: QuantityExpr::Fixed { value: 1 },
                        land_filter: recursive_filter.clone(),
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyTypeProduceableBy {
                count: QuantityExpr::Fixed { value: 1 },
                land_filter: recursive_filter,
            }),
            &mut events,
        )
        .unwrap();

        // Both producers are recursive; no other lands → empty union → no mana.
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    /// CR 106.1b: Reflecting Pool reads "any **type**" — a Wastes you control
    /// (which produces colorless) must contribute `Colorless` to the union.
    #[test]
    fn any_type_produceable_by_includes_colorless() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // Player 0 controls a Wastes (produces {C}).
        let wastes = create_object(
            &mut state,
            CardId(601),
            PlayerId(0),
            "Wastes".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&wastes).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let land_filter = TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));
        let options = crate::game::mana_sources::produceable_mana_types_by_filter(
            &state,
            &land_filter,
            PlayerId(0),
            ObjectId(9999),
        );
        assert!(
            options.contains(&ManaType::Colorless),
            "type union must include Colorless when a Wastes is controlled (CR 106.1b)"
        );
    }

    /// CR 106.7 + CR 106.5: P0 controls Exotic Orchard (`OpponentLandColors`),
    /// P1 controls Reflecting Pool (`AnyTypeProduceableBy`), and neither
    /// player controls any other land. The mutual recursion guard must be
    /// symmetric — `opponent_land_color_options` skips both recursive
    /// producers, so the survey terminates with the empty set rather than
    /// re-anchoring `ControllerRef::You` to the wrong player or looping.
    /// Activating either side produces no mana per CR 106.5.
    #[test]
    fn exotic_orchard_with_opponent_reflecting_pool_no_panic() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // P0 controls Exotic Orchard.
        let orchard = create_object(
            &mut state,
            CardId(701),
            PlayerId(0),
            "Exotic Orchard".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&orchard).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::OpponentLandColors {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        // P1 controls Reflecting Pool.
        let pool = create_object(
            &mut state,
            CardId(702),
            PlayerId(1),
            "Reflecting Pool".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&pool).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyTypeProduceableBy {
                        count: QuantityExpr::Fixed { value: 1 },
                        land_filter: TargetFilter::Typed(
                            TypedFilter::land().controller(ControllerRef::You),
                        ),
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        // P0's Exotic Orchard surveys P1's lands → only finds Reflecting Pool
        // (recursive — skipped) → empty set.
        let orchard_opts =
            crate::game::mana_sources::opponent_land_color_options(&state, PlayerId(0));
        assert!(
            orchard_opts.is_empty(),
            "Exotic Orchard facing only an opponent's Reflecting Pool must yield empty (CR 106.5); got {orchard_opts:?}"
        );

        // P1's Reflecting Pool surveys P1's lands → only itself (recursive,
        // skipped) → empty set. (Cross-controller cycle terminates cleanly.)
        let pool_opts = crate::game::mana_sources::produceable_mana_types_by_filter(
            &state,
            &TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You)),
            PlayerId(1),
            pool,
        );
        assert!(
            pool_opts.is_empty(),
            "Reflecting Pool with no other own lands must yield empty (CR 106.5); got {pool_opts:?}"
        );

        // Both should activate without panic and produce zero mana.
        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            }),
            &mut events,
        )
        .unwrap();
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn colorless_production_counts_creatures_sharing_type_with_triggering_source() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            ControllerRef, FilterProp, QuantityExpr, QuantityRef, SharedQuality,
            SharedQualityRelation, TypedFilter,
        };
        use crate::types::events::GameEvent;
        use crate::types::game_state::ZoneChangeRecord;
        use crate::types::identifiers::CardId;
        use crate::types::player::PlayerId;

        let mut state = GameState::new_two_player(42);
        state.all_creature_types = vec!["Goblin".to_string()];
        let mana_echoes = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mana Echoes".to_string(),
            Zone::Battlefield,
        );
        let goblin_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Goblin A".to_string(),
            Zone::Battlefield,
        );
        let goblin_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Goblin B".to_string(),
            Zone::Battlefield,
        );
        let entering = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Goblin C".to_string(),
            Zone::Battlefield,
        );
        for id in [goblin_a, goblin_b, entering] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Goblin".to_string());
        }

        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: entering,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord::test_minimal(
                entering,
                Some(Zone::Hand),
                Zone::Battlefield,
            )),
        });

        let filter = TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::SharesQuality {
                    quality: SharedQuality::CreatureType,
                    reference: Some(Box::new(TargetFilter::TriggeringSource)),
                    relation: SharedQualityRelation::Shares,
                }]),
        );
        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: filter.clone(),
                        },
                    },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            mana_echoes,
            PlayerId(0),
        );

        let produced = super::resolve_mana_types_for_ability(
            &ManaProduction::Colorless {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { filter },
                },
            },
            &state,
            &ability,
        );
        assert_eq!(
            produced.len(),
            3,
            "Mana Echoes must produce one colorless per matching creature"
        );
    }
}
