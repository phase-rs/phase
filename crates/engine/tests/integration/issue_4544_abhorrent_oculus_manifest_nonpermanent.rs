//! Issue #4544 — Abhorrent Oculus manifest dread must manifest a chosen
//! *non-permanent* (instant/sorcery) card face down as a 2/2 creature, not drop
//! it. A manifested card is a colorless 2/2 creature with no name, mana cost, or
//! other characteristics regardless of the card's hidden types (CR 701.62a,
//! CR 708.2a), so an instant/sorcery must manifest exactly like any other card.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ABHORRENT_OCULUS_ORACLE: &str =
    "Flying\nAt the beginning of each opponent's upkeep, manifest dread.";

fn instant_card_types() -> CardType {
    CardType {
        supertypes: vec![],
        core_types: vec![CoreType::Instant],
        subtypes: vec![],
    }
}

fn advance_to_manifest_dread_choice(runner: &mut GameRunner) {
    for _ in 0..240 {
        match &runner.state().waiting_for {
            WaitingFor::ManifestDreadChoice { .. } => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
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
            _ => return,
        }
    }
}

#[test]
fn abhorrent_oculus_manifests_a_nonpermanent_card_face_down() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D", "Lib E"]);
    }
    scenario
        .add_creature_from_oracle(P0, "Abhorrent Oculus", 5, 5, ABHORRENT_OCULUS_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        });
    scenario.add_card_to_library_top(P0, "Library Top");
    scenario.add_card_to_library_top(P0, "Lightning Bolt");

    let mut runner = scenario.build();
    let lib = runner.state().players[0].library.clone();
    let [top, second] = [lib[0], lib[1]];

    // Make the card the controller will manifest a non-permanent. Manifest must
    // still place it face down as a 2/2 — a non-permanent must not disappear.
    {
        let obj = runner.state_mut().objects.get_mut(&top).unwrap();
        obj.card_types = instant_card_types();
        obj.base_card_types = obj.card_types.clone();
    }

    advance_to_manifest_dread_choice(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManifestDreadChoice { .. }
        ),
        "opponent-upkeep manifest dread must pause for choice, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::SelectCards { cards: vec![top] })
        .expect("choose the non-permanent card to manifest");
    runner.advance_until_stack_empty();

    let obj = runner.state().objects.get(&top).expect("manifested object");
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "a non-permanent must manifest onto the battlefield, not disappear"
    );
    assert!(obj.face_down, "manifested card must be face down");
    assert_eq!(
        obj.power,
        Some(2),
        "a manifested card is a 2/2 (CR 701.62a / 708.2a)"
    );
    assert_eq!(
        obj.toughness,
        Some(2),
        "a manifested card is a 2/2 (CR 701.62a / 708.2a)"
    );
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "manifested card must present as a creature while face down regardless of its hidden type"
    );

    assert_eq!(
        runner.state().objects[&second].zone,
        Zone::Graveyard,
        "the other looked-at card must go to the graveyard"
    );
}
