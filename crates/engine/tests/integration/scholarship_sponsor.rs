//! Scholarship Sponsor — ETB catch-up searches use the scoped-search protocol.
//!
//! The cast pipeline must retain the parser's `each player who controls fewer
//! lands` scope. That routes the two private searches through the shared
//! collection/delivery path: no selected land moves before the final player
//! chooses, then every selected basic land enters tapped under its searcher's
//! control and only those searchers shuffle.

use engine::database::synthesis::synthesize_planechase;
use engine::game::printed_cards::apply_card_face_to_object;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::SearchFoundDisposition;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const SCHOLARSHIP_SPONSOR_ORACLE: &str = "When this creature enters, each player who controls fewer lands than the player who controls the most lands searches their library for a number of basic land cards less than or equal to the difference, puts those cards onto the battlefield tapped, then shuffles.";
const ENIGMA_RIDGES_ORACLE: &str = "When you planeswalk to Enigma Ridges, each player who controls fewer lands than the player who controls the most lands searches their library for a number of basic land cards less than or equal to the difference, reveals them, puts them into their hand, then shuffles.\nWhenever chaos ensues, draw a card, then you may put a land card from your hand onto the battlefield.";

fn add_basic_land_to_library(state: &mut GameState, player: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        player,
        "Forest".to_string(),
        Zone::Library,
    );
    let object = state
        .objects
        .get_mut(&id)
        .expect("created library card must exist");
    object.card_types.core_types.push(CoreType::Land);
    object.card_types.supertypes.push(Supertype::Basic);
    object.base_card_types = object.card_types.clone();
    id
}

fn sponsor_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let sponsor = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Scholarship Sponsor",
            3,
            3,
            SCHOLARSHIP_SPONSOR_ORACLE,
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 3,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    (scenario, sponsor)
}

/// CR 101.4 + CR 603.3b + CR 701.23: Casting Scholarship Sponsor must deliver
/// the parsed ETB through the normal trigger stack and scoped search resolver.
/// Reverting its IR scope rewrite to `Preserve` leaves this cast path without
/// the two-player APNAP `SearchChoice` protocol this test asserts.
#[test]
fn scholarship_sponsor_cast_collects_three_player_searches_before_tapped_delivery() {
    let (mut scenario, sponsor) = sponsor_scenario();
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    for _ in 0..4 {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    for _ in 0..3 {
        scenario.add_basic_land(P2, ManaColor::White);
    }

    let mut runner = scenario.build();
    let p1_forest = add_basic_land_to_library(runner.state_mut(), P1);
    let p2_forest_a = add_basic_land_to_library(runner.state_mut(), P2);
    let p2_forest_b = add_basic_land_to_library(runner.state_mut(), P2);

    runner.cast(sponsor).resolve();
    let WaitingFor::SearchChoice {
        player,
        cards,
        count,
        up_to,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Scholarship Sponsor ETB must enter P1's scoped search, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1);
    assert_eq!(*count, 1, "P1 is one land behind the five-land leader");
    assert!(*up_to);
    assert!(cards.contains(&p1_forest));

    let p1_selection = runner
        .act(GameAction::SelectCards {
            cards: vec![p1_forest],
        })
        .expect("P1's matching basic land is a legal private search choice");
    assert!(
        !p1_selection.events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::ShuffledLibrary,
                ..
            }
        )),
        "the searched-this-way shuffle must wait for P2's APNAP choice"
    );
    for land in [p1_forest, p2_forest_a, p2_forest_b] {
        assert_eq!(
            runner.state().objects[&land].zone,
            Zone::Library,
            "no selected land moves before the final scoped player chooses"
        );
    }
    let pending = runner
        .state()
        .pending_scoped_library_search
        .as_ref()
        .expect("P1's selection must remain in the shared scoped-search frame");
    let engine::types::game_state::ScopedLibrarySearchPhase::CollectSelections {
        selections,
        frozen_dispositions,
        ..
    } = &pending.phase
    else {
        panic!("P1's selection must remain in the selection-collection phase");
    };
    let p1_incarnation = runner.state().objects[&p1_forest].incarnation;
    assert!(
        selections.iter().any(|(player, cards)| {
            *player == P1
                && cards.len() == 1
                && cards[0].object_id == p1_forest
                && cards[0].incarnation == p1_incarnation
        }),
        "P1's original SearchFound survivor must be retained for shared delivery"
    );
    assert!(
        frozen_dispositions.iter().any(|frozen| {
            frozen.searcher == P1
                && frozen.identity.object_id == p1_forest
                && frozen.identity.incarnation == p1_incarnation
                && frozen.disposition == SearchFoundDisposition::Original
        }),
        "P1's original survivor must be frozen before the next private choice"
    );

    let WaitingFor::SearchChoice {
        player,
        cards,
        count,
        up_to,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "P1's answer must advance to P2's private search, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P2);
    assert_eq!(*count, 2, "P2 is two lands behind the five-land leader");
    assert!(*up_to);
    assert!(cards.contains(&p2_forest_a) && cards.contains(&p2_forest_b));

    let zone_changes_before_delivery = runner.state().zone_changes_this_turn.len();
    let delivery = runner
        .act(GameAction::SelectCards {
            cards: vec![p2_forest_a, p2_forest_b],
        })
        .expect("P2's two matching basics are legal private search choices");
    let delivered_to_battlefield: Vec<_> = runner
        .state()
        .zone_changes_this_turn
        .iter()
        .skip(zone_changes_before_delivery)
        .filter_map(|change| match change {
            change
                if change.from_zone == Some(Zone::Library)
                    && change.to_zone == Zone::Battlefield
                    && [p1_forest, p2_forest_a, p2_forest_b].contains(&change.object_id) =>
            {
                Some(change.object_id)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        delivered_to_battlefield.len(),
        3,
        "the final scoped choice must issue one real library-to-battlefield delivery per selected land"
    );
    for (land, controller) in [(p1_forest, P1), (p2_forest_a, P2), (p2_forest_b, P2)] {
        let object = &runner.state().objects[&land];
        assert_eq!(object.zone, Zone::Battlefield);
        assert_eq!(object.controller, controller);
        assert!(
            object.tapped,
            "Scholarship Sponsor's found land enters tapped"
        );
    }
    let shufflers: Vec<_> = delivery
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        shufflers,
        vec![P1, P2],
        "only players who searched this way shuffle once after the shared delivery"
    );
}

#[test]
fn scholarship_sponsor_tied_land_counts_open_no_search() {
    let (mut scenario, sponsor) = sponsor_scenario();
    for player in [P0, P1, P2] {
        for _ in 0..4 {
            scenario.add_basic_land(player, ManaColor::Green);
        }
    }
    let mut runner = scenario.build();
    let libraries: Vec<_> = [P0, P1, P2]
        .into_iter()
        .map(|player| add_basic_land_to_library(runner.state_mut(), player))
        .collect();

    runner.cast(sponsor).resolve();
    assert_eq!(
        runner.state().objects[&sponsor].zone,
        Zone::Battlefield,
        "the sponsor must resolve so its ETB reaches the tied-land branch"
    );
    assert!(
        runner.state().pending_scoped_library_search.is_none(),
        "a tie must finish without parking a scoped-search frame"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::SearchChoice { .. }),
        "a three-way tie has no player with fewer lands and must not open a search"
    );
    assert!(libraries
        .iter()
        .all(|id| runner.state().objects[id].zone == Zone::Library));
}

/// Enigma Ridges uses the same scoped resolver as Scholarship Sponsor but has
/// the public reveal-to-hand delivery. Its parsed plane trigger must pass through
/// the real planeswalk event matcher and trigger stack before the search chain
/// resolves.
#[test]
fn enigma_ridges_reveals_only_after_the_final_scoped_search_choice() {
    let mut state = GameState::new(engine::types::format::FormatConfig::free_for_all(), 3, 42);
    let enigma = create_object(
        &mut state,
        CardId(900),
        P0,
        "Enigma Ridges".to_string(),
        Zone::Command,
    );
    {
        let plane = state
            .objects
            .get_mut(&enigma)
            .expect("Enigma Ridges source exists");
        plane.card_types.core_types.push(CoreType::Plane);
        plane.base_card_types = plane.card_types.clone();
    }
    for (player, count) in [(P0, 5), (P1, 4), (P2, 3)] {
        for _ in 0..count {
            let card_id = CardId(state.next_object_id);
            let land = create_object(
                &mut state,
                card_id,
                player,
                "Land".to_string(),
                Zone::Battlefield,
            );
            let object = state.objects.get_mut(&land).expect("land exists");
            object.card_types.core_types.push(CoreType::Land);
            object.base_card_types = object.card_types.clone();
        }
    }
    let p1_forest = add_basic_land_to_library(&mut state, P1);
    let p2_forest_a = add_basic_land_to_library(&mut state, P2);
    let p2_forest_b = add_basic_land_to_library(&mut state, P2);

    let parsed = parse_oracle_text(
        ENIGMA_RIDGES_ORACLE,
        "Enigma Ridges",
        &[],
        &["Plane".to_string()],
        &["Echoir".to_string()],
    );
    let mut face = CardFace {
        name: "Enigma Ridges".to_string(),
        card_type: state.objects[&enigma].card_types.clone(),
        triggers: parsed.triggers,
        ..Default::default()
    };
    synthesize_planechase(&mut face);
    apply_card_face_to_object(
        state
            .objects
            .get_mut(&enigma)
            .expect("Enigma Ridges source exists"),
        &face,
    );

    let planeswalked = [GameEvent::Planeswalked {
        player_id: P0,
        from: None,
        to: Some(enigma),
    }];
    process_triggers(&mut state, &planeswalked);
    assert!(
        state.stack.iter().any(|entry| entry.source_id == enigma),
        "the parsed planeswalk-to trigger must be placed on the stack"
    );
    let mut runner = GameRunner::from_state(state);

    for _ in 0..3 {
        if matches!(runner.state().waiting_for, WaitingFor::SearchChoice { .. }) {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "the parsed Enigma Ridges trigger must resolve from the normal stack, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass resolves the stacked Enigma Ridges trigger");
    }

    let WaitingFor::SearchChoice {
        player,
        cards,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the first Enigma Ridges search must belong to P1, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1);
    assert_eq!(*count, 1);
    assert!(cards.contains(&p1_forest));

    let first_selection = runner
        .act(GameAction::SelectCards {
            cards: vec![p1_forest],
        })
        .expect("P1 chooses the revealed basic land from their library");
    assert!(
        !first_selection
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::CardsRevealed { .. })),
        "P1's private choice must not reveal a card while P2 still chooses"
    );
    for land in [p1_forest, p2_forest_a, p2_forest_b] {
        assert_eq!(runner.state().objects[&land].zone, Zone::Library);
        assert!(
            !runner.state().revealed_cards.contains(&land),
            "the found card remains hidden until the shared delivery boundary"
        );
    }

    let WaitingFor::SearchChoice {
        player,
        cards,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "P1's answer must advance to P2's search, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P2);
    assert_eq!(*count, 2);
    assert!(cards.contains(&p2_forest_a) && cards.contains(&p2_forest_b));

    let final_selection = runner
        .act(GameAction::SelectCards {
            cards: vec![p2_forest_a, p2_forest_b],
        })
        .expect("P2 chooses both revealed basics from their library");
    let first_reveal = final_selection
        .events
        .iter()
        .position(|event| matches!(event, GameEvent::CardsRevealed { .. }))
        .expect("the shared hand delivery publishes the selected cards");
    let first_delivery = final_selection
        .events
        .iter()
        .position(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged { object_id, .. }
                    if *object_id == p1_forest || *object_id == p2_forest_a || *object_id == p2_forest_b
            )
        })
        .expect("the selected cards move from libraries to hands");
    assert!(
        first_reveal < first_delivery,
        "Enigma Ridges must reveal the selected cards before the shared library-to-hand delivery"
    );
    let revealed: Vec<_> = final_selection
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        revealed.len(),
        3,
        "the terminal public reveal must account for every selected land exactly once"
    );
    assert!(
        [p1_forest, p2_forest_a, p2_forest_b]
            .into_iter()
            .all(|land| revealed.contains(&land)),
        "the terminal public reveal must identify the same lands that are delivered to hand"
    );
    for land in [p1_forest, p2_forest_a, p2_forest_b] {
        assert_eq!(runner.state().objects[&land].zone, Zone::Hand);
    }
}
