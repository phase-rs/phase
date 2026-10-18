//! Cancel/back-out for pre-cost keyword-activation selection states
//! (`EquipTarget`, `StationTarget`, `CrewVehicle`, `SaddleMount`) plus the
//! spell `TargetSelection` prompt, all on `GameAction::CancelCast`.
//!
//! CR 602.2b: keyword activations follow CR 601.2b-i, so their pre-cost
//! target/crew-choice steps can be withdrawn with nothing to unwind.
//! CR 601.2c/601.2h: target selection precedes cost payment — entry handlers
//! pay nothing, so cancel is a pure rollback to `Priority`.
//!
//! All tests drive the real pipeline: production entry into the prompt,
//! `legal_actions` for the offer, `act(CancelCast)` for acceptance.

use engine::ai_support::{legal_actions, legal_actions_full};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, ManaChoicePrompt, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (data/card-data.json, checked at implementation time).
const LIGHTNING_BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";

/// Copied from the established in-repo `brigid_mana_ability.rs` fixture (not a
/// new card premise): `{T}` mana ability with a two-color choice.
const BRIGID_TEXT: &str =
    "{T}: Add X {G} or X {W}, where X is the number of other creatures you control.";

fn pool_total(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].mana_pool.total()
}

fn charge_count(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Generic("charge".to_string()))
        .copied()
        .unwrap_or(0)
}

fn cancel_offer_count(runner: &GameRunner) -> usize {
    legal_actions(runner.state())
        .iter()
        .filter(|action| matches!(action, GameAction::CancelCast))
        .count()
}

fn tap_manual_land(runner: &mut GameRunner, land: ObjectId) {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let action = grouped
        .get(&land)
        .into_iter()
        .flatten()
        .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
        .expect("the engine must offer the land's mana ability")
        .clone();
    runner
        .act(action)
        .expect("the manual mana tap must succeed");
    assert!(runner.state().objects[&land].tapped);
    assert_eq!(pool_total(runner), 1);
    assert_eq!(
        runner.state().lands_tapped_for_mana.get(&P0),
        Some(&vec![land]),
        "reach guard: the real tap opened the mana-undo window"
    );
}

fn undo_manual_land_after_cancel(runner: &mut GameRunner, land: ObjectId) {
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(
        pool_total(runner),
        1,
        "cancel must preserve the unspent mana"
    );
    assert_eq!(
        runner.state().lands_tapped_for_mana.get(&P0),
        Some(&vec![land]),
        "the pre-cost cancel must preserve the open mana-undo window"
    );
    runner
        .act(GameAction::UntapLandForMana { object_id: land })
        .expect("the pre-cost cancel must leave the manual tap reversible");
    assert!(!runner.state().objects[&land].tapped);
    assert_eq!(pool_total(runner), 0, "undo must remove the land's mana");
}

fn assert_mana_undo_closed_after_priority_passes(runner: &mut GameRunner, land: ObjectId) {
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
    assert!(runner.state().objects[&land].tapped);
    assert_eq!(
        pool_total(runner),
        1,
        "the tapped land's mana is still unspent"
    );
    assert!(
        !runner.state().lands_tapped_for_mana.contains_key(&P0),
        "the subsequent accepted priority pass must close the mana-undo window"
    );
    assert!(
        runner
            .act(GameAction::UntapLandForMana { object_id: land })
            .is_err(),
        "the canceled selection must not make mana undo survive a later priority pass"
    );
}

/// CR 602.2b + CR 601.2c: backing out of equip target selection restores
/// priority with zero state change — nothing attached, nothing tapped, no mana
/// spent. Fails pre-fix: the engine rejects `CancelCast` in `EquipTarget`.
#[test]
fn equip_cancel_restores_priority_untouched() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let creature_a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let creature_b = scenario.add_creature(P0, "Elite Vanguard", 2, 1).id();
    let equipment = {
        let mut builder = scenario.add_creature(P0, "Test Equipment", 0, 0);
        builder.as_artifact().with_subtypes(vec!["Equipment"]);
        builder.id()
    };

    let mut runner = scenario.build();
    tap_manual_land(&mut runner, land);
    let pool_before = pool_total(&runner);

    runner
        .act(GameAction::Equip {
            equipment_id: equipment,
            target_id: creature_a,
        })
        .expect("equip entry must reach its target prompt with 2+ creatures");
    match &runner.state().waiting_for {
        WaitingFor::EquipTarget { valid_targets, .. } => {
            assert!(
                valid_targets.len() >= 2,
                "reach guard: 2+ valid targets, so no single-target auto-equip: {valid_targets:?}"
            );
        }
        other => panic!("expected EquipTarget, got {other:?}"),
    }
    assert!(
        legal_actions(runner.state()).contains(&GameAction::CancelCast),
        "equip selection must advertise CancelCast as legal"
    );

    runner
        .act(GameAction::CancelCast)
        .expect("cancelling the equip must succeed");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "cancelling EquipTarget must restore priority"
    );
    assert!(
        runner.state().stack.is_empty(),
        "cancel must leave the stack empty"
    );
    for id in [creature_a, creature_b] {
        let obj = &runner.state().objects[&id];
        assert!(!obj.tapped, "cancel must not tap any creature");
        assert!(
            obj.attachments.is_empty(),
            "cancel must leave the equipment unattached"
        );
    }
    assert_eq!(
        pool_total(&runner),
        pool_before,
        "cancel must not spend mana"
    );
    undo_manual_land_after_cancel(&mut runner, land);

    // Hostile round-trip: cancel leaves no stale state behind — re-activate
    // and complete the attach.
    tap_manual_land(&mut runner, land);
    runner
        .act(GameAction::Equip {
            equipment_id: equipment,
            target_id: creature_a,
        })
        .expect("re-activation after cancel must succeed");
    runner
        .act(GameAction::Equip {
            equipment_id: equipment,
            target_id: creature_b,
        })
        .expect("completing the re-activated equip must succeed");
    runner.advance_until_stack_empty();
    assert_mana_undo_closed_after_priority_passes(&mut runner, land);
    assert!(
        runner.state().objects[&creature_b]
            .attachments
            .contains(&equipment),
        "guard: the completed attach is asserted before the sibling below"
    );
    // Sibling: once the activation completed, Priority offers no CancelCast.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "equipping must settle back at priority"
    );
    assert!(
        !legal_actions(runner.state()).contains(&GameAction::CancelCast),
        "Priority must offer no CancelCast once the activation completed"
    );
}

/// CR 602.2b + CR 702.184a: backing out of station creature selection restores
/// priority — the creature stays untapped and no charge counters are added.
/// Fails pre-fix: the engine rejects `CancelCast` in `StationTarget`.
#[test]
fn station_cancel_restores_priority_untapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let spacecraft = {
        let mut builder = scenario.add_creature(P0, "Test Spacecraft", 5, 5);
        builder
            .as_artifact()
            .with_subtypes(vec!["Spacecraft"])
            .with_keyword(Keyword::Station);
        builder.id()
    };
    let crew_creature = scenario.add_creature(P0, "Station Crew", 3, 3).id();

    let mut runner = scenario.build();
    tap_manual_land(&mut runner, land);

    runner
        .act(GameAction::ActivateStation {
            spacecraft_id: spacecraft,
            creature_id: None,
        })
        .expect("station entry must reach its creature prompt at sorcery speed");
    match &runner.state().waiting_for {
        WaitingFor::StationTarget {
            eligible_creatures, ..
        } => {
            assert!(
                eligible_creatures.contains(&crew_creature),
                "reach guard: the crew creature must be eligible: {eligible_creatures:?}"
            );
        }
        other => panic!("expected StationTarget, got {other:?}"),
    }
    assert!(
        legal_actions(runner.state()).contains(&GameAction::CancelCast),
        "station selection must advertise CancelCast as legal"
    );

    runner
        .act(GameAction::CancelCast)
        .expect("cancelling the station must succeed");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "cancelling StationTarget must restore priority"
    );
    assert!(
        !runner.state().objects[&crew_creature].tapped,
        "cancel must not tap the creature (cost is paid only at announcement)"
    );
    assert_eq!(
        charge_count(&runner, spacecraft),
        0,
        "cancel must add no charge counters"
    );
    assert!(
        runner.state().stack.is_empty(),
        "cancel must leave the stack empty"
    );
    undo_manual_land_after_cancel(&mut runner, land);

    // Hostile round-trip: the same creature can station immediately after the
    // cancel, and resolution adds counters equal to its power.
    tap_manual_land(&mut runner, land);
    runner
        .act(GameAction::ActivateStation {
            spacecraft_id: spacecraft,
            creature_id: None,
        })
        .expect("re-activation after cancel must succeed");
    runner
        .act(GameAction::ActivateStation {
            spacecraft_id: spacecraft,
            creature_id: Some(crew_creature),
        })
        .expect("announcing the station must succeed");
    assert!(
        runner.state().objects[&crew_creature].tapped,
        "announcement taps the chosen creature"
    );
    runner.advance_until_stack_empty();
    assert_mana_undo_closed_after_priority_passes(&mut runner, land);
    assert_eq!(
        charge_count(&runner, spacecraft),
        3,
        "resolution must add charge counters equal to the tapped creature's power"
    );
}

/// CR 602.2b + CR 702.171a: the saddle creature-selection step offers
/// `CancelCast` and accepts it back to priority. Fails pre-fix on the
/// legal-actions assertion (the offer was absent; the engine arm existed).
#[test]
fn saddle_cancel_offered_and_accepted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let mount = {
        let mut builder = scenario.add_creature(P0, "Test Mount", 0, 4);
        builder.with_keyword(Keyword::Saddle(2));
        builder.id()
    };
    let rider = scenario.add_creature(P0, "Rider", 3, 3).id();

    let mut runner = scenario.build();
    tap_manual_land(&mut runner, land);

    runner
        .act(GameAction::SaddleMount {
            mount_id: mount,
            creature_ids: vec![],
        })
        .expect("saddle entry must reach its creature prompt at sorcery speed");
    match &runner.state().waiting_for {
        WaitingFor::SaddleMount {
            eligible_creatures, ..
        } => {
            assert!(
                !eligible_creatures.is_empty(),
                "reach guard: the prompt must list eligible creatures"
            );
        }
        other => panic!("expected SaddleMount, got {other:?}"),
    }
    assert!(
        legal_actions(runner.state()).contains(&GameAction::CancelCast),
        "saddle selection must advertise CancelCast as legal"
    );

    runner
        .act(GameAction::CancelCast)
        .expect("cancelling the saddle must succeed");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "cancelling SaddleMount must restore priority"
    );
    assert!(
        !runner.state().objects[&rider].tapped,
        "cancel must not tap any creature"
    );
    assert!(
        !runner.state().objects[&mount].is_saddled,
        "cancel must leave the Mount unsaddled"
    );
    assert!(
        runner.state().stack.is_empty(),
        "cancel must leave the stack empty"
    );
    undo_manual_land_after_cancel(&mut runner, land);

    tap_manual_land(&mut runner, land);
    runner
        .act(GameAction::SaddleMount {
            mount_id: mount,
            creature_ids: vec![],
        })
        .expect("re-saddle after cancel must reach the selection");
    runner
        .act(GameAction::SaddleMount {
            mount_id: mount,
            creature_ids: vec![rider],
        })
        .expect("the re-announced saddle must succeed");
    assert!(runner.state().objects[&rider].tapped);
    assert!(!runner.state().stack.is_empty());
    runner.advance_until_stack_empty();
    assert!(runner.state().objects[&mount].is_saddled);
    assert_mana_undo_closed_after_priority_passes(&mut runner, land);
}

/// The crew offer moved from its per-state push to the generic push: exactly
/// one `CancelCast` must be offered — two if a per-state push survived
/// deletion, zero if the generic gate regressed (the F1 failure mode).
#[test]
fn crew_cancel_offered_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let vehicle = {
        let mut builder = scenario.add_creature(P0, "Test Vehicle", 6, 5);
        builder
            .as_artifact()
            .with_subtypes(vec!["Vehicle"])
            .with_keyword(Keyword::Crew {
                power: 3,
                once_per_turn: None,
            });
        builder.id()
    };
    let pilot_a = scenario.add_creature(P0, "Pilot A", 2, 2).id();
    let pilot_b = scenario.add_creature(P0, "Pilot B", 2, 2).id();

    let mut runner = scenario.build();
    tap_manual_land(&mut runner, land);

    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![],
        })
        .expect("crew entry must reach its selection prompt");
    match &runner.state().waiting_for {
        WaitingFor::CrewVehicle {
            eligible_creatures, ..
        } => {
            assert!(
                !eligible_creatures.is_empty(),
                "guard: the prompt is real — eligible creatures are listed"
            );
        }
        other => panic!("expected CrewVehicle, got {other:?}"),
    }
    assert!(
        runner.state().waiting_for.allows_cancel_cast(),
        "CrewVehicle must allow cancel"
    );
    assert_eq!(
        cancel_offer_count(&runner),
        1,
        "crew selection must offer CancelCast exactly once"
    );
    assert!(
        legal_actions(runner.state()).iter().any(|action| matches!(
            action,
            GameAction::CrewVehicle { creature_ids, .. } if !creature_ids.is_empty()
        )),
        "guard: at least one crew subset candidate must be present"
    );

    runner
        .act(GameAction::CancelCast)
        .expect("cancelling the crew must succeed");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "cancelling CrewVehicle must restore priority"
    );
    for id in [pilot_a, pilot_b] {
        assert!(
            !runner.state().objects[&id].tapped,
            "cancel must not tap any creature"
        );
    }
    undo_manual_land_after_cancel(&mut runner, land);

    // Round-trip cleanliness: re-crew and resolve through the real pipeline.
    tap_manual_land(&mut runner, land);
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![],
        })
        .expect("re-crew after cancel must succeed");
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![pilot_a, pilot_b],
        })
        .expect("announcing the re-crew must succeed");
    runner.advance_until_stack_empty();
    assert_mana_undo_closed_after_priority_passes(&mut runner, land);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "resolved crew must settle back at priority"
    );
    for id in [pilot_a, pilot_b] {
        assert!(
            runner.state().objects[&id].tapped,
            "resolution must tap the crewing creatures"
        );
    }
}

/// Spell `TargetSelection` cancel: driven manually (`CastSpell` → `CancelCast`)
/// because `cast().resolve()` auto-answers the prompt under test. The bolt
/// returns to hand with life totals, mana pool, and stack untouched; a recast
/// then completes for the full round-trip.
#[test]
fn spell_target_selection_cancel_restores_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    scenario.add_creature(P1, "Elite Vanguard", 2, 1);
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();
    let life_p0_before = runner.state().players[P0.0 as usize].life;
    let life_p1_before = runner.state().players[P1.0 as usize].life;
    let pool_before = pool_total(&runner);

    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id: CardId(bolt.0),
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("bolt cast must reach its target prompt");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { selection, .. } => {
            assert!(
                selection.current_legal_targets.len() >= 2,
                "reach guard: 2+ legal targets, so no single-target auto-assign"
            );
        }
        other => panic!("expected TargetSelection, got {other:?}"),
    }
    assert!(
        legal_actions(runner.state()).contains(&GameAction::CancelCast),
        "spell targeting must advertise CancelCast as legal"
    );

    runner
        .act(GameAction::CancelCast)
        .expect("cancelling the bolt must succeed");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "cancelling TargetSelection must restore priority"
    );
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Hand,
        "the cancelled bolt returns to hand"
    );
    assert!(
        runner.state().stack.is_empty(),
        "cancel must leave the stack empty"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_p0_before,
        "cancel must deal no damage to P0"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_p1_before,
        "cancel must deal no damage to P1"
    );
    assert_eq!(
        pool_total(&runner),
        pool_before,
        "cancel before payment must not spend mana"
    );

    // Hostile round-trip: recast the same bolt and complete it.
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id: CardId(bolt.0),
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("recast after cancel must succeed");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("choosing the recast target must succeed");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_p1_before - 3,
        "the recast bolt must deal 3 to P1"
    );
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Graveyard,
        "the resolved bolt goes to the graveyard"
    );
}

/// Redundancy witness for the generic-gate amendment: a non-keyword
/// pending-cast state (`ManaPayment`) still gets exactly one `CancelCast`
/// offer — the dropped `has_pending_cast` conjunct changed no verdict outside
/// the four keyword states.
#[test]
fn mana_payment_cancel_offered_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Witness Spell", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: CardId(spell.0),
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("manual cast must enter its mana-payment step");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the cast must park in ManaPayment, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        cancel_offer_count(&runner),
        1,
        "ManaPayment must offer CancelCast exactly once via the generic push"
    );

    runner
        .act(GameAction::CancelCast)
        .expect("cancel from mana payment must succeed");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "cancelling ManaPayment must restore priority"
    );
    assert!(
        runner.state().stack.is_empty(),
        "cancel must leave the stack empty"
    );
}

/// Mana-ability cancel is out of scope: a `ChooseManaColor` prompt reached
/// through a real `{T}` activation offers no `CancelCast` (CR 605.3b — mana
/// abilities resolve immediately and are never backed out of). Passes pre and
/// post; documents the boundary.
#[test]
fn mana_color_prompt_offers_no_cancel() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brigid_id = scenario
        .add_creature_from_oracle(P0, "Brigid, Doun's Mind", 2, 3, BRIGID_TEXT)
        .id();
    scenario.add_creature(P0, "Other Creature A", 1, 1);
    scenario.add_creature(P0, "Other Creature B", 1, 1);

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: brigid_id,
            ability_index: 0,
        })
        .expect("activating the mana ability must succeed");
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert!(
                !options.is_empty(),
                "guard: the prompt must offer at least one color: {options:?}"
            );
        }
        other => panic!("expected ChooseManaColor, got {other:?}"),
    }
    assert!(
        !runner.state().waiting_for.allows_cancel_cast(),
        "ChooseManaColor must not allow cancel"
    );
    assert!(
        !legal_actions(runner.state()).contains(&GameAction::CancelCast),
        "mana-ability color choice must offer no CancelCast"
    );
}
