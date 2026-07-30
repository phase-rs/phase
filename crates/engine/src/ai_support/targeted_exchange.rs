//! Bounded, reducer-backed preview for a fully-targeted self-destructive exchange.
//!
//! This intentionally certifies only the narrow class whose complete target
//! declaration is available before mana payment.  It is an engine authority:
//! the AI supplies an issued candidate, while this module preserves interaction
//! ownership, replays the reducer, and reads the fully-bound pending ability.

use crate::ai_support::{validated_candidate_actions_for_semantic_owner, CandidateAction};
use crate::game::effects::resolve_ability_chain;
use crate::game::engine::apply_interaction_for_simulation;
use crate::game::layers::flush_layers;
use crate::game::sba::check_state_based_actions;
use crate::types::ability::{DamageSource, Effect, ResolvedAbility, TargetFilter, TargetRef};
use crate::types::actions::GameAction;
use crate::types::card_type::CoreType;
use crate::types::game_state::{GameState, PendingCast, StackEntryKind, WaitingFor};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Root-cast tactical result. `Indeterminate` deliberately leaves the root
/// candidate available; the preview is a safety veto, not a second rules engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetedExchangeVerdict {
    Reject,
    Allow,
    Indeterminate,
}

const MAX_WITNESS_NODES: usize = 64;
const MAX_WITNESS_BRANCHES: usize = 16;

#[derive(Debug, Clone, Copy)]
enum RootBinding {
    Cast {
        object_id: ObjectId,
    },
    Activation {
        source_id: ObjectId,
        ability_index: usize,
    },
}

impl RootBinding {
    fn from_action(action: &GameAction) -> Option<Self> {
        match action {
            GameAction::CastSpell { object_id, .. } => Some(Self::Cast {
                object_id: *object_id,
            }),
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => Some(Self::Activation {
                source_id: *source_id,
                ability_index: *ability_index,
            }),
            _ => None,
        }
    }

    fn matches_pending(self, pending: &PendingCast) -> bool {
        match self {
            Self::Cast { object_id } => pending.object_id == object_id,
            Self::Activation {
                source_id,
                ability_index,
            } => {
                pending.object_id == source_id
                    && pending.activation_ability_index == Some(ability_index)
            }
        }
    }
}

/// Preview whether every complete, supported target declaration for `root` is
/// the strictly bad exchange where the selected friendly creature dies and the
/// exact opposing recipient survives.
///
/// CR 601.2c: target choices are enumerated from the current reducer-issued
/// candidate set. CR 608.2c: after each target action, `pending_cast.ability`
/// is the single fully-bound carrier; it is inspected before classifying a
/// successor prompt such as normal mana payment.
pub fn targeted_exchange_verdict(
    state: &GameState,
    root: &CandidateAction,
) -> TargetedExchangeVerdict {
    let Some(root_binding) = RootBinding::from_action(&root.action) else {
        return TargetedExchangeVerdict::Indeterminate;
    };
    let Some(semantic_owner) = root.metadata.semantic_owner else {
        return TargetedExchangeVerdict::Indeterminate;
    };
    let Some(mut next) = replay_exact_candidate(state, root) else {
        return TargetedExchangeVerdict::Indeterminate;
    };
    let mut budget = WitnessBudget::default();
    inspect_successor(&mut next, root_binding, semantic_owner, &mut budget)
}

#[derive(Default)]
struct WitnessBudget {
    nodes: usize,
    branches: usize,
}

fn inspect_successor(
    state: &mut GameState,
    root: RootBinding,
    semantic_owner: PlayerId,
    budget: &mut WitnessBudget,
) -> TargetedExchangeVerdict {
    if budget.nodes >= MAX_WITNESS_NODES {
        return TargetedExchangeVerdict::Indeterminate;
    }
    budget.nodes += 1;

    // CR 601.2h: automatic payment finalizes a normal-cost spell immediately
    // after its final target is declared. The target-bound carrier is therefore
    // either the matching PendingCast (manual payment) or the exact announced
    // Spell stack entry (automatic payment), before prompt classification.
    if let Some(ability) = bound_root_ability(state, root) {
        if let Some(verdict) = preview_bound_exchange(state, ability, semantic_owner) {
            return verdict;
        }
    }

    match &state.waiting_for {
        WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
            explore_target_children(state, root, semantic_owner, budget)
        }
        // legal_actions intentionally represents only all-targets (and possibly
        // empty) here; it is not an exhaustive subset enumerator.
        WaitingFor::MultiTargetSelection { .. }
        | WaitingFor::ManaPayment { .. }
        | WaitingFor::ChooseXValue { .. }
        | WaitingFor::ModeChoice { .. }
        | WaitingFor::AbilityModeChoice { .. }
        | WaitingFor::OptionalEffectChoice { .. } => TargetedExchangeVerdict::Indeterminate,
        _ => TargetedExchangeVerdict::Indeterminate,
    }
}

/// Return the finalized target-bound ability only when it can be authenticated
/// to this exact root. Casts retain that authority on their announcement stack
/// entry (CR 601.2a/h); activations use PendingCast because a stack entry does
/// not retain the originating ability index.
fn bound_root_ability(state: &GameState, root: RootBinding) -> Option<&ResolvedAbility> {
    if let Some(pending) = state
        .pending_cast
        .as_deref()
        .filter(|pending| root.matches_pending(pending))
    {
        return Some(pending.ability.as_ref());
    }

    let RootBinding::Cast { object_id } = root else {
        return None;
    };
    state.stack.iter().rev().find_map(|entry| {
        (entry.id == object_id && entry.source_id == object_id)
            .then_some(&entry.kind)
            .and_then(|kind| match kind {
                StackEntryKind::Spell {
                    ability: Some(ability),
                    ..
                } => Some(ability.as_ref()),
                StackEntryKind::Spell { ability: None, .. }
                | StackEntryKind::ActivatedAbility { .. }
                | StackEntryKind::TriggeredAbility { .. }
                | StackEntryKind::KeywordAction { .. } => None,
            })
    })
}

fn explore_target_children(
    state: &GameState,
    root: RootBinding,
    semantic_owner: PlayerId,
    budget: &mut WitnessBudget,
) -> TargetedExchangeVerdict {
    let owner = target_selection_owner(&state.waiting_for);
    let Some(owner) = owner else {
        return TargetedExchangeVerdict::Indeterminate;
    };
    let candidates = validated_candidate_actions_for_semantic_owner(state, owner);
    if candidates
        .iter()
        .any(|candidate| matches!(candidate.action, GameAction::ChooseTarget { target: None }))
    {
        return TargetedExchangeVerdict::Indeterminate;
    }
    let target_children: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| {
            matches!(
                candidate.action,
                GameAction::ChooseTarget { target: Some(_) }
            )
        })
        .collect();
    if target_children.is_empty() || target_children.len() > MAX_WITNESS_BRANCHES {
        return TargetedExchangeVerdict::Indeterminate;
    }

    let mut saw_reject = false;
    let mut saw_indeterminate = false;
    for child in target_children {
        if budget.branches >= MAX_WITNESS_BRANCHES {
            return TargetedExchangeVerdict::Indeterminate;
        }
        budget.branches += 1;
        let Some(mut next) = replay_exact_candidate(state, &child) else {
            return TargetedExchangeVerdict::Indeterminate;
        };
        match inspect_successor(&mut next, root, semantic_owner, budget) {
            TargetedExchangeVerdict::Reject => saw_reject = true,
            TargetedExchangeVerdict::Allow => return TargetedExchangeVerdict::Allow,
            TargetedExchangeVerdict::Indeterminate => saw_indeterminate = true,
        }
    }
    if saw_indeterminate {
        TargetedExchangeVerdict::Indeterminate
    } else if saw_reject {
        TargetedExchangeVerdict::Reject
    } else {
        TargetedExchangeVerdict::Indeterminate
    }
}

fn target_selection_owner(waiting_for: &WaitingFor) -> Option<PlayerId> {
    match waiting_for {
        WaitingFor::TargetSelection { player, .. }
        | WaitingFor::TriggerTargetSelection { player, .. } => Some(*player),
        _ => None,
    }
}

fn replay_exact_candidate(state: &GameState, wanted: &CandidateAction) -> Option<GameState> {
    let semantic_owner = wanted.metadata.semantic_owner?;
    let actor = wanted.metadata.actor?;
    let current = validated_candidate_actions_for_semantic_owner(state, semantic_owner);
    current
        .iter()
        .any(|candidate| {
            candidate.action.cmp_stable(&wanted.action).is_eq()
                && candidate.metadata.semantic_owner == Some(semantic_owner)
                && candidate.metadata.actor == Some(actor)
                && candidate.metadata.tactical_class == wanted.metadata.tactical_class
        })
        .then(|| {
            let mut next = state.clone();
            apply_interaction_for_simulation(
                &mut next,
                actor,
                semantic_owner,
                wanted.action.clone(),
            )
            .ok()
            .map(|_| next)
        })?
}

fn preview_bound_exchange(
    state: &GameState,
    ability: &ResolvedAbility,
    semantic_owner: PlayerId,
) -> Option<TargetedExchangeVerdict> {
    if is_target_sourced_self_damage(ability) {
        return preview_target_sourced_self_damage(state, ability);
    }
    let fight = find_fight_leaf(ability)?;
    preview_fight_exchange(state, ability, fight, semantic_owner)
}

fn preview_target_sourced_self_damage(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Option<TargetedExchangeVerdict> {
    let (source, recipient) = exchange_participants(state, ability)?;
    let mut preview = state.clone();
    flush_layers(&mut preview);
    let source_ref = ObjectIncarnationRef::from_object(preview.objects.get(&source)?);
    let recipient_ref = match recipient {
        TargetRef::Object(recipient) => ExchangeRecipient::Object(
            ObjectIncarnationRef::from_object(preview.objects.get(&recipient)?),
        ),
        TargetRef::Player(recipient) => ExchangeRecipient::Player(recipient),
    };
    let mut events = Vec::new();
    resolve_ability_chain(&mut preview, ability, &mut events, 0).ok()?;
    check_state_based_actions(&mut preview, &mut events);

    let source_left = !same_battlefield_incarnation(&preview, source_ref);
    let recipient_remains = recipient_ref.remains_in_game(&preview);
    Some(if source_left && recipient_remains {
        TargetedExchangeVerdict::Reject
    } else {
        TargetedExchangeVerdict::Allow
    })
}

fn find_fight_leaf(ability: &ResolvedAbility) -> Option<&ResolvedAbility> {
    if matches!(&ability.effect, Effect::Fight { .. }) {
        return Some(ability);
    }
    ability.sub_ability.as_deref().and_then(find_fight_leaf)
}

fn preview_fight_exchange(
    state: &GameState,
    ability: &ResolvedAbility,
    fight: &ResolvedAbility,
    semantic_owner: PlayerId,
) -> Option<TargetedExchangeVerdict> {
    let (first, second) =
        crate::game::effects::fight::resolve_fight_fighters(state, fight).ok()??;
    let first_controller = state.objects.get(&first)?.controller;
    let second_controller = state.objects.get(&second)?.controller;
    let (ai_fighter, opposing_fighter) = match (
        first_controller == semantic_owner,
        second_controller == semantic_owner,
    ) {
        (true, false) => (first, second),
        (false, true) => (second, first),
        // The tactical veto owns only an adverse exchange between one AI
        // creature and one opposing creature. Every other control layout stays
        // available for the normal evaluator.
        (false, false) | (true, true) => return Some(TargetedExchangeVerdict::Allow),
    };
    if !valid_exchange_participants(state, ai_fighter, opposing_fighter) {
        return None;
    }

    let mut preview = state.clone();
    flush_layers(&mut preview);
    let ai_ref = ObjectIncarnationRef::from_object(preview.objects.get(&ai_fighter)?);
    let opposing_ref = ObjectIncarnationRef::from_object(preview.objects.get(&opposing_fighter)?);
    // CR 608.2c + CR 701.14a: replay every already-bound instruction that
    // precedes this Fight (for example, a +2/+2 modifier), then stop at the
    // Fight itself. Later effects must not rewrite the fight's tactical result.
    let mut fight_prefix = ability.clone();
    truncate_after_fight(&mut fight_prefix)?;
    let mut events = Vec::new();
    resolve_ability_chain(&mut preview, &fight_prefix, &mut events, 0).ok()?;
    check_state_based_actions(&mut preview, &mut events);

    let ai_left = !same_battlefield_incarnation(&preview, ai_ref);
    let opposing_remains = same_battlefield_incarnation(&preview, opposing_ref);
    Some(if ai_left && opposing_remains {
        TargetedExchangeVerdict::Reject
    } else {
        TargetedExchangeVerdict::Allow
    })
}

/// Keep the root-to-Fight prefix of an already-bound chain, then remove only
/// the continuation after that Fight. `find_fight_leaf` and this helper share
/// the same continuation traversal, so a preview cannot sever a predecessor.
fn truncate_after_fight(ability: &mut ResolvedAbility) -> Option<()> {
    if matches!(&ability.effect, Effect::Fight { .. }) {
        ability.sub_ability = None;
        ability.else_ability = None;
        return Some(());
    }
    ability
        .sub_ability
        .as_deref_mut()
        .and_then(truncate_after_fight)
}

#[derive(Debug, Clone, Copy)]
enum ExchangeRecipient {
    Object(ObjectIncarnationRef),
    Player(PlayerId),
}

impl ExchangeRecipient {
    fn remains_in_game(self, state: &GameState) -> bool {
        match self {
            Self::Object(reference) => same_battlefield_incarnation(state, reference),
            // CR 704.5a: `check_state_based_actions` marks a player who took
            // lethal damage as eliminated, while a prevention or can't-lose
            // effect correctly leaves that player in the game.
            Self::Player(player) => crate::game::players::is_alive(state, player),
        }
    }
}

fn is_target_sourced_self_damage(ability: &ResolvedAbility) -> bool {
    let ability = match &ability.effect {
        // CR 601.2c: target-subject wording declares its damage-source target
        // on an outer picker node. The actual consecutive damage instructions
        // remain beneath that declaration.
        Effect::TargetOnly { .. } => match ability.sub_ability.as_deref() {
            Some(sub_ability) => sub_ability,
            None => return false,
        },
        _ => ability,
    };
    matches!(
        (&ability.effect, ability.sub_ability.as_deref()),
        (
            Effect::DealDamage {
                damage_source: Some(DamageSource::Target),
                ..
            },
            Some(ResolvedAbility {
                effect: Effect::DealDamage {
                    damage_source: Some(DamageSource::Target),
                    target: TargetFilter::ParentTargetSlot { index: 0 },
                    ..
                },
                sub_ability: None,
                ..
            })
        )
    )
}

fn exchange_participants(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Option<(ObjectId, TargetRef)> {
    let mut targets = crate::game::ability_utils::flatten_targets_in_chain(ability).into_iter();
    let TargetRef::Object(source) = targets.next()? else {
        return None;
    };
    let recipient = targets.next()?;
    valid_targeted_exchange_participants(state, source, &recipient).then_some((source, recipient))
}

fn valid_targeted_exchange_participants(
    state: &GameState,
    source: ObjectId,
    recipient: &TargetRef,
) -> bool {
    let Some(source_object) = state.objects.get(&source) else {
        return false;
    };
    source_object.zone == Zone::Battlefield
        && source_object
            .card_types
            .core_types
            .contains(&CoreType::Creature)
        && match recipient {
            TargetRef::Object(recipient) => {
                source != *recipient
                    && state
                        .objects
                        .get(recipient)
                        .is_some_and(|object| object.zone == Zone::Battlefield)
            }
            TargetRef::Player(recipient) => crate::game::players::is_alive(state, *recipient),
        }
}

fn valid_exchange_participants(state: &GameState, source: ObjectId, recipient: ObjectId) -> bool {
    let Some(source_object) = state.objects.get(&source) else {
        return false;
    };
    let Some(recipient_object) = state.objects.get(&recipient) else {
        return false;
    };
    source != recipient
        && source_object.zone == Zone::Battlefield
        && recipient_object.zone == Zone::Battlefield
        && source_object
            .card_types
            .core_types
            .contains(&CoreType::Creature)
        && recipient_object
            .card_types
            .core_types
            .contains(&CoreType::Creature)
        && source_object.controller != recipient_object.controller
}

fn same_battlefield_incarnation(state: &GameState, reference: ObjectIncarnationRef) -> bool {
    state
        .objects
        .get(&reference.object_id)
        .is_some_and(|object| {
            object.zone == Zone::Battlefield
                && ObjectIncarnationRef::from_object(object) == reference
        })
}
