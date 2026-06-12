use std::collections::HashSet;

use rand::Rng;

use crate::game::quantity::resolve_quantity;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    AbilityDefinition, Effect, EffectError, EffectKind, ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCoinFlip, PendingCoinFlipKind, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;

use super::resolve_ability_chain;

/// CR 705.1 + CR 614.1a: Outcome of routing a single logical coin flip through
/// the replacement pipeline.
enum CoinFlipOutcome {
    /// Exactly one coin was flipped (no Krark-style doubling). `CoinFlipped` has
    /// already been pushed; the bool is the won/heads result (CR 705.2).
    Resolved(bool),
    /// The flip was doubled (Krark's Thumb). The controller must keep one of the
    /// `results` via `WaitingFor::CoinFlipKeepChoice`, which is already set. No
    /// `CoinFlipped` was pushed yet — that happens in `resume_after_keep` once the
    /// player keeps a flip (CR 614.1a: the ignored flips never "happen").
    Suspended,
    /// CR 614.6: a replacement prevented the flip entirely (the event never
    /// happens). No `CoinFlipped` pushed; the caller skips this flip's branch.
    Prevented,
}

/// CR 705.1 + CR 614.1a: Route one logical coin flip through the CR 614
/// replacement pipeline before touching the RNG, mirroring `draw`/`scry`/`mill`.
///
/// Krark's Thumb replaces each individual flip with "flip two and ignore one",
/// so the pipeline may return a doubled `count`. When `count == 1` the flip is
/// performed and `CoinFlipped` emitted inline (`Resolved`). When `count > 1` the
/// coins are flipped but the controller must keep one — the resolver suspends on
/// `WaitingFor::CoinFlipKeepChoice` (`Suspended`) and `resume_after_keep` emits
/// the single surviving `CoinFlipped`.
fn flip_through_replacement(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> CoinFlipOutcome {
    let proposed = ProposedEvent::CoinFlip {
        player_id: player,
        count: 1,
        applied: HashSet::new(),
    };

    let count = match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(ProposedEvent::CoinFlip { count, .. }) => count,
        // A different event was substituted, or nothing matched cleanly — treat
        // as a normal single flip rather than guessing at a foreign event.
        ReplacementResult::Execute(_) => 1,
        ReplacementResult::Prevented => return CoinFlipOutcome::Prevented,
        ReplacementResult::NeedsChoice(choice_player) => {
            // CR 614 interactive replacement (none ship for CoinFlip today, but
            // stay correct if one is added): defer to the replacement-choice UI.
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(choice_player, state);
            return CoinFlipOutcome::Suspended;
        }
    };

    if count == 0 {
        return CoinFlipOutcome::Prevented;
    }

    // CR 705.1: flip each coin with the game's seeded RNG.
    let results: Vec<bool> = (0..count).map(|_| state.rng.random_bool(0.5)).collect();

    if count == 1 {
        let won = results[0];
        events.push(GameEvent::CoinFlipped {
            player_id: player,
            won,
        });
        CoinFlipOutcome::Resolved(won)
    } else {
        // CR 614.1a + CR 705.1: Krark's Thumb — keep one, ignore the rest. The
        // kept flip's `CoinFlipped` is emitted in `resume_after_keep` so the
        // ignored flips never "happen".
        state.waiting_for = WaitingFor::CoinFlipKeepChoice {
            player,
            results,
            keep_count: 1,
        };
        CoinFlipOutcome::Suspended
    }
}

/// CR 705.2: Execute a flip's win/lose branch, preserving its
/// `optional`/`sub_ability`/`condition`/`duration` via the canonical converter.
fn run_flip_branch(
    state: &mut GameState,
    branch: Option<&AbilityDefinition>,
    source_id: ObjectId,
    controller: PlayerId,
    targets: &[TargetRef],
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if let Some(def) = branch {
        let sub = crate::game::ability_utils::build_resolved_from_def_with_targets(
            def,
            source_id,
            controller,
            targets.to_vec(),
        );
        resolve_ability_chain(state, &sub, events, 0)?;
    }
    Ok(())
}

/// CR 705: Flip a coin and optionally execute win/lose effects.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (win_effect, lose_effect) = match &ability.effect {
        Effect::FlipCoin {
            win_effect,
            lose_effect,
        } => (win_effect.as_deref(), lose_effect.as_deref()),
        _ => return Err(EffectError::MissingParam("FlipCoin".to_string())),
    };

    // CR 705.1 + CR 614.1a: route the flip through the replacement pipeline so
    // Krark's Thumb can double it.
    let prior_waiting_for = state.waiting_for.clone();
    let won = match flip_through_replacement(state, ability.controller, events) {
        CoinFlipOutcome::Resolved(won) => won,
        CoinFlipOutcome::Prevented => {
            // CR 614.6: the flip never happened — no branch, report resolved.
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::FlipCoin,
                source_id: ability.source_id,
            });
            return Ok(());
        }
        CoinFlipOutcome::Suspended => {
            // CR 614.1a + CR 705.1: doubled flip — stash the resolution context so
            // `resume_after_keep` can run the kept flip's branch. `EffectResolved`
            // is deferred until the keep choice resolves.
            state.pending_coin_flip = Some(PendingCoinFlip {
                source_id: ability.source_id,
                controller: ability.controller,
                targets: ability.targets.clone(),
                win_effect: win_effect.map(|d| Box::new(d.clone())),
                lose_effect: lose_effect.map(|d| Box::new(d.clone())),
                kind: PendingCoinFlipKind::Single,
            });
            return Ok(());
        }
    };

    // CR 705.2: Execute the appropriate branch. Use the canonical converter so
    // the branch's `optional`, `sub_ability`, `condition`, and `duration` survive
    // — `ResolvedAbility::new` would discard them, dropping e.g. Ral, Monsoon
    // Mage's "you may exile Ral" prompt and his return-transformed sub-ability
    // (CR 712.8e: a nonmodal double-faced permanent put onto the battlefield
    // transformed has its back face up).
    let branch = if won { win_effect } else { lose_effect };
    run_flip_branch(
        state,
        branch,
        ability.source_id,
        ability.controller,
        &ability.targets,
        events,
    )?;

    // CR 608.2c: if an optional branch suspended for `WaitingFor::OptionalEffectChoice`,
    // the controller has not yet finished following the instructions in order — defer
    // `EffectResolved` until the player has chosen. Mirrors the `prior_waiting_for`
    // guard in `pay.rs::resolve_ability_cost_payment`.
    if state.waiting_for == prior_waiting_for {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::FlipCoin,
            source_id: ability.source_id,
        });
    }

    Ok(())
}

/// CR 705: Flip N coins. For each flip that comes up heads (won), execute
/// `win_effect`; for each that comes up tails (lost), execute `lose_effect`.
/// Generalization of `resolve` for "flip N coins" patterns where the Oracle
/// text binds the heads count to a downstream effect (e.g., Ral Zarek's -7:
/// target opponent skips one turn per heads).
pub fn resolve_flip_coins(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count_expr, win_effect, lose_effect) = match &ability.effect {
        Effect::FlipCoins {
            count,
            win_effect,
            lose_effect,
        } => (count, win_effect.as_deref(), lose_effect.as_deref()),
        _ => return Err(EffectError::MissingParam("FlipCoins".to_string())),
    };

    // CR 107.1: resolve `count` in the ability's context; clamp at zero.
    let n =
        resolve_quantity(state, count_expr, ability.controller, ability.source_id).max(0) as u32;

    // CR 705.1 + CR 614.1a: Flip each coin through the replacement pipeline (so
    // Krark's Thumb can double it), routing each outcome through the appropriate
    // branch exactly as the single-flip resolver does.
    let prior_waiting_for = state.waiting_for.clone();
    for i in 0..n {
        let won = match flip_through_replacement(state, ability.controller, events) {
            CoinFlipOutcome::Resolved(won) => won,
            // CR 614.6: this flip was prevented entirely — skip its branch.
            CoinFlipOutcome::Prevented => continue,
            CoinFlipOutcome::Suspended => {
                // CR 614.1a: doubled flip — stash loop position and resume after
                // the keep choice. `remaining` excludes the paused flip itself.
                state.pending_coin_flip = Some(PendingCoinFlip {
                    source_id: ability.source_id,
                    controller: ability.controller,
                    targets: ability.targets.clone(),
                    win_effect: win_effect.map(|d| Box::new(d.clone())),
                    lose_effect: lose_effect.map(|d| Box::new(d.clone())),
                    kind: PendingCoinFlipKind::FlipN {
                        remaining: n - i - 1,
                    },
                });
                return Ok(());
            }
        };
        let branch = if won { win_effect } else { lose_effect };
        run_flip_branch(
            state,
            branch,
            ability.source_id,
            ability.controller,
            &ability.targets,
            events,
        )?;
        // CR 608.2c: a branch may suspend for an optional choice; stop flipping
        // until the player resolves it.
        if state.waiting_for != prior_waiting_for {
            break;
        }
    }

    // CR 608.2c: defer `EffectResolved` if a branch suspended for a player choice.
    if state.waiting_for == prior_waiting_for {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::FlipCoins,
            source_id: ability.source_id,
        });
    }

    Ok(())
}

/// CR 705: Flip coins until you lose a flip, then execute effect.
pub fn resolve_until_lose(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let win_effect = match &ability.effect {
        Effect::FlipCoinUntilLose { win_effect } => win_effect.as_ref(),
        _ => return Err(EffectError::MissingParam("FlipCoinUntilLose".to_string())),
    };

    // CR 705 + CR 614.1a: Flip coins until a flip is lost, routing each flip
    // through the replacement pipeline (Krark's Thumb doubles each flip). Count
    // the wins, then run the win effect once per win.
    let win_count = match flip_until_lose_loop(
        state,
        ability.controller,
        win_effect,
        &ability.targets,
        ability.source_id,
        0,
        events,
    )? {
        Some(count) => count,
        // CR 614.1a: a flip suspended for a Krark's Thumb keep choice — the
        // pending state is stashed; `resume_after_keep` will continue.
        None => return Ok(()),
    };

    finish_until_lose(
        state,
        win_count,
        win_effect,
        &ability.targets,
        ability.source_id,
        ability.controller,
        events,
    )
}

/// CR 705 + CR 614.1a: Flip-until-lose loop body, returning `Some(win_count)`
/// when the losing flip was reached, or `None` if a flip suspended for a keep
/// choice (in which case `pending_coin_flip` is stashed). `wins_so_far` seeds
/// the win count when re-entered from `resume_after_keep`.
fn flip_until_lose_loop(
    state: &mut GameState,
    controller: PlayerId,
    win_effect: &AbilityDefinition,
    targets: &[TargetRef],
    source_id: ObjectId,
    wins_so_far: u32,
    events: &mut Vec<GameEvent>,
) -> Result<Option<u32>, EffectError> {
    // Safety cap prevents infinite loops with pathological RNG seeds.
    const MAX_FLIPS: u32 = 1000;
    let mut win_count = wins_so_far;
    while win_count < MAX_FLIPS {
        match flip_through_replacement(state, controller, events) {
            CoinFlipOutcome::Resolved(true) => win_count += 1,
            CoinFlipOutcome::Resolved(false) => return Ok(Some(win_count)),
            // CR 614.6: a prevented flip is neither a win nor the losing flip.
            CoinFlipOutcome::Prevented => continue,
            CoinFlipOutcome::Suspended => {
                state.pending_coin_flip = Some(PendingCoinFlip {
                    source_id,
                    controller,
                    targets: targets.to_vec(),
                    win_effect: Some(Box::new(win_effect.clone())),
                    lose_effect: None,
                    kind: PendingCoinFlipKind::UntilLose {
                        wins_so_far: win_count,
                    },
                });
                return Ok(None);
            }
        }
    }
    Ok(Some(win_count))
}

/// CR 705.2: Run the win effect once per win, then emit `EffectResolved` unless a
/// win effect suspended for a player choice.
fn finish_until_lose(
    state: &mut GameState,
    win_count: u32,
    win_effect: &AbilityDefinition,
    targets: &[TargetRef],
    source_id: ObjectId,
    controller: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let prior_waiting_for = state.waiting_for.clone();
    for _ in 0..win_count {
        run_flip_branch(
            state,
            Some(win_effect),
            source_id,
            controller,
            targets,
            events,
        )?;
        // CR 608.2c: a win effect may suspend for an optional choice.
        if state.waiting_for != prior_waiting_for {
            break;
        }
    }

    // CR 608.2c: defer `EffectResolved` if the win effect suspended for a player choice.
    if state.waiting_for == prior_waiting_for {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::FlipCoinUntilLose,
            source_id,
        });
    }

    Ok(())
}

/// CR 705.1 + CR 614.1a: Resume a multi-flip resolver after the controller keeps
/// one of the doubled (Krark's Thumb) coins.
///
/// Emits EXACTLY ONE `CoinFlipped` for the kept flip (the ignored flips never
/// "happen", CR 614.6), runs that flip's branch, then continues the resolver's
/// loop from the stashed position. Each re-entered flip may itself re-suspend and
/// re-stash `pending_coin_flip`.
///
/// Returns `Ok(Some(wf))` when the resolver re-suspended for another interactive
/// choice (`wf` is the new `WaitingFor` — a fresh `CoinFlipKeepChoice` or an
/// optional-effect prompt). Returns `Ok(None)` when the whole flip effect
/// completed; the caller then drains the continuation back to Priority.
///
/// On entry the resolving `CoinFlipKeepChoice` is cleared to a neutral
/// `Priority { controller }` so any new resolution-choice `WaitingFor` is an
/// unambiguous re-suspension (`super::waits_for_resolution_choice`), even when a
/// re-suspended flip's results coincide with the one just resolved.
pub fn resume_after_keep(
    state: &mut GameState,
    pending: PendingCoinFlip,
    kept: Vec<bool>,
    events: &mut Vec<GameEvent>,
) -> Result<Option<WaitingFor>, EffectError> {
    let PendingCoinFlip {
        source_id,
        controller,
        targets,
        win_effect,
        lose_effect,
        kind,
    } = pending;

    // CR 705.1 + CR 614.1a: the single surviving flip. The keep validation
    // upstream guarantees exactly one kept result.
    let won = kept[0];
    events.push(GameEvent::CoinFlipped {
        player_id: controller,
        won,
    });

    let effect_kind = match kind {
        PendingCoinFlipKind::Single => EffectKind::FlipCoin,
        PendingCoinFlipKind::FlipN { .. } => EffectKind::FlipCoins,
        PendingCoinFlipKind::UntilLose { .. } => EffectKind::FlipCoinUntilLose,
    };

    // Clear the resolving keep choice so a re-suspension is unambiguous.
    state.waiting_for = WaitingFor::Priority { player: controller };
    let suspended = |state: &GameState| super::waits_for_resolution_choice(&state.waiting_for);

    match kind {
        PendingCoinFlipKind::Single => {
            // CR 705.2: run the kept flip's won/lost branch.
            let branch = if won {
                win_effect.as_deref()
            } else {
                lose_effect.as_deref()
            };
            run_flip_branch(state, branch, source_id, controller, &targets, events)?;
            if suspended(state) {
                return Ok(Some(state.waiting_for.clone()));
            }
            events.push(GameEvent::EffectResolved {
                kind: effect_kind,
                source_id,
            });
            Ok(None)
        }
        PendingCoinFlipKind::FlipN { remaining } => {
            // CR 705.2: run the kept flip's branch, then continue the loop.
            let branch = if won {
                win_effect.as_deref()
            } else {
                lose_effect.as_deref()
            };
            run_flip_branch(state, branch, source_id, controller, &targets, events)?;
            if suspended(state) {
                return Ok(Some(state.waiting_for.clone()));
            }

            for i in 0..remaining {
                match flip_through_replacement(state, controller, events) {
                    CoinFlipOutcome::Resolved(flip_won) => {
                        let branch = if flip_won {
                            win_effect.as_deref()
                        } else {
                            lose_effect.as_deref()
                        };
                        run_flip_branch(state, branch, source_id, controller, &targets, events)?;
                        if suspended(state) {
                            return Ok(Some(state.waiting_for.clone()));
                        }
                    }
                    CoinFlipOutcome::Prevented => continue,
                    CoinFlipOutcome::Suspended => {
                        state.pending_coin_flip = Some(PendingCoinFlip {
                            source_id,
                            controller,
                            targets: targets.clone(),
                            win_effect: win_effect.clone(),
                            lose_effect: lose_effect.clone(),
                            kind: PendingCoinFlipKind::FlipN {
                                remaining: remaining - i - 1,
                            },
                        });
                        return Ok(Some(state.waiting_for.clone()));
                    }
                }
            }
            events.push(GameEvent::EffectResolved {
                kind: effect_kind,
                source_id,
            });
            Ok(None)
        }
        PendingCoinFlipKind::UntilLose { wins_so_far } => {
            let win_effect_def = win_effect
                .as_deref()
                .ok_or_else(|| EffectError::MissingParam("FlipCoinUntilLose".to_string()))?;
            // CR 705: the kept flip counts toward the win streak (won) or ends it.
            if won {
                let seed = wins_so_far + 1;
                match flip_until_lose_loop(
                    state,
                    controller,
                    win_effect_def,
                    &targets,
                    source_id,
                    seed,
                    events,
                )? {
                    Some(win_count) => {
                        finish_until_lose(
                            state,
                            win_count,
                            win_effect_def,
                            &targets,
                            source_id,
                            controller,
                            events,
                        )?;
                        if suspended(state) {
                            Ok(Some(state.waiting_for.clone()))
                        } else {
                            Ok(None)
                        }
                    }
                    // A subsequent flip re-suspended for a keep choice.
                    None => Ok(Some(state.waiting_for.clone())),
                }
            } else {
                // CR 705: the kept flip is a loss — the streak ends here.
                finish_until_lose(
                    state,
                    wins_so_far,
                    win_effect_def,
                    &targets,
                    source_id,
                    controller,
                    events,
                )?;
                if suspended(state) {
                    Ok(Some(state.waiting_for.clone()))
                } else {
                    Ok(None)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    #[test]
    fn flip_coin_emits_event() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::FlipCoin {
                win_effect: None,
                lose_effect: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::CoinFlipped { .. })));
    }

    #[test]
    fn flip_coin_with_branches_resolves_one() {
        let mut state = GameState::new_two_player(42);

        let win_effect = Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: crate::types::ability::TargetFilter::Controller,
            },
        ));
        let lose_effect = Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                target: None,
            },
        ));

        let ability = ResolvedAbility::new(
            Effect::FlipCoin {
                win_effect: Some(win_effect),
                lose_effect: Some(lose_effect),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let initial_life = state.players[0].life;
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Exactly one branch should have fired — life changed
        let new_life = state.players[0].life;
        assert_ne!(new_life, initial_life, "One branch should have fired");
        // Either gained 5 (won) or lost 3 (lost)
        assert!(
            new_life == initial_life + 5 || new_life == initial_life - 3,
            "Expected +5 or -3, got {}",
            new_life - initial_life
        );
    }

    #[test]
    fn flip_coin_until_lose_emits_multiple_events() {
        let mut state = GameState::new_two_player(42);
        // Add cards to library to draw from
        for i in 0..10 {
            crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                crate::types::zones::Zone::Library,
            );
        }

        let ability = ResolvedAbility::new(
            Effect::FlipCoinUntilLose {
                win_effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: crate::types::ability::TargetFilter::Controller,
                    },
                )),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve_until_lose(&mut state, &ability, &mut events);
        assert!(result.is_ok());

        // Must have at least one CoinFlipped event (the losing flip)
        let flip_count = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CoinFlipped { .. }))
            .count();
        assert!(flip_count >= 1);

        // The last CoinFlipped should be a loss
        let last_flip = events
            .iter()
            .rev()
            .find(|e| matches!(e, GameEvent::CoinFlipped { .. }));
        assert!(matches!(
            last_flip,
            Some(GameEvent::CoinFlipped { won: false, .. })
        ));
    }

    #[test]
    fn flip_coins_emits_n_coin_flip_events() {
        // CR 705.1: FlipCoins with count=5 emits exactly 5 CoinFlipped events.
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::FlipCoins {
                count: QuantityExpr::Fixed { value: 5 },
                win_effect: None,
                lose_effect: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_flip_coins(&mut state, &ability, &mut events).unwrap();

        let flip_count = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CoinFlipped { .. }))
            .count();
        assert_eq!(flip_count, 5);
    }

    #[test]
    fn flip_coins_zero_count_is_noop() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::FlipCoins {
                count: QuantityExpr::Fixed { value: 0 },
                win_effect: None,
                lose_effect: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_flip_coins(&mut state, &ability, &mut events).unwrap();
        let flip_count = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CoinFlipped { .. }))
            .count();
        assert_eq!(flip_count, 0);
    }

    #[test]
    fn flip_coins_runs_win_effect_per_heads() {
        // CR 705.2: `win_effect` fires once per heads. With a deterministic
        // seed and 4 coins, the exact heads count is stable; assert that the
        // win_effect ran exactly that many times.
        let mut state = GameState::new_two_player(42);
        let initial_life = state.players[0].life;

        let win_effect = Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: crate::types::ability::TargetFilter::Controller,
            },
        ));

        let ability = ResolvedAbility::new(
            Effect::FlipCoins {
                count: QuantityExpr::Fixed { value: 4 },
                win_effect: Some(win_effect),
                lose_effect: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_flip_coins(&mut state, &ability, &mut events).unwrap();

        let heads = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CoinFlipped { won: true, .. }))
            .count() as i32;
        assert_eq!(state.players[0].life - initial_life, heads);
    }

    // --- Issue #432: Ral, Monsoon Mage coin-flip transform ---------------------
    //
    // Ral's trigger is `FlipCoin { win_effect, lose_effect }` carried on an
    // `AbilityDefinition` whose own `sub_ability` is the return-transformed
    // `ChangeZone` gated by `IfYouDo`. `win_effect` is an OPTIONAL
    // `ChangeZone(Exile, SelfRef)` ("you may exile Ral"). The handler used to
    // rebuild the branch with the lossy `ResolvedAbility::new`, dropping
    // `win_effect.optional` so the player was never prompted and the
    // return-transformed chain never keyed off the exile. These tests drive the
    // genuine resolution pipeline (`build_resolved_from_def` → `resolve_ability_chain`,
    // exactly as `game/triggers.rs` + `game/stack.rs` do) and the genuine
    // `apply(DecideOptionalEffect)` pipeline, with the RNG deterministically
    // seeded for a win or a loss.

    use crate::game::ability_utils::build_resolved_from_def;
    use crate::game::effects::resolve_ability_chain;
    use crate::game::engine::apply;
    use crate::game::game_object::BackFaceData;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityCondition, TargetFilter};
    use crate::types::actions::GameAction;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaCost;
    use crate::types::zones::Zone;
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    /// Build Ral, Monsoon Mage as a battlefield permanent with a back face so
    /// `enter_transformed` has a face to flip to (CR 712.8e).
    fn setup_ral(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Ral, Monsoon Mage".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(1);
        obj.toughness = Some(3);
        obj.base_power = Some(1);
        obj.base_toughness = Some(3);
        obj.back_face = Some(BackFaceData {
            name: "Ral, Leyline Prodigy".to_string(),
            power: None,
            toughness: None,
            loyalty: Some(3),
            defense: None,
            card_types: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Planeswalker],
                subtypes: vec!["Ral".to_string()],
            },
            mana_cost: ManaCost::default(),
            keywords: vec![],
            abilities: vec![],
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: vec![],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            layout_kind: None,
        });
        id
    }

    /// Reproduce Ral's parsed trigger `execute` `AbilityDefinition`:
    /// `FlipCoin` whose `win_effect` is an optional self-exile, with the
    /// return-transformed `ChangeZone` as the definition's `sub_ability`,
    /// gated `IfYouDo`.
    fn ral_trigger_definition() -> AbilityDefinition {
        let win_effect = Box::new({
            let mut def = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Exile,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                },
            );
            def.optional = true;
            def
        });
        let lose_effect = Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                damage_source: None,
            },
        ));
        let return_transformed = {
            let mut def = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Battlefield,
                    target: TargetFilter::ParentTarget,
                    owner_library: false,
                    enter_transformed: true,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                },
            );
            def.condition = Some(AbilityCondition::effect_performed());
            def
        };
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoin {
                win_effect: Some(win_effect),
                lose_effect: Some(lose_effect),
            },
        );
        def.sub_ability = Some(Box::new(return_transformed));
        def
    }

    #[test]
    fn ral_wins_flip_and_accepts_exile_returns_transformed() {
        let mut state = GameState::new_two_player(0);
        // Seed 0 → first `random_bool(0.5)` is a WIN.
        state.rng = ChaCha20Rng::seed_from_u64(0);
        let ral = setup_ral(&mut state);

        let ability = build_resolved_from_def(&ral_trigger_definition(), ral, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Win branch is `optional` → the chain must SUSPEND for the player's
        // "you may exile Ral" choice. Pre-fix, `optional` was dropped and the
        // chain ran straight through with no prompt.
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "expected OptionalEffectChoice, got {:?}",
            state.waiting_for
        );
        // The premature `EffectResolved` guard: while suspended, FlipCoin must
        // NOT have reported itself resolved.
        assert!(
            !events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::FlipCoin,
                    ..
                }
            )),
            "FlipCoin EffectResolved fired before the optional choice was made"
        );

        // Accept the optional exile through the real `apply` pipeline.
        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::DecideOptionalEffect { accept: true },
        )
        .expect("DecideOptionalEffect should succeed");

        // Ral was exiled, then the `IfYouDo` sub-ability returned him to the
        // battlefield transformed (CR 712.8e — back face up).
        let obj = state
            .objects
            .get(&ral)
            .expect("Ral object should still exist");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "Ral should have returned to the battlefield"
        );
        assert!(
            obj.transformed,
            "Ral should be on his back face after returning transformed; events: {:?}",
            result.events
        );
    }

    #[test]
    fn ral_wins_flip_and_declines_exile_stays_front_face() {
        let mut state = GameState::new_two_player(0);
        state.rng = ChaCha20Rng::seed_from_u64(0);
        let ral = setup_ral(&mut state);

        let ability = build_resolved_from_def(&ral_trigger_definition(), ral, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "expected OptionalEffectChoice, got {:?}",
            state.waiting_for
        );

        // Decline the optional exile.
        apply(
            &mut state,
            PlayerId(0),
            GameAction::DecideOptionalEffect { accept: false },
        )
        .expect("DecideOptionalEffect should succeed");

        let obj = state.objects.get(&ral).expect("Ral object should exist");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "Ral should remain on the battlefield when the exile is declined"
        );
        assert!(
            !obj.transformed,
            "Ral should stay on his front face when the exile is declined"
        );
    }

    #[test]
    fn ral_loses_flip_takes_one_damage() {
        let mut state = GameState::new_two_player(1);
        // Seed 1 → first `random_bool(0.5)` is a LOSS.
        state.rng = ChaCha20Rng::seed_from_u64(1);
        let ral = setup_ral(&mut state);
        let initial_life = state.players[0].life;

        let ability = build_resolved_from_def(&ral_trigger_definition(), ral, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Lose branch is non-optional → resolves inline, no suspension.
        assert!(
            !matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "lose branch should not suspend for an optional choice, got {:?}",
            state.waiting_for
        );
        assert_eq!(
            state.players[0].life,
            initial_life - 1,
            "controller should take 1 damage on a lost flip"
        );
        let obj = state.objects.get(&ral).expect("Ral object should exist");
        assert_eq!(obj.zone, Zone::Battlefield, "Ral should not be exiled");
        assert!(!obj.transformed, "Ral should not transform on a loss");
    }

    #[test]
    fn krark_lose_branch_target_is_triggering_spell() {
        use crate::parser::oracle_trigger::parse_trigger_line;
        use crate::types::ability::TargetFilter;

        const KRARK_TRIGGER: &str = "Whenever you cast an instant or sorcery spell, flip a coin. \
            If you lose the flip, return that spell to its owner's hand. \
            If you win the flip, copy that spell, and you may choose new targets for the copy.";

        let trig_def = parse_trigger_line(KRARK_TRIGGER, "Krark, the Thumbless");
        let execute = trig_def.execute.as_ref().unwrap();
        let Effect::FlipCoin { lose_effect, .. } = execute.effect.as_ref() else {
            panic!("expected FlipCoin");
        };
        let lose = lose_effect.as_ref().unwrap();
        match lose.effect.as_ref() {
            Effect::Bounce { target, .. } => {
                assert_eq!(
                    *target,
                    TargetFilter::TriggeringSource,
                    "that spell in a SpellCast trigger must bounce TriggeringSource"
                );
            }
            Effect::ChangeZone {
                target,
                destination: Zone::Hand,
                ..
            } => {
                assert_eq!(
                    *target,
                    TargetFilter::TriggeringSource,
                    "that spell in a SpellCast trigger must return TriggeringSource"
                );
            }
            other => panic!("unexpected lose effect {other:?}"),
        }
    }

    #[test]
    fn krark_isolated_flip_seed0_emits_win_and_runs_copy_branch() {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::game::zones::create_object;
        use crate::parser::oracle_trigger::parse_trigger_line;
        use crate::types::card_type::CoreType;
        use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
        use crate::types::identifiers::CardId;
        use crate::types::player::PlayerId;
        use rand::SeedableRng;
        use rand_chacha::ChaCha20Rng;

        const KRARK_TRIGGER: &str = "Whenever you cast an instant or sorcery spell, flip a coin. \
            If you lose the flip, return that spell to its owner's hand. \
            If you win the flip, copy that spell, and you may choose new targets for the copy.";

        let trig_def = parse_trigger_line(KRARK_TRIGGER, "Krark, the Thumbless");
        let execute = trig_def.execute.as_ref().expect("Krark trigger execute");

        let mut state = GameState::new_two_player(0);
        state.rng = ChaCha20Rng::seed_from_u64(0);

        let krark_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Krark, the Thumbless".to_string(),
            Zone::Battlefield,
        );
        let spell_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Draw Spell".to_string(),
            Zone::Stack,
        );
        {
            let spell = state.objects.get_mut(&spell_id).unwrap();
            spell.card_types.core_types.push(CoreType::Instant);
        }
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(0),
                format!("Library {i}"),
                Zone::Library,
            );
        }
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(2),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        state.current_trigger_event = Some(GameEvent::SpellCast {
            controller: PlayerId(0),
            object_id: spell_id,
            card_id: CardId(2),
        });

        let ability = build_resolved_from_def(execute, krark_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let won = events
            .iter()
            .find_map(|e| match e {
                GameEvent::CoinFlipped { won, .. } => Some(*won),
                _ => None,
            })
            .expect("CoinFlipped event");
        assert!(won, "seed 0 must win the flip");

        // Win branch copies the spell — stack should now have the copy above the original.
        assert!(
            state.stack.len() >= 2,
            "win branch should copy onto stack; stack = {:?}",
            state.stack.len()
        );
        assert_eq!(
            state.objects.get(&spell_id).unwrap().zone,
            Zone::Stack,
            "win branch must not bounce the original spell"
        );
    }

    #[test]
    fn krark_isolated_flip_seed1_emits_loss_and_bounces_spell() {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::game::zones::create_object;
        use crate::parser::oracle_trigger::parse_trigger_line;
        use crate::types::card_type::CoreType;
        use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
        use crate::types::identifiers::CardId;
        use crate::types::player::PlayerId;
        use rand::SeedableRng;
        use rand_chacha::ChaCha20Rng;

        const KRARK_TRIGGER: &str = "Whenever you cast an instant or sorcery spell, flip a coin. \
            If you lose the flip, return that spell to its owner's hand. \
            If you win the flip, copy that spell, and you may choose new targets for the copy.";

        let trig_def = parse_trigger_line(KRARK_TRIGGER, "Krark, the Thumbless");
        let execute = trig_def.execute.as_ref().expect("Krark trigger execute");

        let mut state = GameState::new_two_player(1);
        state.rng = ChaCha20Rng::seed_from_u64(1);

        let krark_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Krark, the Thumbless".to_string(),
            Zone::Battlefield,
        );
        let spell_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Draw Spell".to_string(),
            Zone::Stack,
        );
        {
            let spell = state.objects.get_mut(&spell_id).unwrap();
            spell.card_types.core_types.push(CoreType::Instant);
        }
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(2),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        state.current_trigger_event = Some(GameEvent::SpellCast {
            controller: PlayerId(0),
            object_id: spell_id,
            card_id: CardId(2),
        });

        let ability = build_resolved_from_def(execute, krark_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let won = events
            .iter()
            .find_map(|e| match e {
                GameEvent::CoinFlipped { won, .. } => Some(*won),
                _ => None,
            })
            .expect("CoinFlipped event");
        assert!(!won, "seed 1 must lose the flip");
        assert_eq!(
            state.objects.get(&spell_id).unwrap().zone,
            Zone::Hand,
            "lose branch must return the spell to hand"
        );
        assert!(state.stack.is_empty(), "bounced spell must leave the stack");
    }

    // --- Issue #2940: Krark, the Thumbless win/lose flip branches ----------------

    use crate::parser::oracle_trigger::parse_trigger_line;
    use crate::types::ability::CopyRetargetPermission;

    const KRARK_TRIGGER: &str = "Whenever you cast an instant or sorcery spell, flip a coin. \
        If you lose the flip, return that spell to its owner's hand. \
        If you win the flip, copy that spell, and you may choose new targets for the copy.";

    #[test]
    fn krark_thumbless_parses_flip_branches_in_correct_slots() {
        let def = parse_trigger_line(KRARK_TRIGGER, "Krark, the Thumbless");
        let execute = def
            .execute
            .as_ref()
            .expect("Krark trigger should have execute ability");
        let Effect::FlipCoin {
            win_effect,
            lose_effect,
        } = execute.effect.as_ref()
        else {
            panic!("expected FlipCoin execute, got {:?}", execute.effect);
        };
        let win = win_effect.as_ref().expect("win branch should be populated");
        let lose = lose_effect
            .as_ref()
            .expect("lose branch should be populated");
        assert!(
            matches!(win.effect.as_ref(), Effect::CopySpell { .. }),
            "win branch should copy the spell, got {:?}",
            win.effect
        );
        assert!(
            matches!(
                lose.effect.as_ref(),
                Effect::Bounce { .. }
                    | Effect::ChangeZone {
                        destination: Zone::Hand,
                        ..
                    }
            ),
            "lose branch should return spell to hand, got {:?}",
            lose.effect
        );
        fn copy_retarget(def: &AbilityDefinition) -> CopyRetargetPermission {
            match def.effect.as_ref() {
                Effect::CopySpell { retarget, .. } => retarget.clone(),
                _ => def
                    .sub_ability
                    .as_deref()
                    .map(copy_retarget)
                    .unwrap_or(CopyRetargetPermission::KeepOriginalTargets),
            }
        }
        assert_eq!(
            copy_retarget(win),
            CopyRetargetPermission::MayChooseNewTargets,
            "win copy should allow new targets, got {:?}",
            win.effect
        );
    }
}
