use crate::game::ability_utils::build_resolved_from_def;
use crate::game::players;
use crate::types::ability::{
    AbilityDefinition, Effect, EffectError, EffectKind, ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, IterCtx, IterItem, IterKind, ResolutionFrame, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

use std::collections::VecDeque;

/// CR 701.55a-b + CR 608.2d: Prompt the instructed player to choose one
/// branch at resolution. The branch itself is not pre-validated for
/// possibility; the chosen instructions perform as much as possible.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (chooser, branches) = match &ability.effect {
        Effect::ChooseOneOf { chooser, branches } => (chooser, branches.clone()),
        _ => return Err(EffectError::MissingParam("ChooseOneOf".to_string())),
    };

    if branches.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ChooseOneOf,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let mut players: VecDeque<PlayerId> = VecDeque::from(choosing_players(state, ability, chooser));
    if players.is_empty() {
        // CR 608.2d: A branch choice must be made by an eligible player. An
        // empty chooser set means the effect cannot legally begin — fail loud
        // instead of silently resolving nothing (issue #927 class).
        return Err(EffectError::InvalidParam(format!(
            "ChooseOneOf: no eligible player for chooser {chooser:?}"
        )));
    }
    // CR 701.55d + CR 608.2: Prompt the first eligible chooser, then push an
    // `Iterate` frame carrying the rest. The driver re-prompts the next player
    // each time the previous player's branch finishes resolving.
    let first = players.pop_front().expect("players is non-empty");
    prompt_player(
        state,
        first,
        ability.controller,
        ability.source_id,
        branches.clone(),
        ability.targets.clone(),
        ability.context.clone(),
        players.iter().copied().collect(),
    );
    if !players.is_empty() {
        push_choose_one_of_iterate(
            state,
            ability.controller,
            ability.source_id,
            branches,
            ability.targets.clone(),
            ability.context.clone(),
            players,
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseOneOf,
        source_id: ability.source_id,
    });
    Ok(())
}

/// CR 701.55d + CR 608.2: Push a `ResolutionFrame::Iterate` carrying the players
/// still awaiting their branch prompt. The body is the branch-choice prompt; the
/// driver re-prompts the next player after each branch resolves.
fn push_choose_one_of_iterate(
    state: &mut GameState,
    controller: PlayerId,
    source_id: ObjectId,
    branches: Vec<AbilityDefinition>,
    parent_targets: Vec<TargetRef>,
    context: crate::types::ability::SpellContext,
    remaining_players: VecDeque<PlayerId>,
) {
    state.resolution_stack.push(ResolutionFrame::Iterate {
        remaining: remaining_players
            .into_iter()
            .map(IterItem::Player)
            .collect(),
        ctx: IterCtx {
            source_id,
            controller,
            kind: IterKind::ChooseOneOf {
                branches,
                parent_targets,
                context,
            },
        },
    });
}

/// CR 701.55d: Prompt one player to choose a branch, parking
/// `WaitingFor::ChooseOneOfBranch`. `remaining_players` is carried on the
/// WaitingFor purely for legacy compatibility with consumers that read it; the
/// authoritative remainder lives on the `Iterate` frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prompt_player(
    state: &mut GameState,
    player: PlayerId,
    controller: PlayerId,
    source_id: ObjectId,
    branches: Vec<AbilityDefinition>,
    parent_targets: Vec<TargetRef>,
    context: crate::types::ability::SpellContext,
    remaining_players: Vec<PlayerId>,
) {
    let branch_descriptions = branch_descriptions(&branches);
    state.waiting_for = WaitingFor::ChooseOneOfBranch {
        player,
        controller,
        source_id,
        branches,
        branch_descriptions,
        parent_targets,
        context,
        remaining_players,
    };
    // `priority_player` routing to the chooser is owned by the centralized
    // post-apply sync (`public_state::sync_priority_player_from_waiting_for`),
    // which maps `WaitingFor::ChooseOneOfBranch { player, .. }` through
    // `turn_control::authorized_submitter_for_player` (CR 608.2d).
}

/// CR 701.55d + CR 608.2: Advance a choose-one-of `Iterate` frame: prompt the
/// next remaining player for a branch choice. Returns the shortened remaining
/// players (frame must be re-pushed), or `None` when no players remain.
pub(crate) fn advance_choose_one_of_iterate(
    state: &mut GameState,
    controller: PlayerId,
    source_id: ObjectId,
    branches: Vec<AbilityDefinition>,
    parent_targets: Vec<TargetRef>,
    context: crate::types::ability::SpellContext,
    mut remaining_players: VecDeque<PlayerId>,
) -> Option<VecDeque<PlayerId>> {
    let player = remaining_players.pop_front()?;
    prompt_player(
        state,
        player,
        controller,
        source_id,
        branches,
        parent_targets,
        context,
        remaining_players.iter().copied().collect(),
    );
    Some(remaining_players)
}

pub(crate) struct BranchSelection {
    pub player: PlayerId,
    pub controller: PlayerId,
    pub source_id: ObjectId,
    pub branches: Vec<AbilityDefinition>,
    pub parent_targets: Vec<TargetRef>,
    pub context: crate::types::ability::SpellContext,
    pub remaining_players: Vec<PlayerId>,
    pub index: usize,
}

pub(crate) fn resolve_branch(
    state: &mut GameState,
    selection: BranchSelection,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let BranchSelection {
        player,
        controller,
        source_id,
        branches,
        parent_targets,
        context,
        remaining_players: _remaining_players,
        index,
    } = selection;
    let Some(branch) = branches.get(index) else {
        return Err(EffectError::InvalidParam(format!(
            "ChooseOneOf branch index {index} out of range"
        )));
    };

    // CR 701.55d + CR 608.2: The remaining players awaiting their prompt are
    // carried on the `Iterate` frame already pushed at initial resolution (or on
    // the prior frame advance). The driver re-prompts the next player after this
    // branch finishes resolving, so this resolver no longer stashes a pending
    // struct or calls a resume helper itself.
    let mut resolved = build_resolved_from_def(branch, source_id, controller);
    resolved.context = context;
    resolved.targets = parent_targets;
    resolved.set_scoped_player_recursive(player);
    if !resolved
        .targets
        .iter()
        .any(|target| matches!(target, TargetRef::Player(pid) if *pid == player))
    {
        resolved.targets.push(TargetRef::Player(player));
    }

    super::resolve_ability_chain(state, &resolved, events, 1)?;
    Ok(())
}

fn choosing_players(
    state: &GameState,
    ability: &ResolvedAbility,
    chooser: &crate::types::ability::PlayerFilter,
) -> Vec<PlayerId> {
    use crate::types::ability::PlayerFilter;

    let apnap = players::apnap_order(state);

    // CR 608.2c + CR 108.3 + CR 109.4: Two chooser filters are anchored to
    // resolution-scoped state that `matches_player_scope` cannot see (it carries
    // no `ResolvedAbility`): `ChosenPlayer` reads the player chosen earlier this
    // resolution from `ability.chosen_players`, and `ParentObjectTargetOwner`
    // reads the owner of the ability's first object target. Resolve them here —
    // this is the one caller that has the ability in scope — and order the
    // result in APNAP (CR 701.55d). Both filter out eliminated players (CR
    // 104.3a — a player who loses leaves the game and can no longer be a
    // chooser) and yield a single chooser, which is correct for the
    // villainous-choice patterns these power (The Master, This Is How It Ends).
    let anchored: Option<PlayerId> = match chooser {
        PlayerFilter::ChosenPlayer { index } => {
            ability.chosen_players.get(*index as usize).copied()
        }
        PlayerFilter::ParentObjectTargetOwner => {
            crate::game::ability_utils::parent_target_owner(ability, state)
        }
        _ => None,
    };
    if let Some(player) = anchored {
        let alive = state
            .players
            .iter()
            .any(|p| p.id == player && !p.is_eliminated);
        return if alive { vec![player] } else { Vec::new() };
    }

    let targeted: Vec<PlayerId> = ability
        .targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Player(player) => Some(*player),
            _ => None,
        })
        .filter(|player| {
            super::matches_player_scope(
                state,
                *player,
                chooser,
                ability.controller,
                ability.source_id,
            )
        })
        .collect();

    if !targeted.is_empty() {
        return apnap
            .into_iter()
            .filter(|player| targeted.contains(player))
            .collect();
    }

    apnap
        .into_iter()
        .filter(|player| {
            super::matches_player_scope(
                state,
                *player,
                chooser,
                ability.controller,
                ability.source_id,
            )
        })
        .collect()
}

fn branch_descriptions(branches: &[AbilityDefinition]) -> Vec<String> {
    branches
        .iter()
        .enumerate()
        .map(|(index, branch)| {
            if let Some(description) = branch
                .description
                .as_ref()
                .map(|text| text.trim())
                .filter(|text| !text.is_empty())
            {
                return description.to_string();
            }
            if let Effect::Token { name, .. } = &*branch.effect {
                return format!("Create a {name} token");
            }
            format!("Option {}", index + 1)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AbilityKind, Comparator, PlayerFilter, PlayerRelation, PlayerScope, PtValue, QuantityExpr,
        QuantityRef, TargetFilter,
    };
    use crate::types::format::FormatConfig;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::ObjectId;
    use crate::types::PlayerId;

    #[test]
    fn empty_chooser_set_fails_loudly() {
        let mut state = GameState::new_two_player(42);
        state.players[0].is_eliminated = true;
        state.players[1].is_eliminated = true;

        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![branch],
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let err = resolve(&mut state, &ability, &mut events).unwrap_err();
        assert!(
            err.to_string().contains("no eligible player"),
            "expected chooser failure, got {err}"
        );
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ));
    }

    #[test]
    fn token_branches_without_descriptions_get_create_labels() {
        let food = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "Food".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".into(), "Food".into()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
        );
        let treasure = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "Treasure".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".into(), "Treasure".into()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
        );
        let labels = branch_descriptions(&[food, treasure]);
        assert_eq!(
            labels,
            vec!["Create a Food token", "Create a Treasure token"]
        );
    }

    #[test]
    fn chosen_player_chooser_prompts_chosen_opponent() {
        // CR 608.2c + CR 109.4: A `ChooseOneOf` whose chooser is
        // `PlayerFilter::ChosenPlayer { index: 0 }` must prompt the player
        // recorded in `ability.chosen_players[0]` (the opponent chosen earlier
        // this resolution — The Master, Gallifrey's End), not the controller.
        let mut state = GameState::new(FormatConfig::commander(), 3, 42);

        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let mut ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::ChosenPlayer { index: 0 },
                branches: vec![branch],
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        ability.chosen_players = vec![PlayerId(2)];
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player,
                remaining_players,
                ..
            } => {
                assert_eq!(*player, PlayerId(2));
                assert!(remaining_players.is_empty());
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }
    }

    #[test]
    fn parent_object_target_owner_chooser_prompts_target_owner() {
        // CR 108.3 + CR 109.4: A `ChooseOneOf` whose chooser is
        // `ParentObjectTargetOwner` must prompt the owner of the ability's first
        // object target (This Is How It Ends — the targeted creature's owner
        // faces the villainous choice).
        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        // Create an object owned by player 2 and bind it as the parent target.
        let obj_id = ObjectId(99);
        let obj = crate::game::game_object::GameObject::new(
            obj_id,
            crate::types::identifiers::CardId(0),
            PlayerId(2),
            "Test Creature".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state.objects.insert(obj_id, obj);

        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::ParentObjectTargetOwner,
                branches: vec![branch],
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch { player, .. } => {
                assert_eq!(*player, PlayerId(2), "owner of target should be chooser");
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }
    }

    #[test]
    fn life_lost_player_attribute_chooser_prompts_only_matching_opponents() {
        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        state.players[1].life_lost_this_turn = 3;
        state.players[2].life_lost_this_turn = 2;

        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::PlayerAttribute {
                    relation: PlayerRelation::Opponent,
                    attr: Box::new(QuantityRef::LifeLostThisTurn {
                        player: PlayerScope::ScopedPlayer,
                    }),
                    comparator: Comparator::GE,
                    value: Box::new(QuantityExpr::Fixed { value: 3 }),
                },
                branches: vec![branch],
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player,
                remaining_players,
                ..
            } => {
                assert_eq!(*player, PlayerId(1));
                assert!(remaining_players.is_empty());
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }
    }
}
