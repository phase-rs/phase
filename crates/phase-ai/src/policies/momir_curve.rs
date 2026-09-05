//! Momir curve policy — when to activate a random-creature mana sink, and for
//! how much.
//!
//! Momir's Madness gives every player a game-start command-zone emblem reading
//! "{X}, Discard a card: Create a token that's a copy of a creature card with
//! mana value X chosen at random. Activate only as a sorcery and only once each
//! turn." (CR 707.2 copy semantics, CR 202.3 mana value, CR 701.9a discard.)
//! Without this policy the AI never activates it: the effect's polarity is
//! `Contextual` (`effect_classify.rs`), so no other policy has an opinion and
//! `PassPriority` wins by default.
//!
//! # The schedule
//!
//! The deck is 60 lands, so a player's land count equals their own turn count
//! and X is bounded by it. The default line is "spend the whole turn on the
//! sink", capped at 8 — beyond 8 the creature pool thins out and the extra mana
//! buys little:
//!
//! | own turn | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9+ |
//! |---|---|---|---|---|---|---|---|---|---|
//! | on the play | — | — | 3 | 4 | 5 | 6 | 7 | 8 | 8 |
//! | on the draw | — | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 8 |
//!
//! CR 103.8a: in a two-player game the player on the play skips their first
//! draw step, so they are a card behind all game. Every activation costs a
//! discard (CR 701.9a), so that player holds off one extra turn rather than
//! spending a card on a two-drop; the player on the draw, up a card, starts on
//! their second turn.
//!
//! # Detection is structural, not by name
//!
//! The policy binds to `Effect::CreateTokenCopyFromPool` on the activated
//! ability, never to an emblem name or the format. Any future card or emblem
//! printing that effect gets the same treatment.
//!
//! # Turn accounting
//!
//! `GameState::turn_number` counts *player* turns, not rounds — it increments
//! on every turn change (`game/turns.rs`). In the two-player table Momir
//! mandates (`FormatConfig::momir` fixes `min_players == max_players == 2`),
//! a player's own turn index is therefore derived from `turn_number` and
//! whether they are `current_starting_player`.

use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::mulligan::TurnOrder;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Largest X the schedule ever asks for. Past this the marginal creature is not
/// worth the extra land drop's worth of mana, and the pool at very high mana
/// values is thin.
pub const MAX_SCHEDULED_X: u32 = 8;

/// First own-turn on which the player on the play activates. CR 103.8a: they
/// skipped a draw step, so they are a card down and each activation costs a
/// discard (CR 701.9a).
pub const ON_PLAY_FIRST_TURN: u32 = 3;

/// First own-turn on which the player on the draw activates.
pub const ON_DRAW_FIRST_TURN: u32 = 2;

/// CR 202.3: Emrakul, the Aeons Torn's mana value.
///
/// Once a player can actually pay 15, the schedule abandons its cap and rolls
/// at 15 every turn until it hits Emrakul. At that mana value the eligible pool
/// is only five creatures wide, so each activation is roughly a one-in-five
/// shot at the best creature in the format — a gamble worth repeating, and one
/// that stops being worth it the moment it pays off.
pub const EMRAKUL_MANA_VALUE: u32 = 15;

/// The prize the 15-mana line is hunting. Matched by name because that is what
/// "until Emrakul is found" means — this is an identity lookup against a token
/// the draw already created, not a structural classification standing in for
/// one. Nothing else in this policy matches on a name.
pub const EMRAKUL_NAME: &str = "Emrakul, the Aeons Torn";

pub struct MomirCurvePolicy;

/// Whether `player` is on the play or on the draw.
fn turn_order(state: &GameState, player: PlayerId) -> TurnOrder {
    if state.current_starting_player == player {
        TurnOrder::OnPlay
    } else {
        TurnOrder::OnDraw
    }
}

/// The player's own turn index (their Nth turn), derived from the shared
/// `turn_number` player-turn counter on a two-player table.
///
/// `turn_number` is 1-based and increments per player turn, so the player on
/// the play owns the odd turns and the player on the draw the even ones.
fn own_turn_index(state: &GameState, player: PlayerId) -> u32 {
    match turn_order(state, player) {
        TurnOrder::OnPlay => state.turn_number.div_ceil(2),
        TurnOrder::OnDraw => state.turn_number / 2,
    }
}

/// First own-turn this player activates on.
fn first_activation_turn(state: &GameState, player: PlayerId) -> u32 {
    match turn_order(state, player) {
        TurnOrder::OnPlay => ON_PLAY_FIRST_TURN,
        TurnOrder::OnDraw => ON_DRAW_FIRST_TURN,
    }
}

/// Whether `player` already controls an Emrakul token, which ends the 15-mana
/// hunt.
///
/// A battlefield scan, so it runs LAST: every caller reaches it only after the
/// cheap turn check and the `affordable >= EMRAKUL_MANA_VALUE` test, and only
/// for a candidate already confirmed to be the pool sink.
///
/// "Found" is read as "controls one now" rather than "ever created one": if the
/// Emrakul is answered, the hunt is worth resuming, and no ever-created ledger
/// exists to consult without adding one.
fn controls_emrakul(state: &GameState, player: PlayerId) -> bool {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .any(|object| object.controller == player && object.name == EMRAKUL_NAME)
}

/// The scheduled X for this player right now, or `None` when they should not
/// activate at all.
///
/// `affordable` is the largest X this player could actually pay right now. It
/// is an INPUT rather than something this function computes, because the two
/// callers already hold it from different authorities: the activation decision
/// prices it with `max_x_value`, and the `{X}` prompt is handed it as the
/// prompt's own `max`.
///
/// Returns `None` — meaning "do not activate" — before the first scheduled turn
/// and whenever nothing can be paid. That second guard is load-bearing: without
/// it the AI activated on a turn whose lands were already tapped, was offered
/// `min=0 max=0`, and spent its once-per-turn activation and a card on a
/// mana-value-0 creature.
fn scheduled_x(state: &GameState, player: PlayerId, affordable: u32) -> Option<u32> {
    let own_turn = own_turn_index(state, player);
    if own_turn < first_activation_turn(state, player) || affordable == 0 {
        return None;
    }
    // The Emrakul hunt outranks the cap, but only once it is genuinely payable.
    if affordable >= EMRAKUL_MANA_VALUE && !controls_emrakul(state, player) {
        return Some(EMRAKUL_MANA_VALUE);
    }
    Some(own_turn.min(MAX_SCHEDULED_X).min(affordable))
}

/// The largest X this player could pay for the ability at `ability_index` on
/// `source_id`, or 0 when it has no `{X}` mana leg.
///
/// `max_x_value` is a board-wide affordability sweep, so per the inner-loop
/// ordering rule it runs only after `is_pool_sink_activation` has confirmed the
/// candidate is the sink — one object in the whole game, at most once per
/// activation candidate.
fn affordable_x(
    state: &GameState,
    player: PlayerId,
    source_id: &engine::types::identifiers::ObjectId,
    ability_index: usize,
) -> u32 {
    state
        .objects
        .get(source_id)
        .and_then(|object| object.abilities.get(ability_index))
        .and_then(|ability| ability.cost.as_ref())
        .and_then(engine::game::extract_x_mana_cost)
        .map(|(mana_cost, _residual)| {
            engine::game::max_x_value(state, player, &mana_cost, Some(*source_id))
        })
        .unwrap_or(0)
}

/// Whether the ability at `ability_index` on `source_id` is a random-pool
/// creature sink. Card-local: reads one object's ability chain, no board sweep.
fn is_pool_sink_activation(
    state: &GameState,
    source_id: &engine::types::identifiers::ObjectId,
    ability_index: usize,
) -> bool {
    state
        .objects
        .get(source_id)
        .and_then(|object| object.abilities.get(ability_index))
        .map(|ability| {
            crate::ability_chain::collect_chain_effects(ability)
                .iter()
                .any(|effect| matches!(effect, Effect::CreateTokenCopyFromPool { .. }))
        })
        .unwrap_or(false)
}

/// Whether the X prompt the AI is answering belongs to a random-pool sink.
fn pending_x_is_pool_sink(state: &GameState) -> bool {
    let WaitingFor::ChooseXValue { pending_cast, .. } = &state.waiting_for else {
        return false;
    };
    // `collect_ability_effects` walks the whole `sub_ability` chain, not just
    // the head effect.
    super::context::collect_ability_effects(&pending_cast.ability)
        .iter()
        .any(|effect| matches!(effect, Effect::CreateTokenCopyFromPool { .. }))
}

fn na() -> PolicyVerdict {
    PolicyVerdict::neutral(PolicyReason::new("momir_curve_na"))
}

impl TacticalPolicy for MomirCurvePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::MomirCurve
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility, DecisionKind::ChooseX]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Cheapest possible opt-out: a plain bool field read. The random-pool
        // sink is a command-zone emblem, so a format with no command zone can
        // never present one and the registry skips `verdict` entirely. The
        // precise, card-local check lives in `verdict` per the inner-loop
        // ordering rule (cheap structural gate first, never a board sweep).
        state.format_config.command_zone.then_some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let penalties = &ctx.config.policy_penalties;
        match &ctx.candidate.action {
            // CR 117.1b: activating the sink is a normal priority action; the
            // engine already enforces "only as a sorcery, only once each turn".
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => {
                if !is_pool_sink_activation(ctx.state, source_id, *ability_index) {
                    return na();
                }
                let affordable = affordable_x(ctx.state, ctx.ai_player, source_id, *ability_index);
                match scheduled_x(ctx.state, ctx.ai_player, affordable) {
                    Some(target) => PolicyVerdict::strong(
                        penalties.momir_curve_activation,
                        PolicyReason::new("momir_curve_activate")
                            .with_fact("scheduled_x", i64::from(target))
                            .with_fact("affordable_x", i64::from(affordable))
                            .with_fact(
                                "own_turn",
                                i64::from(own_turn_index(ctx.state, ctx.ai_player)),
                            ),
                    ),
                    // Either the schedule has not opened yet — the discard
                    // (CR 701.9a) costs more than the small creature it would
                    // buy — or nothing is payable, which would burn the
                    // once-each-turn activation on a mana-value-0 creature.
                    None => PolicyVerdict::reject(
                        PolicyReason::new("momir_curve_not_scheduled")
                            .with_fact("affordable_x", i64::from(affordable))
                            .with_fact(
                                "own_turn",
                                i64::from(own_turn_index(ctx.state, ctx.ai_player)),
                            ),
                    ),
                }
            }
            // CR 202.3: X is the created creature's mana value, so this choice
            // IS the schedule.
            GameAction::ChooseX { value } => {
                if !pending_x_is_pool_sink(ctx.state) {
                    return na();
                }
                let WaitingFor::ChooseXValue { min, max, .. } = &ctx.state.waiting_for else {
                    return na();
                };
                // The prompt's own `max` IS the affordability authority here —
                // the engine already priced it — so the schedule needs no board
                // sweep of its own at this decision.
                let Some(target) = scheduled_x(ctx.state, ctx.ai_player, *max) else {
                    return na();
                };
                let target = target.clamp(*min, *max);
                if *value == target {
                    return PolicyVerdict::strong(
                        penalties.momir_curve_x_on_schedule,
                        PolicyReason::new("momir_curve_x_on_schedule")
                            .with_fact("chosen_x", i64::from(*value)),
                    );
                }
                // Every other X is a veto, in BOTH directions. A schedule
                // expressed as a preference is not a schedule: the search's own
                // value function reads "bigger creature" as strictly better and
                // outbids any in-band score by more than the preference band is
                // wide. Measured on a full AI-vs-AI game, a graduated penalty
                // produced `target + 1` at every rung when mana was plentiful
                // and `target - 1` where the search preferred to hold it.
                // `target` is clamped into the prompt's own `min..=max`, so it
                // is always a legal answer and this can never veto every
                // candidate.
                PolicyVerdict::reject(
                    PolicyReason::new("momir_curve_x_off_schedule")
                        .with_fact("chosen_x", i64::from(*value))
                        .with_fact("scheduled_x", i64::from(target)),
                )
            }
            _ => na(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::deck_loading::momir_emblem_ability;
    use engine::types::format::FormatConfig;
    use engine::types::identifiers::ObjectId;
    use engine::types::player::PlayerId;

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);

    /// A Momir state where `starter` took the first turn and the shared
    /// player-turn counter reads `turn_number`.
    fn momir_state(starter: PlayerId, turn_number: u32) -> GameState {
        let mut state = GameState::new(FormatConfig::momir(), 2, 42);
        state.current_starting_player = starter;
        state.turn_number = turn_number;
        state
    }

    /// The schedule P0 follows across their own turns 1..=11, as X values with
    /// `None` for "pass". `starter` decides whether P0 is on the play.
    ///
    /// `affordable` is set to the player's own turn index, which is what a real
    /// Momir board yields: the deck is 60 lands, so a player has exactly one
    /// land per turn taken and X is bounded by that.
    fn observed_schedule(starter: PlayerId) -> Vec<Option<u32>> {
        (1..=11)
            .map(|own_turn| {
                // `turn_number` is the shared per-player-turn counter: the
                // player on the play owns the odd turns, the other the even.
                let turn_number = if starter == P0 {
                    own_turn * 2 - 1
                } else {
                    own_turn * 2
                };
                let state = momir_state(starter, turn_number);
                assert_eq!(
                    own_turn_index(&state, P0),
                    own_turn,
                    "own-turn derivation must invert the turn_number mapping"
                );
                scheduled_x(&state, P0, own_turn)
            })
            .collect()
    }

    /// The requested default line, on the play: pass twice, then 3, 4, 5, 6, 7,
    /// 8, and 8 from there on.
    #[test]
    fn on_the_play_schedule_matches_the_specified_curve() {
        assert_eq!(
            observed_schedule(P0),
            vec![
                None,
                None,
                Some(3),
                Some(4),
                Some(5),
                Some(6),
                Some(7),
                Some(8),
                Some(8),
                Some(8),
                Some(8),
            ]
        );
    }

    /// The requested default line, on the draw: pass once, then 2, 3, 4, 5, 6,
    /// 7, 8, and 8 from there on. CR 103.8a: the player on the draw did not skip
    /// a draw step, so they are a card up and start one turn earlier.
    #[test]
    fn on_the_draw_schedule_matches_the_specified_curve() {
        assert_eq!(
            observed_schedule(P1),
            vec![
                None,
                Some(2),
                Some(3),
                Some(4),
                Some(5),
                Some(6),
                Some(7),
                Some(8),
                Some(8),
                Some(8),
                Some(8),
            ]
        );
    }

    #[test]
    fn turn_order_follows_the_starting_player() {
        assert_eq!(turn_order(&momir_state(P0, 1), P0), TurnOrder::OnPlay);
        assert_eq!(turn_order(&momir_state(P1, 1), P0), TurnOrder::OnDraw);
    }

    /// The cap is what stops the AI dumping a whole late-game mana base into a
    /// single creature; it must hold no matter how far the game runs.
    #[test]
    fn schedule_never_exceeds_the_cap() {
        for own_turn in 1..=40u32 {
            let state = momir_state(P0, own_turn * 2 - 1);
            // Affordability held below the Emrakul threshold so this asserts
            // the ordinary cap, not the 15-mana hunt.
            if let Some(x) = scheduled_x(&state, P0, MAX_SCHEDULED_X) {
                assert!(
                    x <= MAX_SCHEDULED_X,
                    "own turn {own_turn} scheduled X={x} above cap {MAX_SCHEDULED_X}"
                );
            }
        }
    }

    #[test]
    fn activation_opts_out_without_a_command_zone() {
        let mut state = momir_state(P0, 5);
        state.format_config = FormatConfig::standard();
        assert!(MomirCurvePolicy
            .activation(&DeckFeatures::default(), &state, P0)
            .is_none());
    }

    #[test]
    fn activation_opts_in_with_a_command_zone() {
        let state = momir_state(P0, 5);
        assert!(state.format_config.command_zone, "precondition");
        assert!(MomirCurvePolicy
            .activation(&DeckFeatures::default(), &state, P0)
            .is_some());
    }

    /// Detection is structural: it binds to the effect, never to an emblem name
    /// or the format.
    #[test]
    fn pool_sink_detection_reads_the_effect_not_the_name() {
        let mut state = momir_state(P0, 5);
        let emblem = engine::game::effects::create_emblem::grant_emblem(
            &mut state,
            P0,
            Vec::new(),
            Vec::new(),
            vec![momir_emblem_ability()],
        );
        assert!(is_pool_sink_activation(&state, &emblem, 0));
        // A different ability index on the same object is not the sink.
        assert!(!is_pool_sink_activation(&state, &emblem, 1));
        // An object with no abilities at all is not the sink.
        assert!(!is_pool_sink_activation(&state, &ObjectId(9999), 0));
    }

    /// PRODUCTION PATH. The schedule only matters if it survives the real
    /// `PolicyRegistry`, where every other policy also scores the candidate —
    /// asserting `scheduled_x` alone would pass even if the registry then
    /// picked a different X, which is exactly what happened before the
    /// off-schedule veto (a graduated penalty was outbid by the search's own
    /// "bigger creature is better" value).
    #[test]
    fn registry_priors_elevate_the_scheduled_x_over_every_other_value() {
        use crate::context::AiContext;
        use crate::policies::context::{PriorsEnv, SearchDepth};
        use crate::policies::registry::PolicyRegistry;
        use engine::ai_support::AiDecisionContext;
        use engine::ai_support::{ActionMetadata, CandidateAction, TacticalClass};
        use engine::types::game_state::PendingCast;
        use engine::types::identifiers::CardId;
        use engine::types::mana::{ManaCost, ManaCostShard};

        // Own turn 6 on the play (turn_number 11), so the schedule wants X=6.
        let mut state = momir_state(P0, 11);
        let emblem = engine::game::effects::create_emblem::grant_emblem(
            &mut state,
            P0,
            Vec::new(),
            Vec::new(),
            vec![momir_emblem_ability()],
        );
        assert_eq!(own_turn_index(&state, P0), 6);

        let max = 9;
        let pending = PendingCast::new(
            emblem,
            CardId(0),
            engine::types::ability::ResolvedAbility::new(
                *momir_emblem_ability().effect.clone(),
                Vec::new(),
                emblem,
                P0,
            ),
            ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            },
        );
        state.waiting_for = WaitingFor::ChooseXValue {
            player: P0,
            min: 0,
            max,
            pending_cast: Box::new(pending.clone()),
            convoke_mode: None,
            x_cost_previews: vec![],
        };

        let config = crate::config::AiConfig::default();
        let ai_context = AiContext::empty(&config.weights);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidates: Vec<CandidateAction> = (0..=max)
            .map(|value| CandidateAction {
                action: GameAction::ChooseX { value },
                metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Selection),
            })
            .collect();

        let env = PriorsEnv {
            state: &state,
            decision: &decision,
            ai_player: P0,
            config: &config,
            context: &ai_context,
            search_depth: SearchDepth::Lookahead,
        };
        let priors = PolicyRegistry::shared().priors(&env, &candidates);

        let best = priors
            .iter()
            .max_by(|a, b| a.prior.partial_cmp(&b.prior).expect("finite priors"))
            .expect("priors for every candidate");
        assert_eq!(
            best.candidate.action,
            GameAction::ChooseX { value: 6 },
            "the registry must top-rank the scheduled X, not the largest payable one"
        );
    }

    /// Nothing payable means the once-each-turn activation would buy a
    /// mana-value-0 creature for a card. Measured in a full AI-vs-AI game
    /// before this guard: the AI activated on a turn whose lands were already
    /// tapped, was offered `min=0 max=0`, and spent the turn on it.
    #[test]
    fn nothing_payable_means_do_not_activate() {
        let state = momir_state(P0, 11);
        assert_eq!(scheduled_x(&state, P0, 0), None);
        // One mana is enough to be worth it.
        assert_eq!(scheduled_x(&state, P0, 1), Some(1));
    }

    /// The schedule never asks for more than the player can actually pay.
    #[test]
    fn schedule_is_capped_by_affordability() {
        // Own turn 6 wants 6, but only 2 mana is available.
        let state = momir_state(P0, 11);
        assert_eq!(own_turn_index(&state, P0), 6);
        assert_eq!(scheduled_x(&state, P0, 2), Some(2));
    }

    /// Once 15 is payable the schedule abandons the cap and rolls for Emrakul.
    #[test]
    fn fifteen_payable_hunts_emrakul_instead_of_capping_at_eight() {
        let state = momir_state(P0, 11);
        assert_eq!(own_turn_index(&state, P0), 6, "cap would otherwise say 6");
        assert_eq!(
            scheduled_x(&state, P0, EMRAKUL_MANA_VALUE),
            Some(EMRAKUL_MANA_VALUE)
        );
        // Fourteen is not enough — the hunt only opens at a payable 15.
        assert_eq!(scheduled_x(&state, P0, 14), Some(6));
    }

    /// The hunt ends when it succeeds: with an Emrakul already on the
    /// battlefield the schedule returns to its ordinary cap.
    #[test]
    fn controlling_emrakul_ends_the_hunt_and_restores_the_cap() {
        let mut state = momir_state(P0, 21);
        assert_eq!(own_turn_index(&state, P0), 11);
        assert_eq!(
            scheduled_x(&state, P0, EMRAKUL_MANA_VALUE),
            Some(EMRAKUL_MANA_VALUE),
            "precondition: the hunt is open"
        );

        let emrakul = engine::game::zones::create_object(
            &mut state,
            engine::types::identifiers::CardId(4242),
            P0,
            EMRAKUL_NAME.to_string(),
            engine::types::zones::Zone::Battlefield,
        );
        state.objects.get_mut(&emrakul).unwrap().controller = P0;

        assert!(controls_emrakul(&state, P0));
        assert_eq!(
            scheduled_x(&state, P0, EMRAKUL_MANA_VALUE),
            Some(MAX_SCHEDULED_X),
            "with Emrakul found, the cap applies again"
        );
    }

    /// An opponent's Emrakul does not end this player's hunt.
    #[test]
    fn an_opponents_emrakul_does_not_end_the_hunt() {
        let mut state = momir_state(P0, 21);
        let emrakul = engine::game::zones::create_object(
            &mut state,
            engine::types::identifiers::CardId(4243),
            P1,
            EMRAKUL_NAME.to_string(),
            engine::types::zones::Zone::Battlefield,
        );
        state.objects.get_mut(&emrakul).unwrap().controller = P1;

        assert!(!controls_emrakul(&state, P0));
        assert_eq!(
            scheduled_x(&state, P0, EMRAKUL_MANA_VALUE),
            Some(EMRAKUL_MANA_VALUE)
        );
    }

    /// A non-Momir emblem in the same command zone must not be mistaken for the
    /// sink — otherwise this policy would schedule unrelated activations.
    #[test]
    fn unrelated_command_zone_ability_is_not_a_pool_sink() {
        use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr};

        let mut state = momir_state(P0, 5);
        let draw = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        );
        let emblem = engine::game::effects::create_emblem::grant_emblem(
            &mut state,
            P0,
            Vec::new(),
            Vec::new(),
            vec![draw],
        );
        assert!(!is_pool_sink_activation(&state, &emblem, 0));
    }
}
