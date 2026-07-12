//! Offline analysis simulation harness around [`GameRunner::act`].
//!
//! PR-0 gave us [`ResourceVector`], whose **state-readable** axes (mana, life,
//! library size, counters) [`ResourceVector::snapshot`] fills directly out of a
//! `GameState`. Its **event-fed** axes — damage dealt, tokens created, cards
//! drawn, casts, landfall/combat/extra-turn counts, the `*_triggers`, and
//! `generic_triggers` — are events, not totals a single `GameState` retains, so
//! `snapshot` leaves them at `Default`.
//!
//! This module is the substrate that *feeds* those event-fed axes. Each
//! `GameRunner::act` returns an `ActionResult { events, .. }` — the game-event
//! stream produced by that action's resolution. [`LoopProbe`] wraps a
//! `GameRunner`, accumulates those events into a running [`ResourceVector`] as
//! actions are driven, and takes a state-readable `snapshot` at iteration
//! boundaries. The per-iteration [`ResourceVector::delta`] it produces therefore
//! has *both* halves populated — the measurement a CR 732.2a net-progress
//! detector (PR-2) consumes.
//!
//! It is **purely offline / additive**: it only *observes* the runner. It never
//! mutates game state, never alters resolution, SBAs, or the reducer, and emits
//! no game events of its own. The detector hook itself is PR-2.

use crate::analysis::resource::{ResourceVector, TriggerKind};
use crate::game::engine::EngineError;
use crate::game::scenario::GameRunner;
use crate::types::ability::{EffectKind, TargetRef};
use crate::types::actions::GameAction;
use crate::types::card_type::CoreType;
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::ActionResult;
use crate::types::zones::Zone;

/// Fold one action's worth of game events into the **event-fed** axes of `acc`.
///
/// This is the heart of PR-1: it maps each [`GameEvent`] the runner surfaces to
/// the [`ResourceVector`] field that counts it. Only the event-fed axes are
/// touched here — the state-readable axes (mana, life, library, counters) are
/// captured separately by [`ResourceVector::snapshot`] at iteration boundaries,
/// because a single `GameState` already holds their absolute levels.
///
/// `acc` accumulates monotonically across however many actions make up one loop
/// iteration; [`ResourceVector::delta`] of two boundary snapshots (each carrying
/// its own accumulated event tallies) then yields the per-cycle change.
///
/// The mapping is event-driven, not card-driven, so it covers a *class* of
/// cards: any effect that emits `TokenCreated` feeds `tokens_created`, any
/// `DamageDealt` to a player feeds `damage_dealt`, and so on.
pub fn accumulate_events(acc: &mut ResourceVector, events: &[GameEvent]) {
    for event in events {
        match event {
            // CR 120.1: damage dealt to a player. Non-combat (burn, pings) and
            // combat damage arrive on distinct events; both feed the same axis.
            GameEvent::DamageDealt {
                target: TargetRef::Player(player),
                amount,
                ..
            } => {
                *acc.damage_dealt.entry(*player).or_insert(0) += *amount as i64;
            }
            // CR 120.1 + CR 510.2: combat damage dealt to a player, batched per
            // step with the authoritative total. Damage to objects (creatures,
            // planeswalkers, battles) is a board change captured by the
            // state-readable snapshot, not a player-resource axis.
            GameEvent::CombatDamageDealtToPlayer {
                player_id,
                total_damage,
                ..
            } => {
                *acc.damage_dealt.entry(*player_id).or_insert(0) += *total_damage as i64;
            }

            // CR 111.1: a token entered the battlefield (or another zone) as it
            // was created.
            GameEvent::TokenCreated { .. } => acc.tokens_created += 1,

            // CR 121.1: a single card was drawn (effect/turn-based draws emit one
            // `CardDrawn` per card).
            GameEvent::CardDrawn { .. } => acc.cards_drawn += 1,
            // CR 121.1: a batched draw event carrying its own count (mulligan and
            // other batch draws). Disjoint emission path from `CardDrawn`, so
            // counting both never double-counts a single draw.
            GameEvent::CardsDrawn { count, .. } => acc.cards_drawn += *count as i64,

            // CR 601.2a: a spell was cast and put on the stack (storm / cast-count
            // loops). A *copy* (CR 707.10 `SpellCopied`) isn't cast, so it is not
            // counted here.
            GameEvent::SpellCast { .. } => acc.casts_this_step += 1,

            // CR 500.7: an EXTRA turn is created when `Effect::ExtraTurn` resolves
            // and pushes onto `state.extra_turns` (one resolve == one turn). The
            // creation event — not `TurnStarted`, which also fires on every natural
            // turn — is what keeps ordinary turn progression off this axis.
            GameEvent::EffectResolved {
                kind: EffectKind::ExtraTurn,
                ..
            } => acc.extra_turns += 1,

            // CR 603.6a / CR 603.6c: battlefield zone changes drive the ETB / LTB /
            // dies / landfall trigger axes. The `ZoneChangeRecord` carries the
            // object's core types as of the change, so landfall (a land entering)
            // is recognized from the event itself with no post-state lookup.
            GameEvent::ZoneChanged {
                from, to, record, ..
            } => {
                // CR 603.6a: enters-the-battlefield. `from != Battlefield` so a
                // within-battlefield reshuffle is not an ETB.
                if *to == Zone::Battlefield && *from != Some(Zone::Battlefield) {
                    acc.etb_triggers += 1;
                    // CR 305.1: landfall is the land-enters-the-battlefield
                    // trigger family; recognized by the entering object's type.
                    if record.core_types.contains(&CoreType::Land) {
                        acc.landfall_triggers += 1;
                    }
                }
                // CR 603.6c: leaves-the-battlefield (to any zone).
                if *from == Some(Zone::Battlefield) && *to != Zone::Battlefield {
                    acc.ltb_triggers += 1;
                    // CR 700.4: "dies" is the LTB special case battlefield ->
                    // graveyard.
                    if *to == Zone::Graveyard {
                        acc.death_triggers += 1;
                    }
                }
            }

            // CR 701.21: a permanent was sacrificed.
            GameEvent::PermanentSacrificed { .. } => acc.sac_triggers += 1,

            // CR 701.34: proliferate — the canonical mana-neutral trigger axis. It
            // is surfaced as a player action, not a counter change (the counters it
            // adds are already captured state-readably).
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::Proliferate,
                ..
            } => {
                *acc.generic_triggers
                    .entry(TriggerKind::Proliferate)
                    .or_insert(0) += 1;
            }

            _ => {}
        }
    }
}

/// An offline probe that drives a [`GameRunner`] while measuring the per-iteration
/// [`ResourceVector`] of a candidate loop.
///
/// Construction snapshots the runner's current **state-readable** resources and
/// zeroes the event tally. [`LoopProbe::act`] forwards an action to the runner and
/// folds the returned events into the running tally (so the event-fed axes
/// accumulate across the actions of an iteration). [`LoopProbe::iteration_delta`]
/// then differences a fresh state-readable snapshot against the boundary and
/// splices the accumulated event tally in as the event-fed half of the result,
/// resetting both the boundary and the tally for the next iteration.
///
/// The two halves are combined differently on purpose. State-readable axes (mana,
/// life, library, counters) are *absolute levels* the engine stores, so their
/// per-iteration change is a `delta(before, after)`. Event-fed axes (damage,
/// tokens, draws, casts, triggers) are *counts of events that occurred during the
/// iteration* — already a per-iteration quantity — so they are taken verbatim, not
/// differenced. (Differencing them would cancel a steady per-cycle gain to zero.)
///
/// CR 732.2a: this is exactly the per-cycle measurement a net-progress shortcut
/// detector reads. The probe itself takes no game decisions and changes no game
/// state; it only observes the runner.
pub struct LoopProbe<'r> {
    runner: &'r mut GameRunner,
    /// State-readable resources at the most recent iteration boundary (event-fed
    /// axes always zero — those are tracked in `events_since_boundary`).
    boundary: ResourceVector,
    /// Event-fed axes accumulated since the most recent boundary.
    events_since_boundary: ResourceVector,
}

impl<'r> LoopProbe<'r> {
    /// Begin probing `runner`, anchoring the first iteration boundary at its
    /// current state.
    pub fn new(runner: &'r mut GameRunner) -> LoopProbe<'r> {
        let boundary = ResourceVector::snapshot(runner.state());
        LoopProbe {
            runner,
            boundary,
            events_since_boundary: ResourceVector::default(),
        }
    }

    /// Drive one action through the runner, folding its events into the running
    /// event tally. The `ActionResult` is returned unchanged for callers that
    /// drive the loop manually (e.g. inspecting `waiting_for`).
    pub fn act(&mut self, action: GameAction) -> Result<ActionResult, EngineError> {
        let result = self.runner.act(action)?;
        accumulate_events(&mut self.events_since_boundary, &result.events);
        Ok(result)
    }

    /// Close the current iteration and return the per-iteration
    /// [`ResourceVector`]. The new boundary becomes the start of the next
    /// iteration and the event tally resets.
    ///
    /// The returned vector has both halves populated:
    /// - **state-readable** axes = `delta(boundary_snapshot, current_snapshot)`;
    /// - **event-fed** axes = the events accumulated since the last boundary,
    ///   taken verbatim (each is already a per-iteration count).
    ///
    /// This is the input to [`ResourceVector::is_net_progress`] /
    /// [`ResourceVector::unbounded_components`].
    pub fn iteration_delta(&mut self) -> ResourceVector {
        // State-readable half: difference of two absolute-level snapshots.
        // `snapshot` leaves the event-fed axes zero, so the delta's event-fed
        // axes are zero here and get filled from the event tally below.
        let after = ResourceVector::snapshot(self.runner.state());
        let mut delta = ResourceVector::delta(&self.boundary, &after);

        // Event-fed half: the per-iteration event counts, taken as-is (not
        // differenced — they already describe only this iteration).
        splice_event_fed(&mut delta, &self.events_since_boundary);

        // Roll the boundary forward (state-readable only) and reset the tally.
        self.boundary = after;
        self.events_since_boundary = ResourceVector::default();
        delta
    }

    /// Borrow the underlying runner (read-only) — e.g. to inspect `waiting_for`
    /// or the game state between actions.
    pub fn runner(&self) -> &GameRunner {
        self.runner
    }

    /// The events accumulated since the most recent iteration boundary, as the
    /// event-fed axes of a [`ResourceVector`]. Lets a caller assert mid-iteration
    /// progress without closing the boundary.
    pub fn events_since_boundary(&self) -> &ResourceVector {
        &self.events_since_boundary
    }
}

/// Copy the event-fed axes of `events` into `target`, leaving `target`'s
/// state-readable axes (mana, life, library, counters) untouched. Used by
/// [`LoopProbe::iteration_delta`] to fill the event-fed half of the per-iteration
/// result (the state-readable half is already a snapshot delta).
fn splice_event_fed(target: &mut ResourceVector, events: &ResourceVector) {
    target.damage_dealt = events.damage_dealt.clone();
    target.tokens_created = events.tokens_created;
    target.cards_drawn = events.cards_drawn;
    target.casts_this_step = events.casts_this_step;
    target.landfall_triggers = events.landfall_triggers;
    target.extra_turns = events.extra_turns;
    target.death_triggers = events.death_triggers;
    target.etb_triggers = events.etb_triggers;
    target.ltb_triggers = events.ltb_triggers;
    target.sac_triggers = events.sac_triggers;
    target.generic_triggers = events.generic_triggers.clone();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::{GameScenario, P0, P1};
    use crate::types::game_state::{CastPaymentMode, WaitingFor, ZoneChangeRecord};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;

    fn zone_change(from: Option<Zone>, to: Zone, core_types: Vec<CoreType>) -> GameEvent {
        let mut record = ZoneChangeRecord::test_minimal(ObjectId(1), from, to);
        record.core_types = core_types;
        GameEvent::ZoneChanged {
            object_id: ObjectId(1),
            from,
            to,
            record: Box::new(record),
        }
    }

    /// Building-block coverage: every event-fed axis is populated by the
    /// [`GameEvent`] variant the engine actually emits for it. This is the
    /// per-axis revert probe — deleting any single match arm in
    /// [`accumulate_events`] leaves that axis at 0 and flips its assertion.
    #[test]
    fn accumulate_events_feeds_every_axis() {
        let events = vec![
            // damage to two distinct players (noncombat + combat paths).
            GameEvent::DamageDealt {
                source_id: ObjectId(9),
                target: TargetRef::Player(PlayerId(1)),
                amount: 2,
                is_combat: false,
                excess: 0,
            },
            GameEvent::CombatDamageDealtToPlayer {
                player_id: PlayerId(1),
                source_amounts: vec![(ObjectId(9), 3)],
                total_damage: 3,
            },
            GameEvent::TokenCreated {
                object_id: ObjectId(20),
                name: "Servo".to_string(),
                source_id: ObjectId(9),
            },
            GameEvent::CardDrawn {
                player_id: PlayerId(0),
                object_id: ObjectId(21),
                nth_in_turn: 1,
                nth_in_step: 1,
            },
            GameEvent::CardsDrawn {
                player_id: PlayerId(0),
                count: 2,
            },
            GameEvent::SpellCast {
                card_id: CardId(5),
                controller: PlayerId(0),
                object_id: ObjectId(22),
            },
            GameEvent::PhaseChanged {
                phase: Phase::BeginCombat,
            },
            GameEvent::EffectResolved {
                kind: EffectKind::ExtraTurn,
                source_id: ObjectId(9),
            },
            // ETB of a land == landfall + etb.
            zone_change(Some(Zone::Hand), Zone::Battlefield, vec![CoreType::Land]),
            // dies (battlefield -> graveyard) == ltb + death.
            zone_change(
                Some(Zone::Battlefield),
                Zone::Graveyard,
                vec![CoreType::Creature],
            ),
            GameEvent::PermanentSacrificed {
                object_id: ObjectId(23),
                player_id: PlayerId(0),
            },
            GameEvent::PlayerPerformedAction {
                player_id: PlayerId(0),
                action: PlayerActionKind::Proliferate,
            },
        ];

        let mut acc = ResourceVector::default();
        accumulate_events(&mut acc, &events);

        // damage: 2 (noncombat) + 3 (combat) to player 1.
        assert_eq!(acc.damage_dealt.get(&PlayerId(1)).copied(), Some(5));
        assert_eq!(acc.tokens_created, 1);
        // 1 (CardDrawn) + 2 (CardsDrawn batch) — disjoint emission paths.
        assert_eq!(acc.cards_drawn, 3);
        assert_eq!(acc.casts_this_step, 1);
        assert_eq!(
            acc.combat_phases, 0,
            "PhaseChanged no longer feeds combat_phases — it is state-readable now"
        );
        assert_eq!(acc.extra_turns, 1);
        assert_eq!(acc.landfall_triggers, 1);
        // the land ETB and the creature death each contribute one etb / ltb.
        assert_eq!(acc.etb_triggers, 1);
        assert_eq!(acc.ltb_triggers, 1);
        assert_eq!(acc.death_triggers, 1);
        assert_eq!(acc.sac_triggers, 1);
        assert_eq!(
            acc.generic_triggers.get(&TriggerKind::Proliferate).copied(),
            Some(1)
        );
    }

    /// REVERT PROBE for the event feed as a whole: an empty event stream feeds
    /// nothing, so every event-fed axis stays at its `Default` (0). If
    /// [`accumulate_events`] were to populate axes from anything other than the
    /// passed events, this would fail.
    #[test]
    fn accumulate_events_no_events_is_noop() {
        let mut acc = ResourceVector::default();
        accumulate_events(&mut acc, &[]);
        assert_eq!(acc, ResourceVector::default());
    }

    /// A within-battlefield "from == to == Battlefield" change is neither an ETB
    /// nor an LTB (guards against double-counting a control change as a zone
    /// move).
    #[test]
    fn accumulate_events_within_battlefield_is_not_etb_or_ltb() {
        let mut acc = ResourceVector::default();
        accumulate_events(
            &mut acc,
            &[zone_change(
                Some(Zone::Battlefield),
                Zone::Battlefield,
                vec![CoreType::Creature],
            )],
        );
        assert_eq!(acc.etb_triggers, 0);
        assert_eq!(acc.ltb_triggers, 0);
        assert_eq!(acc.death_triggers, 0);
    }

    /// A non-Proliferate player action must not feed the proliferate axis — the
    /// match is on the specific [`PlayerActionKind`], not the wrapper event.
    #[test]
    fn accumulate_events_non_proliferate_player_action_ignored() {
        let mut acc = ResourceVector::default();
        accumulate_events(
            &mut acc,
            &[GameEvent::PlayerPerformedAction {
                player_id: PlayerId(0),
                action: PlayerActionKind::Scry,
            }],
        );
        assert!(acc.generic_triggers.is_empty());
    }

    /// END-TO-END through the real `apply()` pipeline: cast Lightning Bolt at a
    /// player via [`LoopProbe::act`] and drive resolution, then close the
    /// iteration. The harness must feed `damage_dealt` from the runner's real
    /// `DamageDealt` event AND `casts_this_step` from the real `SpellCast`,
    /// while the state-readable `life` axis falls straight out of the snapshot.
    ///
    /// Revert probe: the `damage_dealt` / `casts_this_step` arms in
    /// [`accumulate_events`] are the only feed for those axes. Remove them and
    /// `iteration_delta()` reports 0 for both even though the bolt still resolved
    /// (the negative-control `accumulate_events_no_events_is_noop` pins the floor).
    #[test]
    fn loop_probe_feeds_damage_from_real_pipeline() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bolt = scenario.add_bolt_to_hand(P0);
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.active_player = P0;
            state.priority_player = P0;
        }

        let bolt_card = runner.state().objects[&bolt].card_id;
        let life_before = runner.state().players[1].life as i64;

        let mut probe = LoopProbe::new(&mut runner);

        // CR 601.2: announce the cast (auto-pay; bolt as built has no mana cost).
        let cast = probe
            .act(GameAction::CastSpell {
                object_id: bolt,
                card_id: bolt_card,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast bolt");
        assert!(
            matches!(cast.waiting_for, WaitingFor::TargetSelection { .. }),
            "bolt must prompt for a target"
        );

        // CR 601.2c: choose the opponent as the target.
        probe
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("target opponent");

        // CR 608: resolve by passing priority until the stack empties.
        for _ in 0..8 {
            if probe.runner().state().stack.is_empty() {
                break;
            }
            if probe.act(GameAction::PassPriority).is_err() {
                break;
            }
        }

        let life_after = probe.runner().state().players[1].life as i64;
        assert_eq!(life_after, life_before - 3, "bolt dealt 3 to the opponent");

        let delta = probe.iteration_delta();

        // Event-fed: damage dealt to the opponent, surfaced from the real
        // `DamageDealt` event the resolver emitted.
        assert_eq!(
            delta.damage_dealt.get(&P1).copied(),
            Some(3),
            "harness must feed damage_dealt from the runner event stream"
        );
        // Event-fed: exactly one spell was cast.
        assert_eq!(delta.casts_this_step, 1, "one bolt cast");
        // State-readable: the opponent's life dropped by 3 (snapshot diff).
        assert_eq!(delta.life.get(&P1).copied(), Some(-3));
    }

    /// Two-iteration boundary roll-forward: each [`LoopProbe::iteration_delta`]
    /// must report ONLY that iteration's event-fed progress (the accumulated
    /// tally resets at the boundary), not the running total. Without the boundary
    /// reset the second iteration would report 6 damage instead of 3.
    #[test]
    fn loop_probe_iteration_delta_isolates_each_iteration() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P1, 40); // survive two bolts
        let bolt_a = scenario.add_bolt_to_hand(P0);
        let bolt_b = scenario.add_bolt_to_hand(P0);
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.active_player = P0;
            state.priority_player = P0;
        }
        let mut probe = LoopProbe::new(&mut runner);

        let cast_and_resolve = |probe: &mut LoopProbe, bolt: ObjectId| {
            let card = probe.runner().state().objects[&bolt].card_id;
            probe
                .act(GameAction::CastSpell {
                    object_id: bolt,
                    card_id: card,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("cast");
            probe
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Player(P1)],
                })
                .expect("target");
            for _ in 0..8 {
                if probe.runner().state().stack.is_empty() {
                    break;
                }
                if probe.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        };

        cast_and_resolve(&mut probe, bolt_a);
        let first = probe.iteration_delta();
        assert_eq!(first.damage_dealt.get(&P1).copied(), Some(3));
        assert_eq!(first.casts_this_step, 1);

        cast_and_resolve(&mut probe, bolt_b);
        let second = probe.iteration_delta();
        assert_eq!(
            second.damage_dealt.get(&P1).copied(),
            Some(3),
            "second iteration reports only its own 3 damage, not the cumulative 6"
        );
        assert_eq!(second.casts_this_step, 1);
        // State-readable life delta is likewise per-iteration: -3 each time.
        assert_eq!(second.life.get(&P1).copied(), Some(-3));
    }

    /// The poison axis is **state-readable**, not event-fed: a poison counter
    /// added between boundaries surfaces in `iteration_delta` even though no
    /// event-feed arm handles `PlayerCounterChanged`. This pins the division of
    /// labor — state-readable axes come from the snapshot, event-fed axes from
    /// the events — so a future edit can't accidentally route a state axis
    /// through the event feed (or vice versa).
    #[test]
    fn loop_probe_state_readable_axis_independent_of_event_feed() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mut runner = scenario.build();
        let mut probe = LoopProbe::new(&mut runner);

        // Mutate a state-readable resource directly (no event emitted to the
        // probe), then close the iteration.
        probe.runner.state_mut().players[1].poison_counters = 4;
        let delta = probe.iteration_delta();

        // CR 704.5c: poison is per-victim in `delta.poison`, read from the state snapshot.
        assert_eq!(
            delta.poison.get(&PlayerId(1)).copied(),
            Some(4),
            "poison is read from the state snapshot, not the event feed"
        );
        // And no event-fed axis spuriously moved.
        assert!(delta.generic_triggers.is_empty());
        assert!(delta.damage_dealt.is_empty());
    }

    /// N1 — REVERT PROBE for the deleted `PhaseChanged{BeginCombat}` arm.
    /// Drive a probe through natural phase progression into the beginning-of-
    /// combat step via `PassPriority`. The raw runner event stream MUST contain a
    /// natural `PhaseChanged{BeginCombat}` (asserted below) — that is what the OLD
    /// arm counted, so this fixture is non-vacuous: against the deleted code
    /// `delta.combat_phases` would be 1. With the fix it is 0, because a natural
    /// combat is not an *extra* combat (the state-readable snapshot only counts
    /// `combat_phases_started_this_turn - 1`, i.e. zero extra combats here).
    #[test]
    fn natural_begin_combat_is_not_extra_combat() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.active_player = P0;
            state.priority_player = P0;
        }
        let mut probe = LoopProbe::new(&mut runner);

        // CR 500.5 / CR 506.1: pass priority until natural progression enters the
        // beginning-of-combat step. Stop the instant the event is captured — the
        // step may be auto-passed in the same action (phase already advanced past
        // BeginCombat), so latch on the event, not on the current phase, to avoid
        // driving into a later empty-library draw (a state-based loss).
        let mut saw_natural_begin_combat = false;
        for _ in 0..32 {
            let result = probe.act(GameAction::PassPriority).expect("pass priority");
            if result.events.iter().any(|e| {
                matches!(
                    e,
                    GameEvent::PhaseChanged {
                        phase: Phase::BeginCombat
                    }
                )
            }) {
                saw_natural_begin_combat = true;
                break;
            }
        }
        assert!(
            saw_natural_begin_combat,
            "non-vacuity: the runner must have emitted a natural PhaseChanged{{BeginCombat}} \
             (the OLD deleted arm would have counted it)"
        );

        let delta = probe.iteration_delta();
        assert_eq!(
            delta.combat_phases, 0,
            "a NATURAL begin-combat is not an extra combat"
        );
    }

    /// N2 — REVERT PROBE for the deleted `TurnStarted` arm.
    /// Drive a probe through a natural turn rollover via `PassPriority`. The raw
    /// runner event stream MUST contain a natural `TurnStarted` (asserted below) —
    /// the OLD arm counted that, so against the deleted code `delta.extra_turns`
    /// would be >= 1. With the fix it is 0, because no `EffectResolved{ExtraTurn}`
    /// (a creation signal) ever fired — only a natural turn began.
    #[test]
    fn natural_next_turn_is_not_extra_turn() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.active_player = P0;
            state.priority_player = P0;
        }
        let mut probe = LoopProbe::new(&mut runner);

        // CR 500.1 / CR 500.7: pass priority through the rest of the turn until a
        // natural next turn begins.
        let mut saw_natural_turn_started = false;
        for _ in 0..64 {
            let result = probe.act(GameAction::PassPriority).expect("pass priority");
            if result
                .events
                .iter()
                .any(|e| matches!(e, GameEvent::TurnStarted { .. }))
            {
                saw_natural_turn_started = true;
                break;
            }
        }
        assert!(
            saw_natural_turn_started,
            "non-vacuity: the runner must have emitted a natural TurnStarted \
             (the OLD deleted arm would have counted it)"
        );

        let delta = probe.iteration_delta();
        assert_eq!(
            delta.extra_turns, 0,
            "a NATURAL next turn is not an extra turn"
        );
    }

    /// P1 — the extra-turn axis is fed by the `EffectResolved{ExtraTurn}`
    /// CREATION event, and ONLY that kind. Discriminates the `kind` match: a
    /// different `EffectKind` (here `DealDamage`) must not touch `extra_turns`.
    #[test]
    fn extra_turn_creation_feeds_axis() {
        let mut acc = ResourceVector::default();
        accumulate_events(
            &mut acc,
            &[GameEvent::EffectResolved {
                kind: EffectKind::ExtraTurn,
                source_id: ObjectId(7),
            }],
        );
        assert_eq!(
            acc.extra_turns, 1,
            "an ExtraTurn creation event feeds the extra-turns axis"
        );

        // A different EffectKind must NOT increment the axis (kind discrimination).
        let mut other = ResourceVector::default();
        accumulate_events(
            &mut other,
            &[GameEvent::EffectResolved {
                kind: EffectKind::DealDamage,
                source_id: ObjectId(7),
            }],
        );
        assert_eq!(
            other.extra_turns, 0,
            "a non-ExtraTurn EffectResolved must not feed the extra-turns axis"
        );
    }

    /// P2 — the combat-phase axis is fed by `snapshot` from `state.extra_phases`:
    /// one queued `ExtraPhase{phase: BeginCombat}` (anchor irrelevant) is counted
    /// as one extra combat, with the natural combat tally at its baseline (1, the
    /// single natural combat already entered).
    #[test]
    fn extra_combat_creation_feeds_axis() {
        use crate::types::game_state::{ExtraPhase, GameState};

        let mut state = GameState::new_two_player(7);
        // CR 506.1: the one natural combat already entered this turn.
        state.combat_phases_started_this_turn = 1;
        // CR 500.8: one queued extra combat (Aurelia-style "after this phase").
        state.extra_phases.push(ExtraPhase {
            anchor: Phase::EndCombat,
            phase: Phase::BeginCombat,
            attacker_restriction: None,
            attacker_restriction_source: None,
        });

        let v = ResourceVector::snapshot(&state);
        assert_eq!(
            v.combat_phases, 1,
            "one queued BeginCombat extra phase is one extra combat (entered=0 + queued=1)"
        );
    }

    /// P3 (Obeka class / pitfall-1 rebuttal) — a multi-push of N extra combats
    /// must count ALL N from the queued term, proving the snapshot is NOT a
    /// 1-per-event undercount (a per-event fold would have collapsed a single
    /// AdditionalPhase resolution that pushes 3 phases to 1).
    #[test]
    fn multi_push_extra_combats_counted() {
        use crate::types::game_state::{ExtraPhase, GameState};

        let mut state = GameState::new_two_player(7);
        state.combat_phases_started_this_turn = 1; // one natural combat
        for _ in 0..3 {
            state.extra_phases.push(ExtraPhase {
                anchor: Phase::EndCombat,
                phase: Phase::BeginCombat,
                attacker_restriction: None,
                attacker_restriction_source: None,
            });
        }

        let v = ResourceVector::snapshot(&state);
        assert_eq!(
            v.combat_phases, 3,
            "all 3 queued extra combats are counted (entered=0 + queued=3), not undercounted to 1"
        );
    }

    /// H1 (pitfall-2 / CR 500.10a rebuttal) — with NO queued extra combats and
    /// the natural combat tally at the baseline 1, `combat_phases` is 0. In the
    /// real engine a *no-op* AdditionalPhase (CR 500.10a: "you get" an extra phase
    /// on a non-controller's turn adds nothing) still EMITS
    /// `EffectResolved{AdditionalPhase}`; the snapshot path is immune because that
    /// no-op pushed nothing onto `state.extra_phases` and entered no extra combat.
    /// combat_phases is fed by STATE, not by the AdditionalPhase event, so the
    /// no-op event cannot inflate it.
    #[test]
    fn no_op_additional_phase_not_counted() {
        use crate::types::game_state::GameState;

        let mut state = GameState::new_two_player(7);
        state.combat_phases_started_this_turn = 1; // only the natural combat
        debug_assert!(state.extra_phases.is_empty());

        let v = ResourceVector::snapshot(&state);
        assert_eq!(
            v.combat_phases, 0,
            "a no-op AdditionalPhase pushes nothing onto state, so the state-readable axis stays 0"
        );
    }

    /// H2 (pitfall-3 rebuttal) — a NON-combat queued extra phase (an Obeka-style
    /// extra Upkeep) must be filtered out: only `Phase::BeginCombat` entries feed
    /// the combat axis.
    #[test]
    fn non_combat_extra_phase_not_counted() {
        use crate::types::game_state::{ExtraPhase, GameState};

        let mut state = GameState::new_two_player(7);
        state.combat_phases_started_this_turn = 1;
        state.extra_phases.push(ExtraPhase {
            anchor: Phase::Upkeep,
            phase: Phase::Upkeep,
            attacker_restriction: None,
            attacker_restriction_source: None,
        });

        let v = ResourceVector::snapshot(&state);
        assert_eq!(
            v.combat_phases, 0,
            "a queued non-combat (Upkeep) extra phase must not feed the combat axis"
        );
    }

    /// H3 (Route-B consume-to-zero rebuttal) — create-then-consume: one extra
    /// combat was ENTERED (tally = 2: one natural + one extra) and then consumed
    /// (`state.extra_phases` empty). The entered term (started - 1 = 1) retains
    /// that consumed extra combat, so `combat_phases` is 1 — a naive backlog-only
    /// measure would report 0.
    #[test]
    fn extra_combat_survives_consumption() {
        use crate::types::game_state::GameState;

        let mut state = GameState::new_two_player(7);
        // One natural + one extra combat ENTERED; the extra phase was removed from
        // the queue when `advance_phase` consumed it (turns.rs:58 before enter).
        state.combat_phases_started_this_turn = 2;
        debug_assert!(state.extra_phases.is_empty());

        let v = ResourceVector::snapshot(&state);
        assert_eq!(
            v.combat_phases, 1,
            "a consumed extra combat is retained by the entered term (started - 1)"
        );
    }
}
