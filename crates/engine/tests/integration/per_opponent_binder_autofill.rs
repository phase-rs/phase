//! CR 115.10a: the `SpecificPlayer` half of a per-opponent target fanout is a
//! structurally pinned BINDER, not a target. The engine announces it on the
//! controller's behalf, so the first prompt the controller sees is the real
//! choice — the card in that opponent's graveyard.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::PlayerId;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);
const DILUVIAN_ORACLE: &str = "Flying\nWhen this creature enters, for each opponent, you may cast up to one target instant or sorcery card from that player's graveyard without paying its mana cost. If a spell cast this way would be put into a graveyard, exile it instead.";

fn advance_to_trigger_target_selection(runner: &mut GameRunner) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass while reaching the Diluvian trigger");
            }
            ref other => panic!("unexpected state while reaching the Diluvian trigger: {other:?}"),
        }
    }
    panic!("Diluvian Primordial ETB never reached its target prompt");
}

fn advance_to_free_cast_window(runner: &mut GameRunner) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::CastOffer {
                kind: CastOfferKind::FreeCastWindow { .. },
                ..
            } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass while resolving the Diluvian trigger");
            }
            ref other => panic!("unexpected state while reaching the Diluvian window: {other:?}"),
        }
    }
    panic!("Diluvian Primordial never opened its free-cast window");
}

fn cast_diluvian(runner: &mut GameRunner, primordial: ObjectId) {
    let card_id = runner.state().objects[&primordial].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: primordial,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Diluvian Primordial must succeed");
}

fn seed_controller_mana(scenario: &mut GameScenario) {
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
}

/// CR 115.10a + CR 115.1d: the reported shape — 4-player Commander, controller
/// plus three opponents, one of whom has an empty graveyard. Diluvian's word
/// "target" attaches to the instant or sorcery card, never to the opponent, so
/// the opponent slot is announced by the engine and the FIRST prompt the
/// controller sees is the object slot.
#[test]
fn four_player_fanout_first_prompt_is_the_object_slot() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    seed_controller_mana(&mut scenario);
    let primordial = scenario
        .add_creature_to_hand_from_oracle(P0, "Diluvian Primordial", 5, 5, DILUVIAN_ORACLE)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    // P1's graveyard stays empty: that opponent contributes no slots at all.
    let p2_selected = scenario
        .add_spell_to_graveyard(P2, "P2 Selected", true)
        .from_oracle_text("Draw a card.")
        .id();
    let p2_extra = scenario
        .add_spell_to_graveyard(P2, "P2 Extra", false)
        .from_oracle_text("Draw a card.")
        .id();
    let p3_selected = scenario
        .add_spell_to_graveyard(P3, "P3 Selected", true)
        .from_oracle_text("Draw a card.")
        .id();

    let mut runner = scenario.build();
    cast_diluvian(&mut runner, primordial);
    advance_to_trigger_target_selection(&mut runner);

    match runner.state().waiting_for.clone() {
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => {
            // Reach guard: the binder slots still exist and each still carries
            // exactly the one pinned opponent. Calibrated against the measured
            // BASE repro (slot count 4, slot 0 legal = [Player(P2)]).
            assert_eq!(
                target_slots.len(),
                4,
                "P1 has no instant or sorcery, so only P2's and P3's pairs are built"
            );
            assert_eq!(target_slots[0].legal_targets, vec![TargetRef::Player(P2)]);
            assert!(!target_slots[0].optional);
            assert_eq!(target_slots[2].legal_targets, vec![TargetRef::Player(P3)]);

            // The discriminating pair: the walk opens on the object slot, and
            // what it offers is the card, never the opponent.
            assert_eq!(
                selection.current_slot, 1,
                "the pinned opponent is announced by the engine, not prompted"
            );
            assert_eq!(
                selection.current_legal_targets,
                vec![TargetRef::Object(p2_selected), TargetRef::Object(p2_extra)],
                "the first prompt offers P2's graveyard cards"
            );
            assert!(
                !selection
                    .current_legal_targets
                    .iter()
                    .any(|target| matches!(target, TargetRef::Player(_))),
                "CR 115.10a: an affected-but-untargeted opponent is never offered as a choice"
            );
            assert_eq!(
                selection.selected_slots,
                vec![Some(TargetRef::Player(P2))],
                "the auto-filled binder is announced into the walk exactly as a click would be"
            );
            let _ = p3_selected;
        }
        other => panic!("expected the Diluvian target prompt, got {other:?}"),
    }
}

/// CR 601.2c + CR 115.3: the engine-announced binder lands in `ability.targets`
/// at the binder's own index, so each opponent's object slot is scoped to that
/// opponent's own graveyard and the resolved free-cast pool is the two selected
/// cards — one per opponent, with no cross-binding.
#[test]
fn autofilled_binder_survives_into_the_fanout_target_vector() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    seed_controller_mana(&mut scenario);
    let primordial = scenario
        .add_creature_to_hand_from_oracle(P0, "Diluvian Primordial", 5, 5, DILUVIAN_ORACLE)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let p1_selected = scenario
        .add_spell_to_graveyard(P1, "P1 Selected", true)
        .from_oracle_text("Draw a card.")
        .id();
    let p1_extra = scenario
        .add_spell_to_graveyard(P1, "P1 Extra", false)
        .from_oracle_text("Draw a card.")
        .id();
    let p2_selected = scenario
        .add_spell_to_graveyard(P2, "P2 Selected", true)
        .from_oracle_text("Draw a card.")
        .id();
    let p2_extra = scenario
        .add_spell_to_graveyard(P2, "P2 Extra", false)
        .from_oracle_text("Draw a card.")
        .id();

    let mut runner = scenario.build();
    cast_diluvian(&mut runner, primordial);
    advance_to_trigger_target_selection(&mut runner);

    match runner.state().waiting_for.clone() {
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => {
            assert_eq!(target_slots.len(), 4);
            assert_eq!(selection.current_slot, 1);
            assert_eq!(
                selection.current_legal_targets,
                vec![TargetRef::Object(p1_selected), TargetRef::Object(p1_extra)],
                "P1's binder is bound, so its object slot is scoped to P1's graveyard"
            );
        }
        other => panic!("expected the Diluvian target prompt, got {other:?}"),
    }

    // Submit ONLY object refs — the binder halves are the engine's to announce.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(p1_selected)),
        })
        .expect("P1's graveyard card must be selectable without announcing P1");

    match runner.state().waiting_for.clone() {
        WaitingFor::TriggerTargetSelection { selection, .. } => {
            assert_eq!(
                selection.current_slot, 3,
                "P2's binder is announced too, so the walk lands on P2's object slot"
            );
            assert_eq!(
                selection.current_legal_targets,
                vec![TargetRef::Object(p2_selected), TargetRef::Object(p2_extra)],
                "no cross-binding: the second object slot is scoped to P2's own graveyard"
            );
        }
        other => panic!("expected P2's object prompt, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(p2_selected)),
        })
        .expect("P2's graveyard card must be selectable without announcing P2");
    advance_to_free_cast_window(&mut runner);

    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            kind:
                CastOfferKind::FreeCastWindow {
                    member_pool,
                    remaining_casts,
                    ..
                },
            ..
        } => {
            // Reach guard first: a pool that is empty because the trigger was
            // dropped fails here before the pool assertion can pass vacuously.
            assert_eq!(remaining_casts, Some(2), "both pairs reached resolution");
            assert_eq!(
                member_pool,
                vec![p1_selected, p2_selected],
                "one card from each opponent's own graveyard"
            );
        }
        other => panic!("expected the Diluvian FreeCastWindow, got {other:?}"),
    }
}
