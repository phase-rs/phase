//! Runtime regression for #3867 — Volcanic Offering's "of an opponent's choice"
//! per-slot target announcer.
//!
//! "Destroy target nonbasic land you don't control and target nonbasic land of an
//! opponent's choice you don't control. Volcanic Offering deals 7 damage to target
//! creature you don't control and 7 damage to target creature of an opponent's
//! choice you don't control."
//!
//! CR 601.2c: the controller normally announces every target; this card text
//! overrides the announcer for the second land slot and the second creature slot
//! (the opponent chooses those). CR 115.1: regardless of who announced a slot,
//! the spell is controlled, paid for, and put on the stack by its controller.
//!
//! These tests drive the real cast pipeline (`apply`) and assert:
//!   1. The `WaitingFor::TargetSelection.player` flips controller→opponent→
//!      controller→opponent across the per-slot ChooseTarget walk.
//!   2. B1: after the FINAL (opponent-chosen) slot completes, the spell is on the
//!      stack controlled by the controller (not the opponent who announced it).
//!   3. End-to-end: both opponent nonbasic lands are destroyed and both opponent
//!      creatures take 7 damage.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const VOLCANIC_OFFERING: &str = "Destroy target nonbasic land you don't control \
     and target nonbasic land of an opponent's choice you don't control. Volcanic \
     Offering deals 7 damage to target creature you don't control and 7 damage to \
     target creature of an opponent's choice you don't control.";

/// Add a nonbasic land controlled by `player` to the battlefield. `add_creature`
/// plus `as_land()` yields a Land with no Basic supertype, which satisfies the
/// "nonbasic land you don't control" filter.
fn nonbasic_land(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
) -> engine::types::identifiers::ObjectId {
    scenario.add_creature(player, name, 0, 0).as_land().id()
}

fn build_scenario() -> (GameScenario, [engine::types::identifiers::ObjectId; 5]) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // The opponent (P1) controls two nonbasic lands and two creatures.
    let land_a = nonbasic_land(&mut scenario, P1, "Opponent Land A");
    let land_b = nonbasic_land(&mut scenario, P1, "Opponent Land B");
    let creature_a = scenario.add_creature(P1, "Opp Creature A", 5, 5).id();
    let creature_b = scenario.add_creature(P1, "Opp Creature B", 5, 5).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Volcanic Offering", true, VOLCANIC_OFFERING)
        .with_mana_cost(ManaCost::zero())
        .id();

    (scenario, [spell, land_a, land_b, creature_a, creature_b])
}

fn current_announcer(waiting_for: &WaitingFor) -> PlayerId {
    match waiting_for {
        WaitingFor::TargetSelection { player, .. } => *player,
        other => panic!("expected TargetSelection, got {other:?}"),
    }
}

#[test]
fn waiting_for_player_flips_controller_opponent_across_slots_and_controller_keeps_spell() {
    let (scenario, [spell, land_a, land_b, creature_a, creature_b]) = build_scenario();
    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the free instant must succeed");

    // Slot 0: controller (P0) announces the first nonbasic land.
    assert_eq!(
        current_announcer(&runner.state().waiting_for),
        P0,
        "slot 0 (target nonbasic land you don't control) is announced by the controller"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(land_a)),
        })
        .expect("controller announces land A");

    // Slot 1: opponent (P1) announces the second nonbasic land ("of an
    // opponent's choice"). THIS is the chooser flip.
    assert_eq!(
        current_announcer(&runner.state().waiting_for),
        P1,
        "slot 1 (of an opponent's choice) is announced by the opponent"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(land_b)),
        })
        .expect("opponent announces land B");

    // Slot 2: controller again (damage to a creature it doesn't control).
    assert_eq!(
        current_announcer(&runner.state().waiting_for),
        P0,
        "slot 2 (creature you don't control) is announced by the controller"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(creature_a)),
        })
        .expect("controller announces creature A");

    // Slot 3: opponent again ("target creature of an opponent's choice").
    assert_eq!(
        current_announcer(&runner.state().waiting_for),
        P1,
        "slot 3 (of an opponent's choice) is announced by the opponent"
    );
    // This is the FINAL slot. Completing it must NOT leave the opponent in
    // control of the spell (B1).
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(creature_b)),
        })
        .expect("opponent announces creature B");

    // B1: after the final (opponent-chosen) slot completes, the spell is on the
    // stack controlled by its CONTROLLER (P0), and priority returns to P0.
    match &runner.state().waiting_for {
        WaitingFor::Priority { player } => assert_eq!(
            *player, P0,
            "priority returns to the spell controller after the opponent-announced final slot"
        ),
        other => panic!("expected Priority after final slot, got {other:?}"),
    }
    let stack_entry = runner.state().stack.last().expect("spell is on the stack");
    assert_eq!(
        stack_entry.controller, P0,
        "CR 115.1: the spell is controlled by its controller, not the opponent who announced a slot"
    );

    // Resolve and confirm both lands destroyed, both creatures took 7 damage.
    runner.advance_until_stack_empty();

    // CR 701.7b: a destroyed permanent is moved to its owner's graveyard, where it
    // keeps its card types — so "destroyed" means it left the battlefield.
    let land_a_gone = runner
        .state()
        .objects
        .get(&land_a)
        .is_none_or(|o| o.zone != Zone::Battlefield);
    let land_b_gone = runner
        .state()
        .objects
        .get(&land_b)
        .is_none_or(|o| o.zone != Zone::Battlefield);
    assert!(land_a_gone, "opponent land A must be destroyed");
    assert!(land_b_gone, "opponent land B must be destroyed");

    // Creatures with 5 toughness taking 7 damage die as a state-based action.
    assert!(
        !runner.state().objects.contains_key(&creature_a)
            || runner.state().objects[&creature_a].damage_marked >= 7,
        "opponent creature A must take 7 damage (and die)"
    );
    assert!(
        !runner.state().objects.contains_key(&creature_b)
            || runner.state().objects[&creature_b].damage_marked >= 7,
        "opponent creature B must take 7 damage (and die)"
    );
}

/// CR 601.2c: Each printed instance of "target" may choose the same legal object.
/// Volcanic Offering's official ruling specifically confirms that the opponent may
/// choose the same nonbasic land or creature that the controller chose.
#[test]
fn opponent_may_choose_the_controller_selected_land_and_creature() {
    let (scenario, [spell, land_a, _land_b, creature_a, _creature_b]) = build_scenario();
    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the free instant must succeed");

    for (announcer, target) in [
        (P0, land_a),
        (P1, land_a),
        (P0, creature_a),
        (P1, creature_a),
    ] {
        assert_eq!(current_announcer(&runner.state().waiting_for), announcer);
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("the repeated target remains legal for a distinct target word");
    }

    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::Priority { player } if *player == P0
        ),
        "the spell was announced and priority returned to its controller"
    );
}

#[test]
fn bulk_select_targets_for_mixed_chooser_spell_is_rejected() {
    let (scenario, [spell, land_a, land_b, creature_a, creature_b]) = build_scenario();
    let _ = (land_b, creature_a, creature_b);
    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the free instant must succeed");

    // M4: a bulk SelectTargets submission for a mixed-chooser spell must error —
    // each slot must be announced one at a time by the correct player.
    let err = runner.act(GameAction::SelectTargets {
        targets: vec![TargetRef::Object(land_a)],
    });
    assert!(
        err.is_err(),
        "bulk SelectTargets must be rejected for a spell with an opponent-chosen slot"
    );
}

/// CR 601.2c + CR 115.1 (#4349 review [HIGH]): In a multiplayer game the spell
/// CONTROLLER chooses which opponent announces an "of an opponent's choice"
/// slot — not seat order. This drives a 3-player cast and proves the controller
/// can pick the NON-first opponent (P2) as the announcer; the previous
/// placeholder always routed the choice to the first seat-order opponent (P1).
#[test]
fn controller_chooses_non_first_opponent_as_announcer_in_three_player_game() {
    let p2 = PlayerId(2);

    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // Both opponents control a nonbasic land and a creature, so every slot has a
    // legal target regardless of which opponent the controller picks.
    let land_p1 = nonbasic_land(&mut scenario, P1, "P1 Land");
    let creature_p1 = scenario.add_creature(P1, "P1 Creature", 5, 5).id();
    let land_p2 = nonbasic_land(&mut scenario, p2, "P2 Land");
    let creature_p2 = scenario.add_creature(p2, "P2 Creature", 5, 5).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Volcanic Offering", true, VOLCANIC_OFFERING)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the free instant must succeed");

    // With two opponents, the controller is asked which one announces EACH
    // "of an opponent's choice" effect (the second land and the second creature
    // are decided independently). Pick the NON-first opponent (P2) for both,
    // proving the non-first selection is honored per effect.
    for effect in ["second land", "second creature"] {
        match &runner.state().waiting_for {
            WaitingFor::ChooseAnnouncingOpponent {
                player, candidates, ..
            } => {
                assert_eq!(
                    *player, P0,
                    "the controller chooses the announcing opponent ({effect})"
                );
                assert!(
                    candidates.contains(&P1) && candidates.contains(&p2),
                    "both opponents are candidates ({effect}), got {candidates:?}"
                );
            }
            other => panic!("expected ChooseAnnouncingOpponent for {effect}, got {other:?}"),
        }
        runner
            .act(GameAction::ChooseAnnouncingOpponent { opponent: p2 })
            .expect("controller picks P2 as the announcer");
    }

    // Slot 0: controller announces a nonbasic land it doesn't control.
    assert_eq!(current_announcer(&runner.state().waiting_for), P0);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(land_p1)),
        })
        .expect("controller announces P1's land");

    // Slot 1 ("of an opponent's choice"): announced by the CHOSEN opponent P2,
    // proving the controller's non-first selection is honored (not seat-order P1).
    assert_eq!(
        current_announcer(&runner.state().waiting_for),
        p2,
        "the opponent-chosen slot is announced by the controller-selected opponent P2"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(land_p2)),
        })
        .expect("P2 announces the second land");

    // Slot 2: controller again.
    assert_eq!(current_announcer(&runner.state().waiting_for), P0);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(creature_p1)),
        })
        .expect("controller announces P1's creature");

    // Slot 3 ("of an opponent's choice"): announced by P2 again.
    assert_eq!(
        current_announcer(&runner.state().waiting_for),
        p2,
        "the final opponent-chosen slot is announced by P2"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(creature_p2)),
        })
        .expect("P2 announces the second creature");

    // CR 115.1: the spell is controlled by its controller, not the announcer.
    let stack_entry = runner.state().stack.last().expect("spell is on the stack");
    assert_eq!(stack_entry.controller, P0);
}

/// CR 601.2c + CR 115.1 (#4349): the controller may name DIFFERENT opponents for
/// each "of an opponent's choice" effect — Volcanic Offering's rulings allow the
/// same or different opponents for the second land vs. the second creature. Each
/// effect is prompted/recorded independently, not one announcer stamped across
/// the whole ability chain. Here P1 announces the second land and P2 the second
/// creature.
#[test]
fn controller_may_choose_different_announcing_opponents_per_effect() {
    let p2 = PlayerId(2);

    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let land_p1 = nonbasic_land(&mut scenario, P1, "P1 Land");
    let creature_p1 = scenario.add_creature(P1, "P1 Creature", 5, 5).id();
    let land_p2 = nonbasic_land(&mut scenario, p2, "P2 Land");
    let creature_p2 = scenario.add_creature(p2, "P2 Creature", 5, 5).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Volcanic Offering", true, VOLCANIC_OFFERING)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the free instant must succeed");

    // First prompt — the second-LAND effect's announcer. Choose P1.
    match &runner.state().waiting_for {
        WaitingFor::ChooseAnnouncingOpponent {
            player,
            candidates,
            choice_index,
            choice_count,
            target_type,
            ..
        } => {
            assert_eq!(*player, P0);
            assert!(candidates.contains(&P1) && candidates.contains(&p2));
            assert_eq!((*choice_index, *choice_count), (1, 2));
            assert_eq!(*target_type, Some(CoreType::Land));
        }
        other => panic!("expected ChooseAnnouncingOpponent (land), got {other:?}"),
    }
    runner
        .act(GameAction::ChooseAnnouncingOpponent { opponent: P1 })
        .expect("P1 announces the second-land effect");

    // Second prompt — the second-CREATURE effect's announcer. Choose P2.
    match &runner.state().waiting_for {
        WaitingFor::ChooseAnnouncingOpponent {
            player,
            choice_index,
            choice_count,
            target_type,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!((*choice_index, *choice_count), (2, 2));
            assert_eq!(*target_type, Some(CoreType::Creature));
        }
        other => panic!("expected ChooseAnnouncingOpponent (creature), got {other:?}"),
    }
    runner
        .act(GameAction::ChooseAnnouncingOpponent { opponent: p2 })
        .expect("P2 announces the second-creature effect");

    // Slot 0: controller announces a nonbasic land it doesn't control.
    assert_eq!(current_announcer(&runner.state().waiting_for), P0);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(land_p1)),
        })
        .expect("controller announces P1's land");

    // Slot 1 (second land, "of an opponent's choice"): announced by P1.
    assert_eq!(
        current_announcer(&runner.state().waiting_for),
        P1,
        "the land effect's opponent-chosen slot is announced by P1"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(land_p2)),
        })
        .expect("P1 announces the second land");

    // Slot 2: controller announces a creature it doesn't control.
    assert_eq!(current_announcer(&runner.state().waiting_for), P0);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(creature_p1)),
        })
        .expect("controller announces P1's creature");

    // Slot 3 (second creature, "of an opponent's choice"): announced by P2 — a
    // DIFFERENT opponent than the land effect's announcer, which the old single
    // `announcing_opponent` could not express.
    assert_eq!(
        current_announcer(&runner.state().waiting_for),
        p2,
        "the creature effect's opponent-chosen slot is announced by P2"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(creature_p2)),
        })
        .expect("P2 announces the second creature");

    // CR 115.1: the spell is controlled by its controller, not either announcer.
    let stack_entry = runner.state().stack.last().expect("spell is on the stack");
    assert_eq!(stack_entry.controller, P0);
}
