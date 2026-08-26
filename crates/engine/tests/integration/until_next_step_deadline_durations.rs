//! Misparse root cause 25 ("wrong / dropped effect duration"): the duration
//! grammar's step-deadline family only recognized two hardcoded phrasings
//! ("your next end step", "the next end step"), so every other
//! "until \[the beginning of\] <possessor> next <step>" phrase fell through
//! `oracle_nom::duration::parse_duration` and its clause lost the duration.
//!
//! Two distinct harms shipped from that one gap:
//!
//!   * **Orcish Farmer** — "{T}: Target land becomes a Swamp until its
//!     controller's next untap step." The trailing clause never matched, so
//!     `strip_trailing_duration` returned no duration and the type change was
//!     stamped `Duration::Permanent` (CR 611.2a's no-stated-duration default).
//!     The card looked fully supported — no `Effect::Unimplemented`, no parse
//!     warning — while permanently turning an opponent's land into a Swamp.
//!   * **"until your next upkeep"** (Xenic Poltergeist, Erhnam Djinn, Gabriel
//!     Angelfire, Cycle of Life, Spatial Binding, plus Elkin Bottle / Grinning
//!     Totem's "until the beginning of your next upkeep" spelling) — the clause
//!     lowered to `Effect::Unimplemented` and nothing happened at all.
//!
//! Coverage honesty: recognizing the duration does not by itself make all of
//! those cards supported. Most carry a second, independent gap in the same
//! clause (Cycle of Life's "target creature you cast this turn" restriction,
//! Spatial Binding's "can't phase out", Xenic Poltergeist's "becomes an artifact
//! creature with power and toughness each equal to its mana value"), which still
//! lowers to `Effect::unimplemented`. Only the duration axis is claimed here.
//!
//! The fix factors the production into its two real axes (possessor →
//! `PlayerScope`, step → `Phase`) and maps every pair onto the existing
//! `Duration::UntilNextStepOf` variant. No new engine variant is introduced,
//! and each emitted pair has a runtime expiry authority implementing CR 500.4
//! ("as a step or phase begins, if there are effects that last until that step
//! or phase, those effects expire"): `layers::prune_controller_untap_step_effects`
//! (CR 502.3), the new `layers::prune_until_next_upkeep_effects` (CR 503.1), and
//! `layers::prune_until_next_end_step_effects` (CR 513.1).
//!
//! Both tests drive the real activate/cast → target → resolve pipeline and then
//! the real turn machinery (`turns::auto_advance`), and both fail on `main`:
//! the first because the Swamp change never wears off, the second because the
//! grant never happens at all.
//!
//! The one-step untap/upkeep boundary itself is pinned by the `layers.rs` unit
//! tests (`until_next_upkeep_effect_survives_untap_and_expires_at_controllers_upkeep`)
//! rather than here, because no player receives priority during the untap step
//! (CR 502.4), so no runtime observation point exists between it and upkeep.

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{CastingPermission, Duration};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

/// Drive the real turn machinery until `phase` is reached, answering the combat
/// turn-based-action prompts with "no attackers / no blockers" so a creature on
/// the battlefield cannot stall the advance. Mirrors
/// `charging_cinderhorn_issue_2868.rs::advance_to_end_step`; `GameRunner::advance_to_phase`
/// cannot be used here because it stops on any non-priority prompt.
fn drive_to(runner: &mut GameRunner, phase: Phase, min_turn: u32) {
    for _ in 0..400 {
        if runner.state().phase == phase && runner.state().turn_number >= min_turn {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while advancing");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            other => panic!(
                "unexpected prompt advancing to {phase:?}: {other:?} (phase={:?}, turn={})",
                runner.state().phase,
                runner.state().turn_number
            ),
        }
    }
    panic!("never reached {phase:?} on turn >= {min_turn}");
}

/// True iff `id` currently has `keyword` after a fresh layer evaluation —
/// the same helper idiom the suite uses in
/// `angelic_field_marshal_lieutenant_2885.rs`.
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

/// Land subtypes of `id` after a fresh layer evaluation (CR 613.1d layer 4).
fn land_subtypes(runner: &mut GameRunner, id: ObjectId) -> Vec<String> {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    runner.state().objects[&id].card_types.subtypes.clone()
}

/// Orcish Farmer, verbatim.
const ORCISH_FARMER: &str =
    "{T}: Target land becomes a Swamp until its controller's next untap step.";
/// Gabriel Angelfire's flying mode, isolated as a one-shot instant so a single
/// clause exercises the same duration arm its upkeep trigger reaches. (The same
/// isolation precedent the suite uses in
/// `arcum_weathervane_supertype_removal.rs`.)
const GAINS_FLYING_UNTIL_YOUR_NEXT_UPKEEP: &str =
    "Target creature gains flying until your next upkeep.";
/// Elkin Bottle, verbatim (the "the beginning of" spelling of the same
/// deadline). Placed on a creature body so the activated ability is reachable
/// without a separate artifact fixture; the clause parse is body-independent.
const ELKIN_BOTTLE: &str = "{3}, {T}: Exile the top card of your library. Until the beginning of your next upkeep, you may play that card.";

/// CR 502.3 + CR 305.7: the Swamp change must wear off at the untap step of the
/// LAND'S controller — not never (the `Permanent` misparse) and not at end of
/// turn.
#[test]
fn orcish_farmer_swamp_change_expires_at_the_lands_controllers_untap_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let farmer = scenario
        .add_creature_from_oracle(P0, "Orcish Farmer", 1, 1, ORCISH_FARMER)
        .id();
    // The opponent's basic Forest is the target, so "its controller" is P1 while
    // the resolving ability's controller is P0 — the two are deliberately
    // different players.
    let forest = scenario.add_basic_land(P1, ManaColor::Green);
    let mut runner = scenario.build();

    assert!(
        land_subtypes(&mut runner, forest).contains(&"Forest".to_string()),
        "precondition: the targeted land starts as a Forest"
    );

    let waiting = {
        let outcome = runner
            .activate(farmer, 0)
            .target_objects(&[forest])
            .resolve();
        format!("{:?}", outcome.state().waiting_for)
    };
    let subtypes = land_subtypes(&mut runner, forest);
    assert!(
        subtypes.contains(&"Swamp".to_string()) && !subtypes.contains(&"Forest".to_string()),
        "CR 305.7: setting a land's basic type replaces its other land types; \
         got subtypes={subtypes:?} waiting_for={waiting}",
    );

    // Still a Swamp at P0's end step: the deadline is a step on a LATER turn,
    // so an `UntilEndOfTurn` mapping would already be wrong here.
    let farmer_turn = runner.state().turn_number;
    drive_to(&mut runner, Phase::End, farmer_turn);
    let subtypes = land_subtypes(&mut runner, forest);
    assert!(
        subtypes.contains(&"Swamp".to_string()),
        "the change must outlive the turn it was created on; subtypes={subtypes:?}",
    );

    // P1's turn begins: their untap step runs, and the deadline is reached.
    // Observed at P1's upkeep, the first priority window after that untap step
    // (CR 502.4 gives none inside it).
    drive_to(&mut runner, Phase::Upkeep, farmer_turn + 1);
    assert_eq!(
        runner.state().active_player,
        P1,
        "the observation point must be the LAND controller's turn"
    );
    let subtypes = land_subtypes(&mut runner, forest);
    assert!(
        subtypes.contains(&"Forest".to_string()) && !subtypes.contains(&"Swamp".to_string()),
        "the Swamp change must expire at its controller's next untap step \
         (CR 502.3); still a Swamp means the duration was dropped and stamped \
         Permanent. subtypes={subtypes:?}",
    );
}

/// CR 500.4 + CR 503.1: the same deadline on a *casting permission* rather than
/// a continuous effect. Elkin Bottle lowers its clause to
/// `CastingPermission::PlayFromExile { duration: UntilNextStepOf { Upkeep, .. } }`,
/// which `prune_until_next_upkeep_effects` does not touch — that half of the
/// deadline is owned by `prune_upkeep_step_casting_permissions`. Without it the
/// exiled card stays playable forever.
#[test]
fn elkin_bottle_play_permission_expires_at_the_controllers_next_upkeep() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest"]);
    let bottle = scenario
        .add_creature_from_oracle(P0, "Elkin Bottle", 1, 1, ELKIN_BOTTLE)
        .id();
    // Three untapped lands to pay the ability's {3}.
    let lands: Vec<_> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    let mut runner = scenario.build();

    let waiting = {
        let outcome = runner.activate(bottle, 0).pay_with(&lands).resolve();
        format!("{:?}", outcome.state().waiting_for)
    };
    let granted = |runner: &GameRunner| {
        runner
            .state()
            .objects
            .values()
            .filter(|o| {
                o.casting_permissions.iter().any(|p| {
                    matches!(
                        p,
                        CastingPermission::PlayFromExile {
                            duration: Duration::UntilNextStepOf {
                                step: Phase::Upkeep,
                                ..
                            },
                            ..
                        }
                    )
                })
            })
            .count()
    };
    assert_eq!(
        granted(&runner),
        1,
        "the exiled card must carry an upkeep-scoped play permission — on main \
         the clause lowered to Effect::Unimplemented. waiting_for={waiting}",
    );

    // The opponent's upkeep is not the grantee's upkeep (CR 109.5).
    let cast_turn = runner.state().turn_number;
    drive_to(&mut runner, Phase::Upkeep, cast_turn + 1);
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(
        granted(&runner),
        1,
        "the permission must survive an OPPONENT's upkeep"
    );

    drive_to(&mut runner, Phase::Upkeep, cast_turn + 2);
    assert_eq!(runner.state().active_player, P0);
    assert_eq!(
        granted(&runner),
        0,
        "the play permission must expire as its grantee's upkeep step begins \
         (CR 500.4 + CR 503.1); still granted means nothing prunes the \
         casting-permission half of the deadline"
    );
}

/// CR 503.1: an "until your next upkeep" grant must be created at all (it was
/// `Effect::Unimplemented` before), must survive the intervening opponent turn,
/// and must expire when its CONTROLLER's upkeep step begins.
#[test]
fn until_your_next_upkeep_grant_survives_the_opponents_turn_and_expires_at_your_upkeep() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The deadline is two turns out, so both players take a draw step; stock
    // their libraries so nobody decks out (CR 704.5b) before the observation
    // point.
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest"]);
    let grantee = scenario.add_creature(P0, "Grantee", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Angelfire Flying Mode",
            true,
            GAINS_FLYING_UNTIL_YOUR_NEXT_UPKEEP,
        )
        .id();
    let mut runner = scenario.build();

    let waiting = {
        let outcome = runner.cast(spell).target_objects(&[grantee]).resolve();
        format!("{:?}", outcome.state().waiting_for)
    };
    assert!(
        has_kw(&mut runner, grantee, &Keyword::Flying),
        "the grant must happen at all — on main the clause lowered to \
         Effect::Unimplemented and nothing was granted. waiting_for={waiting}",
    );

    // The opponent's upkeep is NOT "your next upkeep" (CR 109.5): a
    // Controller-scoped deadline must not expire there.
    let cast_turn = runner.state().turn_number;
    drive_to(&mut runner, Phase::Upkeep, cast_turn + 1);
    assert_eq!(
        runner.state().active_player,
        P1,
        "expected the opponent's turn, at phase {:?}",
        runner.state().phase
    );
    assert!(
        has_kw(&mut runner, grantee, &Keyword::Flying),
        "a Controller-scoped upkeep deadline must survive an OPPONENT's upkeep"
    );

    // Back around to P0's upkeep: the deadline is reached.
    drive_to(&mut runner, Phase::Upkeep, cast_turn + 2);
    assert_eq!(
        runner.state().active_player,
        P0,
        "expected the grant controller's own next turn"
    );
    assert!(
        !has_kw(&mut runner, grantee, &Keyword::Flying),
        "the grant must expire as its controller's upkeep step begins \
         (CR 500.4 + CR 503.1); still flying means nothing prunes \
         UntilNextStepOf {{ step: Upkeep }}"
    );
}
