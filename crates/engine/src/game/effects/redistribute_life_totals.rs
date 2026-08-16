//! CR 119.7 + CR 119.8: Resolver for `Effect::RedistributeLifeTotals`.
//!
//! "Redistribute any number of players' life totals. (Each of those players gets
//! one life total back.)" (Reverse the Sands, The Doctor's Tomb). The controlling
//! player chooses how to reassign the participating players' current life totals
//! among themselves — a controller-chosen permutation of the life-total pool.
//!
//! Because choosing "any number of players" and permuting only that subset's
//! totals is equivalent to a permutation of the *full* candidate set that fixes
//! the unchosen players, the resolver enumerates every permutation of the
//! candidate players' current life totals, filters each receiver by CR 119.7
//! (can't gain) / CR 119.8 (can't lose), dedupes behaviorally-identical outcomes,
//! and installs `WaitingFor::RedistributeLifeTotals` for the controller to pick.
//! The identity ("keep current totals") assignment is always legal and always
//! offered. When it is the only legal outcome, the effect resolves as a rules-
//! correct no-op with no prompt.
//!
//! CR 810.9f (a player may not affect more than one member of each team) is out
//! of scope: the engine is fixed two-player with no teams.

use crate::game::static_abilities::{player_has_cant_gain_life, player_has_cant_lose_life};
use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, LifeRedistributionOption, WaitingFor};
use crate::types::player::PlayerId;

/// Factorial-blowup guard: above this many participating players the resolver
/// offers only the identity assignment. CR 119.7 + CR 119.8 places no bound on player
/// count, so the cap is an engine implementation limit; at the engine's fixed
/// two-player size the enumeration is exactly `{keep, swap}`.
const MAX_REDISTRIBUTE_CANDIDATES: usize = 6;

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 119.7 + CR 119.8: participants are the players still in the game (each keeps or
    // receives exactly one life total). Snapshot current totals in seat order.
    let candidates: Vec<(PlayerId, i32)> = state
        .players
        .iter()
        .filter(|p| !p.is_eliminated)
        .map(|p| (p.id, p.life))
        .collect();

    let options = enumerate_options(state, &candidates);

    let resolved_kind = EffectKind::from(&ability.effect);

    // CR 119.7 + CR 119.8: when the identity assignment is the only legal outcome (one
    // participant, all totals equal, or every non-identity swap is blocked by
    // can't-gain/can't-lose), the effect is a no-op — resolve without a prompt.
    if options.len() > 1 {
        state.waiting_for = WaitingFor::RedistributeLifeTotals {
            player: ability.controller,
            options,
        };
    }

    events.push(GameEvent::EffectResolved {
        kind: resolved_kind,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// Enumerate the legal, deduplicated life-total assignments for `candidates`.
/// The identity assignment is always first; non-identity permutations that are
/// legal per CR 119.7 + CR 119.8 and behaviorally distinct follow.
fn enumerate_options(
    state: &GameState,
    candidates: &[(PlayerId, i32)],
) -> Vec<LifeRedistributionOption> {
    let n = candidates.len();

    // Identity ("keep current totals") — always legal (no gain/loss).
    let identity: Vec<(PlayerId, i32)> = candidates.to_vec();
    let mut options = vec![LifeRedistributionOption {
        assignment: identity,
    }];

    if !(2..=MAX_REDISTRIBUTE_CANDIDATES).contains(&n) {
        return options;
    }

    // Dedup on the resulting-life vector (seat order); identity seeds it.
    let mut seen: Vec<Vec<i32>> = vec![candidates.iter().map(|&(_, life)| life).collect()];

    for perm in permutations(n) {
        // Player at seat i receives the current life total of the player at seat
        // perm[i] — a permutation of the life-total pool (CR 119.7 + CR 119.8).
        let resulting: Vec<i32> = (0..n).map(|i| candidates[perm[i]].1).collect();
        if seen.contains(&resulting) {
            continue;
        }
        seen.push(resulting.clone());

        // CR 119.7 / CR 119.8: a receiver can't be raised if it can't gain life,
        // nor lowered if it can't lose life. An illegal receiver rules the whole
        // assignment out.
        let legal = (0..n).all(|i| {
            let (pid, current) = candidates[i];
            match resulting[i].cmp(&current) {
                std::cmp::Ordering::Greater => !player_has_cant_gain_life(state, pid),
                std::cmp::Ordering::Less => !player_has_cant_lose_life(state, pid),
                std::cmp::Ordering::Equal => true,
            }
        });
        if !legal {
            continue;
        }

        let assignment = (0..n).map(|i| (candidates[i].0, resulting[i])).collect();
        options.push(LifeRedistributionOption { assignment });
    }

    options
}

/// All permutations of `[0, n)` (n! entries). Bounded by
/// `MAX_REDISTRIBUTE_CANDIDATES`, so this is at most 720 entries.
fn permutations(n: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut current: Vec<usize> = (0..n).collect();
    permute(&mut current, 0, &mut out);
    out
}

fn permute(arr: &mut Vec<usize>, k: usize, out: &mut Vec<Vec<usize>>) {
    if k == arr.len() {
        out.push(arr.clone());
        return;
    }
    for i in k..arr.len() {
        arr.swap(k, i);
        permute(arr, k + 1, out);
        arr.swap(k, i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ControllerRef, Effect, QuantityExpr, ReplacementDefinition, ReplacementMode,
        StaticDefinition, TargetFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;

    fn redistribute_ability(controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::RedistributeLifeTotals,
            vec![],
            ObjectId(100),
            controller,
        )
    }

    fn cant_gain_life_for(state: &mut GameState, target_player: PlayerId) {
        let lock = create_object(
            state,
            CardId(901),
            target_player,
            "Lock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&lock)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantGainLife).affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
            );
    }

    fn optional_life_reduced_replacement(state: &mut GameState, controller: PlayerId) {
        let shield = create_object(
            state,
            CardId(902),
            controller,
            "Life Shield".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&shield)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::LifeReduced)
                    .mode(ReplacementMode::Optional { decline: None })
                    .description("Life Shield".to_string()),
            );
    }

    /// CR 119.7 + CR 119.8: two-player 20/5 enumerates exactly {keep, swap}, and the
    /// resolver prompts the controller.
    #[test]
    fn two_player_offers_keep_and_swap() {
        let mut state = GameState::new_two_player(1);
        state.players[0].life = 20;
        state.players[1].life = 5;

        let ability = redistribute_ability(PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::RedistributeLifeTotals { player, options } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(options.len(), 2, "keep + swap");
                // Identity first.
                assert_eq!(
                    options[0].assignment,
                    vec![(PlayerId(0), 20), (PlayerId(1), 5)]
                );
                // Swap second.
                assert_eq!(
                    options[1].assignment,
                    vec![(PlayerId(0), 5), (PlayerId(1), 20)]
                );
            }
            other => panic!("expected RedistributeLifeTotals, got {other:?}"),
        }
    }

    /// CR 119.7: a receiver that can't gain life rules out the swap (P1 would rise
    /// 5→20), leaving only identity → auto no-op, no prompt, totals unchanged.
    #[test]
    fn cant_gain_receiver_blocks_swap_auto_noop() {
        let mut state = GameState::new_two_player(2);
        state.players[0].life = 20;
        state.players[1].life = 5;
        cant_gain_life_for(&mut state, PlayerId(1));

        let ability = redistribute_ability(PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "only identity survives → no prompt"
        );
        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[1].life, 5);
    }

    /// CR 119.7 + CR 119.8: equal totals dedupe to the identity assignment only → no prompt.
    #[test]
    fn equal_totals_dedupe_to_identity_auto_noop() {
        let mut state = GameState::new_two_player(3);
        state.players[0].life = 10;
        state.players[1].life = 10;

        let ability = redistribute_ability(PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    /// Runtime proof: resolve installs the prompt, submitting the swap option via
    /// the real resolution-choice handler swaps the totals and returns to Priority.
    #[test]
    fn submit_swap_applies_permutation() {
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(4);
        state.players[0].life = 20;
        state.players[1].life = 5;

        let ability = redistribute_ability(PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let waiting = state.waiting_for.clone();
        assert!(matches!(waiting, WaitingFor::RedistributeLifeTotals { .. }));

        // Option index 1 is the swap (index 0 is keep).
        let mut events = Vec::new();
        handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SubmitLifeRedistribution { option_index: 1 },
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].life, 5);
        assert_eq!(state.players[1].life, 20);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    /// Submitting the identity option leaves totals unchanged and returns to
    /// Priority (rules-correct "keep current totals").
    #[test]
    fn submit_identity_keeps_totals() {
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(5);
        state.players[0].life = 20;
        state.players[1].life = 5;

        let ability = redistribute_ability(PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let waiting = state.waiting_for.clone();

        let mut events = Vec::new();
        handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SubmitLifeRedistribution { option_index: 0 },
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[1].life, 5);
    }

    /// CR 608.2c: a chained sub-ability pauses on the redistribution choice and
    /// resumes only after the player submits. Drives the real
    /// `resolve_ability_chain` continuation path (proves the effect is registered
    /// in `waits_for_resolution_choice`).
    #[test]
    fn chained_sub_ability_pauses_and_resumes_after_submit() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_resolution_choices::handle_resolution_choice;

        let mut state = GameState::new_two_player(7);
        state.players[0].life = 20;
        state.players[1].life = 5;

        // Redistribute, then "controller gains 2 life" as a chained sub-ability.
        let sub = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::RedistributeLifeTotals,
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Paused on the choice; the sub-ability's life gain has NOT run yet.
        assert!(matches!(
            state.waiting_for,
            WaitingFor::RedistributeLifeTotals { .. }
        ));
        assert_eq!(state.players[0].life, 20);

        // Submit the swap (option 1) → totals swap, THEN the chained +2 runs.
        let waiting = state.waiting_for.clone();
        let mut events = Vec::new();
        handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SubmitLifeRedistribution { option_index: 1 },
            &mut events,
        )
        .unwrap();

        // Swap gave P0 5, then +2 from the resumed sub-ability = 7.
        assert_eq!(state.players[0].life, 7);
        assert_eq!(state.players[1].life, 20);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    /// CR 608.2c + CR 616.1: if submitting a redistribution option pauses on a
    /// life-gain/loss replacement choice, the real `ChooseReplacement` resume
    /// path must finish the assignment and then drain the original chained
    /// continuation.
    #[test]
    fn replacement_choice_during_submit_resumes_chained_continuation() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine::apply_as_current;
        use crate::game::engine_resolution_choices::handle_resolution_choice;

        let mut state = GameState::new_two_player(8);
        state.players[0].life = 20;
        state.players[1].life = 5;
        optional_life_reduced_replacement(&mut state, PlayerId(0));

        let sub = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::RedistributeLifeTotals,
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::RedistributeLifeTotals { .. }
        ));
        assert!(
            state.active_ability_continuation().is_some(),
            "sub-ability must be stashed while redistribution waits"
        );

        let waiting = state.waiting_for.clone();
        let mut events = Vec::new();
        handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SubmitLifeRedistribution { option_index: 1 },
            &mut events,
        )
        .unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[1].life, 5);
        assert!(
            state.active_life_total_assignment().is_some(),
            "remaining redistribution deltas must survive the replacement prompt"
        );

        let WaitingFor::ReplacementChoice { player, .. } = state.waiting_for.clone() else {
            panic!("expected replacement choice");
        };
        state.active_player = player;
        state.priority_player = player;

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept life-loss replacement");

        assert_eq!(state.players[0].life, 7, "swap to 5, then chained +2");
        assert_eq!(state.players[1].life, 20);
        assert!(state.active_life_total_assignment().is_none());
        assert!(state.active_ability_continuation().is_none());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    /// An out-of-range option index is rejected.
    #[test]
    fn out_of_range_index_is_rejected() {
        use crate::game::engine::EngineError;
        use crate::game::engine_resolution_choices::handle_resolution_choice;

        let mut state = GameState::new_two_player(6);
        state.players[0].life = 20;
        state.players[1].life = 5;

        let ability = redistribute_ability(PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let waiting = state.waiting_for.clone();

        let mut events = Vec::new();
        let result = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SubmitLifeRedistribution { option_index: 99 },
            &mut events,
        );
        assert!(matches!(result, Err(EngineError::InvalidAction(_))));
    }
}
