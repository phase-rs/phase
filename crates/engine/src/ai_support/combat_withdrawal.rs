//! Exact combat facts for a currently selected remove-from-combat target.
//!
//! This facade deliberately bridges only the narrow, already-announced target
//! forms that remove that target from combat. It is not target generation and
//! never substitutes the broader slot target list for the engine's current
//! legal-target set.

use std::ops::ControlFlow;

use crate::game::combat_damage::{assess_combat_impact, CombatImpact};
use crate::types::ability::{Effect, ResolvedAbility, TargetRef};
use crate::types::ability_visit::visit_ability_def;
use crate::types::actions::GameAction;
use crate::types::game_state::{
    GameState, TargetSelectionProgress, TargetSelectionSlot, WaitingFor,
};
use crate::types::identifiers::ObjectIncarnationRef;
use crate::types::player::PlayerId;

/// Whether the selected creature is attacking or blocking in the exact combat pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombatWithdrawalTargetRole {
    Attacker,
    Blocker,
}

/// A conservative fact about removing the currently selected creature from combat.
///
/// `None` from [`combat_withdrawal_fact_for_current_target`] means the current
/// prompt is not an exact combat-withdrawal target. `NoCombatPair` means it is
/// one, but the selected object has no one-on-one combat pair to assess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombatWithdrawalFact {
    NoCombatPair {
        source: ObjectIncarnationRef,
        target: ObjectIncarnationRef,
    },
    CombatPair {
        source: ObjectIncarnationRef,
        target: ObjectIncarnationRef,
        target_role: CombatWithdrawalTargetRole,
        attacker: ObjectIncarnationRef,
        blocker: ObjectIncarnationRef,
        attacker_controller: PlayerId,
        blocker_controller: PlayerId,
        impact: CombatImpact,
    },
}

/// Return a fact for an exact, current target selection that removes its target from combat.
///
/// The facade accepts only a single unconditioned target slot whose resolved
/// ability is either a direct `RemoveFromCombat` or a single target-producing
/// parent followed immediately by `RemoveFromCombat { target: ParentTarget }`.
/// This avoids guessing across sibling effects, conditional branches, or target
/// slots that are not semantically bound to the removal instruction.
pub fn combat_withdrawal_fact_for_current_target(
    state: &GameState,
    actor: PlayerId,
    selected_target: &TargetRef,
) -> Option<CombatWithdrawalFact> {
    let WaitingFor::TargetSelection {
        player,
        pending_cast,
        target_slots,
        selection,
        ..
    } = &state.waiting_for
    else {
        return None;
    };
    if *player != actor
        || !is_single_current_target_selection(target_slots, selection)
        || !selection.current_legal_targets.contains(selected_target)
        || !is_exact_combat_withdrawal_path(&pending_cast.ability)
        || pending_cast.object_id != pending_cast.ability.source_id
    {
        return None;
    }

    let TargetRef::Object(target_id) = selected_target else {
        return None;
    };
    let source_object = state.objects.get(&pending_cast.object_id)?;
    if source_object.controller != actor {
        return None;
    }
    let source = ObjectIncarnationRef::from_object(source_object);
    if pending_cast
        .ability
        .source_incarnation
        .is_some_and(|incarnation| incarnation != source.incarnation)
    {
        return None;
    }
    let target = ObjectIncarnationRef::from_object(state.objects.get(target_id)?);
    let Some(combat) = state.combat.as_ref() else {
        return Some(CombatWithdrawalFact::NoCombatPair { source, target });
    };

    let pair = if combat
        .attackers
        .iter()
        .any(|entry| entry.object_id == *target_id)
    {
        combat
            .blocker_assignments
            .get(target_id)
            .and_then(|members| single_member(members))
            .and_then(|blocker_id| {
                state.objects.get(&blocker_id).map(|blocker_object| {
                    (
                        CombatWithdrawalTargetRole::Attacker,
                        target,
                        ObjectIncarnationRef::from_object(blocker_object),
                    )
                })
            })
    } else {
        combat
            .blocker_to_attacker
            .get(target_id)
            .and_then(|members| single_member(members))
            .and_then(|attacker_id| {
                state.objects.get(&attacker_id).map(|attacker_object| {
                    (
                        CombatWithdrawalTargetRole::Blocker,
                        ObjectIncarnationRef::from_object(attacker_object),
                        target,
                    )
                })
            })
    };
    let Some((target_role, attacker, blocker)) = pair else {
        return Some(CombatWithdrawalFact::NoCombatPair { source, target });
    };
    let attacker_controller = state.objects.get(&attacker.object_id)?.controller;
    let blocker_controller = state.objects.get(&blocker.object_id)?.controller;

    Some(CombatWithdrawalFact::CombatPair {
        source,
        target,
        target_role,
        attacker,
        blocker,
        attacker_controller,
        blocker_controller,
        impact: assess_combat_impact(state, attacker, blocker),
    })
}

/// Necessary-condition filter for "activating this ability can open an exact
/// combat-withdrawal target selection".
///
/// A `false` answer proves that applying `action` to `state` cannot leave the
/// game in a target selection that [`combat_withdrawal_fact_for_current_target`]
/// accepts, because that facade requires a `RemoveFromCombat` effect in the
/// announced ability and no ability of the source object carries one. A `true`
/// answer proves nothing: the caller must still simulate the activation.
///
/// The filter never guesses. It answers `true` (fall open) whenever the
/// structural question cannot be decided from the source object's own lists:
///
/// - `action` is not `GameAction::ActivateAbility`;
/// - the game is not waiting on `WaitingFor::Priority`, the only state in which
///   the activation is known to bind a definition of the source object;
/// - the source object is missing;
/// - a layer flush is pending, so the live `abilities` list may be out of step
///   with what the reducer will bind;
/// - `ability_index` is at or past the source's `abilities` list, where the
///   reducer resolves the definition from runtime-synthesized grants that are
///   stored in no list scanned here.
///
/// Otherwise it scans every definition in `base_abilities` and `abilities`
/// (not only the indexed one, which is deliberately conservative) through
/// [`visit_ability_def`], which also walks sub-abilities, else-branches, modes
/// and costs.
pub fn activation_may_yield_combat_withdrawal(state: &GameState, action: &GameAction) -> bool {
    let GameAction::ActivateAbility {
        source_id,
        ability_index,
    } = action
    else {
        return true;
    };
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        return true;
    }
    let Some(source) = state.objects.get(source_id) else {
        return true;
    };
    if state.layers_dirty.is_dirty() {
        return true;
    }
    if *ability_index >= source.abilities.len() {
        return true;
    }
    source
        .base_abilities
        .iter()
        .chain(source.abilities.iter())
        .any(|def| {
            visit_ability_def(def, &mut |effect| {
                if matches!(effect, Effect::RemoveFromCombat { .. }) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            })
            .is_break()
        })
}

fn is_single_current_target_selection(
    target_slots: &[TargetSelectionSlot],
    selection: &TargetSelectionProgress,
) -> bool {
    target_slots.len() == 1 && selection.current_slot == 0 && selection.selected_slots.is_empty()
}

fn is_exact_combat_withdrawal_path(ability: &ResolvedAbility) -> bool {
    if ability.condition.is_some() || ability.else_ability.is_some() {
        return false;
    }

    match &ability.effect {
        Effect::RemoveFromCombat { target } => {
            !target.is_context_ref() && ability.sub_ability.is_none()
        }
        _ => {
            let Some(parent_target) = ability.effect.target_filter() else {
                return false;
            };
            if parent_target.is_context_ref() {
                return false;
            }

            let Some(removal) = ability.sub_ability.as_deref() else {
                return false;
            };
            removal.condition.is_none()
                && removal.else_ability.is_none()
                && removal.sub_ability.is_none()
                && matches!(
                    removal.effect,
                    Effect::RemoveFromCombat {
                        target: crate::types::ability::TargetFilter::ParentTarget
                    }
                )
        }
    }
}

fn single_member(
    members: &[crate::types::identifiers::ObjectId],
) -> Option<crate::types::identifiers::ObjectId> {
    let [member] = members else {
        return None;
    };
    Some(*member)
}

#[cfg(test)]
mod tests {
    use crate::types::game_state::TargetEffectDetail;
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use crate::game::engine::apply_as_current_for_simulation;
    use crate::game::zones::create_object;
    use crate::types::ability::EffectKind;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PtValue, TargetFilter, TargetRef, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{PendingCast, TargetSelectionProgress, TargetSelectionSlot};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::ManaCost;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    const ACTOR: PlayerId = PlayerId(0);
    const OPPONENT: PlayerId = PlayerId(1);

    fn creature(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let object_id = create_object(
            state,
            CardId(state.objects.len() as u64 + 1),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let object = state
            .objects
            .get_mut(&object_id)
            .expect("created object exists");
        object.card_types.core_types.push(CoreType::Creature);
        object.power = Some(power);
        object.toughness = Some(toughness);
        object_id
    }

    fn combat_state() -> (GameState, ObjectId, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let source = creature(&mut state, ACTOR, "Withdrawal Source", 1, 1);
        let attacker = creature(&mut state, OPPONENT, "Attacker", 4, 4);
        let blocker = creature(&mut state, ACTOR, "Blocker", 2, 2);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker,
                defending_player: ACTOR,
                attack_target: AttackTarget::Player(ACTOR),
                blocked: true,
                band_id: None,
            }],
            blocker_assignments: HashMap::from([(attacker, vec![blocker])]),
            blocker_to_attacker: HashMap::from([(blocker, vec![attacker])]),
            ..Default::default()
        });
        (state, source, attacker, blocker)
    }

    fn install_target_selection(
        state: &mut GameState,
        source: ObjectId,
        ability: ResolvedAbility,
        legal_targets: Vec<TargetRef>,
    ) {
        let mut pending = PendingCast::new(source, CardId(99), ability, ManaCost::zero());
        pending.activation_ability_index = Some(0);
        state.waiting_for = WaitingFor::TargetSelection {
            player: ACTOR,
            pending_cast: Box::new(pending),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: legal_targets.clone(),
                optional: false,
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
            }],
            mode_labels: Vec::new(),
            selection: TargetSelectionProgress {
                current_slot: 0,
                selected_slots: Vec::new(),
                current_legal_targets: legal_targets,
            },
        };
    }

    fn direct_removal(source: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::RemoveFromCombat {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            Vec::new(),
            source,
            ACTOR,
        )
    }

    #[test]
    fn single_member_rejects_empty_and_multi_member_slices() {
        let member = ObjectId(1);

        assert_eq!(single_member(&[]), None);
        assert_eq!(single_member(&[member]), Some(member));
        assert_eq!(single_member(&[member, ObjectId(2)]), None);
    }

    #[test]
    fn direct_target_reports_exact_attacker_fact_without_mutating_state() {
        let (mut state, source, attacker, blocker) = combat_state();
        install_target_selection(
            &mut state,
            source,
            direct_removal(source),
            vec![TargetRef::Object(attacker)],
        );
        let before = state.clone();

        let fact =
            combat_withdrawal_fact_for_current_target(&state, ACTOR, &TargetRef::Object(attacker))
                .expect("direct removal target has an exact combat fact");

        assert_eq!(
            state, before,
            "fact lookup must not apply or replay effects"
        );
        let CombatWithdrawalFact::CombatPair {
            target_role,
            attacker: fact_attacker,
            blocker: fact_blocker,
            impact,
            ..
        } = fact
        else {
            panic!("blocked attacker must have a combat pair");
        };
        assert_eq!(target_role, CombatWithdrawalTargetRole::Attacker);
        assert_eq!(fact_attacker.object_id, attacker);
        assert_eq!(fact_blocker.object_id, blocker);
        assert!(matches!(impact, CombatImpact::Fixed { .. }));
    }

    #[test]
    fn parent_target_removal_reports_exact_blocker_fact() {
        let (mut state, source, attacker, blocker) = combat_state();
        let mut ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            Vec::new(),
            source,
            ACTOR,
        );
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::RemoveFromCombat {
                target: TargetFilter::ParentTarget,
            },
            Vec::new(),
            source,
            ACTOR,
        )));
        install_target_selection(
            &mut state,
            source,
            ability,
            vec![TargetRef::Object(blocker)],
        );

        let fact =
            combat_withdrawal_fact_for_current_target(&state, ACTOR, &TargetRef::Object(blocker))
                .expect("parent target removal target has an exact combat fact");

        let CombatWithdrawalFact::CombatPair {
            target_role,
            target,
            attacker: fact_attacker,
            ..
        } = fact
        else {
            panic!("blocked creature must have a combat pair");
        };
        assert_eq!(target_role, CombatWithdrawalTargetRole::Blocker);
        assert_eq!(target.object_id, blocker);
        assert_eq!(fact_attacker.object_id, attacker);
    }

    #[test]
    fn rejects_hostile_actor_and_noncurrent_target_without_slot_fallback() {
        let (mut state, source, attacker, blocker) = combat_state();
        install_target_selection(
            &mut state,
            source,
            direct_removal(source),
            vec![TargetRef::Object(attacker)],
        );

        assert!(combat_withdrawal_fact_for_current_target(
            &state,
            OPPONENT,
            &TargetRef::Object(attacker),
        )
        .is_none());

        let WaitingFor::TargetSelection { selection, .. } = &mut state.waiting_for else {
            panic!("target selection installed");
        };
        selection.current_legal_targets.clear();

        assert!(combat_withdrawal_fact_for_current_target(
            &state,
            ACTOR,
            &TargetRef::Object(attacker),
        )
        .is_none());
        assert!(combat_withdrawal_fact_for_current_target(
            &state,
            ACTOR,
            &TargetRef::Object(blocker),
        )
        .is_none());
    }

    #[test]
    fn rejects_stale_pending_source_incarnation_and_unblocked_attacker() {
        let (mut state, source, attacker, _) = combat_state();
        let mut ability = direct_removal(source);
        ability.source_incarnation = Some(
            state
                .objects
                .get(&source)
                .expect("source exists")
                .incarnation
                + 1,
        );
        install_target_selection(
            &mut state,
            source,
            ability,
            vec![TargetRef::Object(attacker)],
        );
        assert!(combat_withdrawal_fact_for_current_target(
            &state,
            ACTOR,
            &TargetRef::Object(attacker),
        )
        .is_none());

        let (mut state, source, attacker, _) = combat_state();
        state
            .combat
            .as_mut()
            .expect("combat exists")
            .blocker_assignments
            .clear();
        state
            .combat
            .as_mut()
            .expect("combat exists")
            .blocker_to_attacker
            .clear();
        state
            .combat
            .as_mut()
            .expect("combat exists")
            .attackers
            .iter_mut()
            .find(|entry| entry.object_id == attacker)
            .expect("attacker exists")
            .blocked = false;
        install_target_selection(
            &mut state,
            source,
            direct_removal(source),
            vec![TargetRef::Object(attacker)],
        );
        assert!(matches!(
            combat_withdrawal_fact_for_current_target(&state, ACTOR, &TargetRef::Object(attacker),),
            Some(CombatWithdrawalFact::NoCombatPair { .. })
        ));
    }

    #[test]
    fn rejects_conditional_or_sibling_removal_paths() {
        let (mut state, source, attacker, _) = combat_state();
        let mut ability = direct_removal(source);
        ability.condition = Some(crate::types::ability::AbilityCondition::WhenYouDo);
        install_target_selection(
            &mut state,
            source,
            ability,
            vec![TargetRef::Object(attacker)],
        );
        assert!(combat_withdrawal_fact_for_current_target(
            &state,
            ACTOR,
            &TargetRef::Object(attacker),
        )
        .is_none());

        let (mut state, source, attacker, _) = combat_state();
        let mut ability = direct_removal(source);
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            ACTOR,
        )));
        install_target_selection(
            &mut state,
            source,
            ability,
            vec![TargetRef::Object(attacker)],
        );
        assert!(combat_withdrawal_fact_for_current_target(
            &state,
            ACTOR,
            &TargetRef::Object(attacker),
        )
        .is_none());
    }

    #[test]
    fn real_activation_target_selection_replays_through_the_facade() {
        let (mut state, source, attacker, _) = combat_state();
        let source_object = state.objects.get_mut(&source).expect("source exists");
        Arc::make_mut(&mut source_object.abilities).push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::RemoveFromCombat {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        ));
        state.waiting_for = WaitingFor::Priority { player: ACTOR };
        state.priority_player = ACTOR;

        apply_as_current_for_simulation(
            &mut state,
            GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            },
        )
        .expect("activation reaches target selection");
        let WaitingFor::TargetSelection { selection, .. } = &state.waiting_for else {
            panic!("targeted activation must enter TargetSelection");
        };
        assert!(
            selection
                .current_legal_targets
                .contains(&TargetRef::Object(attacker)),
            "the engine's current legal target set contains the attacker"
        );

        let first =
            combat_withdrawal_fact_for_current_target(&state, ACTOR, &TargetRef::Object(attacker));
        let second =
            combat_withdrawal_fact_for_current_target(&state, ACTOR, &TargetRef::Object(attacker));
        assert_eq!(first, second, "replayed fact lookup is deterministic");
        assert!(
            matches!(first, Some(CombatWithdrawalFact::CombatPair { .. })),
            "real engine prompt binds the selected target"
        );
    }

    /// A draw effect: the shape used for "no withdrawal anywhere" definitions.
    fn draw_effect() -> Effect {
        Effect::Draw {
            count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        }
    }

    /// A direct remove-from-combat effect on a typed creature target.
    fn removal_effect() -> Effect {
        Effect::RemoveFromCombat {
            target: TargetFilter::Typed(TypedFilter::creature()),
        }
    }

    /// Build a clean-layer priority state whose source carries `abilities` as
    /// its live list and `base_abilities` as its printed list.
    fn precondition_state(
        abilities: Vec<AbilityDefinition>,
        base_abilities: Vec<AbilityDefinition>,
    ) -> (GameState, ObjectId) {
        let (mut state, source, _, _) = combat_state();
        let source_object = state.objects.get_mut(&source).expect("source exists");
        source_object.abilities = Arc::new(abilities);
        source_object.base_abilities = Arc::new(base_abilities);
        state.waiting_for = WaitingFor::Priority { player: ACTOR };
        state.priority_player = ACTOR;
        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;
        (state, source)
    }

    /// The activation of the source's first ability.
    fn activate_first(source: ObjectId) -> GameAction {
        GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        }
    }

    /// With no `RemoveFromCombat` in any ability of the source the filter proves
    /// the activation cannot open a withdrawal prompt, and the real reducer
    /// agrees: the simulated activation yields no exact withdrawal fact.
    #[test]
    fn precondition_rejects_an_activation_without_any_removal_effect() {
        let draw = AbilityDefinition::new(AbilityKind::Activated, draw_effect());
        let (state, source) = precondition_state(vec![draw.clone()], vec![draw]);

        assert!(!activation_may_yield_combat_withdrawal(
            &state,
            &activate_first(source)
        ));

        let mut simulated = state.clone();
        apply_as_current_for_simulation(&mut simulated, activate_first(source))
            .expect("reach-guard: the draw activation applies");
        assert!(
            !matches!(simulated.waiting_for, WaitingFor::TargetSelection { .. }),
            "a draw activation opens no target selection, so no fact can exist"
        );
    }

    /// The filter answers "may" for every placement of the removal effect that
    /// the fact facade can read: direct, nested in a sub-ability, and present
    /// only in the printed list.
    #[test]
    fn precondition_accepts_direct_nested_and_printed_only_removal() {
        let draw = AbilityDefinition::new(AbilityKind::Activated, draw_effect());
        let direct = AbilityDefinition::new(AbilityKind::Activated, removal_effect());
        let nested = AbilityDefinition::new(AbilityKind::Activated, draw_effect()).sub_ability(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::RemoveFromCombat {
                    target: TargetFilter::ParentTarget,
                },
            ),
        );

        let (state, source) = precondition_state(vec![direct.clone()], vec![direct.clone()]);
        assert!(activation_may_yield_combat_withdrawal(
            &state,
            &activate_first(source)
        ));

        let (state, source) = precondition_state(vec![nested.clone()], vec![nested]);
        assert!(activation_may_yield_combat_withdrawal(
            &state,
            &activate_first(source)
        ));

        let (state, source) = precondition_state(vec![draw], vec![direct]);
        assert!(
            activation_may_yield_combat_withdrawal(&state, &activate_first(source)),
            "a removal present only in base_abilities must still answer may"
        );
    }

    /// Every undecidable case falls open. Each arm starts from the state the
    /// rejecting test uses, so the `true` answer is caused by the single
    /// perturbation and not by a removal effect.
    #[test]
    fn precondition_falls_open_when_the_structure_cannot_be_decided() {
        let draw = AbilityDefinition::new(AbilityKind::Activated, draw_effect());
        let fixture = || precondition_state(vec![draw.clone()], vec![draw.clone()]);

        let (state, source) = fixture();
        assert!(
            !activation_may_yield_combat_withdrawal(&state, &activate_first(source)),
            "reach-guard: the unperturbed fixture is rejected"
        );

        let (mut state, source) = fixture();
        state.objects.remove(&source);
        assert!(
            activation_may_yield_combat_withdrawal(&state, &activate_first(source)),
            "missing source"
        );

        let (mut state, source) = fixture();
        state.layers_dirty.mark_full();
        assert!(
            activation_may_yield_combat_withdrawal(&state, &activate_first(source)),
            "dirty layers"
        );

        let (state, source) = fixture();
        assert!(
            activation_may_yield_combat_withdrawal(
                &state,
                &GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 1,
                },
            ),
            "index past the abilities list"
        );

        let (mut state, source) = fixture();
        install_target_selection(&mut state, source, direct_removal(source), Vec::new());
        assert!(
            activation_may_yield_combat_withdrawal(&state, &activate_first(source)),
            "non-priority state"
        );

        let (state, _) = fixture();
        assert!(
            activation_may_yield_combat_withdrawal(&state, &GameAction::PassPriority),
            "non-activation action"
        );
    }
}
