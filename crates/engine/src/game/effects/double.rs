use crate::game::mana_sources::mana_color_to_type;
use crate::types::ability::{
    DoubleTarget, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::counter::CounterType;
use crate::types::events::{GameEvent, ManaTapState};
use crate::types::game_state::{GameState, PendingCounterAddition, PendingEffectResolved};
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaColor, ManaType, ManaUnit};
use crate::types::player::PlayerId;

/// CR 701.10d-f: Double counters on a permanent, a player's life total, or mana pool.
/// Dispatches on `DoubleTarget` variant.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::Double {
        target_kind,
        target,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam("expected Double effect".into()));
    };

    match target_kind {
        DoubleTarget::Counters { counter_type } => {
            resolve_double_counters(state, ability, events, target, counter_type.as_ref())
        }
        DoubleTarget::LifeTotal => resolve_double_life(state, ability, events, target),
        DoubleTarget::ManaPool { color } => {
            resolve_double_mana(state, ability, events, target, color.as_ref())
        }
    }
}

/// CR 701.10e: Double the number of a kind of counter (or all kinds) on target permanent(s).
fn resolve_double_counters(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    target: &TargetFilter,
    counter_type: Option<&CounterType>,
) -> Result<(), EffectError> {
    // CR 608.2c + CR 603.7c: the ordinary dispatch first — SelfRef,
    // context anaphors and chosen targets all bind here. `resolve_object_targets`
    // delegates to `targeting::resolved_targets`' unified 3-tier dispatch.
    let mut obj_ids = resolve_object_targets(ability, target, state);
    // CR 701.10e + CR 608.2d: only when nothing was chosen or bound is this a
    // DESCRIBED population ("each Spider and legendary creature you control").
    // The shared helper's gate refuses every targeted shape (CR 601.2c +
    // CR 608.2b) and every unclassifiable filter (CR 115.1). In particular it
    // refuses Zimone, Paradox Sculptor's `MultiTargetSpec { min: 0, max: 2 }`
    // when the controller announced zero targets — see the apply()-level test
    // below.
    if obj_ids.is_empty() {
        if let Some(population) =
            super::counters::nontargeted_counter_population_ids(state, ability, target)
        {
            obj_ids = population;
        }
    }
    let mut additions = Vec::new();

    for obj_id in obj_ids {
        // Snapshot current counters to avoid borrow issues
        let counters_snapshot: Vec<(crate::types::counter::CounterType, u32)> = {
            let obj = state
                .objects
                .get(&obj_id)
                .ok_or(EffectError::ObjectNotFound(obj_id))?;
            if let Some(ct) = counter_type {
                // CR 701.10e: Double only the specified counter type
                let count = obj.counters.get(ct).copied().unwrap_or(0);
                if count > 0 {
                    vec![(ct.clone(), count)]
                } else {
                    vec![]
                }
            } else {
                // CR 701.10e: Double each kind of counter on the permanent
                obj.counters
                    .iter()
                    .filter(|(_, &count)| count > 0)
                    .map(|(ct, &count)| (ct.clone(), count))
                    .collect()
            }
        };

        // CR 701.10e: Add N more of each counter type where N = current count.
        // CR 614.1: doubling is a "put counters" event, so route it through the
        // AddCounter replacement pipeline (Doubling Season / Vorinclex / Hardened
        // Scales / counter prevention), matching the specific-type
        // `MultiplyCounter` path (`counters::resolve_multiply`). The raw
        // `apply_counter_addition` primitive bypassed replacements.
        for (ct, current_count) in counters_snapshot {
            additions.push(PendingCounterAddition::Object {
                actor: ability.controller,
                object_id: obj_id,
                counter_type: ct,
                count: current_count,
            });
        }
    }

    let completion = PendingEffectResolved::new(EffectKind::Double, ability.source_id);
    for (index, addition) in additions.iter().cloned().enumerate() {
        let PendingCounterAddition::Object {
            actor,
            object_id,
            counter_type,
            count,
        } = addition
        else {
            continue;
        };
        if !super::counters::add_counter_with_replacement(
            state,
            actor,
            object_id,
            counter_type,
            count,
            events,
        ) {
            super::counters::stash_pending_counter_additions(
                state,
                additions[index + 1..].to_vec(),
                completion,
            );
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Double,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 701.10d: Double a player's life total.
/// If life > 0: gain life equal to current total (new total = 2x).
/// If life < 0: lose life equal to |current total| (new total = 2x negative).
/// If life == 0: no change.
///
/// Routes the gain/loss through `apply_life_gain` / `apply_life_loss`
/// so the same replacement-pipeline and can't-gain / can't-lose short-circuits
/// that govern all other life-change events apply here too (CR 119.7 + 119.8).
fn resolve_double_life(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    target: &TargetFilter,
) -> Result<(), EffectError> {
    let player_id = resolve_player_target(ability, target);

    let current_life = state
        .players
        .iter()
        .find(|p| p.id == player_id)
        .ok_or(EffectError::PlayerNotFound)?
        .life;

    if current_life > 0 {
        // CR 701.10d: Gain life equal to current total.
        if crate::game::effects::life::apply_life_gain(
            state,
            player_id,
            current_life as u32,
            events,
        )
        .is_err()
        {
            return Ok(());
        }
    } else if current_life < 0 {
        // CR 701.10d: Lose |current_life| additional life so the new total is 2x.
        if crate::game::effects::life::apply_life_loss(
            state,
            player_id,
            (-current_life) as u32,
            events,
        )
        .is_err()
        {
            return Ok(());
        }
    }
    // life == 0: no change.

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Double,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 701.10f: Double the amount of a type of mana in a player's mana pool.
fn resolve_double_mana(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    target: &TargetFilter,
    color: Option<&ManaColor>,
) -> Result<(), EffectError> {
    let player_id = resolve_player_target(ability, target);

    // Collect the mana types and counts to add
    let mana_to_add: Vec<(ManaType, usize)> = {
        let player = state
            .players
            .iter()
            .find(|p| p.id == player_id)
            .ok_or(EffectError::PlayerNotFound)?;

        if let Some(c) = color {
            let mt = mana_color_to_type(c);
            let count = player.mana_pool.count_color(mt);
            if count > 0 {
                vec![(mt, count)]
            } else {
                vec![]
            }
        } else {
            // All colors
            ManaColor::ALL
                .iter()
                .map(|c| {
                    let mt = mana_color_to_type(c);
                    (mt, player.mana_pool.count_color(mt))
                })
                .filter(|(_, count)| *count > 0)
                .collect()
        }
    };

    // CR 701.10f: Add equal amount of each mana type
    if !state.players.iter().any(|p| p.id == player_id) {
        return Err(EffectError::PlayerNotFound);
    }

    for (mana_type, count) in mana_to_add {
        for _ in 0..count {
            // CR 118.3a: stamp a pip id on pool entry so the unit can be pinned.
            let _ = state.add_mana_to_pool(
                player_id,
                ManaUnit {
                    color: mana_type,
                    source_id: ability.source_id,
                    pip_id: crate::types::mana::ManaPipId(0),
                    supertype: None,
                    source_could_produce_two_or_more_colors: false,
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                },
            );

            events.push(GameEvent::ManaAdded {
                player_id,
                mana_type,
                source_id: ability.source_id,
                tap_state: ManaTapState::NotFromTap,
            });
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Double,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// Resolve object targets from ability targets or self-ref.
///
/// CR 608.2c + 603.10a: Delegates to the unified 3-tier dispatch
/// (`targeting::resolved_targets`) so `SelfRef` always resolves to the source
/// object regardless of `ability.targets` (issue #323 class — chained
/// `Double { target: SelfRef }` sub-abilities would otherwise inherit the
/// parent's targets via chain propagation in
/// `effects::mod.rs::resolve_ability_chain`). `None` falls back to the
/// source only when `ability.targets` is empty.
fn resolve_object_targets(
    ability: &ResolvedAbility,
    target: &TargetFilter,
    state: &GameState,
) -> Vec<ObjectId> {
    let effective_targets = crate::game::targeting::resolved_targets(ability, target, state);
    super::effect_object_targets(target, &effective_targets)
}

/// Resolve a player target from the ability.
fn resolve_player_target(ability: &ResolvedAbility, target: &TargetFilter) -> PlayerId {
    match target {
        TargetFilter::Controller | TargetFilter::SelfRef => ability.controller,
        _ => ability
            .targets
            .iter()
            .find_map(|t| {
                if let TargetRef::Player(pid) = t {
                    Some(*pid)
                } else {
                    None
                }
            })
            .unwrap_or(ability.controller),
    }
}

#[cfg(test)]
mod tests {

    /// CR 115.10a: the non-targeted population tier is gated on
    /// `TargetChoiceTiming::Resolution` — the stamp
    /// `lower::target_choice_timing_for_clause` puts on a clause whose recipient
    /// carries no literal "target". A hand-built `ResolvedAbility` defaults to
    /// `Stack` (`ResolvedAbility::new`), which the real parser never produces for
    /// a descriptor population, so these resolver-level fixtures set it
    /// explicitly. Without it every refusal below would be attributable to the
    /// TIMING conjunct rather than to the conjunct actually under test, and the
    /// negatives would pass for the wrong reason.
    fn resolution_timed(
        effect: Effect,
        targets: Vec<TargetRef>,
        source: ObjectId,
        controller: PlayerId,
    ) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(effect, targets, source, controller);
        ability.target_choice_timing = crate::types::ability::TargetChoiceTiming::Resolution;
        ability
    }
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::ability::{
        AbilityKind, QuantityModification, ReplacementDefinition, SpellContext, TypedFilter,
    };
    use crate::types::counter::CounterType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    fn make_double_ability(
        target_kind: DoubleTarget,
        target: TargetFilter,
        controller: PlayerId,
        targets: Vec<TargetRef>,
    ) -> ResolvedAbility {
        ResolvedAbility {
            detached_remainder: crate::types::ability::DetachedRemainder::NoProducer,
            effect: Effect::Double {
                target_kind,
                target,
            },
            controller,
            original_controller: None,
            scoped_player: None,
            target_chooser: None,
            source_id: ObjectId(100),
            cast_occurrence: None,
            source_incarnation: None,
            trigger_source: None,
            trigger_definition_ref: None,
            force_block_attacker: None,
            target_incarnations: Vec::new(),
            selected_target_incarnations: Vec::new(),
            targets,
            kind: AbilityKind::Spell,
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: SpellContext::default(),
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            noted_mana_payment: None,
            cost_paid_object_ids: Vec::new(),
            effect_context_object: None,
            amassed_army_object: None,
            ability_index: None,
            may_trigger_origin: None,
            optional_targeting: false,
            optional: false,
            optional_player: None,
            optional_for: None,
            multi_target: None,
            target_constraints: Vec::new(),
            target_choice_timing: crate::types::ability::TargetChoiceTiming::Stack,
            description: None,
            selected_mode_labels: Vec::new(),
            modal_instruction_ordinal: None,
            repeat_for: None,
            min_x_value: 0,
            announced_x: None,
            cant_be_copied: false,
            copy_count_status: crate::types::ability::CopyCountStatus::Pending,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            distribute: None,
            target_selection_mode: crate::types::ability::TargetSelectionMode::Chosen,
            chosen_players: Vec::new(),
            repeat_until: None,
            replacement_applied: Default::default(),
            sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
            sibling_condition: crate::types::ability::SiblingCondition::Dependent,
            modal: None,
            mode_abilities: vec![],
            parent_target_missing_reason: None,
        }
    }

    /// Zimone, Paradox Sculptor's real activated line, committed verbatim at
    /// `crates/engine/src/parser/swallow_check.rs`.
    const ZIMONE_ACTIVATED_LINE: &str = "{G}{U}, {T}: Double the number of each kind of counter \
        on up to two target creatures and/or artifacts you control.";

    /// Two creatures for P0 and one for P1 on a real battlefield. P0's first
    /// creature carries a second counter KIND so the untyped "each kind" semantics
    /// are observable.
    fn double_counter_board() -> (GameState, [ObjectId; 3]) {
        use crate::game::zones::create_object;
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let ids = [
            create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Mine A".to_string(),
                Zone::Battlefield,
            ),
            create_object(
                &mut state,
                CardId(2),
                PlayerId(0),
                "Mine B".to_string(),
                Zone::Battlefield,
            ),
            create_object(
                &mut state,
                CardId(3),
                PlayerId(1),
                "Theirs".to_string(),
                Zone::Battlefield,
            ),
        ];
        for id in ids {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Plus1Plus1, 2);
        }
        state
            .objects
            .get_mut(&ids[0])
            .unwrap()
            .counters
            .insert(CounterType::Lore, 3);
        (state, ids)
    }

    fn plus1(state: &GameState, id: ObjectId) -> u32 {
        state.objects[&id]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    }

    /// V3b — CR 601.2c + CR 608.2b, through `apply()`: an "up to two target ..."
    /// counter doubling announced with ZERO targets affects nothing.
    ///
    /// GUARD-FAILING, NOT REVERT-FAILING, and that is stated rather than hidden:
    /// before this change `Effect::Double { Counters }` had no mass tier at all,
    /// so zero announced targets already did nothing. This row exists because
    /// `resolve_double_counters` now FALLS THROUGH to
    /// `counters::nontargeted_counter_population_ids`, and it proves end-to-end —
    /// through cost payment, target announcement and resolution — that declining
    /// every optional target changes nothing on a real board.
    ///
    /// What it does NOT do, stated so the next editor does not miscredit it: it
    /// does not discriminate the gate's `multi_target` conjunct. Zimone's clause
    /// contains the literal word "target", so the clause lowers `Stack` and the
    /// gate's LEADING `target_choice_timing == Resolution` conjunct refuses before
    /// `multi_target` is ever consulted. The rows that actually discriminate
    /// `multi_target` / `optional_targeting` are the `resolution_timed`
    /// resolver-level tests in this module and in `game/effects/counters.rs`.
    #[test]
    fn zimone_paradox_sculptor_zero_announced_targets_double_counters_affects_nothing_through_apply(
    ) {
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::ability::MultiTargetSpec;
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::phase::Phase;

        fn board() -> (crate::game::scenario::GameRunner, [ObjectId; 4]) {
            let mut scenario = GameScenario::new_n_player(2, 42);
            scenario.at_phase(Phase::PreCombatMain);
            let src = {
                let mut builder = scenario.add_creature_from_oracle(
                    P0,
                    "Zimone, Paradox Sculptor",
                    2,
                    3,
                    ZIMONE_ACTIVATED_LINE,
                );
                builder.as_legendary();
                builder.id()
            };
            let mine_a = scenario.add_creature(P0, "Mine A", 1, 1).id();
            let mine_b = scenario.add_creature(P0, "Mine B", 1, 1).id();
            let theirs = scenario.add_creature(P1, "Theirs", 1, 1).id();
            scenario.with_counter(src, CounterType::Plus1Plus1, 1);
            scenario.with_counter(mine_a, CounterType::Plus1Plus1, 2);
            scenario.with_counter(mine_a, CounterType::Lore, 3);
            scenario.with_counter(mine_b, CounterType::Plus1Plus1, 4);
            scenario.with_counter(theirs, CounterType::Plus1Plus1, 7);
            // CR 602.1a: {G}{U} for the activation cost (everything before the colon).
            scenario.with_mana_pool(
                P0,
                vec![
                    ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
                    ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
                ],
            );
            (scenario.build(), [src, mine_a, mine_b, theirs])
        }

        // SHAPE REACH-GUARD: the line really is an optional min-0 target set over
        // a two-leg "you control" union, or the zero-target claim is vacuous.
        let (runner, [src, mine_a, mine_b, theirs]) = board();
        let ability = &runner.state().objects[&src].abilities[0];
        assert_eq!(
            ability.multi_target,
            Some(MultiTargetSpec::fixed(0, 2)),
            "\"up to two target\" must lower to MultiTargetSpec{{min:0,max:2}}"
        );
        assert!(
            matches!(
                &*ability.effect,
                Effect::Double {
                    target_kind: DoubleTarget::Counters { counter_type: None },
                    target: TargetFilter::Or { .. },
                }
            ),
            "expected an untyped counter doubling over an Or union, got {:?}",
            ability.effect
        );

        // --- Zero announced targets: nothing may change.
        let mut runner = runner;
        let outcome = runner.activate(src, 0).resolve();
        assert!(
            matches!(
                outcome.final_waiting_for(),
                crate::types::game_state::WaitingFor::Priority { .. }
            ),
            "the activation must halt at a clean priority window, got {:?}",
            outcome.final_waiting_for()
        );
        assert_eq!(outcome.stack_size(), 0, "the ability must have resolved");
        let state = outcome.state();
        assert_eq!(plus1(state, src), 1, "the source is untouched");
        assert_eq!(plus1(state, mine_a), 2, "zero targets ⇒ Mine A untouched");
        assert_eq!(
            state.objects[&mine_a]
                .counters
                .get(&CounterType::Lore)
                .copied()
                .unwrap_or(0),
            3,
            "zero targets ⇒ Mine A's Lore counters untouched"
        );
        assert_eq!(plus1(state, mine_b), 4, "zero targets ⇒ Mine B untouched");
        assert_eq!(plus1(state, theirs), 7, "the opponent is untouched");

        // --- PAIRED POSITIVE REACH-GUARD, same test: one declared target is
        // doubled in EVERY counter kind, and nothing else moves. This also pins
        // the untyped "each kind" semantics that `MultiplyCounter` cannot express.
        let (runner, [src, mine_a, mine_b, theirs]) = board();
        let mut runner = runner;
        let outcome = runner.activate(src, 0).target_object(mine_a).resolve();
        let state = outcome.state();
        assert_eq!(
            plus1(state, mine_a),
            4,
            "the declared target's +1/+1: 2 → 4"
        );
        assert_eq!(
            state.objects[&mine_a]
                .counters
                .get(&CounterType::Lore)
                .copied()
                .unwrap_or(0),
            6,
            "CR 701.10e: EACH KIND — the declared target's Lore counters: 3 → 6"
        );
        assert_eq!(
            plus1(state, mine_b),
            4,
            "an undeclared creature is untouched"
        );
        assert_eq!(plus1(state, theirs), 7, "the opponent is untouched");
    }

    /// The new fall-through's positive path: a genuinely non-targeted
    /// `Double { Counters }` enumerates the described battlefield population
    /// (CR 608.2d) and doubles every kind of counter on it.
    ///
    /// REVERT-FAILING: before this change `Effect::Double` had no mass tier, so a
    /// descriptor population doubled nothing at all.
    #[test]
    fn double_counters_nontargeted_population_enumerates_matching_permanents() {
        let (mut state, ids) = double_counter_board();
        let ability = resolution_timed(
            Effect::Double {
                target_kind: DoubleTarget::Counters { counter_type: None },
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(crate::types::ability::ControllerRef::You),
                ),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        assert_eq!(plus1(&state, ids[0]), 4, "a controlled creature is doubled");
        assert_eq!(
            state.objects[&ids[0]].counters[&CounterType::Lore],
            6,
            "CR 701.10e: every kind of counter is doubled, not just +1/+1"
        );
        assert_eq!(plus1(&state, ids[1]), 4, "a controlled creature is doubled");
        assert_eq!(
            plus1(&state, ids[2]),
            2,
            "`controller: You` keeps the opponent out of the population"
        );
    }

    /// V3c — the gate's `optional_targeting` conjunct (CR 608.2b) on the untyped
    /// half. Resolver-level: no Oracle text produces `optional_targeting: true` on
    /// a counter multiplication, so this conjunct has no `apply()` reachability;
    /// the `multi_target` sibling is exercised end-to-end above.
    #[test]
    fn double_counters_optional_targeting_zero_target_affects_nothing() {
        use crate::types::ability::MultiTargetSpec;

        let effect = || Effect::Double {
            target_kind: DoubleTarget::Counters { counter_type: None },
            target: TargetFilter::Typed(
                TypedFilter::creature().controller(crate::types::ability::ControllerRef::You),
            ),
        };

        let (mut state, ids) = double_counter_board();
        let mut ability = resolution_timed(effect(), vec![], ObjectId(100), PlayerId(0));
        ability.optional_targeting = true;
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        for id in ids {
            assert_eq!(
                plus1(&state, id),
                2,
                "an optionally-targeted ability with no chosen target affects nothing"
            );
        }

        let mut ability = resolution_timed(effect(), vec![], ObjectId(100), PlayerId(0));
        ability.multi_target = Some(MultiTargetSpec::fixed(0, 2));
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        for id in ids {
            assert_eq!(
                plus1(&state, id),
                2,
                "min-0 multi-target refuses identically"
            );
        }

        // PAIRED POSITIVE, same test: with neither conjunct the tier is live.
        let ability = resolution_timed(effect(), vec![], ObjectId(100), PlayerId(0));
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        assert_eq!(plus1(&state, ids[0]), 4);
        assert_eq!(plus1(&state, ids[1]), 4);
        assert_eq!(plus1(&state, ids[2]), 2);
    }

    /// V4's hostile row on the untyped half (CR 115.1 + CR 608.2c): the new mass
    /// fall-through must never rescue a recipient into a battlefield sweep.
    ///
    /// Two hostile classes, each with its own mechanism:
    ///  * a CONTEXT REF (`SelfRef`) never reaches the helper at all:
    ///    `resolve_object_targets` binds it in the ordinary dispatch above, so
    ///    `obj_ids` is non-empty and the mass fall-through is skipped. It doubles
    ///    exactly the source and nothing else, even when the source is outside the
    ///    population the surrounding board would offer. (The helper's own
    ///    `is_context_ref()` conjunct is a second, unreachable guard on this path —
    ///    the sibling comment in `game/effects/counters.rs` describes the same
    ///    mechanism.);
    ///  * a recipient the parser could not classify (a contentless `Typed`, or
    ///    `TargetFilter::Any`) is refused by `names_enumerable_population()` —
    ///    without it, both match EVERY object and would double every counter on
    ///    every permanent BOTH players control.
    #[test]
    fn double_counters_context_ref_recipient_does_not_sweep_battlefield() {
        // `ids[2]` is the OPPONENT's creature, so a `controller: You` population
        // built from this ability would not contain it: if the source is doubled,
        // it can only be the SelfRef tier that bound it.
        let (mut state, ids) = double_counter_board();
        let ability = ResolvedAbility::new(
            Effect::Double {
                target_kind: DoubleTarget::Counters { counter_type: None },
                target: TargetFilter::SelfRef,
            },
            vec![],
            ids[2],
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        assert_eq!(
            plus1(&state, ids[2]),
            4,
            "SelfRef binds the source through its own tier"
        );
        assert_eq!(
            plus1(&state, ids[0]),
            2,
            "a context ref must not fall through to a battlefield sweep"
        );
        assert_eq!(plus1(&state, ids[1]), 2);
    }

    /// The other half of V4's hostile row: a recipient that names no enumerable
    /// population doubles nothing, on the untyped form.
    #[test]
    fn double_counters_unenumerable_population_does_not_sweep_battlefield() {
        for recipient in [
            TargetFilter::Typed(TypedFilter::default()),
            TargetFilter::Any,
        ] {
            let (mut state, ids) = double_counter_board();
            let ability = resolution_timed(
                Effect::Double {
                    target_kind: DoubleTarget::Counters { counter_type: None },
                    target: recipient.clone(),
                },
                vec![],
                ids[0],
                PlayerId(0),
            );
            resolve(&mut state, &ability, &mut Vec::new()).unwrap();
            for id in ids {
                assert_eq!(
                    plus1(&state, id),
                    2,
                    "{recipient:?} names no population and must double nothing"
                );
            }
            assert_eq!(
                state.objects[&ids[0]].counters[&CounterType::Lore],
                3,
                "and no other counter kind moves either"
            );
        }

        // PAIRED POSITIVE, same test: a described population on the same board
        // does double, so the negatives above are not vacuous.
        let (mut state, ids) = double_counter_board();
        let ability = resolution_timed(
            Effect::Double {
                target_kind: DoubleTarget::Counters { counter_type: None },
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(crate::types::ability::ControllerRef::You),
                ),
            },
            vec![],
            ids[0],
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        assert_eq!(plus1(&state, ids[0]), 4);
        assert_eq!(plus1(&state, ids[1]), 4);
        assert_eq!(plus1(&state, ids[2]), 2);
    }

    #[test]
    fn double_counters_specific_type() {
        let mut state = GameState::default();
        let obj_id = ObjectId(1);
        let mut obj = GameObject::new(
            obj_id,
            CardId(0),
            PlayerId(0),
            "Test".into(),
            Zone::Battlefield,
        );
        obj.counters.insert(CounterType::Plus1Plus1, 3);
        state.objects.insert(obj_id, obj);
        state.battlefield.push_back(obj_id);

        let mut events = Vec::new();
        let ability = make_double_ability(
            DoubleTarget::Counters {
                counter_type: Some(CounterType::Plus1Plus1),
            },
            TargetFilter::Any,
            PlayerId(0),
            vec![TargetRef::Object(obj_id)],
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.10e: 3 counters doubled → 6 counters
        assert_eq!(
            state.objects[&obj_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            6
        );
    }

    #[test]
    fn double_counters_replacement_choice_stashes_remaining_counter_additions() {
        let mut state = GameState::default();
        for (id, modification) in [
            (ObjectId(90), QuantityModification::DOUBLE),
            (ObjectId(91), QuantityModification::Plus { value: 1 }),
        ] {
            let mut source = GameObject::new(
                id,
                CardId(id.0),
                PlayerId(0),
                "Counter Modifier".into(),
                Zone::Battlefield,
            );
            source.replacement_definitions =
                vec![ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .valid_card(TargetFilter::Typed(TypedFilter::creature()))
                    .quantity_modification(modification)]
                .into();
            state.objects.insert(id, source);
            state.battlefield.push_back(id);
        }

        let obj_id = ObjectId(1);
        let mut obj = GameObject::new(
            obj_id,
            CardId(1),
            PlayerId(0),
            "Test Creature".into(),
            Zone::Battlefield,
        );
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.counters.insert(CounterType::Plus1Plus1, 1);
        obj.counters.insert(CounterType::Stun, 1);
        state.objects.insert(obj_id, obj);
        state.battlefield.push_back(obj_id);

        let mut events = Vec::new();
        let ability = make_double_ability(
            DoubleTarget::Counters { counter_type: None },
            TargetFilter::Any,
            PlayerId(0),
            vec![TargetRef::Object(obj_id)],
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            crate::types::game_state::WaitingFor::ReplacementChoice { .. }
        ));
        let pending = state
            .active_counter_additions()
            .expect("remaining double-counter additions should be queued");
        assert_eq!(pending.remaining.len(), 1);
        assert!(matches!(
            pending.completion,
            Some(PendingEffectResolved {
                kind: EffectKind::Double,
                source_id: ObjectId(100),
                player_action: None,
                ..
            })
        ));
    }

    #[test]
    fn double_counters_is_prevented_by_solemnity() {
        let mut state = GameState::default();
        let solemnity_id = ObjectId(99);
        let mut solemnity = GameObject::new(
            solemnity_id,
            CardId(99),
            PlayerId(0),
            "Solemnity".into(),
            Zone::Battlefield,
        );
        solemnity.replacement_definitions =
            vec![ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .valid_card(TargetFilter::Typed(TypedFilter::creature()))
                .quantity_modification(QuantityModification::Prevent)]
            .into();
        state.objects.insert(solemnity_id, solemnity);
        state.battlefield.push_back(solemnity_id);

        let obj_id = ObjectId(1);
        let mut obj = GameObject::new(
            obj_id,
            CardId(0),
            PlayerId(0),
            "Test Creature".into(),
            Zone::Battlefield,
        );
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.counters.insert(CounterType::Plus1Plus1, 3);
        state.objects.insert(obj_id, obj);
        state.battlefield.push_back(obj_id);

        let mut events = Vec::new();
        let ability = make_double_ability(
            DoubleTarget::Counters {
                counter_type: Some(CounterType::Plus1Plus1),
            },
            TargetFilter::Any,
            PlayerId(0),
            vec![TargetRef::Object(obj_id)],
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&obj_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            3
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, GameEvent::CounterAdded { .. })),
            "Solemnity must prevent doubling counters from adding counters"
        );
    }

    #[test]
    fn double_counters_all_kinds() {
        let mut state = GameState::default();
        let obj_id = ObjectId(1);
        let mut obj = GameObject::new(
            obj_id,
            CardId(0),
            PlayerId(0),
            "Test".into(),
            Zone::Battlefield,
        );
        obj.counters.insert(CounterType::Plus1Plus1, 2);
        obj.counters
            .insert(CounterType::Generic("charge".to_string()), 1);
        state.objects.insert(obj_id, obj);
        state.battlefield.push_back(obj_id);

        let mut events = Vec::new();
        let ability = make_double_ability(
            DoubleTarget::Counters { counter_type: None },
            TargetFilter::Any,
            PlayerId(0),
            vec![TargetRef::Object(obj_id)],
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.10e: 2 +1/+1 → 4, 1 charge → 2
        let obj = &state.objects[&obj_id];
        assert_eq!(
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            4
        );
        assert_eq!(
            obj.counters
                .get(&CounterType::Generic("charge".to_string()))
                .copied()
                .unwrap_or(0),
            2
        );
    }

    /// CR 701.10e + CR 614.1a: doubling counters is a "put counters" event, so it
    /// must pass through the AddCounter replacement pipeline - a Doubling-Season /
    /// Vorinclex / Hardened Scales class effect applies to the counters the
    /// doubling adds. With a doubling replacement in play, doubling 4 +1/+1
    /// counters adds 4 -> replaced to 8 -> total 12 (Vorel of the Hull Clade under
    /// Doubling Season). The raw `apply_counter_addition` path bypassed the
    /// pipeline and produced 8.
    #[test]
    fn double_counters_applies_addcounter_replacement() {
        let mut state = GameState::default();
        let obj_id = ObjectId(1);
        let mut obj = GameObject::new(
            obj_id,
            CardId(0),
            PlayerId(0),
            "Vorel".into(),
            Zone::Battlefield,
        );
        obj.counters.insert(CounterType::Plus1Plus1, 4);
        state.objects.insert(obj_id, obj);
        state.battlefield.push_back(obj_id);

        // Doubling-Season fixture: a permanent carrying an AddCounter replacement
        // that doubles the count (avoids depending on a specific card).
        let doubler_id = ObjectId(2);
        let mut doubler = GameObject::new(
            doubler_id,
            CardId(1),
            PlayerId(0),
            "Counter Doubler".into(),
            Zone::Battlefield,
        );
        let mut repl = ReplacementDefinition::new(ReplacementEvent::AddCounter);
        repl.valid_card = Some(TargetFilter::Any);
        repl.quantity_modification = Some(QuantityModification::DOUBLE);
        doubler.replacement_definitions.push(repl);
        state.objects.insert(doubler_id, doubler);
        state.battlefield.push_back(doubler_id);

        let mut events = Vec::new();
        let ability = make_double_ability(
            DoubleTarget::Counters { counter_type: None },
            TargetFilter::Any,
            PlayerId(0),
            vec![TargetRef::Object(obj_id)],
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // 4 base + (4 added, doubled to 8) = 12.
        assert_eq!(
            state.objects[&obj_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            12,
            "doubling must route adds through the AddCounter replacement pipeline"
        );
    }

    #[test]
    fn double_life_total() {
        let mut state = GameState::default();
        // Set player 0's life to 15
        state.players[0].life = 15;

        let mut events = Vec::new();
        let ability = make_double_ability(
            DoubleTarget::LifeTotal,
            TargetFilter::Controller,
            PlayerId(0),
            vec![],
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.10d: 15 life → 30 life
        assert_eq!(state.players[0].life, 30);
    }

    /// CR 701.10d + CR 119.7: Doubling life routes through `apply_life_gain`, so
    /// a CantGainLife static on the affected player suppresses the doubling.
    #[test]
    fn double_life_total_blocked_by_cant_gain_life() {
        use crate::game::zones::create_object;
        use crate::types::ability::{ControllerRef, StaticDefinition, TypedFilter};
        use crate::types::identifiers::CardId;
        use crate::types::statics::StaticMode;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        state.players[0].life = 15;

        // Attach a CantGainLife static affecting PlayerId(0).
        let lock_id = create_object(
            &mut state,
            CardId(999),
            PlayerId(0),
            "Life Lock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&lock_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantGainLife).affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
            );

        let mut events = Vec::new();
        let ability = make_double_ability(
            DoubleTarget::LifeTotal,
            TargetFilter::Controller,
            PlayerId(0),
            vec![],
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // Life total must be unchanged — the Double effect's life-gain half is
        // short-circuited by the CantGainLife lock before the pipeline runs.
        assert_eq!(state.players[0].life, 15);
    }

    #[test]
    fn double_mana_pool() {
        let mut state = GameState::default();
        // Add 3 red mana to player 0's pool
        let p0 = state.players[0].id;
        for _ in 0..3 {
            let _ = state.add_mana_to_pool(
                p0,
                ManaUnit {
                    color: ManaType::Red,
                    source_id: ObjectId(50),
                    pip_id: crate::types::mana::ManaPipId(0),
                    supertype: None,
                    source_could_produce_two_or_more_colors: false,
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                },
            );
        }

        let mut events = Vec::new();
        let ability = make_double_ability(
            DoubleTarget::ManaPool {
                color: Some(ManaColor::Red),
            },
            TargetFilter::Controller,
            PlayerId(0),
            vec![],
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.10f: 3 red → 6 red
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 6);
    }
}
