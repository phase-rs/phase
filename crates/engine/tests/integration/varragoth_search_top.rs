//! Varragoth, Bloodsky Sire's target-player Boast must keep the found card out
//! of the shuffled subset and put it directly on top of that player's library.
//!
//! This drives the production activation, targeting, search-choice, shuffle,
//! and positional-placement pipeline. The event assertion is the discriminator:
//! reverting the parser suppression restores a transient Library -> Hand move.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::AbilityTag;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const VARRAGOTH_ORACLE: &str = "Deathtouch\nBoast — {1}{B}: Target player searches their library for a card, then shuffles and puts that card on top. (Activate only if this creature attacked this turn and only once each turn.)";
const THIRD_PERSON_ORDINAL_ORACLE: &str = "Boast — {1}{B}: Target player searches their library for a card, then shuffles and puts that card third from the top. (Activate only if this creature attacked this turn and only once each turn.)";

fn black_pool(count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Black, ObjectId(9_999), false, vec![],); count]
}

fn boast_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.ability_tag == Some(AbilityTag::Boast))
        .expect("the source must carry a Boast-tagged activated ability")
}

fn assert_no_library_to_hand_event(events: &[GameEvent], chosen: ObjectId) {
    assert!(
        !events.iter().any(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Library),
                    to: Zone::Hand,
                    ..
                } if *object_id == chosen
            )
        }),
        "the selected card must never transition from the searched library to hand"
    );
}

#[test]
fn varragoth_target_player_search_never_moves_found_card_through_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let varragoth = scenario
        .add_creature(P0, "Varragoth, Bloodsky Sire", 2, 3)
        .from_oracle_text_with_keywords(&["Deathtouch"], VARRAGOTH_ORACLE)
        .id();
    scenario.with_mana_pool(P0, black_pool(2));
    let controller_card = scenario.add_card_to_library_top(P0, "Controller's Library Card");
    let decoy = scenario.add_card_to_library_top(P1, "Target Player Decoy");
    let chosen = scenario.add_card_to_library_top(P1, "Target Player Choice");

    let mut runner = scenario.build();
    // CR 702.142a: Boast is available because this source attacked this turn.
    runner
        .state_mut()
        .creatures_attacked_this_turn
        .insert(varragoth);
    let ability_index = boast_index(&runner, varragoth);

    let outcome = runner
        .activate(varragoth, ability_index)
        .target_player(P1)
        .resolve();

    match outcome.final_waiting_for() {
        WaitingFor::SearchChoice { player, cards, .. } => {
            assert_eq!(
                *player, P1,
                "the targeted player must make Varragoth's search choice"
            );
            assert!(
                cards.contains(&chosen),
                "the chosen P1 card must reach the production SearchChoice"
            );
            assert!(
                cards.contains(&decoy),
                "the other P1 library card must also be a legal search choice"
            );
            assert!(
                !cards.contains(&controller_card),
                "the source controller's library card must not be a legal target-player search choice"
            );
        }
        other => panic!("expected P1 SearchChoice after Varragoth resolves, got {other:?}"),
    }

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("P1 selecting a library card must resume Varragoth's continuation");

    // CR 701.24b + CR 608.2c: the found card stays outside the shuffled subset
    // before the later instruction puts it at the specified library position.
    // Reverting the parser fix injects exactly this forbidden transition.
    assert_no_library_to_hand_event(&result.events, chosen);
    assert!(
        !matches!(result.waiting_for, WaitingFor::EffectZoneChoice { .. }),
        "the positional continuation must not open a separate zone-choice prompt"
    );
    assert_eq!(runner.state().objects[&chosen].zone, Zone::Library);
    assert_eq!(
        runner.state().players[P1.0 as usize].library[0],
        chosen,
        "the selected card must finish on top of P1's library"
    );
    assert!(
        !runner.state().players[P0.0 as usize]
            .library
            .contains(&chosen),
        "the target-player card must not be rebound to the source controller's library"
    );
    assert!(
        runner.state().players[P1.0 as usize]
            .library
            .contains(&decoy),
        "the shuffled remainder must stay in P1's library"
    );
}

#[test]
fn third_person_ordinal_search_never_moves_found_card_through_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let source = scenario
        .add_creature(P0, "Ordinal Searcher", 2, 3)
        .from_oracle_text_with_keywords(&[], THIRD_PERSON_ORDINAL_ORACLE)
        .id();
    scenario.with_mana_pool(P0, black_pool(2));
    let controller_card = scenario.add_card_to_library_top(P0, "Controller's Library Card");
    let first_decoy = scenario.add_card_to_library_top(P1, "First Target Player Decoy");
    let second_decoy = scenario.add_card_to_library_top(P1, "Second Target Player Decoy");
    let chosen = scenario.add_card_to_library_top(P1, "Target Player Choice");

    let mut runner = scenario.build();
    // CR 702.142a: Boast is available because this source attacked this turn.
    runner
        .state_mut()
        .creatures_attacked_this_turn
        .insert(source);
    let ability_index = boast_index(&runner, source);

    let outcome = runner
        .activate(source, ability_index)
        .target_player(P1)
        .resolve();

    match outcome.final_waiting_for() {
        WaitingFor::SearchChoice { player, cards, .. } => {
            assert_eq!(*player, P1);
            assert!(cards.contains(&chosen));
            assert!(cards.contains(&first_decoy));
            assert!(cards.contains(&second_decoy));
            assert!(!cards.contains(&controller_card));
        }
        other => panic!("expected P1 SearchChoice for ordinal search, got {other:?}"),
    }

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("P1 selecting a library card must resume the ordinal continuation");

    // CR 701.24b + CR 608.2c: third-person verb agreement does not change the
    // found-card exclusion or the later positional instruction.
    assert_no_library_to_hand_event(&result.events, chosen);
    assert_eq!(runner.state().objects[&chosen].zone, Zone::Library);
    assert_eq!(
        runner.state().players[P1.0 as usize].library[2],
        chosen,
        "the selected card must finish third from the top of P1's library"
    );
}
