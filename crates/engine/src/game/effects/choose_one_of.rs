use std::collections::HashSet;

use crate::game::ability_utils::build_resolved_from_def;
use crate::game::players;
use crate::types::ability::{
    AbilityDefinition, Effect, EffectError, EffectKind, ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingChooseOneOf, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ReplacementId;

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
    if players.is_empty() {
        // CR 608.2d: A branch choice must be made by an eligible player. An
        // empty chooser set means the effect cannot legally begin — fail loud
        // instead of silently resolving nothing (issue #927 class).
        return Err(EffectError::InvalidParam(format!(
            "ChooseOneOf: no eligible player for chooser {chooser:?}"
        )));
    }
    prompt_next(
        state,
        PromptRequest {
            controller: ability.controller,
            source_id: ability.source_id,
            branches,
            parent_targets: ability.targets.clone(),
            context: ability.context.clone(),
            replacement_applied: ability.replacement_applied.clone(),
            players,
        },
    );

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseOneOf,
        source_id: ability.source_id,
    });
    Ok(())
}

pub(crate) struct PromptRequest {
    pub controller: PlayerId,
    pub source_id: ObjectId,
    pub branches: Vec<AbilityDefinition>,
    pub parent_targets: Vec<TargetRef>,
    pub context: crate::types::ability::SpellContext,
    pub replacement_applied: HashSet<ReplacementId>,
    pub players: Vec<PlayerId>,
}

pub(crate) fn prompt_next(state: &mut GameState, request: PromptRequest) {
    let PromptRequest {
        controller,
        source_id,
        branches,
        parent_targets,
        context,
        replacement_applied,
        mut players,
    } = request;
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
        replacement_applied,
        remaining_players: players,
    };
    // `priority_player` routing to the chooser is owned by the centralized
    // post-apply sync (`public_state::sync_priority_player_from_waiting_for`),
    // which maps `WaitingFor::ChooseOneOfBranch { player, .. }` through
    // `turn_control::authorized_submitter_for_player` (CR 608.2d).
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
        PromptRequest {
            controller: pending.controller,
            source_id: pending.source_id,
            branches: pending.branches,
            parent_targets: pending.parent_targets,
            context: pending.context,
            replacement_applied: pending.replacement_applied,
            players: pending.remaining_players,
        },
    );
}

pub(crate) struct BranchSelection {
    pub player: PlayerId,
    pub controller: PlayerId,
    pub source_id: ObjectId,
    pub branches: Vec<AbilityDefinition>,
    pub parent_targets: Vec<TargetRef>,
    pub context: crate::types::ability::SpellContext,
    pub replacement_applied: HashSet<ReplacementId>,
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
        replacement_applied,
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
        replacement_applied: replacement_applied.clone(),
        remaining_players,
    });

    let mut resolved = build_resolved_from_def(branch, source_id, controller);
    resolved.context = context;
    resolved.targets = parent_targets;
    resolved.set_replacement_applied_recursive(replacement_applied);
    resolved.set_scoped_player_recursive(player);
    if !resolved
        .targets
        .iter()
        .any(|target| matches!(target, TargetRef::Player(pid) if *pid == player))
    {
        resolved.targets.push(TargetRef::Player(player));
    }

    super::resolve_ability_chain(state, &resolved, events, 1)?;
    resume_pending(state, events);
    // NOTE: the token-choice applied seed is intentionally NOT cleared here.
    // A branch may stash a token-bearing sub-ability into `pending_continuation`
    // (effects/mod.rs) that drains only later, from the ChooseBranch handler at
    // `engine_resolution_choices.rs` via `drain_pending_continuation`. Clearing
    // here — just because `waiting_for` is momentarily back at Priority — would
    // wipe the seed before that stashed token sub-ability proposes, re-prompting
    // the originating token-choice replacement (issue #4886, review #3). The
    // seed is cleared at true full-drain in `drain_pending_continuation`
    // (Priority + no pending_continuation + no pending_repeat_iteration).
    Ok(())
}

fn choosing_players(
    state: &GameState,
    ability: &ResolvedAbility,
    chooser: &crate::types::ability::PlayerFilter,
) -> Vec<PlayerId> {
    use crate::types::ability::PlayerFilter;

    let apnap = players::apnap_order(state);

    // CR 608.2c + CR 108.3 + CR 109.4: Three chooser filters are anchored to
    // resolution-scoped state that `matches_player_scope` cannot see (it carries
    // no `ResolvedAbility`): `ChosenPlayer` reads the player chosen earlier this
    // resolution from `ability.chosen_players`; `ParentObjectTargetOwner` reads
    // the owner of the ability's first object target (CR 108.3); and
    // `ParentObjectTargetController` reads its controller (CR 109.4) — the chooser
    // for "that creature's controller faces a villainous choice" (Hunted by The
    // Family), where the targeted creature's controller (not owner) makes the
    // choice and the two differ for a stolen creature. Resolve them here — this
    // is the one caller that has the ability in scope — and order the result in
    // APNAP (CR 701.55d). All filter out eliminated players (CR 104.3a — a player
    // who loses leaves the game and can no longer be a chooser) and yield a
    // single chooser, which is correct for the villainous-choice patterns these
    // power (The Master, This Is How It Ends, Hunted by The Family).
    let anchored: Option<PlayerId> = match chooser {
        PlayerFilter::ChosenPlayer { index } => {
            ability.chosen_players.get(*index as usize).copied()
        }
        PlayerFilter::ParentObjectTargetOwner => {
            crate::game::ability_utils::parent_target_owner(ability, state)
        }
        PlayerFilter::ParentObjectTargetController => {
            crate::game::ability_utils::parent_target_controller(ability, state)
        }
        _ => None,
    };
    if let Some(player) = anchored {
        let alive = state
            .players
            .iter()
            .any(|p| p.id == player && !p.is_eliminated);
        let players = if alive { vec![player] } else { Vec::new() };
        return expand_extra_villainous_instances(state, players);
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
        let players = apnap
            .into_iter()
            .filter(|player| targeted.contains(player))
            .collect();
        return expand_extra_villainous_instances(state, players);
    }

    let players = apnap
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
        .collect();
    expand_extra_villainous_instances(state, players)
}

/// CR 701.55c: Count the number of ADDITIONAL villainous-choice instances a
/// `facing` player must perform — one per active `GrantsExtraVillainousChoice`
/// static on a battlefield permanent controlled by an OPPONENT of that player
/// (The Valeyard — "If an opponent would face a villainous choice, they face
/// that choice an additional time."). Returns the additional count (default 0),
/// not `1 + count`: the base instance is already represented by the player's
/// single occurrence in the facing-player list.
///
/// This is the controller-inverted mirror of `vote::votes_per_session_for`
/// (CR 701.38d), where the source is controlled by the voting player themselves;
/// here the source (the Valeyard) is controlled by the facing player's opponent.
fn villainous_extra_instances_for(state: &GameState, facing: PlayerId) -> u32 {
    use crate::game::functioning_abilities::active_static_definitions;
    use crate::types::statics::StaticMode;

    let mut extras: u32 = 0;
    for &src_id in state.battlefield.iter() {
        let Some(obj) = state.objects.get(&src_id) else {
            continue;
        };
        if !players::is_opponent(state, facing, obj.controller) {
            continue;
        }
        for s in active_static_definitions(state, obj) {
            if matches!(s.mode, StaticMode::GrantsExtraVillainousChoice) {
                extras = extras.saturating_add(1);
            }
        }
    }
    extras
}

/// CR 701.55c + CR 701.55d: Expand a facing-player list so each player appears
/// once per total instance of the villainous choice they must face — their base
/// occurrence plus `villainous_extra_instances_for` additional copies, inserted
/// consecutively so APNAP order (CR 701.55d) across distinct players is
/// preserved while each player resolves all of their instances one at a time
/// (CR 701.55c).
fn expand_extra_villainous_instances(state: &GameState, players: Vec<PlayerId>) -> Vec<PlayerId> {
    let mut expanded = Vec::with_capacity(players.len());
    for p in players {
        expanded.push(p);
        for _ in 0..villainous_extra_instances_for(state, p) {
            expanded.push(p);
        }
    }
    expanded
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
    fn explicit_branch_descriptions_reach_waiting_for_prompt() {
        let mut state = GameState::new_two_player(42);
        let colorless = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .description("Colorless".to_string());
        let white = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .description("White".to_string());
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![colorless, white],
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                branch_descriptions,
                ..
            } => {
                assert_eq!(
                    branch_descriptions,
                    &vec!["Colorless".to_string(), "White".to_string()]
                );
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }
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

    /// CR 701.55c (cluster 32, Class D — The Valeyard): A
    /// `GrantsExtraVillainousChoice` static on a battlefield permanent
    /// controlled by an OPPONENT of the facing player makes that player face the
    /// choice one additional time. The facing-player list expands so the player
    /// appears twice consecutively (base + 1 extra); without the static they
    /// appear exactly once. Tests the building block
    /// (`expand_extra_villainous_instances`) via the live resolver, not a card.
    #[test]
    fn villainous_choice_doubled_when_opponent_controls_extra_instance_static() {
        // Player 1 faces the choice; player 0 controls a Valeyard-like source.
        let mut state = GameState::new(FormatConfig::commander(), 3, 42);

        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );

        // Sanity baseline: with no extra-instance source, player 1 faces the
        // choice exactly once (no remaining players queued for a re-face).
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::ParentObjectTargetOwner,
                branches: vec![branch.clone()],
            },
            vec![TargetRef::Object(ObjectId(99))],
            ObjectId(1),
            PlayerId(0),
        );
        // Bind the parent target to an object owned by player 1.
        let obj_id = ObjectId(99);
        let target_obj = crate::game::game_object::GameObject::new(
            obj_id,
            crate::types::identifiers::CardId(0),
            PlayerId(1),
            "Faced Creature".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state.objects.insert(obj_id, target_obj);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player,
                remaining_players,
                ..
            } => {
                assert_eq!(*player, PlayerId(1));
                assert!(
                    remaining_players.is_empty(),
                    "without an extra-instance static the facing player faces the choice once"
                );
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }

        // Now add a Valeyard-like permanent controlled by player 0 (an opponent
        // of the facing player 1) carrying GrantsExtraVillainousChoice.
        let valeyard_id = ObjectId(50);
        let mut valeyard = crate::game::game_object::GameObject::new(
            valeyard_id,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "The Valeyard".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        valeyard.static_definitions.push(
            crate::types::ability::StaticDefinition::new(
                crate::types::statics::StaticMode::GrantsExtraVillainousChoice,
            )
            .affected(TargetFilter::Player),
        );
        state.objects.insert(valeyard_id, valeyard);
        state.battlefield.push_back(valeyard_id);

        let mut events2 = Vec::new();
        resolve(&mut state, &ability, &mut events2).unwrap();
        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player,
                remaining_players,
                ..
            } => {
                assert_eq!(*player, PlayerId(1));
                // CR 701.55c: the same player faces the choice one more time,
                // queued consecutively right after their first instance.
                assert_eq!(
                    remaining_players.as_slice(),
                    &[PlayerId(1)],
                    "the facing player must face the choice twice (base + 1 extra)"
                );
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }
    }

    /// CR 701.55c (cluster 32, Class D): an extra-instance source controlled by
    /// the FACING player themselves (not an opponent) grants no extra instance —
    /// the static reads "if an OPPONENT would face a villainous choice". Guards
    /// the controller-inversion in `villainous_extra_instances_for`.
    #[test]
    fn villainous_choice_not_doubled_by_self_controlled_static() {
        let mut state = GameState::new(FormatConfig::commander(), 3, 42);

        let obj_id = ObjectId(99);
        let target_obj = crate::game::game_object::GameObject::new(
            obj_id,
            crate::types::identifiers::CardId(0),
            PlayerId(1),
            "Faced Creature".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state.objects.insert(obj_id, target_obj);

        // Source controlled by player 1 (the facing player) — must NOT count.
        let src_id = ObjectId(50);
        let mut src = crate::game::game_object::GameObject::new(
            src_id,
            crate::types::identifiers::CardId(1),
            PlayerId(1),
            "Self Valeyard".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        src.static_definitions.push(
            crate::types::ability::StaticDefinition::new(
                crate::types::statics::StaticMode::GrantsExtraVillainousChoice,
            )
            .affected(TargetFilter::Player),
        );
        state.objects.insert(src_id, src);
        state.battlefield.push_back(src_id);

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
            WaitingFor::ChooseOneOfBranch {
                player,
                remaining_players,
                ..
            } => {
                assert_eq!(*player, PlayerId(1));
                assert!(
                    remaining_players.is_empty(),
                    "a self-controlled extra-instance static must not double the choice"
                );
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }
    }
}
