//! CR 701.38: Vote — Council's dilemma family.
//!
//! Each player, starting with a specified player and proceeding in turn order
//! (CR 101.4), chooses one of the listed options. After every player has cast
//! their votes, the per-choice sub-effects resolve once for each vote tallied
//! against that choice.
//!
//! CR 701.38d: A player who has multiple votes (granted by a static ability
//! such as Tivit's "While voting, you may vote an additional time") makes
//! those choices at the same time they would otherwise have voted.
//!
//! The resolver entry point sets `WaitingFor::VoteChoice` for the starting
//! voter, embeds `per_choice_effect` directly on the `WaitingFor` (so the
//! tally flows through state filtering and live multiplayer echoes without
//! reaching back into the source ability), and stashes only the parent's
//! post-Vote sub_ability on a pending continuation. The
//! `engine_resolution_choices.rs` handler tallies each vote, advances voters
//! in APNAP order, and finally calls `resolve_tally` to fan out the per-choice
//! sub-effects.

use crate::types::ability::{
    AbilityDefinition, ControllerRef, Effect, EffectError, EffectKind, QuantityExpr,
    ResolvedAbility,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingContinuation, WaitingFor};
use crate::types::player::PlayerId;

use super::resolve_ability_chain;

/// CR 701.38a + CR 101.4: Initiate a vote. Builds the APNAP voter queue
/// starting from `starting_with` (resolved against the ability controller),
/// computes each voter's total votes (1 + extra-vote grants from
/// `Player::extra_votes_per_session`), and parks on `WaitingFor::VoteChoice`
/// for the first voter.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::Vote {
        choices,
        per_choice_effect,
        starting_with,
    } = &ability.effect
    else {
        return Err(EffectError::InvalidParam(
            "vote::resolve called with non-Vote effect".into(),
        ));
    };

    // Parser invariant: one sub-effect per choice. Surfaced as a hard error so
    // misparses fail fast rather than silently dropping ballots.
    if choices.len() != per_choice_effect.len() {
        return Err(EffectError::InvalidParam(format!(
            "Effect::Vote choices/per_choice_effect length mismatch: {} vs {}",
            choices.len(),
            per_choice_effect.len()
        )));
    }
    if choices.is_empty() {
        return Err(EffectError::InvalidParam(
            "Effect::Vote requires at least one choice".into(),
        ));
    }

    let controller = ability.controller;
    let starting_player = resolve_starting_voter(state, controller, starting_with.clone());

    // CR 101.4 + CR 701.38a: Build APNAP voter order from the starting player.
    let voters_in_order = apnap_order_from(state, starting_player);
    if voters_in_order.is_empty() {
        // No eligible voters (e.g., everyone eliminated). Emit EffectResolved
        // and let the chain continue — nothing to tally.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Vote,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let voter_queue: Vec<(PlayerId, u32)> = voters_in_order
        .into_iter()
        .map(|pid| (pid, votes_per_session_for(state, pid)))
        .collect();

    let (first_player, first_votes) = voter_queue[0];
    let remaining_voters = voter_queue[1..].to_vec();

    // Display labels: title-case each choice for the modal. Engine compares
    // votes against the lowercase canonical `choices` field.
    let option_labels: Vec<String> = choices.iter().map(|c| title_case_word(c)).collect();
    let tallies = vec![0u32; choices.len()];

    state.waiting_for = WaitingFor::VoteChoice {
        player: first_player,
        remaining_votes: first_votes,
        options: choices.clone(),
        option_labels,
        remaining_voters,
        tallies,
        per_choice_effect: per_choice_effect.clone(),
        controller,
        source_id: ability.source_id,
    };

    // Stash the parent's sub_ability tail so it resumes after the tally fans
    // out. The Vote effect itself does NOT belong on the continuation — the
    // tally handler in engine_resolution_choices.rs explicitly calls
    // `resolve_tally`, then drains this continuation to run any post-Vote
    // chained effects. Mirrors clash::stash_sub.
    if let Some(sub) = ability.sub_ability.as_ref() {
        state.pending_continuation = Some(PendingContinuation::new(sub.clone()));
    }

    Ok(())
}

/// CR 701.38: After every voter has cast all their votes, fan out the per-choice
/// sub-effects. For each `i`, `per_choice_effect[i]` is resolved once per vote
/// tallied for `choices[i]`. Sub-effect resolutions inherit the source object
/// and controller of the originating Vote ability.
///
/// Called from `engine_resolution_choices.rs` once the voter queue empties.
pub fn resolve_tally(
    state: &mut GameState,
    source_id: crate::types::identifiers::ObjectId,
    controller: PlayerId,
    options: &[String],
    per_choice_effect: &[Box<AbilityDefinition>],
    tallies: &[u32],
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    debug_assert_eq!(options.len(), per_choice_effect.len());
    debug_assert_eq!(options.len(), tallies.len());

    for (idx, votes) in tallies.iter().enumerate() {
        if *votes == 0 {
            continue;
        }
        // Resolve `per_choice_effect[idx]` once per vote via repeat_for.
        // CR 609.3: repeat_for runs the same effect N times within the same
        // ability resolution, exactly mirroring "for each [X] vote, [effect]".
        let chain = ResolvedAbility {
            effect: (*per_choice_effect[idx].effect).clone(),
            targets: Vec::new(),
            source_id,
            controller,
            kind: per_choice_effect[idx].kind,
            sub_ability: per_choice_effect[idx]
                .sub_ability
                .as_ref()
                .map(|sub| Box::new(resolved_from_def(sub, source_id, controller))),
            else_ability: None,
            duration: per_choice_effect[idx].duration.clone(),
            condition: per_choice_effect[idx].condition.clone(),
            context: Default::default(),
            optional_targeting: per_choice_effect[idx].optional_targeting,
            optional: per_choice_effect[idx].optional,
            optional_for: None,
            multi_target: None,
            description: per_choice_effect[idx].description.clone(),
            repeat_for: Some(QuantityExpr::Fixed {
                value: *votes as i32,
            }),
            forward_result: per_choice_effect[idx].forward_result,
            unless_pay: None,
            distribution: None,
            player_scope: None,
            chosen_x: None,
            ability_index: None,
        };
        resolve_ability_chain(state, &chain, events, 0)?;
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Vote,
        source_id,
    });
    Ok(())
}

/// Convert a stored `AbilityDefinition` (typically a sub-effect) into a
/// `ResolvedAbility` carrying the same source/controller as the parent Vote.
fn resolved_from_def(
    def: &AbilityDefinition,
    source_id: crate::types::identifiers::ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    ResolvedAbility {
        effect: (*def.effect).clone(),
        targets: Vec::new(),
        source_id,
        controller,
        kind: def.kind,
        sub_ability: def
            .sub_ability
            .as_ref()
            .map(|sub| Box::new(resolved_from_def(sub, source_id, controller))),
        else_ability: None,
        duration: def.duration.clone(),
        condition: def.condition.clone(),
        context: Default::default(),
        optional_targeting: def.optional_targeting,
        optional: def.optional,
        optional_for: None,
        multi_target: None,
        description: def.description.clone(),
        repeat_for: None,
        forward_result: def.forward_result,
        unless_pay: None,
        distribution: None,
        player_scope: None,
        chosen_x: None,
        ability_index: None,
    }
}

/// CR 701.38a: Resolve `ControllerRef::You` (and friends) to the concrete
/// starting voter PlayerId. Falls back to `controller` if the ref doesn't
/// resolve to a non-eliminated player.
fn resolve_starting_voter(
    _state: &GameState,
    controller: PlayerId,
    starting_with: ControllerRef,
) -> PlayerId {
    match starting_with {
        ControllerRef::You => controller,
        // Other refs (TargetPlayer, etc.) are not currently produced by the
        // Council's dilemma parser. Default to controller — extending this is
        // a one-line change when "starting with the affected player" / similar
        // phrasings appear.
        _ => controller,
    }
}

/// CR 101.4: Build a turn-order voter sequence beginning with `start`, walking
/// forward through PlayerId order and skipping eliminated players. Supports
/// arbitrary player counts (multiplayer).
fn apnap_order_from(state: &GameState, start: PlayerId) -> Vec<PlayerId> {
    let n = state.players.len();
    if n == 0 {
        return Vec::new();
    }
    let start_idx = state
        .players
        .iter()
        .position(|p| p.id == start)
        .unwrap_or(0);
    (0..n)
        .map(|offset| (start_idx + offset) % n)
        .filter_map(|i| {
            let p = &state.players[i];
            (!p.is_eliminated).then_some(p.id)
        })
        .collect()
}

/// CR 701.38d: A player's total votes for one Council's dilemma session is
/// 1 plus the count of `StaticMode::GrantsExtraVote` permanents the player
/// currently controls (Tivit, Seller of Secrets — "While voting, you may vote
/// an additional time").
///
/// Snapshotted once at vote-session start (CR 701.38d: extra votes happen at
/// the same time the player would otherwise have voted), so granting
/// permanents that enter or leave mid-session do not retroactively change
/// vote counts.
fn votes_per_session_for(state: &GameState, player: PlayerId) -> u32 {
    use crate::game::functioning_abilities::active_static_definitions;
    use crate::types::statics::StaticMode;

    let mut extras: u32 = 0;
    for &src_id in state.battlefield.iter() {
        let Some(obj) = state.objects.get(&src_id) else {
            continue;
        };
        if obj.controller != player {
            continue;
        }
        for s in active_static_definitions(state, obj) {
            if matches!(s.mode, StaticMode::GrantsExtraVote) {
                extras = extras.saturating_add(1);
            }
        }
    }
    1 + extras
}

/// Title-case the first character of a single word for display labels. The
/// engine never compares against this value — only `options` (lowercase).
fn title_case_word(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::AbilityKind;
    use crate::types::identifiers::ObjectId;

    /// CR 701.38a + CR 101.4: Initiating a Vote sets `WaitingFor::VoteChoice`
    /// for the controller, queuing the opponent next, with no extra-vote
    /// granters present (so each player gets exactly 1 vote).
    #[test]
    fn vote_initiates_with_controller_first() {
        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;

        let inv_def = AbilityDefinition::new(AbilityKind::Spell, Effect::Investigate);
        let token_def = AbilityDefinition::new(AbilityKind::Spell, Effect::Investigate); // simple stand-in

        let ability = ResolvedAbility {
            effect: Effect::Vote {
                choices: vec!["evidence".to_string(), "bribery".to_string()],
                per_choice_effect: vec![Box::new(inv_def), Box::new(token_def)],
                starting_with: ControllerRef::You,
            },
            targets: vec![],
            source_id: ObjectId(1),
            controller,
            kind: AbilityKind::Spell,
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: Default::default(),
            optional_targeting: false,
            optional: false,
            optional_for: None,
            multi_target: None,
            description: None,
            repeat_for: None,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            player_scope: None,
            chosen_x: None,
            ability_index: None,
        };

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("vote resolves");

        match state.waiting_for {
            WaitingFor::VoteChoice {
                player,
                remaining_votes,
                ref options,
                ref tallies,
                ref remaining_voters,
                ..
            } => {
                assert_eq!(player, controller);
                assert_eq!(remaining_votes, 1);
                assert_eq!(
                    options,
                    &vec!["evidence".to_string(), "bribery".to_string()]
                );
                assert_eq!(tallies, &vec![0u32, 0]);
                // Opponent queued next with their 1 vote.
                assert_eq!(remaining_voters.len(), 1);
                assert_ne!(remaining_voters[0].0, controller);
                assert_eq!(remaining_voters[0].1, 1);
            }
            other => panic!("expected VoteChoice, got {:?}", other),
        }
    }
}
