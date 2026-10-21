use crate::types::game_state::RandomDraw;
use std::collections::HashSet;

use rand::Rng;

use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    AbilityDefinition, CoinFlipResult, CoinFlipRun, Effect, EffectError, EffectKind,
    ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, ResolutionCoinFlip, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::resolution::{PendingCoinFlip, PendingCoinFlipKind};

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
    let results: Vec<bool> = (0..count)
        .map(|_| state.rng.draw(RandomDraw::Outcome).random_bool(0.5))
        .collect();

    if count == 1 {
        let won = results[0];
        events.push(GameEvent::CoinFlipped {
            player_id: player,
            won,
        });
        // CR 705.2: record the flipper's result so a `WhileCondition` loop gate
        // ("if you lose the flip, repeat this process") can read it after the
        // resolution's tail runs. Overwrite-on-produce, mirroring reveal tracking.
        state.resolution_coin_flip = Some(ResolutionCoinFlip {
            flipper: player,
            result: CoinFlipResult::from_won(won),
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
    chain_root_targets: &[TargetRef],
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if let Some(def) = branch {
        // CR 608.2h: propagate so a counter-gated "that many" nested in the
        // branch still resolves against the live chain-root target (see
        // `build_resolved_from_def_with_chain_root`'s doc).
        let sub = crate::game::ability_utils::build_resolved_from_def_with_targets_and_chain_root(
            def,
            source_id,
            controller,
            targets.to_vec(),
            chain_root_targets.to_vec(),
        );
        resolve_ability_chain(state, &sub, events, 0)?;
    }
    Ok(())
}

// Safety cap prevents infinite loops with pathological RNG seeds.
const MAX_FLIPS: u32 = 1000;

/// The run's fixed context, borrowed from the resolving ability or the parked frame.
struct FlipRun<'a> {
    source_id: ObjectId,
    controller: PlayerId,
    flipper: PlayerId,
    targets: &'a [TargetRef],
    chain_root_targets: &'a [TargetRef],
    win_effect: Option<&'a AbilityDefinition>,
    lose_effect: Option<&'a AbilityDefinition>,
}

impl FlipRun<'_> {
    /// CR 614.1a + CR 705.1: the frame a run parks while a doubled flip waits for its keep
    /// choice. `kind` is the position after the paused flip. The branches are cloned only here,
    /// on suspension.
    fn park(&self, kind: PendingCoinFlipKind) -> PendingCoinFlip {
        PendingCoinFlip {
            source_id: self.source_id,
            controller: self.controller,
            flipper: self.flipper,
            targets: self.targets.to_vec(),
            win_effect: self.win_effect.map(|d| Box::new(d.clone())),
            lose_effect: self.lose_effect.map(|d| Box::new(d.clone())),
            kind,
            chain_root_targets: self.chain_root_targets.to_vec(),
        }
    }

    /// CR 705.2: run the won or lost branch in the source controller's context
    /// (CR 608.2c: the controller follows the spell or ability's instructions).
    fn run_branch(
        &self,
        state: &mut GameState,
        won: bool,
        events: &mut Vec<GameEvent>,
    ) -> Result<(), EffectError> {
        let branch = if won {
            self.win_effect
        } else {
            self.lose_effect
        };
        run_flip_branch(
            state,
            branch,
            self.source_id,
            self.controller,
            self.targets,
            self.chain_root_targets,
            events,
        )
    }
}

/// CR 608.2c: how the run decides that its instructions have all been followed, so it
/// can report `EffectResolved`.
enum RunCompletionTest {
    /// The pre-merge single-flip contract: the flip's branch left `waiting_for` exactly as it
    /// was before the flip. Used only by a fresh single flip.
    WaitingForUnchanged(Box<WaitingFor>),
    /// No interactive resolution choice is pending (`waits_for_resolution_choice`).
    NoResolutionChoicePending,
}

/// Where a run enters `drive_flip_run`.
enum FlipRunStart {
    /// A fresh run: `kind` is the full owed position (`Exactly { remaining: n }` or
    /// `UntilLoss { wins_so_far: 0 }`).
    Fresh(PendingCoinFlipKind),
    /// Resuming a parked frame: `won` is the kept result of the paused flip, `kind` the
    /// frame's position (the position after that flip).
    Kept {
        kind: PendingCoinFlipKind,
        won: bool,
    },
}

/// What the run owes next.
enum FlipRunStep {
    /// Another flip is owed; the position is the one before that flip.
    Flip(PendingCoinFlipKind),
    /// No flip is owed; `owed_wins` rewards remain (always 0 for `Exactly`).
    Finished { owed_wins: u32 },
    /// A keep frame was parked, or a branch raised a resolution choice.
    Suspended,
}

/// How a run left `drive_flip_run`.
enum FlipRunOutcome {
    Complete,
    Suspended,
}

/// CR 705.1 + CR 705.2: the run-end authority, plus the engine's `MAX_FLIPS` safety cap. Decides whether another flip
/// is owed from `kind`, the position after the last settled flip.
fn next_step(kind: PendingCoinFlipKind) -> FlipRunStep {
    match kind {
        PendingCoinFlipKind::Exactly { remaining: 0 } => FlipRunStep::Finished { owed_wins: 0 },
        // The cap is checked before every flip, including the first after a resume.
        PendingCoinFlipKind::UntilLoss { wins_so_far } if wins_so_far >= MAX_FLIPS => {
            FlipRunStep::Finished {
                owed_wins: wins_so_far,
            }
        }
        PendingCoinFlipKind::Exactly { .. } | PendingCoinFlipKind::UntilLoss { .. } => {
            FlipRunStep::Flip(kind)
        }
    }
}

/// CR 705.1 + CR 705.2 + CR 608.2c: settle one flip's result and report what the run owes
/// next. `kind_after` is the position after this flip.
///
/// - `Exactly`: the flip's own branch runs as it settles (CR 705.2).
/// - `UntilLoss`, won: the flip is only counted; its reward is owed until the loss
///   (CR 608.2c: the reward instruction follows the flipping instruction).
/// - `UntilLoss`, lost: the lose branch runs as the losing flip settles, then the owed
///   rewards are due.
fn settle_flip(
    state: &mut GameState,
    run: &FlipRun<'_>,
    kind_after: PendingCoinFlipKind,
    won: bool,
    events: &mut Vec<GameEvent>,
) -> Result<FlipRunStep, EffectError> {
    match kind_after {
        PendingCoinFlipKind::Exactly { .. } => {
            run.run_branch(state, won, events)?;
            // CR 608.2c: stop only for an interactive resolution choice. A token branch
            // hands priority back without suspending, and must not truncate the remaining
            // coin flips.
            if super::waits_for_resolution_choice(&state.waiting_for) {
                Ok(FlipRunStep::Suspended)
            } else {
                Ok(next_step(kind_after))
            }
        }
        PendingCoinFlipKind::UntilLoss { wins_so_far } if won => {
            Ok(next_step(PendingCoinFlipKind::UntilLoss {
                wins_so_far: wins_so_far + 1,
            }))
        }
        PendingCoinFlipKind::UntilLoss { wins_so_far } => {
            run.run_branch(state, false, events)?;
            if super::waits_for_resolution_choice(&state.waiting_for) {
                Ok(FlipRunStep::Suspended)
            } else {
                Ok(FlipRunStep::Finished {
                    owed_wins: wins_so_far,
                })
            }
        }
    }
}

/// CR 705.1 + CR 705.2 + CR 614.1a + CR 614.6 + CR 608.2c: drive a coin-flip run from
/// `start` until it completes or suspends. Every flip goes through
/// `flip_through_replacement`, so Krark's Thumb can double it. A doubled flip parks the
/// run's position in a coin-flip frame; `resume_after_keep` re-enters here with the kept
/// result. A prevented flip never happened (CR 614.6): an `Exactly` run moves on without
/// running that flip's branch, and an `UntilLoss` run flips again without counting it.
fn drive_flip_run(
    state: &mut GameState,
    run: &FlipRun<'_>,
    start: FlipRunStart,
    test: RunCompletionTest,
    events: &mut Vec<GameEvent>,
) -> Result<FlipRunOutcome, EffectError> {
    let mut step = match start {
        FlipRunStart::Fresh(kind) => next_step(kind),
        FlipRunStart::Kept { kind, won } => settle_flip(state, run, kind, won, events)?,
    };
    loop {
        match step {
            FlipRunStep::Suspended => return Ok(FlipRunOutcome::Suspended),
            FlipRunStep::Finished { owed_wins } => {
                return complete_run(state, run, owed_wins, &test, events);
            }
            FlipRunStep::Flip(kind) => {
                let kind_after = match kind {
                    PendingCoinFlipKind::Exactly { remaining } => PendingCoinFlipKind::Exactly {
                        remaining: remaining - 1,
                    },
                    PendingCoinFlipKind::UntilLoss { wins_so_far } => {
                        PendingCoinFlipKind::UntilLoss { wins_so_far }
                    }
                };
                step = match flip_through_replacement(state, run.flipper, events) {
                    CoinFlipOutcome::Resolved(won) => {
                        settle_flip(state, run, kind_after, won, events)?
                    }
                    CoinFlipOutcome::Prevented => match kind_after {
                        PendingCoinFlipKind::Exactly { .. } => next_step(kind_after),
                        PendingCoinFlipKind::UntilLoss { .. } => FlipRunStep::Flip(kind_after),
                    },
                    CoinFlipOutcome::Suspended => {
                        // CR 614.1a: doubled flip — stash the run position and resume after
                        // the keep choice. The position excludes the paused flip itself.
                        state.push_coin_flip_frame(run.park(kind_after));
                        FlipRunStep::Suspended
                    }
                };
            }
        }
    }
}

/// CR 608.2c: finish a run. Pays the `owed_wins` rewards, then reports `EffectResolved` if
/// `test` passes. **The only `EffectResolved` emitter for a coin-flip run.**
///
/// The rewards stop at the first one that suspends for an interactive resolution choice
/// (an optional "you may", scry, etc.). A reward that merely creates permanents hands
/// priority back to the active player (`Priority{opponent}` -> `Priority{controller}`),
/// which is NOT a suspension — gating on `waits_for_resolution_choice` rather than any
/// `waiting_for` change lets a multi-win token creator (Mirror March, #5966) run the
/// reward for every win instead of stopping after one.
fn complete_run(
    state: &mut GameState,
    run: &FlipRun<'_>,
    owed_wins: u32,
    test: &RunCompletionTest,
    events: &mut Vec<GameEvent>,
) -> Result<FlipRunOutcome, EffectError> {
    for _ in 0..owed_wins {
        run.run_branch(state, true, events)?;
        if super::waits_for_resolution_choice(&state.waiting_for) {
            break;
        }
    }

    let resolution_choice_pending = super::waits_for_resolution_choice(&state.waiting_for);
    // CR 608.2c: defer `EffectResolved` while a branch is suspended for a player choice.
    let followed_every_instruction = match test {
        RunCompletionTest::WaitingForUnchanged(prior) => state.waiting_for == **prior,
        RunCompletionTest::NoResolutionChoicePending => !resolution_choice_pending,
    };
    if followed_every_instruction {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::FlipCoins,
            source_id: run.source_id,
            subject: None,
        });
    }

    Ok(if resolution_choice_pending {
        FlipRunOutcome::Suspended
    } else {
        FlipRunOutcome::Complete
    })
}

/// CR 705: Flip coins and execute the win/lose branches.
///
/// The one dispatch target for every coin-flip effect: [`Effect::FlipCoins`] carries the
/// [`CoinFlipRun`] plus its branches and flipper.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::FlipCoins {
        flips,
        win_effect,
        lose_effect,
        flipper,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam("FlipCoins".to_string()));
    };

    // CR 705.2: "only the player who flips a coin wins or loses the flip." The
    // `flipper` names that player ("that player flips a coin" → the triggering
    // player, Mirrored Depths / Planar Chaos); the bare "flip a coin" defaults
    // to the controller. The flipper drives the RNG, the recorded `CoinFlipped`,
    // and the win/lose branch — the branch still runs in the SOURCE controller's
    // context (CR 608.2c: the controller follows the spell/ability's
    // instructions), so e.g. Mirrored Depths' controller is the one who counters
    // the spell on a lost flip. "each player flips a coin" is NOT this case — it
    // rides the surrounding `player_scope` iteration, which rebinds
    // `ability.controller` per player, so a `Controller` flipper there flips once
    // per player (CR 101.4 APNAP).
    // CR 608.2b: a declared player whose target was illegal flips no coin.
    let Some(flipper) = super::resolve_player_for_context_ref(state, ability, flipper) else {
        return Ok(());
    };

    // CR 107.1b + CR 608.2h: the flip count is resolved once, in the ability's
    // context, when the run starts; a negative result becomes zero.
    let kind = match flips {
        CoinFlipRun::Exactly { count } => PendingCoinFlipKind::Exactly {
            remaining: resolve_quantity_with_targets(state, count, ability).max(0) as u32,
        },
        CoinFlipRun::UntilLoss => PendingCoinFlipKind::UntilLoss { wins_so_far: 0 },
    };

    // CR 608.2c: a single flip reports `EffectResolved` once its branch has left
    // `waiting_for` as it was before the flip; every other run, once no interactive
    // resolution choice is pending.
    let test = if flips.is_single_flip() {
        RunCompletionTest::WaitingForUnchanged(Box::new(state.waiting_for.clone()))
    } else {
        RunCompletionTest::NoResolutionChoicePending
    };

    let run = FlipRun {
        source_id: ability.source_id,
        controller: ability.controller,
        flipper,
        targets: &ability.targets,
        chain_root_targets: &ability.context.chain_root_targets,
        win_effect: win_effect.as_deref(),
        lose_effect: lose_effect.as_deref(),
    };
    drive_flip_run(state, &run, FlipRunStart::Fresh(kind), test, events)?;
    Ok(())
}

/// CR 705.1 + CR 614.1a: Resume a coin-flip run after the controller keeps
/// one of the doubled (Krark's Thumb) coins.
///
/// Emits EXACTLY ONE `CoinFlipped` for the kept flip (the ignored flips never
/// "happen", CR 614.6), settles that flip, then continues the run from the stashed
/// position. Each re-entered flip may itself re-suspend and re-park the coin-flip frame.
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
        flipper,
        targets,
        win_effect,
        lose_effect,
        kind,
        chain_root_targets,
    } = pending;

    // CR 705.1 + CR 614.1a + CR 705.2: the single surviving flip is recorded for
    // the flipper (the player who flipped), not necessarily the source's
    // controller (Mirrored Depths / Planar Chaos, "that player flips a coin").
    let won = kept[0];
    events.push(GameEvent::CoinFlipped {
        player_id: flipper,
        won,
    });
    // CR 705.2: record the kept flip's result for the flipper (not the source's
    // controller) so a `WhileCondition` loop gate reads the surviving flip.
    state.resolution_coin_flip = Some(ResolutionCoinFlip {
        flipper,
        result: CoinFlipResult::from_won(won),
    });

    // Clear the resolving keep choice so a re-suspension is unambiguous.
    state.waiting_for = WaitingFor::Priority { player: controller };

    let run = FlipRun {
        source_id,
        controller,
        flipper,
        targets: &targets,
        chain_root_targets: &chain_root_targets,
        win_effect: win_effect.as_deref(),
        lose_effect: lose_effect.as_deref(),
    };
    // CR 608.2c: every resumed path completes once no interactive resolution choice is pending.
    match drive_flip_run(
        state,
        &run,
        FlipRunStart::Kept { kind, won },
        RunCompletionTest::NoResolutionChoicePending,
        events,
    )? {
        FlipRunOutcome::Suspended => Ok(Some(state.waiting_for.clone())),
        FlipRunOutcome::Complete => Ok(None),
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
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: None,
                lose_effect: None,
                flipper: crate::types::ability::TargetFilter::Controller,
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
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(win_effect),
                lose_effect: Some(lose_effect),
                flipper: crate::types::ability::TargetFilter::Controller,
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
            Effect::FlipCoins {
                flips: CoinFlipRun::UntilLoss,
                win_effect: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: crate::types::ability::TargetFilter::Controller,
                    },
                ))),
                lose_effect: None,
                flipper: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
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
                flips: CoinFlipRun::Exactly {
                    count: QuantityExpr::Fixed { value: 5 },
                },
                win_effect: None,
                lose_effect: None,
                flipper: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

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
                flips: CoinFlipRun::Exactly {
                    count: QuantityExpr::Fixed { value: 0 },
                },
                win_effect: None,
                lose_effect: None,
                flipper: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
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
                flips: CoinFlipRun::Exactly {
                    count: QuantityExpr::Fixed { value: 4 },
                },
                win_effect: Some(win_effect),
                lose_effect: None,
                flipper: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let heads = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CoinFlipped { won: true, .. }))
            .count() as i32;
        assert_eq!(state.players[0].life - initial_life, heads);
    }

    #[test]
    fn flip_coins_continues_after_token_branch_returns_priority() {
        let mut state = GameState::new_two_player(42);
        let win_effect = Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "Coin Token".to_string(),
                power: crate::types::ability::PtValue::Fixed(1),
                toughness: crate::types::ability::PtValue::Fixed(1),
                types: vec!["Creature".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: crate::types::ability::TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
        ));
        let ability = ResolvedAbility::new(
            Effect::FlipCoins {
                flips: CoinFlipRun::Exactly {
                    count: QuantityExpr::Fixed { value: 8 },
                },
                win_effect: Some(win_effect),
                lose_effect: None,
                flipper: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::CoinFlipped { .. }))
                .count(),
            8,
            "a token branch returns priority but must not stop the flip loop"
        );
    }

    // --- Issue #432: Ral, Monsoon Mage coin-flip transform ---------------------
    //
    // Ral's trigger is a single-flip `FlipCoins { win_effect, lose_effect }` carried on an
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
            is_swap_snapshot: false,
            trigger_printed_origins: Vec::new(),
            name: "Ral, Leyline Prodigy".to_string(),
            power: None,
            toughness: None,
            loyalty: Some(3),
            printed_loyalty: None,
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
            parse_warnings: vec![],
        });
        id
    }

    /// Reproduce Ral's parsed trigger `execute` `AbilityDefinition`:
    /// Single-flip `FlipCoins` whose `win_effect` is an optional self-exile, with the
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
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
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
                excess: None,
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
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            );
            def.condition = Some(AbilityCondition::effect_performed());
            def
        };
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(win_effect),
                lose_effect: Some(lose_effect),
                flipper: crate::types::ability::TargetFilter::Controller,
            },
        );
        def.sub_ability = Some(Box::new(return_transformed));
        def
    }

    #[test]
    fn ral_wins_flip_and_accepts_exile_returns_transformed() {
        let mut state = GameState::new_two_player(0);
        // Seed 0 → first `random_bool(0.5)` is a WIN.
        state.rng = crate::types::game_state::GameRng::seed_from_u64(0);
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
        // The premature `EffectResolved` guard: while suspended, FlipCoins must
        // NOT have reported itself resolved.
        assert!(
            !events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::FlipCoins,
                    ..
                }
            )),
            "FlipCoins EffectResolved fired before the optional choice was made"
        );
        assert!(state.active_optional_effect_frame().is_some());
        assert!(matches!(
            state.resolution_stack.active_predecessor(),
            Some(crate::types::resolution::ResolutionFrame::AbilityContinuation(_))
        ));

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
        state.rng = crate::types::game_state::GameRng::seed_from_u64(0);
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
        state.rng = crate::types::game_state::GameRng::seed_from_u64(1);
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
        let Effect::FlipCoins {
            flips, lose_effect, ..
        } = execute.effect.as_ref()
        else {
            panic!("expected FlipCoins");
        };
        assert!(flips.is_single_flip(), "Krark flips a single coin");
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

        const KRARK_TRIGGER: &str = "Whenever you cast an instant or sorcery spell, flip a coin. \
            If you lose the flip, return that spell to its owner's hand. \
            If you win the flip, copy that spell, and you may choose new targets for the copy.";

        let trig_def = parse_trigger_line(KRARK_TRIGGER, "Krark, the Thumbless");
        let execute = trig_def.execute.as_ref().expect("Krark trigger execute");

        let mut state = GameState::new_two_player(0);
        state.rng = crate::types::game_state::GameRng::seed_from_u64(0);

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
            cast_mana_value: None,
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

        const KRARK_TRIGGER: &str = "Whenever you cast an instant or sorcery spell, flip a coin. \
            If you lose the flip, return that spell to its owner's hand. \
            If you win the flip, copy that spell, and you may choose new targets for the copy.";

        let trig_def = parse_trigger_line(KRARK_TRIGGER, "Krark, the Thumbless");
        let execute = trig_def.execute.as_ref().expect("Krark trigger execute");

        let mut state = GameState::new_two_player(1);
        state.rng = crate::types::game_state::GameRng::seed_from_u64(1);

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
            cast_mana_value: None,
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
        let Effect::FlipCoins {
            flips,
            win_effect,
            lose_effect,
            ..
        } = execute.effect.as_ref()
        else {
            panic!("expected FlipCoins execute, got {:?}", execute.effect);
        };
        assert!(flips.is_single_flip(), "Krark flips a single coin");
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

    // --- PR #3349: subject flipper (Mirrored Depths / Planar Chaos) -----------
    //
    // CR 705.2: "only the player who flips a coin wins or loses the flip." When a
    // NON-controller casts a spell into "Whenever a player casts a spell, that
    // player flips a coin", the SUBJECT (the casting player) must flip, and the
    // recorded `CoinFlipped` and the win/lose outcome belong to that player — not
    // the enchantment/source controller. The parser binds this as
    // single-flip `FlipCoins { flipper: TriggeringPlayer }` (see imperative.rs parser tests);
    // these tests assert the runtime resolver honors it.

    /// Build the Mirrored Depths flip ability: `FlipCoins { flipper:
    /// TriggeringPlayer }` whose lose branch counters the cast spell. The flip
    /// effect's controller is P0 (the enchantment owner); a SpellCast event names
    /// the actual caster.
    fn triggering_player_flip(lose_effect: Option<Box<AbilityDefinition>>) -> Effect {
        Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: None,
            lose_effect,
            flipper: TargetFilter::TriggeringPlayer,
        }
    }

    #[test]
    fn opponent_casts_into_subject_flip_opponent_is_the_flipper() {
        // CR 705.2: P1 (the opponent) casts the spell; P0 controls the flip
        // source. The `CoinFlipped` must be recorded for P1, the flipper.
        let mut state = GameState::new_two_player(0);
        state.rng = crate::types::game_state::GameRng::seed_from_u64(0);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mirrored Depths".to_string(),
            Zone::Battlefield,
        );
        // P1 is the casting (triggering) player.
        state.current_trigger_event = Some(GameEvent::SpellCast {
            controller: PlayerId(1),
            object_id: ObjectId(999),
            card_id: CardId(2),
            cast_mana_value: None,
        });

        let ability =
            ResolvedAbility::new(triggering_player_flip(None), vec![], source, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let flip = events
            .iter()
            .find_map(|e| match e {
                GameEvent::CoinFlipped { player_id, won } => Some((*player_id, *won)),
                _ => None,
            })
            .expect("a coin was flipped");
        assert_eq!(
            flip.0,
            PlayerId(1),
            "the OPPONENT (caster) flips, not the source controller P0"
        );
    }

    #[test]
    fn opponent_casts_into_subject_flip_lose_branch_resolves_for_the_flip() {
        // CR 705.2: P1 casts; with a seed that loses the flip, the lose branch
        // (here: the caster P1 loses 3 life) must fire — driven by P1's result.
        // Seed 1 → first flip is a LOSS (mirrors `ral_loses_flip_takes_one_damage`).
        let mut state = GameState::new_two_player(1);
        state.rng = crate::types::game_state::GameRng::seed_from_u64(1);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Planar Chaos".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::SpellCast {
            controller: PlayerId(1),
            object_id: ObjectId(999),
            card_id: CardId(2),
            cast_mana_value: None,
        });

        // Lose branch: "that player loses 3 life" — bound to TriggeringPlayer so
        // it drains the flipper, proving the loss is attributed to P1.
        let lose = Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                target: Some(TargetFilter::TriggeringPlayer),
            },
        ));
        let p1_initial = state.players[1].life;
        let p0_initial = state.players[0].life;

        let ability = ResolvedAbility::new(
            triggering_player_flip(Some(lose)),
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The recorded flip is the opponent's and is a loss.
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::CoinFlipped {
                    player_id: PlayerId(1),
                    won: false
                }
            )),
            "expected P1 to lose the flip, events: {events:?}"
        );
        assert_eq!(
            state.players[1].life,
            p1_initial - 3,
            "the flipper (P1) takes the lose-branch loss"
        );
        assert_eq!(
            state.players[0].life, p0_initial,
            "the source controller (P0) is unaffected"
        );
    }

    #[test]
    fn each_player_flips_once_per_player_not_collapsed_to_one() {
        // CR 101.4 + CR 705.2: "each player flips a coin" rides player_scope=All
        // iteration; `flipper = Controller` resolves to each iterated player, so a
        // 2-player game records exactly 2 flips, one per player — NOT a single
        // controller flip.
        let mut state = GameState::new_two_player(7);
        state.rng = crate::types::game_state::GameRng::seed_from_u64(7);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Each-Player Flipper".to_string(),
            Zone::Battlefield,
        );

        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: None,
                lose_effect: None,
                flipper: TargetFilter::Controller,
            },
        );
        def.player_scope = Some(crate::types::ability::PlayerFilter::All);

        let ability = build_resolved_from_def(&def, source, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let flippers: Vec<PlayerId> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::CoinFlipped { player_id, .. } => Some(*player_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            flippers.len(),
            2,
            "each of 2 players flips exactly once, got {flippers:?}"
        );
        assert!(
            flippers.contains(&PlayerId(0)) && flippers.contains(&PlayerId(1)),
            "both players must flip their own coin, got {flippers:?}"
        );
    }

    #[test]
    fn flip_coin_flipper_serde_back_compat_and_roundtrip() {
        use crate::types::ability::Effect;

        // (a) CR 705.2: a current `FlipCoins` with no `flipper` field deserializes to
        // the Controller default.
        let current: Effect = serde_json::from_str(
            r#"{"type":"FlipCoins","flips":{"type":"Exactly","count":{"type":"Fixed","value":1}},"win_effect":null,"lose_effect":null}"#,
        )
        .expect("current FlipCoins without flipper must deserialize");
        assert!(
            matches!(
                current,
                Effect::FlipCoins {
                    flips: CoinFlipRun::Exactly {
                        count: QuantityExpr::Fixed { value: 1 }
                    },
                    flipper: TargetFilter::Controller,
                    ..
                }
            ),
            "missing flipper must default to Controller, got {current:?}"
        );
        let until_loss: Effect =
            serde_json::from_str(r#"{"type":"FlipCoins","flips":{"type":"UntilLoss"}}"#)
                .expect("an UntilLoss run with no branches must deserialize");
        assert!(
            matches!(
                until_loss,
                Effect::FlipCoins {
                    flips: CoinFlipRun::UntilLoss,
                    win_effect: None,
                    lose_effect: None,
                    flipper: TargetFilter::Controller,
                }
            ),
            "got {until_loss:?}"
        );

        // (b) The default flipper is skipped on serialize (back-compatible output).
        let controller_flip = Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: None,
            lose_effect: None,
            flipper: TargetFilter::Controller,
        };
        let json = serde_json::to_string(&controller_flip).unwrap();
        assert!(
            !json.contains("flipper"),
            "default Controller flipper must be omitted from JSON, got {json}"
        );

        // (c) A non-default flipper round-trips.
        let subject_flip = Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: None,
            lose_effect: None,
            flipper: TargetFilter::TriggeringPlayer,
        };
        let json = serde_json::to_string(&subject_flip).unwrap();
        assert!(
            json.contains("flipper"),
            "non-default flipper must serialize"
        );
        let back: Effect = serde_json::from_str(&json).unwrap();
        assert_eq!(back, subject_flip, "non-default flipper must round-trip");

        // (d) Card data fails closed: the retired shapes are `Err` at the type level. Their
        // only reader is the persisted-state migration (`migrate_legacy_coin_flip_effects`);
        // an alias or a `flips` default here would let them parse silently, and a legacy
        // `FlipCoins { count }` would read as one flip because `count` is ignored.
        for legacy in [
            r#"{"type":"FlipCoin","win_effect":null,"lose_effect":null}"#,
            r#"{"type":"FlipCoinUntilLose","win_effect":{"kind":"Spell","effect":{"type":"Draw","count":{"type":"Fixed","value":1}}}}"#,
            r#"{"type":"FlipCoins","count":{"type":"Fixed","value":3},"win_effect":null,"lose_effect":null}"#,
        ] {
            assert!(
                serde_json::from_str::<Effect>(legacy).is_err(),
                "retired coin-flip shape must be rejected at the type level: {legacy}"
            );
        }
        let missing_flips = serde_json::from_str::<Effect>(
            r#"{"type":"FlipCoins","count":{"type":"Fixed","value":3}}"#,
        )
        .unwrap_err();
        assert!(
            missing_flips.to_string().contains("flips"),
            "a legacy counted FlipCoins fails on the missing `flips` field, got {missing_flips}"
        );
    }

    /// CR 705.2: resolving a single flip records the flipper's result in
    /// `state.resolution_coin_flip` so a downstream `WhileCondition` loop gate
    /// can read it. The recorded result must agree with the emitted `CoinFlipped`.
    #[test]
    fn flip_records_resolution_coin_flip() {
        use crate::types::ability::TargetFilter;
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: None,
                lose_effect: None,
                flipper: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let recorded = state
            .resolution_coin_flip
            .expect("flip must record resolution_coin_flip");
        assert_eq!(recorded.flipper, PlayerId(0), "flipper is the controller");
        let event_won = events.iter().find_map(|e| match e {
            GameEvent::CoinFlipped { won, .. } => Some(*won),
            _ => None,
        });
        assert_eq!(
            Some(recorded.result),
            event_won.map(CoinFlipResult::from_won),
            "recorded result must match the CoinFlipped event"
        );
    }

    /// CR 705.2 + CR 608.2c: end-to-end runtime proof of the new primitive — a
    /// bare-flip body under `WhileCondition { CoinFlipOutcome{Lost} }` re-follows
    /// the process while the controller keeps losing and STOPS the instant a flip
    /// is won. Deterministic regardless of seed: the loop only continues on a
    /// loss, so it must terminate on a win and the final flip is a win. Mirrors
    /// the `flip_coin_until_lose` termination assertion (inverted polarity).
    #[test]
    fn while_condition_lost_flip_loops_until_win() {
        use crate::types::ability::{
            AbilityCondition, CoinFlipResult, RepeatContinuation, TargetFilter,
        };

        let mut state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: None,
                lose_effect: None,
                flipper: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        ability.repeat_until = Some(RepeatContinuation::WhileCondition {
            condition: Box::new(AbilityCondition::CoinFlipOutcome {
                result: CoinFlipResult::Lost,
            }),
            max_iterations: None,
        });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let flips: Vec<bool> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::CoinFlipped { won, .. } => Some(*won),
                _ => None,
            })
            .collect();
        assert!(!flips.is_empty(), "at least one flip must occur");
        // Every flip before the last must be a loss (the loop's continue gate).
        assert!(
            flips[..flips.len() - 1].iter().all(|won| !won),
            "all non-final flips must be losses, got {flips:?}"
        );
        // The loop only stops on a win, so the final flip is a win.
        assert_eq!(
            flips.last(),
            Some(&true),
            "the loop must terminate on a won flip, got {flips:?}"
        );
        assert_eq!(
            state.resolution_coin_flip,
            Some(ResolutionCoinFlip {
                flipper: PlayerId(0),
                result: CoinFlipResult::Won,
            }),
            "final resolution flip is the winning flip for the controller"
        );
    }

    /// CR 705.2 + CR 608.2c (matt's blocker on #5682): the resolution-scoped flip
    /// result must NOT outlive its resolution. A stale winning flip left over
    /// from a prior resolution must be cleared at the authoritative
    /// resolution-lifetime boundary (top-level `resolve_ability_chain` entry), so
    /// a later resolution whose own `CoinFlipOutcome` gate never flips a coin
    /// reads `false` and does not run the gated effect.
    #[test]
    fn stale_flip_does_not_leak_into_next_resolution() {
        use crate::types::ability::{AbilityCondition, CoinFlipResult, QuantityExpr, TargetFilter};

        let mut state = GameState::new_two_player(42);
        // Simulate a prior resolution that ended having WON a flip.
        state.resolution_coin_flip = Some(ResolutionCoinFlip {
            flipper: PlayerId(0),
            result: CoinFlipResult::Won,
        });

        // A fresh, unrelated resolution that performs NO coin flip but is gated on
        // "if you win the flip". Pre-fix, the stale Won would satisfy this gate and
        // the draw would wrongly fire.
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        ability.condition = Some(AbilityCondition::CoinFlipOutcome {
            result: CoinFlipResult::Won,
        });

        let hand_before = state.players[0].hand.len();
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // The top-level boundary clear wiped the stale flip before any instruction
        // ran, so the gate is false and no card was drawn.
        assert!(
            state.resolution_coin_flip.is_none(),
            "stale flip must be cleared at the resolution-lifetime boundary, got {:?}",
            state.resolution_coin_flip
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::CardDrawn { .. })),
            "gated draw must not fire off a stale flip, events: {events:?}"
        );
        assert_eq!(
            state.players[0].hand.len(),
            hand_before,
            "no card should have been drawn"
        );
    }

    /// CR 705.2 + CR 608.2c (matt's required runtime scenario for #5682): drive
    /// the real Unleash the Flux process end-to-end through the action pipeline.
    /// Each player sacrifices a nonland permanent of their choice, then the
    /// controller flips a coin, and "if you lose the flip, repeat this process."
    /// Proves at runtime that an actual LOSS re-follows the whole process (a fresh
    /// each-player sacrifice prompt appears) and an actual WIN stops it — driving
    /// the each-player sacrifice choices (`SelectCards`), the flip continuation
    /// resume, and the `WhileCondition` repeat decision.
    #[test]
    fn unleash_the_flux_runtime_loss_repeats_win_stops() {
        use crate::game::ability_utils::build_resolved_from_def_with_targets;
        use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
        use crate::parser::parse_oracle_text;
        use crate::types::ability::CoinFlipResult;
        use crate::types::actions::GameAction;
        use crate::types::phase::Phase;

        // Parse the real card and lift its encounter-trigger execute body: the
        // each-player-sacrifice root (`player_scope`), the flip sub-ability, and
        // the `WhileCondition { CoinFlipOutcome{Lost} }` repeat.
        let parsed = parse_oracle_text(
            "When you encounter Unleash the Flux, each player sacrifices a nonland permanent of their choice, then you flip a coin. If you lose the flip, repeat this process. (Then planeswalk away from this phenomenon.)",
            "Unleash the Flux",
            &[],
            &["Phenomenon".to_string()],
            &[],
        );
        let execute = parsed
            .triggers
            .iter()
            .find_map(|t| t.execute.as_ref())
            .expect("Unleash the Flux must have an encounter trigger with an execute body");

        // Two players, three nonland permanents each: enough for two PROMPTED
        // iterations. With count == 1, a step is auto-resolved (no prompt) once a
        // player has a single eligible permanent; keeping >= 2 candidates in both
        // iterations keeps each flip on its own action so the test can prime the
        // RNG per flip. (3 → prompt in iter 1, then 2 → prompt in iter 2.)
        let mut scenario = GameScenario::new_n_player(2, 7);
        scenario.at_phase(Phase::PreCombatMain);
        for _ in 0..3 {
            scenario.add_vanilla(P0, 2, 2);
            scenario.add_vanilla(P1, 2, 2);
        }
        // Off-battlefield stand-in for the phenomenon as the ability source; a
        // hand card is never a "nonland permanent" sacrifice candidate.
        let source = scenario.add_card_to_hand(P0, "Unleash the Flux");
        let mut runner = scenario.build();

        let ability = build_resolved_from_def_with_targets(execute, source, P0, vec![]);

        // Prime iteration 1's flip as a LOSS (seed 1 → first flip loses). Nothing
        // else draws the RNG before the flip, so this survives to the flip.
        runner.state_mut().rng = crate::types::game_state::GameRng::seed_from_u64(1);
        let mut events = Vec::new();
        resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
            .expect("initial resolution must set up the first sacrifice choice");

        // Submit the first eligible card from the current each-player sacrifice
        // prompt (asserting the process actually paused on an interactive choice),
        // returning any coin-flip results the resulting drain produced.
        let sacrifice_first = |runner: &mut GameRunner| -> Vec<bool> {
            let cards = match runner.state().waiting_for.clone() {
                WaitingFor::EffectZoneChoice { cards, .. } => cards,
                other => panic!("expected an each-player sacrifice prompt, got {other:?}"),
            };
            let result = runner
                .act(GameAction::SelectCards {
                    cards: vec![cards[0]],
                })
                .expect("submitting a sacrifice choice must be accepted");
            result
                .events
                .iter()
                .filter_map(|e| match e {
                    GameEvent::CoinFlipped { won, .. } => Some(*won),
                    _ => None,
                })
                .collect()
        };

        // ── Iteration 1: both players sacrifice, controller LOSES the flip ──
        // The flip only happens once every player has chosen, so the first
        // choice produces no flip and the second produces the LOSS.
        assert!(
            sacrifice_first(&mut runner).is_empty(),
            "no coin is flipped until every player has sacrificed"
        );
        assert_eq!(
            sacrifice_first(&mut runner),
            vec![false],
            "iteration 1 must flip exactly once and the controller must LOSE it"
        );

        assert_eq!(
            runner.state().players[0].graveyard.len() + runner.state().players[1].graveyard.len(),
            2,
            "one nonland permanent per player must be sacrificed in iteration 1"
        );
        // The LOST flip re-follows the process: a fresh each-player sacrifice
        // prompt is open again (the stored flip was already cleared by the next
        // iteration's intra-loop reset, which is why the repeat itself is the
        // observable proof of the loss).
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::EffectZoneChoice { .. }
            ),
            "a LOST flip must repeat the process — a fresh sacrifice prompt is expected, got {:?}",
            runner.state().waiting_for
        );

        // ── Iteration 2: both players sacrifice, controller WINS the flip ──
        assert!(
            sacrifice_first(&mut runner).is_empty(),
            "no coin is flipped until every player has sacrificed (iteration 2)"
        );
        // Prime iteration 2's flip as a WIN (seed 0 → first flip wins) just before
        // the action that performs the sacrifices and resumes into the flip.
        runner.state_mut().rng = crate::types::game_state::GameRng::seed_from_u64(0);
        assert_eq!(
            sacrifice_first(&mut runner),
            vec![true],
            "iteration 2 must flip exactly once and the controller must WIN it"
        );

        assert_eq!(
            runner.state().players[0].graveyard.len() + runner.state().players[1].graveyard.len(),
            4,
            "a second nonland permanent per player is sacrificed in iteration 2"
        );
        assert!(
            !matches!(
                runner.state().waiting_for,
                WaitingFor::EffectZoneChoice { .. }
            ),
            "a WON flip must STOP the process — no further sacrifice prompt, got {:?}",
            runner.state().waiting_for
        );
        assert_eq!(
            runner.state().resolution_coin_flip,
            Some(ResolutionCoinFlip {
                flipper: P0,
                result: CoinFlipResult::Won,
            }),
            "the process stops on the controller's winning flip"
        );
        assert!(
            runner.state().active_repeat_until().is_none(),
            "the repeat loop must be fully drained after the winning flip"
        );
    }

    // --- Run-level characterization -------------------------------------------
    //
    // These pin how a whole coin-flip run behaves (reward timing, count latching,
    // the safety cap, and the single completion marker) independently of how the
    // resolver is structured, so a later consolidation of the flip resolvers must
    // keep them green unchanged.

    /// Counts the `EffectResolved` markers a coin-flip run reported for `source`.
    fn coin_run_markers(events: &[GameEvent], source: ObjectId) -> usize {
        events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::FlipCoins,
                        source_id,
                        ..
                    } if *source_id == source
                )
            })
            .count()
    }

    /// The `won` result of every `CoinFlipped` event, in order.
    fn coin_flip_results(events: &[GameEvent]) -> Vec<bool> {
        events
            .iter()
            .filter_map(|e| match e {
                GameEvent::CoinFlipped { won, .. } => Some(*won),
                _ => None,
            })
            .collect()
    }

    fn gain_life_definition(amount: i32) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: amount },
                player: TargetFilter::Controller,
            },
        )
    }

    fn until_loss_gain_life() -> Effect {
        Effect::FlipCoins {
            flips: CoinFlipRun::UntilLoss,
            win_effect: Some(Box::new(gain_life_definition(1))),
            lose_effect: None,
            flipper: TargetFilter::Controller,
        }
    }

    /// CR 705.1 + CR 608.2c: "flip a coin until you lose a flip. For each flip you
    /// won, [reward]" follows its instructions in written order, so every flip of
    /// the run happens before any reward is paid.
    #[test]
    fn until_loss_rewards_follow_every_flip() {
        let mut reached = false;
        for seed in 0..64u64 {
            let mut state = GameState::new_two_player(42);
            state.rng = crate::types::game_state::GameRng::seed_from_u64(seed);
            let start_life = state.players[0].life;
            let ability =
                ResolvedAbility::new(until_loss_gain_life(), vec![], ObjectId(1), PlayerId(0));
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();

            let flips = coin_flip_results(&events);
            let wins = flips.iter().filter(|won| **won).count();
            // Reach guard: at least two won flips, so a reward interleaved after
            // the first win would sit between two flips.
            if wins < 2 {
                continue;
            }
            reached = true;

            assert_eq!(
                flips.last(),
                Some(&false),
                "the run ends on the losing flip, got {flips:?}"
            );
            let last_flip = events
                .iter()
                .rposition(|e| matches!(e, GameEvent::CoinFlipped { .. }))
                .expect("the run flipped");
            let rewards: Vec<usize> = events
                .iter()
                .enumerate()
                .filter(|(_, e)| matches!(e, GameEvent::LifeChanged { .. }))
                .map(|(index, _)| index)
                .collect();
            assert_eq!(
                rewards.len(),
                wins,
                "one reward per won flip (seed {seed}, flips {flips:?})"
            );
            assert!(
                rewards.iter().all(|index| *index > last_flip),
                "every reward comes after the last flip (seed {seed}, flips {flips:?})"
            );
            assert_eq!(state.players[0].life - start_life, wins as i32);
            break;
        }
        assert!(reached, "some seed in 0..64 must win at least two flips");
    }

    /// Engine contract (no printed card has this shape): an `UntilLoss` run's
    /// `lose_effect` runs as the losing flip settles (CR 705.2), before the owed
    /// `win_effect` rewards are paid (CR 608.2c: the reward instruction follows the
    /// flipping instruction). Win GainLife 1, lose GainLife 7.
    #[test]
    fn until_loss_lose_branch_runs_at_the_loss_before_rewards() {
        let mut reached = false;
        for seed in 0..64u64 {
            let mut state = GameState::new_two_player(42);
            state.rng = crate::types::game_state::GameRng::seed_from_u64(seed);
            let start_life = state.players[0].life;
            let ability = ResolvedAbility::new(
                Effect::FlipCoins {
                    flips: CoinFlipRun::UntilLoss,
                    win_effect: Some(Box::new(gain_life_definition(1))),
                    lose_effect: Some(Box::new(gain_life_definition(7))),
                    flipper: TargetFilter::Controller,
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            );
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();

            let flips = coin_flip_results(&events);
            let wins = flips.iter().filter(|won| **won).count();
            // Reach guard: at least one owed reward, so the order is observable.
            if wins < 1 {
                continue;
            }
            reached = true;

            assert_eq!(
                flips.last(),
                Some(&false),
                "the run ends on the losing flip, got {flips:?}"
            );
            let last_flip = events
                .iter()
                .rposition(|e| matches!(e, GameEvent::CoinFlipped { .. }))
                .expect("the run flipped");
            let life_changes: Vec<(usize, i32)> = events
                .iter()
                .enumerate()
                .filter_map(|(index, e)| match e {
                    GameEvent::LifeChanged { amount, .. } => Some((index, *amount)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                life_changes.len(),
                wins + 1,
                "one lose-branch change plus one reward per won flip (seed {seed}, flips {flips:?})"
            );
            let (lose_index, lose_amount) = life_changes[0];
            assert_eq!(lose_amount, 7, "the lose branch runs first");
            assert!(
                lose_index > last_flip,
                "the lose branch runs once the losing flip has settled"
            );
            assert!(
                life_changes[1..].iter().all(|(_, amount)| *amount == 1),
                "every later change is a +1 reward, got {life_changes:?}"
            );
            assert_eq!(state.players[0].life - start_life, wins as i32 + 7);
            assert_eq!(coin_run_markers(&events, ObjectId(1)), 1);
            break;
        }
        assert!(reached, "some seed in 0..64 must win at least one flip");
    }

    /// CR 608.2h + CR 107.1b: "flip N coins" determines N once, when the run
    /// starts. A won flip that draws a card grows the hand, but the run still
    /// flips exactly the count the hand size gave at the start.
    #[test]
    fn exactly_count_is_latched_when_the_run_starts() {
        use crate::types::ability::{PlayerScope, QuantityRef};

        let mut reached = false;
        for seed in 0..64u64 {
            let mut state = GameState::new_two_player(42);
            state.rng = crate::types::game_state::GameRng::seed_from_u64(seed);
            for i in 0..10 {
                create_object(
                    &mut state,
                    CardId(100 + i),
                    PlayerId(0),
                    format!("Library Card {i}"),
                    Zone::Library,
                );
            }
            for i in 0..2 {
                create_object(
                    &mut state,
                    CardId(200 + i),
                    PlayerId(0),
                    format!("Hand Card {i}"),
                    Zone::Hand,
                );
            }
            assert_eq!(
                state.players[0].hand.len(),
                2,
                "the run starts with 2 cards"
            );

            let ability = ResolvedAbility::new(
                Effect::FlipCoins {
                    flips: CoinFlipRun::Exactly {
                        count: QuantityExpr::Ref {
                            qty: QuantityRef::HandSize {
                                player: PlayerScope::Controller,
                            },
                        },
                    },
                    win_effect: Some(Box::new(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ))),
                    lose_effect: None,
                    flipper: TargetFilter::Controller,
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            );
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();

            let flips = coin_flip_results(&events);
            assert_eq!(
                flips.len(),
                2,
                "the count is latched at the starting hand size (seed {seed}, flips {flips:?})"
            );
            // Reach guard: the first flip was won, so the hand had already grown
            // to 3 when the second flip happened. A per-flip re-read of the hand
            // size would have flipped a third coin.
            if !flips[0] {
                continue;
            }
            reached = true;
            let wins = flips.iter().filter(|won| **won).count();
            assert_eq!(
                state.players[0].hand.len(),
                2 + wins,
                "each won flip drew exactly one card"
            );
            break;
        }
        assert!(reached, "some seed in 0..64 must win the first flip");
    }

    /// A run parked at a kept flip, for the cap tests below.
    fn pending_until_lose(wins_so_far: u32) -> PendingCoinFlip {
        PendingCoinFlip {
            source_id: ObjectId(1),
            controller: PlayerId(0),
            flipper: PlayerId(0),
            targets: vec![],
            win_effect: Some(Box::new(gain_life_definition(1))),
            lose_effect: None,
            kind: PendingCoinFlipKind::UntilLoss { wins_so_far },
            chain_root_targets: vec![],
        }
    }

    /// CR 705.1 + CR 608.2c: a kept win that lands the streak on the safety cap
    /// ends the run without another flip, pays every owed reward, and reports the
    /// run resolved exactly once.
    #[test]
    fn kept_win_at_the_cap_pays_every_reward_and_reports_once() {
        // The safety cap is the module-level `MAX_FLIPS`. `wins_so_far` counts the
        // wins before the kept one.
        // At the cap: wins_so_far = MAX_FLIPS - 1, and the kept flip is the MAX_FLIPS-th win.
        let mut state = GameState::new_two_player(42);
        let start_life = state.players[0].life;
        let mut events = Vec::new();
        let outcome = resume_after_keep(
            &mut state,
            pending_until_lose(MAX_FLIPS - 1),
            vec![true],
            &mut events,
        )
        .expect("resuming the kept win succeeds");
        assert_eq!(outcome, None, "the run completed");
        assert_eq!(
            coin_flip_results(&events),
            vec![true],
            "only the kept flip happens: no further flip at the cap"
        );
        assert_eq!(
            state.players[0].life - start_life,
            MAX_FLIPS as i32,
            "every one of the {MAX_FLIPS} owed rewards is paid"
        );
        assert_eq!(
            coin_run_markers(&events, ObjectId(1)),
            1,
            "the completed run reports resolved exactly once"
        );

        // Negative pair: one win short of the cap, the kept win still flips again.
        let mut state = GameState::new_two_player(42);
        let start_life = state.players[0].life;
        let mut events = Vec::new();
        let outcome = resume_after_keep(
            &mut state,
            pending_until_lose(MAX_FLIPS - 2),
            vec![true],
            &mut events,
        )
        .expect("resuming the kept win succeeds");
        assert_eq!(outcome, None, "the run completed");
        let flips = coin_flip_results(&events);
        assert!(
            flips.len() >= 2,
            "below the cap the run flips again after the kept win, got {flips:?}"
        );
        let later_wins = flips.iter().skip(1).filter(|won| **won).count() as i32;
        assert_eq!(
            state.players[0].life - start_life,
            (MAX_FLIPS - 1) as i32 + later_wins,
            "rewards paid == the kept win plus every owed and later win"
        );
        assert_eq!(coin_run_markers(&events, ObjectId(1)), 1);
    }

    /// Engine contract: a run that completes with no pending resolution choice
    /// reports `EffectResolved` exactly once, for every run form.
    #[test]
    fn fresh_runs_report_resolved_once() {
        let source = ObjectId(1);
        let fresh_state = || {
            let mut state = GameState::new_two_player(42);
            state.waiting_for = WaitingFor::Priority {
                player: PlayerId(0),
            };
            state
        };
        let ability = |effect: Effect| ResolvedAbility::new(effect, vec![], source, PlayerId(0));

        // Single flip, no branches.
        let mut state = fresh_state();
        let mut events = Vec::new();
        resolve(
            &mut state,
            &ability(Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: None,
                lose_effect: None,
                flipper: TargetFilter::Controller,
            }),
            &mut events,
        )
        .unwrap();
        assert_eq!(coin_flip_results(&events).len(), 1);
        assert_eq!(coin_run_markers(&events, source), 1, "single flip");

        // Three flips, no branches.
        let mut state = fresh_state();
        let mut events = Vec::new();
        resolve(
            &mut state,
            &ability(Effect::FlipCoins {
                flips: CoinFlipRun::Exactly {
                    count: QuantityExpr::Fixed { value: 3 },
                },
                win_effect: None,
                lose_effect: None,
                flipper: TargetFilter::Controller,
            }),
            &mut events,
        )
        .unwrap();
        assert_eq!(coin_flip_results(&events).len(), 3);
        assert_eq!(coin_run_markers(&events, source), 1, "three flips");

        // Zero flips: no flip happens, the run still reports resolved once.
        let mut state = fresh_state();
        let mut events = Vec::new();
        resolve(
            &mut state,
            &ability(Effect::FlipCoins {
                flips: CoinFlipRun::Exactly {
                    count: QuantityExpr::Fixed { value: 0 },
                },
                win_effect: None,
                lose_effect: None,
                flipper: TargetFilter::Controller,
            }),
            &mut events,
        )
        .unwrap();
        assert!(coin_flip_results(&events).is_empty(), "no coin is flipped");
        assert_eq!(coin_run_markers(&events, source), 1, "zero flips");

        // Until-loss with a reward.
        let mut state = fresh_state();
        let mut events = Vec::new();
        resolve(&mut state, &ability(until_loss_gain_life()), &mut events).unwrap();
        assert_eq!(
            coin_flip_results(&events).last(),
            Some(&false),
            "the until-loss run ends on a loss"
        );
        assert_eq!(coin_run_markers(&events, source), 1, "until-loss run");
    }
}
