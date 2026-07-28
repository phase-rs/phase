//! Layer 2 — Plan: derived schedule (static prior) + per-decision realization.
//!
//! `derive_snapshot` lives in `curves.rs` and consumes a `DeckFeatures` prior
//! to produce a `PlanSnapshot`. The snapshot is consumed by mulligan bottoming
//! and by feature-aware curve policies; `PlanState` is the cheap live
//! realization shape for per-decision consumers — `card_value::keep_tier` is
//! the first one.

pub mod curves;

pub use curves::derive_snapshot;

use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::eval::board_stats;

/// Tempo classification of a deck — a coarse strategic axis used by the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TempoClass {
    Aggro,
    #[default]
    Midrange,
    Ramp,
    Control,
    Combo,
}

/// Static deck prior — computed once per deck.
#[derive(Debug, Clone, Default)]
pub struct PlanSnapshot {
    pub expected_lands: [u8; 15],
    pub expected_mana: [u8; 15],
    pub expected_threats: [u8; 15],
    pub tempo_class: TempoClass,
}

/// Live per-decision realization — derived cheaply from snapshot + current state.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanState {
    pub lands_behind: i8,
    pub mana_behind: i8,
    pub threats_behind: i8,
}

/// Number of lands `player` controls on the battlefield.
///
/// One caller today (`PlanState::realize`). Two structurally identical inline
/// counters also exist, in `sacrifice_land_protection.rs` and
/// `cycling_discipline.rs`; both are `controlled_lands(ctx.state, ctx.ai_player)`
/// and are deliberately NOT converted here — those files belong to a different
/// unit's scope this round. This is where that dedup lands when it happens.
pub(crate) fn controlled_lands(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.controller == player && obj.card_types.core_types.contains(&CoreType::Land)
        })
        .count()
}

/// Size of `player`'s standing manabase — lands plus every permanent carrying a
/// *renewable* mana ability (rocks, dorks).
///
/// CR 106.1: mana is the primary resource. The predicate is
/// [`zone_eval::is_intrinsic_mana_source`], deliberately shared rather than
/// re-derived: it already ignores tapped state and summoning sickness (a
/// development measure must not collapse when the player taps out — which at a
/// CR 514.1 cleanup step is the normal condition) and already excludes one-shot
/// self-sacrificing sources (Treasure, Gold, Lotus Petal) per CR 701.21.
///
/// **Counts permanents, not pips** — a Sol Ring counts once though it produces
/// two mana. That is the campaign-wide convention (`BoardStats::mana_sources`
/// states it in the same words), and the resulting bias is one-directional and
/// safe: a deck full of multi-mana rocks reads slightly *behind* rather than
/// slightly ahead, so the realization can only be over-protective of mana
/// sources, never wrongly willing to pitch one.
///
/// The count **is** `BoardStats::mana_sources`, by delegation. This body was a
/// deliberate byte-identical copy of the then-private `eval::mana_source_count`,
/// left with an instruction to dedup here once `eval.rs`'s in-flight unit
/// landed; Unit 5 landed it, deleted that function, and honored the instruction.
pub(crate) fn controlled_mana_sources(state: &GameState, player: PlayerId) -> usize {
    // `BoardStats::mana_sources` increments from zero — never negative, the cast is lossless.
    board_stats(state, player).mana_sources as usize
}

impl PlanSnapshot {
    /// The number of lands this deck's curve wants in play once its mana is
    /// mature — the value `expected_lands` plateaus at (6 for a default deck,
    /// 7 for a `wants_ramp_curve` deck; see `curves::expected_lands_for`).
    ///
    /// **Turn-agnostic, deliberately, and that IS the shipped semantics.**
    /// `expected_lands[i]` is indexed by the player's *own* turn ordinal —
    /// `expected_lands_for` writes `min(i + 1, 6)` at index `i`. `GameState`
    /// exposes no per-player turn ordinal: `turn_number` is a single global
    /// counter bumped once per *seat's* turn in `game/turns.rs`, so in a
    /// 4-player game it runs roughly 4x ahead of any one player's own turn
    /// count, and it is off by one against this index even in a two-player
    /// game. Reading the schedule on that clock compares a player against a
    /// turn they have not taken.
    ///
    /// Both pre-existing consumers of `expected_lands` are turn-agnostic too
    /// (`cycling_discipline::next_planned_land_target` scans the whole
    /// schedule; `search::plan_bottoming_land_target` reads a fixed index), so
    /// this reads the plateau rather than inventing a turn convention. The
    /// resulting rule is exactly "a player below their deck's mature land
    /// target is behind; above it they are flooded" — no turn sensitivity is
    /// claimed or implemented. Restoring a turn axis requires a per-player
    /// turn ordinal in `GameState`, which is an engine change.
    ///
    /// Every `plan` entry in an `AiSession` comes from `derive_snapshot`, whose
    /// baseline loop always fills the schedule, so the `unwrap_or` is
    /// unreachable in production and returning `0` there is the inert reading
    /// (`lands_behind <= 0` for an empty board).
    pub fn land_target(&self) -> u8 {
        self.expected_lands.iter().copied().max().unwrap_or(0)
    }

    /// The amount of mana this deck's curve wants available once its mana is
    /// mature — the **terminal** slot of `expected_mana` (6 for a default deck,
    /// 7 for a ramp deck).
    ///
    /// **Deliberately NOT `max()`, unlike [`land_target`].** `expected_lands` is
    /// monotone, so its plateau and its terminal slot coincide. `expected_mana`
    /// is not: `curves::expected_mana_for` adds a fixed ramp bonus on turns 2–6
    /// only (+1, +1, +2, +2, +2), so a ramp deck's schedule is
    /// `[1, 3, 5, 7, 8, 9, 7, 7, …]` — it PEAKS at 9 on turn 6 and falls back to
    /// 7. Reading `max()` there would hold the deck to a transient bonus window
    /// forever and report a permanent deficit. `plan::tests::
    /// mana_target_is_the_terminal_slot_because_the_schedule_is_not_monotone`
    /// pins the non-monotonicity so the two accessors are not "unified" later.
    ///
    /// Turn-agnostic for exactly the reason [`land_target`] is: `GameState` has
    /// no per-player turn ordinal.
    pub fn mana_target(&self) -> u8 {
        self.expected_mana.last().copied().unwrap_or(0)
    }
}

impl PlanState {
    /// Live realization of the plan schedule against the current board.
    ///
    /// CR 305.2: a player normally plays only one land per turn, so mana
    /// development is path-dependent and cannot be re-derived from a cached
    /// count — this is recomputed per decision, and a land played this turn
    /// immediately reduces `lands_behind`.
    ///
    /// **Two axes, deliberately separate.** `lands_behind` measures the board
    /// against the LAND schedule and counts only lands; `mana_behind` measures
    /// it against the MANA schedule and counts the whole standing manabase.
    /// They are not interchangeable, and collapsing them was a real defect: a
    /// mana rock already on the battlefield does not reduce `lands_behind` (CR
    /// 305.1 — only a land can be *played* as a land), so a player with two
    /// lands and three rocks in a turn-10 Commander game read as four lands
    /// behind forever, and every spare rock in hand was promoted by a deficit
    /// that playing it could never close. `card_value::keep_tier` therefore
    /// reads `lands_behind` for `ManaRole::LandDrop` and `mana_behind` for
    /// `ManaRole::Accelerant`.
    ///
    /// CR 305.2: a player normally plays only one land per turn, so mana
    /// development is path-dependent and cannot be re-derived from a cached
    /// count — both axes are recomputed per decision, and a land played this
    /// turn immediately reduces `lands_behind`.
    ///
    /// `threats_behind` is still left at its `Default`: no consumer needs it,
    /// and `expected_threats` has no live counterpart to measure against.
    pub fn realize(state: &GameState, player: PlayerId, plan: &PlanSnapshot) -> Self {
        Self {
            lands_behind: behind(plan.land_target(), controlled_lands(state, player)),
            mana_behind: behind(plan.mana_target(), controlled_mana_sources(state, player)),
            ..Self::default()
        }
    }
}

/// `target - have`, saturated into `i8`. Positive means behind, negative means
/// past the target, zero means exactly on plan.
fn behind(target: u8, have: usize) -> i8 {
    let target = i32::from(target);
    let have = i32::try_from(have).unwrap_or(i32::MAX);
    (target - have).clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{DeckFeatures, ManaRampFeature};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
    };
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaColor;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const P0: PlayerId = PlayerId(0);

    fn add_land(state: &mut GameState, owner: PlayerId) {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Swamp".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
    }

    /// A Sol Ring-shaped permanent: an artifact with an untargeted `{T}: Add`
    /// mana ability, so `is_renewable_mana_ability` (and therefore
    /// `is_intrinsic_mana_source`) accepts it. It is NOT a land, so
    /// `controlled_lands` must ignore it.
    fn add_rock(state: &mut GameState, owner: PlayerId) {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Mana Rock".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Black],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        Arc::make_mut(&mut obj.abilities).push(ability);
    }

    fn state_with_lands(count: usize, turn_number: u32) -> GameState {
        let mut state = GameState::new_two_player(7);
        state.turn_number = turn_number;
        for _ in 0..count {
            add_land(&mut state, P0);
        }
        state
    }

    fn ramp_features() -> DeckFeatures {
        DeckFeatures {
            mana_ramp: ManaRampFeature {
                dork_count: 8,
                commitment: 0.96,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// The land target is read off the REAL derivation, not a hand-built
    /// snapshot: `derive_snapshot` is the only producer of every `plan` entry
    /// in an `AiSession`, so these two values (6 and 7) are the only land
    /// targets production can present to `realize`.
    #[test]
    fn land_target_is_the_derived_schedule_plateau() {
        let baseline = derive_snapshot(&DeckFeatures::default());
        assert_eq!(
            baseline.land_target(),
            6,
            "default deck plateaus at 6 lands"
        );
        assert_eq!(
            baseline.land_target(),
            *baseline.expected_lands.last().unwrap(),
            "the plateau IS the terminal slot — the schedule is monotone"
        );

        let ramp = derive_snapshot(&ramp_features());
        assert_eq!(ramp.land_target(), 7, "a ramp deck plateaus at 7 lands");
    }

    /// The shipped semantics, pinned: `realize` reads the deck's mature land
    /// target, NOT a turn slot. The same board yields the same answer on the
    /// first global turn and on the twenty-fifth, so no reader can mistake the
    /// realization for a turn-sensitive one. If a per-player turn ordinal is
    /// ever added to `GameState` and the turn axis is restored, THIS TEST MUST
    /// CHANGE — deliberately.
    #[test]
    fn realize_is_turn_agnostic_and_reads_the_land_target() {
        let plan = derive_snapshot(&DeckFeatures::default());

        for turn_number in [1_u32, 5, 12, 25] {
            let state = state_with_lands(3, turn_number);
            assert_eq!(
                PlanState::realize(&state, P0, &plan).lands_behind,
                3,
                "target 6 - 3 lands = 3, independent of turn_number {turn_number}"
            );
        }

        // The flooded direction, at the same reachable plan shape.
        let flooded = state_with_lands(8, 5);
        assert_eq!(
            PlanState::realize(&flooded, P0, &plan).lands_behind,
            -2,
            "8 lands against a mature target of 6 is flooded by 2"
        );

        // Exactly on target is the `Ordinary` boundary.
        let on_curve = state_with_lands(6, 5);
        assert_eq!(
            PlanState::realize(&on_curve, P0, &plan).lands_behind,
            0,
            "6 lands against a mature target of 6 is exactly on plan"
        );
    }

    /// Why [`PlanSnapshot::mana_target`] reads the terminal slot and
    /// [`PlanSnapshot::land_target`] reads `max()`: the land schedule is
    /// monotone and the mana schedule is NOT. `expected_mana_for` adds its ramp
    /// bonus on turns 2–6 only, so a ramp deck's mana schedule peaks mid-curve
    /// and falls back. If this test ever fails because the schedules became
    /// monotone, the two accessors may be unified — until then they must not be.
    #[test]
    fn mana_target_is_the_terminal_slot_because_the_schedule_is_not_monotone() {
        let ramp = derive_snapshot(&ramp_features());
        let peak = *ramp.expected_mana.iter().max().unwrap();
        let terminal = *ramp.expected_mana.last().unwrap();

        assert!(
            peak > terminal,
            "the ramp bonus window makes expected_mana peak ({peak}) above its \
             terminal value ({terminal}); max() would hold the deck to a \
             transient bonus forever"
        );
        assert_eq!(
            ramp.mana_target(),
            terminal,
            "mana_target() must be the TERMINAL slot (.last()), not the peak: \
             expected_mana peaks at {peak} during the ramp bonus window and \
             falls back to {terminal}. If you just unified this with \
             land_target()'s max(), that is the break — land_target()'s \
             schedule is monotone so max() is safe there, this one is not, and \
             max() here would hold the deck to a transient bonus forever."
        );
        assert_eq!(
            ramp.mana_target(),
            7,
            "the ramp schedule's terminal value is 7 (peak 9 at turn 6); a \
             max()-based mana_target() returns 9 here. See this test's name."
        );

        // The default deck's schedule IS flat, so both readings coincide there
        // — which is exactly why a max()-based implementation would have passed
        // every default-deck fixture and failed only on ramp decks.
        let baseline = derive_snapshot(&DeckFeatures::default());
        assert_eq!(
            baseline.mana_target(),
            6,
            "control: the default deck's terminal mana target is 6. If only this \
             moved, the default schedule changed and the ramp assertions above \
             are still describing the old one."
        );
        assert_eq!(
            baseline.mana_target(),
            *baseline.expected_mana.iter().max().unwrap(),
            "control: the default deck's schedule is FLAT, so terminal and peak \
             must coincide. This is the reason a max()-based mana_target() \
             passed every default-deck fixture and failed only on ramp decks — \
             if this reddens, the default schedule is no longer flat and this \
             test's negative control is gone."
        );
    }

    /// The two deficits are measured on two different populations. A rock on
    /// the battlefield closes `mana_behind` and leaves `lands_behind` alone —
    /// which is the whole point of the split.
    #[test]
    fn realize_measures_lands_and_mana_on_separate_populations() {
        let plan = derive_snapshot(&DeckFeatures::default());
        // The reported board: two lands plus three mana rocks, turn 10.
        let mut state = state_with_lands(2, 10);
        for _ in 0..3 {
            add_rock(&mut state, P0);
        }

        let realized = PlanState::realize(&state, P0, &plan);
        assert_eq!(
            realized.lands_behind, 4,
            "land target 6 - 2 lands: the rocks do NOT count, because CR 305.1 \
             only lets a land be played as a land"
        );
        assert_eq!(
            realized.mana_behind, 1,
            "mana target 6 - 5 sources (2 lands + 3 rocks): the rocks DO count"
        );

        // Deploying a fourth rock closes the mana axis and moves the land axis
        // not at all — the feedback loop the single-axis version never had.
        add_rock(&mut state, P0);
        let after = PlanState::realize(&state, P0, &plan);
        assert_eq!(after.mana_behind, 0);
        assert_eq!(after.lands_behind, 4);
    }

    /// A tapped-out board is the normal condition at a CR 514.1 cleanup step.
    /// `mana_behind` must not collapse there — `is_intrinsic_mana_source` is a
    /// development predicate, and if it were swapped for an availability one
    /// (`zone_eval::available_mana`) every cleanup discard would read as
    /// maximally mana-screwed.
    #[test]
    fn mana_behind_ignores_tapped_state() {
        let plan = derive_snapshot(&DeckFeatures::default());
        let mut state = state_with_lands(2, 10);
        for _ in 0..3 {
            add_rock(&mut state, P0);
        }
        let untapped = PlanState::realize(&state, P0, &plan).mana_behind;

        for id in state.battlefield.clone() {
            state.objects.get_mut(&id).unwrap().tapped = true;
        }
        assert_eq!(
            PlanState::realize(&state, P0, &plan).mana_behind,
            untapped,
            "tapping the whole board must not move the development deficit"
        );
        assert_eq!(untapped, 1, "reach-guard: the fixture is not already at 0");
    }

    /// `realize` counts only the named player's lands: an opponent's board
    /// never moves the subject's `lands_behind`.
    #[test]
    fn realize_counts_only_the_named_players_lands() {
        let plan = derive_snapshot(&DeckFeatures::default());
        let mut state = state_with_lands(3, 5);
        // Six lands for the opponent — enough to flip the sign if miscounted.
        for _ in 0..6 {
            add_land(&mut state, PlayerId(1));
        }
        assert_eq!(PlanState::realize(&state, P0, &plan).lands_behind, 3);
        assert_eq!(
            PlanState::realize(&state, PlayerId(1), &plan).lands_behind,
            0
        );
    }
}
