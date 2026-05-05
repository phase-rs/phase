use crate::game::ability_utils::build_resolved_from_def;
use crate::game::players;
use crate::types::ability::{
    AbilityDefinition, Effect, EffectError, EffectKind, ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingChooseOneOf, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

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

    let players = choosing_players(state, ability, chooser);
    prompt_next(
        state,
        ability.controller,
        ability.source_id,
        branches,
        ability.targets.clone(),
        ability.context.clone(),
        players,
    );

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseOneOf,
        source_id: ability.source_id,
    });
    Ok(())
}

pub(crate) fn prompt_next(
    state: &mut GameState,
    controller: PlayerId,
    source_id: ObjectId,
    branches: Vec<AbilityDefinition>,
    parent_targets: Vec<TargetRef>,
    context: crate::types::ability::SpellContext,
    mut players: Vec<PlayerId>,
) {
    let Some(player) = players.first().copied() else {
        return;
    };
    players.remove(0);
    let branch_descriptions = branch_descriptions(&branches);
    state.waiting_for = WaitingFor::ChooseOneOfBranch {
        player,
        controller,
        source_id,
        branches,
        branch_descriptions,
        parent_targets,
        context,
        remaining_players: players,
    };
}

pub(crate) fn resume_pending(state: &mut GameState, _events: &mut Vec<GameEvent>) {
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        return;
    }
    let Some(pending) = state.pending_choose_one_of.take() else {
        return;
    };
    prompt_next(
        state,
        pending.controller,
        pending.source_id,
        pending.branches,
        pending.parent_targets,
        pending.context,
        pending.remaining_players,
    );
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
        remaining_players,
        index,
    } = selection;
    let Some(branch) = branches.get(index) else {
        return Err(EffectError::InvalidParam(format!(
            "ChooseOneOf branch index {index} out of range"
        )));
    };

    state.pending_choose_one_of = (!remaining_players.is_empty()).then(|| PendingChooseOneOf {
        controller,
        source_id,
        branches: branches.clone(),
        parent_targets: parent_targets.clone(),
        context: context.clone(),
        remaining_players,
    });

    let mut resolved = build_resolved_from_def(branch, source_id, controller);
    resolved.context = context;
    resolved.targets = parent_targets;
    if !resolved
        .targets
        .iter()
        .any(|target| matches!(target, TargetRef::Player(pid) if *pid == player))
    {
        resolved.targets.push(TargetRef::Player(player));
    }

    super::resolve_ability_chain(state, &resolved, events, 1)?;
    resume_pending(state, events);
    Ok(())
}

fn choosing_players(
    state: &GameState,
    ability: &ResolvedAbility,
    chooser: &crate::types::ability::PlayerFilter,
) -> Vec<PlayerId> {
    let apnap = players::apnap_order(state);
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
            branch
                .description
                .clone()
                .unwrap_or_else(|| format!("Option {}", index + 1))
        })
        .collect()
}
