//! Card-level regression for **Kozilek, the Broken Reality** —
//! "When you cast this spell, up to two target players each manifest two
//! cards from their hands. For each card manifested this way, you draw a
//! card."
//!
//! Drives the REAL registered cast trigger end to end: the creature is built
//! from its printed Oracle text, cast through `GameAction::CastSpell`, the
//! cast trigger's up-to-two player targets are chosen through
//! `GameAction::SelectTargets`, and each targeted player answers their OWN
//! `WaitingFor::ChooseFromZoneChoice` in APNAP order — the exact path the
//! client takes.
//!
//! DISCRIMINATORS (anti-hollow-win):
//! - each player's choice prompt offers ONLY that player's hand (P0's prompt
//!   never contains P1's cards and vice versa — the per-iteration
//!   `ZoneOwner::Each(PerPlayerScope::TargetedPlayers)` scoping).
//! - each manifested card enters under ITS OWN player's control — the
//!   opponent's manifests must NOT land under the caster's control
//!   (CR 701.40a: the manifesting player puts the card onto the battlefield).
//! - the "for each card manifested this way" rider draws exactly the number
//!   of accumulated picks (4), read from the chain's tracked set.
//!
//! CR 701.40a: Manifest — face-down 2/2 creature.
//! CR 101.4: multiple players making choices do so in APNAP order.
//! CR 601.2c + CR 115.1d: "up to two target players" is a multi-target
//! selection bound when the trigger goes on the stack.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KOZILEK_ORACLE: &str = "When you cast this spell, up to two target players each manifest two cards from their hands. For each card manifested this way, you draw a card.\nOther colorless creatures you control get +3/+2.";

#[test]
fn kozilek_cast_trigger_manifests_two_from_each_targeted_players_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0's hand: Kozilek plus two pickable cards (creature + noncreature mix).
    let koz = scenario
        .add_creature_to_hand_from_oracle(P0, "Kozilek, the Broken Reality", 9, 9, KOZILEK_ORACLE)
        .id();
    let a1 = scenario.add_creature_to_hand(P0, "P0 Pick One", 3, 3).id();
    let a2 = scenario
        .add_spell_to_hand_from_oracle(P0, "P0 Pick Two", false, "Draw a card.")
        .id();
    // P1's hand: two pickable cards.
    let b1 = scenario.add_creature_to_hand(P1, "P1 Pick One", 4, 4).id();
    let b2 = scenario.add_creature_to_hand(P1, "P1 Pick Two", 1, 1).id();
    // P0 needs library cards for the four rider draws.
    for i in 0..6 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
    }

    let mut runner = scenario.build();

    let koz_card_id = runner.state().objects[&koz].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: koz,
            card_id: koz_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Kozilek, the Broken Reality");

    // The cast trigger goes on the stack and asks for its up-to-two player
    // targets. A parse regression (Unimplemented body) never raises the
    // selection — the loop below then panics.
    let mut chosen = 0;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                // CR 115.1d: answer one "up to two target players" slot at a
                // time, picking P0 first and P1 second.
                let want = if chosen == 0 { P0 } else { P1 };
                let slot = &target_slots[selection.current_slot];
                let choice = slot
                    .legal_targets
                    .iter()
                    .find(|t| **t == TargetRef::Player(want))
                    .cloned();
                assert!(
                    choice.is_some(),
                    "player {want:?} must be a legal target of the cast trigger, got {:?}",
                    slot.legal_targets
                );
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("choosing a targeted player must be accepted");
                chosen += 1;
                if chosen == 2 {
                    break;
                }
            }
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("advance toward the cast trigger's target selection");
            }
        }
    }
    assert_eq!(
        chosen,
        2,
        "the cast trigger must offer both player target slots, got {:?}",
        runner.state().waiting_for
    );

    // Resolve the trigger: each targeted player picks two of their OWN hand
    // cards, in APNAP order.
    let mut p0_chose = false;
    let mut p1_chose = false;
    // CR 101.4: record the order the prompts arrive in — the active player
    // (P0) must be asked first.
    let mut prompt_order: Vec<engine::types::player::PlayerId> = Vec::new();
    for _ in 0..24 {
        if p0_chose && p1_chose {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
                prompt_order.push(player);
                if player == P0 {
                    // DISCRIMINATOR — P0's prompt offers only P0's hand.
                    assert!(
                        cards.contains(&a1) && cards.contains(&a2),
                        "P0's own hand cards must be offered, got {cards:?}"
                    );
                    assert!(
                        !cards.contains(&b1) && !cards.contains(&b2),
                        "P1's hand must NOT appear in P0's choice, got {cards:?}"
                    );
                    runner
                        .act(GameAction::SelectCards {
                            cards: vec![a1, a2],
                        })
                        .expect("P0 picks both hand cards");
                    p0_chose = true;
                } else {
                    assert_eq!(player, P1, "only the two targeted players choose");
                    // DISCRIMINATOR — P1's prompt offers only P1's hand.
                    assert!(
                        cards.contains(&b1) && cards.contains(&b2),
                        "P1's own hand cards must be offered, got {cards:?}"
                    );
                    assert!(
                        !cards.contains(&a1) && !cards.contains(&a2),
                        "P0's hand must NOT appear in P1's choice, got {cards:?}"
                    );
                    runner
                        .act(GameAction::SelectCards {
                            cards: vec![b1, b2],
                        })
                        .expect("P1 picks both hand cards");
                    p1_chose = true;
                }
            }
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("advance toward the per-player hand choices");
            }
        }
    }
    assert!(
        p0_chose && p1_chose,
        "both targeted players must get their own hand choice, got {:?}",
        runner.state().waiting_for
    );
    // DISCRIMINATOR — CR 101.4: simultaneous choices are made in APNAP order,
    // so the active player is prompted FIRST. Branching on `player` alone
    // would accept either order; this pins it.
    assert_eq!(
        prompt_order,
        vec![P0, P1],
        "the per-player hand choices must arrive in APNAP order (active player first)"
    );

    // Let the manifest + draw tail finish.
    runner.advance_until_stack_empty();

    // All four picks are manifested face-down creatures.
    for (id, label) in [(a1, "a1"), (a2, "a2"), (b1, "b1"), (b2, "b2")] {
        let obj = &runner.state().objects[&id];
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "{label} must be manifested onto the battlefield"
        );
        assert!(obj.face_down, "{label} must be face down");
        assert_eq!(obj.base_power, Some(2), "{label} is a printed-over 2/2");
        assert_eq!(obj.base_toughness, Some(2), "{label} is a printed-over 2/2");
    }
    // DISCRIMINATOR — control follows the manifesting player (CR 701.40a), and
    // Kozilek's own static ("Other colorless creatures you control get +3/+2")
    // proves it independently of the `controller` field: a face-down card is
    // colorless (CR 202.2b), so the CASTER's two manifests are pumped to 5/4
    // while the opponent's stay 2/2. A resolver that put every manifest under
    // the caster's control would pump all four.
    assert_eq!(
        runner.state().objects[&a1].controller,
        P0,
        "P0's manifests enter under P0's control"
    );
    assert_eq!(
        runner.state().objects[&b1].controller,
        P1,
        "P1's manifests enter under P1's control — not the caster's"
    );
    assert_eq!(runner.state().objects[&b2].controller, P1);
    assert_eq!(
        (
            runner.state().objects[&a1].power,
            runner.state().objects[&a1].toughness
        ),
        (Some(5), Some(4)),
        "the caster's colorless manifest gets Kozilek's +3/+2"
    );
    assert_eq!(
        (
            runner.state().objects[&b1].power,
            runner.state().objects[&b1].toughness
        ),
        (Some(2), Some(2)),
        "the opponent's manifest is NOT pumped — it is not a creature P0 controls"
    );

    // Kozilek itself resolved onto the battlefield.
    assert_eq!(
        runner.state().objects[&koz].zone,
        Zone::Battlefield,
        "Kozilek resolves normally after its cast trigger"
    );

    // DISCRIMINATOR — the rider drew one card per manifested card: 4 draws.
    // P0's hand: emptied by the cast (Kozilek) and both picks, then +4 draws.
    let p0 = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists");
    assert_eq!(
        p0.hand.len(),
        4,
        "P0 draws exactly one card per manifested card (4), got hand {:?}",
        p0.hand
    );
    let p1 = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1 exists");
    assert_eq!(
        p1.hand.len(),
        0,
        "P1's hand is emptied by their two picks and draws nothing"
    );
}
