//! Natural Balance — exact-keeper, sequential search, and scoped final shuffle.
//!
//! The test drives the exact verified Oracle text through the normal card parser
//! and `GameScenario` cast pipeline. It covers the protocol-specific boundaries:
//! the player chooses exactly five lands before sacrificing the rest, two
//! post-sacrifice eligible players receive private local-X searches, no found
//! card moves until both have chosen, selected basic lands enter tapped, and the
//! final "searched this way" shuffle resolves exactly once per searcher.

use engine::game::filter_state_for_viewer;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, ReplacementDefinition, ReplacementMode,
    TargetFilter, TriggerDefinition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

/// Scryfall Oracle text, checked 2026-07-14.
const NATURAL_BALANCE_ORACLE: &str = "Each player who controls six or more lands chooses five lands they control and sacrifices the rest. Each player who controls four or fewer lands may search their library for up to X basic land cards and put them onto the battlefield, where X is five minus the number of lands they control. Then each player who searched their library this way shuffles.";
const P2: PlayerId = PlayerId(2);

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

fn land_count(state: &GameState, player: PlayerId, zone: Zone) -> usize {
    state
        .objects
        .values()
        .filter(|object| {
            object.controller == player
                && object.zone == zone
                && object.card_types.core_types.contains(&CoreType::Land)
        })
        .count()
}

/// A deliberately narrow observable trigger: it fires only for land sacrifices,
/// so a generic `ZoneChanged` fallback cannot make this regression pass.
fn land_sacrifice_life_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Sacrificed)
        .valid_card(TargetFilter::Typed(TypedFilter::land()))
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ))
        .trigger_zones(vec![Zone::Battlefield])
}

/// CR 701.21a + CR 614.1 + CR 603.2: When Natural Balance's queued
/// sacrifice pauses on an inner `Moved` replacement, accepting that real
/// replacement choice must resume the sacrifice event exactly once. The
/// watcher is deliberately `Sacrificed`-mode (rather than a leaves-the-
/// battlefield trigger), so its life gain distinguishes preserved sacrifice
/// provenance from merely moving the land to a graveyard.
#[test]
fn natural_balance_moved_replacement_resumes_one_sacrifice_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut spell = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Natural Balance",
        false,
        NATURAL_BALANCE_ORACLE,
    );
    spell.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Green, ManaCostShard::Green],
        generic: 2,
    });
    let natural_balance = spell.id();

    scenario
        .add_creature(P0, "Sacrifice watcher", 1, 1)
        .with_trigger_definition(land_sacrifice_life_trigger());
    let pause_source = scenario
        .add_creature(P0, "Moved pause source", 0, 0)
        .as_enchantment()
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .destination_zone(Zone::Graveyard)
                .mode(ReplacementMode::Optional { decline: None })
                .description("Pause this Natural Balance sacrifice".to_string()),
        )
        .id();
    let lands: Vec<ObjectId> = (0..6)
        .map(|_| scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green))
        .collect();
    // P1 remains above Natural Balance's search threshold after P0 sacrifices,
    // keeping the regression focused on the replacement and trigger path.
    for _ in 0..5 {
        scenario.add_basic_land(P1, engine::types::mana::ManaColor::Blue);
    }
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let sacrificed = lands[5];
    let mut runner = scenario.build();
    let pause_source = runner
        .state_mut()
        .objects
        .get_mut(&pause_source)
        .expect("the replacement source must be on the battlefield");
    pause_source.replacement_definitions[0].valid_card =
        Some(TargetFilter::SpecificObject { id: sacrificed });
    std::sync::Arc::make_mut(&mut pause_source.base_replacement_definitions)[0].valid_card =
        Some(TargetFilter::SpecificObject { id: sacrificed });

    let outcome = runner.cast(natural_balance).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::KeepExactPermanentsChoice {
            player: P0,
            required_count: 5,
            ..
        }
    ));
    drop(outcome);

    runner
        .act(GameAction::ChooseKeptPermanents {
            kept: lands[..5].to_vec(),
        })
        .expect("Natural Balance's exact five-land keeper choice must be legal");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));

    let replacement_resume = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the real Moved replacement must resume the queued sacrifice");
    assert_eq!(
        replacement_resume
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PermanentSacrificed {
                    object_id,
                    player_id: P0,
                } if *object_id == sacrificed
            ))
            .count(),
        1,
        "the resumed inner zone change must publish exactly one sacrifice event"
    );
    assert_eq!(runner.state().objects[&sacrificed].zone, Zone::Graveyard);

    let life_before = runner.life(P0);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "the Sacrificed-only watcher must fire once after the Moved replacement resumes"
    );
}

/// CR 101.4 + CR 701.21a + CR 701.23i + CR 701.24a: Natural Balance must keep
/// exactly five lands, collect both eligible players' private local-X searches
/// before delivering any found land, and shuffle each accepted searcher exactly
/// once after the shared delivery batch.
#[test]
fn natural_balance_collects_two_local_x_searches_before_one_shuffle_each() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let mut spell = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Natural Balance",
        false,
        NATURAL_BALANCE_ORACLE,
    );
    spell.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Green, ManaCostShard::Green],
        generic: 2,
    });
    let natural_balance = spell.id();

    // The replacement is deliberately restricted to P1's found Forest after
    // the scenario is built. It pauses the real Moved pipeline without
    // modifying the entry, so the test can verify that Natural Balance keeps
    // its entire selected-land batch and its searched-this-way shuffle tail
    // parked across the replacement choice.
    let entry_pause_source = scenario
        .add_creature(P0, "Entry pause source", 0, 0)
        .as_enchantment()
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .destination_zone(Zone::Battlefield)
                .mode(ReplacementMode::Optional { decline: None })
                .description("Pause this selected land's entry".to_string()),
        )
        .id();

    let kept: Vec<ObjectId> = (0..6)
        .map(|_| scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green))
        .collect();
    for _ in 0..4 {
        scenario.add_basic_land(P1, engine::types::mana::ManaColor::Blue);
    }
    for _ in 0..3 {
        scenario.add_basic_land(P2, engine::types::mana::ManaColor::White);
    }
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let p1_forest = add_basic_land_to_library(runner.state_mut(), P1);
    let p2_forest_a = add_basic_land_to_library(runner.state_mut(), P2);
    let p2_forest_b = add_basic_land_to_library(runner.state_mut(), P2);
    let entry_pause = runner
        .state_mut()
        .objects
        .get_mut(&entry_pause_source)
        .expect("the replacement source must be on the battlefield");
    entry_pause.replacement_definitions[0].valid_card =
        Some(TargetFilter::SpecificObject { id: p1_forest });
    std::sync::Arc::make_mut(&mut entry_pause.base_replacement_definitions)[0].valid_card =
        Some(TargetFilter::SpecificObject { id: p1_forest });

    let outcome = runner.cast(natural_balance).resolve();
    let WaitingFor::KeepExactPermanentsChoice {
        player,
        required_count,
        eligible,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "Natural Balance must pause for the six-land player's exact keeper choice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*required_count, 5);
    assert_eq!(eligible.len(), 6);
    drop(outcome);

    runner
        .act(GameAction::ChooseKeptPermanents {
            kept: kept[..5].to_vec(),
        })
        .expect("five distinct controlled lands must be a legal exact keeper choice");
    assert_eq!(land_count(runner.state(), P0, Zone::Battlefield), 5);
    assert_eq!(land_count(runner.state(), P0, Zone::Graveyard), 1);

    let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for else {
        panic!(
            "the first four-or-fewer-land player should receive Natural Balance's optional search, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P1 may accept the scoped library search");

    let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for else {
        panic!(
            "every optional answer must be collected before any hidden library is exposed, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P2);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P2 may accept the scoped library search");

    let WaitingFor::SearchChoice {
        player,
        cards,
        count,
        up_to,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "accepting the optional search must reveal P1's private search choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1);
    assert_eq!(*count, 1, "X must be five minus P1's four lands");
    assert!(*up_to);
    assert!(cards.contains(&p1_forest));

    let p1_selection = runner
        .act(GameAction::SelectCards {
            cards: vec![p1_forest],
        })
        .expect("the selected P1 basic land must be a legal search result");
    assert!(
        !p1_selection.events.iter().any(|event| matches!(
            event,
            engine::types::events::GameEvent::PlayerPerformedAction {
                action: engine::types::events::PlayerActionKind::ShuffledLibrary,
                ..
            }
        )),
        "the shared shuffle tail must not run until every eligible player has selected"
    );
    assert_eq!(
        runner.state().objects[&p1_forest].zone,
        Zone::Library,
        "P1's found land must remain in its library until P2 has made the later APNAP choice"
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
            "P1's completed selection must advance to P2's prepared private choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P2);
    assert_eq!(*count, 2, "X must be five minus P2's three lands");
    assert!(*up_to);
    assert!(cards.contains(&p2_forest_a) && cards.contains(&p2_forest_b));

    // P1's submitted library id must remain private while P2 sees its current
    // candidates; P0 sees neither player's private library objects.
    let p2_view = filter_state_for_viewer(runner.state(), P2);
    let p2_pending = p2_view
        .pending_scoped_library_search
        .as_ref()
        .expect("scoped search remains pending until P2 selects");
    let engine::types::game_state::ScopedLibrarySearchPhase::CollectSelections {
        selections: p2_selections,
        ..
    } = &p2_pending.phase
    else {
        panic!("expected CollectSelections")
    };
    assert_eq!(p2_selections[0].1[0].object_id, ObjectId(0));
    assert!(matches!(
        p2_view.waiting_for,
        WaitingFor::SearchChoice { cards, .. }
            if cards.contains(&p2_forest_a) && cards.contains(&p2_forest_b)
    ));
    let p0_view = filter_state_for_viewer(runner.state(), P0);
    let p0_pending = p0_view
        .pending_scoped_library_search
        .as_ref()
        .expect("non-searcher keeps the public pending-state shape");
    let engine::types::game_state::ScopedLibrarySearchPhase::CollectSelections {
        selections: p0_selections,
        ..
    } = &p0_pending.phase
    else {
        panic!("expected CollectSelections")
    };
    assert_eq!(p0_selections[0].1[0].object_id, ObjectId(0));
    assert!(matches!(
        p0_view.waiting_for,
        WaitingFor::SearchChoice { cards, .. } if cards == vec![ObjectId(0), ObjectId(0)]
    ));

    let p2_selection = runner
        .act(GameAction::SelectCards {
            cards: vec![p2_forest_a, p2_forest_b],
        })
        .expect("the selected P2 basic lands must be legal local-X search results");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player, .. } if player == P1
    ));
    assert!(
        !p2_selection.events.iter().any(|event| matches!(
            event,
            engine::types::events::GameEvent::PlayerPerformedAction {
                action: engine::types::events::PlayerActionKind::ShuffledLibrary,
                ..
            }
        )),
        "the shuffle tail must remain parked while a selected land's Moved replacement is pending"
    );
    for found_land in [p1_forest, p2_forest_a, p2_forest_b] {
        assert_eq!(
            runner.state().objects[&found_land].zone,
            Zone::Library,
            "a pause during one delivery must keep the whole selected batch pending"
        );
    }

    let delivery_completion = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the selected land's replacement must resume the entire delivery batch");

    assert_eq!(
        runner.state().objects[&p1_forest].zone,
        Zone::Battlefield,
        "the selected basic land must enter from P1's library"
    );
    assert!(
        runner.state().objects[&p1_forest].tapped,
        "Natural Balance's searched land enters tapped"
    );
    assert_eq!(land_count(runner.state(), P1, Zone::Battlefield), 5);
    for p2_forest in [p2_forest_a, p2_forest_b] {
        assert_eq!(
            runner.state().objects[&p2_forest].zone,
            Zone::Battlefield,
            "every P2 found basic land must enter after the shared delivery"
        );
        assert!(
            runner.state().objects[&p2_forest].tapped,
            "Natural Balance's searched lands enter tapped"
        );
    }
    assert_eq!(land_count(runner.state(), P2, Zone::Battlefield), 5);

    let shuffles: Vec<PlayerId> = delivery_completion
        .events
        .iter()
        .filter_map(|event| match event {
            engine::types::events::GameEvent::PlayerPerformedAction {
                player_id,
                action: engine::types::events::PlayerActionKind::ShuffledLibrary,
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        shuffles,
        vec![P1, P2],
        "each accepted searcher must shuffle exactly once; a duplicate completion would add another shuffle"
    );
}
