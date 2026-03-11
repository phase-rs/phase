//! Integration smoke tests for the Oracle text card loading pipeline.
//!
//! Validates that MTGJSON metadata loaded through `CardDatabase::from_mtgjson()`
//! works correctly, and that loaded cards function through the engine's `apply()`
//! pipeline for spell casting and combat.

use std::path::Path;

use engine::database::card_db::CardDatabase;
use engine::game::apply;
use engine::game::deck_loading::create_object_from_card_face;
use engine::types::ability::{AbilityCost, AbilityKind, Effect, ManaProduction, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card::CardLayout;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn data_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data")
}

fn load_test_db() -> CardDatabase {
    let data = data_dir();
    CardDatabase::from_mtgjson(&data.join("mtgjson/test_fixture.json"))
        .expect("CardDatabase::from_mtgjson should succeed")
}

// ---------------------------------------------------------------------------
// Card loading tests
// ---------------------------------------------------------------------------

#[test]
fn test_load_all_smoke_test_cards() {
    let db = load_test_db();
    assert!(
        db.card_count() >= 8,
        "Expected at least 8 cards, got {}",
        db.card_count()
    );
}

#[test]
fn test_forest_has_synthesized_mana_ability() {
    let db = load_test_db();
    let forest = db
        .get_face_by_name("Forest")
        .expect("Forest should be loaded");
    let has_mana_ability = forest.abilities.iter().any(|a| {
        matches!(
            &a.effect,
            Effect::Mana {
                produced: ManaProduction::Fixed { colors }
            } if *colors == vec![ManaColor::Green]
        ) && a.cost == Some(AbilityCost::Tap)
    });
    assert!(
        has_mana_ability,
        "Forest should have a synthesized {{T}}: Add {{G}} mana ability"
    );
}

#[test]
fn test_bonesplitter_has_synthesized_equip_ability() {
    let db = load_test_db();
    let bonesplitter = db
        .get_face_by_name("Bonesplitter")
        .expect("Bonesplitter should be loaded");
    let has_equip = bonesplitter.abilities.iter().any(|a| {
        a.kind == AbilityKind::Activated
            && matches!(&a.effect, Effect::Attach { .. })
            && matches!(&a.cost, Some(AbilityCost::Mana { cost }) if *cost == ManaCost::Cost { generic: 1, shards: vec![] })
    });
    assert!(
        has_equip,
        "Bonesplitter should have a synthesized Equip {{1}} activated ability"
    );
}

#[test]
fn test_delver_transform_layout() {
    let db = load_test_db();
    let delver = db
        .get_by_name("Delver of Secrets")
        .expect("Delver of Secrets should be loaded");
    match &delver.layout {
        CardLayout::Transform(face_a, face_b) => {
            assert_eq!(face_a.name, "Delver of Secrets");
            assert_eq!(face_b.name, "Insectile Aberration");
        }
        other => panic!(
            "Expected Transform layout for Delver of Secrets, got {:?}",
            std::mem::discriminant(other)
        ),
    }
}

#[test]
fn test_giant_killer_adventure_layout() {
    let db = load_test_db();
    if let Some(gk) = db.get_by_name("Giant Killer") {
        match &gk.layout {
            CardLayout::Adventure(face_a, face_b) => {
                assert_eq!(face_a.name, "Giant Killer");
                assert_eq!(face_b.name, "Chop Down");
            }
            other => panic!(
                "Expected Adventure layout for Giant Killer, got {:?}",
                std::mem::discriminant(other)
            ),
        }
    }
}

#[test]
fn test_scryfall_oracle_id_populated() {
    let db = load_test_db();
    let bolt = db
        .get_face_by_name("Lightning Bolt")
        .expect("Lightning Bolt should be loaded");
    assert!(
        bolt.scryfall_oracle_id.is_some(),
        "Lightning Bolt should have scryfall_oracle_id populated"
    );
}

// ---------------------------------------------------------------------------
// Smoke game tests — prove loaded cards work through apply()
// ---------------------------------------------------------------------------

#[test]
fn test_smoke_game_cast_spell() {
    let db = load_test_db();

    // Get card faces from loaded database
    let forest_face = db
        .get_face_by_name("Forest")
        .expect("Forest should be loaded");
    let bolt_face = db
        .get_face_by_name("Lightning Bolt")
        .expect("Lightning Bolt should be loaded");

    // Set up game state at main phase
    let mut state = engine::game::new_game(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // Add Forest to P0's battlefield
    let forest_id = create_object_from_card_face(&mut state, forest_face, PlayerId(0));
    {
        let obj = state.objects.get_mut(&forest_id).unwrap();
        obj.zone = Zone::Battlefield;
        obj.entered_battlefield_turn = Some(1);
    }
    state.battlefield.push(forest_id);
    state.players[0].library.retain(|id| *id != forest_id);

    assert!(
        !state.objects[&forest_id].abilities.is_empty(),
        "Forest game object should have a mana ability"
    );

    // Add Lightning Bolt to P0's hand
    let bolt_id = create_object_from_card_face(&mut state, bolt_face, PlayerId(0));
    let bolt_card_id = state.objects[&bolt_id].card_id;
    {
        let obj = state.objects.get_mut(&bolt_id).unwrap();
        obj.zone = Zone::Hand;
    }
    state.players[0].hand.push(bolt_id);
    state.players[0].library.retain(|id| *id != bolt_id);

    assert_eq!(state.players[1].life, 20, "P1 starts at 20 life");

    // Add red mana to P0's pool
    state
        .players
        .iter_mut()
        .find(|p| p.id == PlayerId(0))
        .unwrap()
        .mana_pool
        .add(ManaUnit {
            color: ManaType::Red,
            source_id: forest_id,
            snow: false,
            restrictions: vec![],
        });

    // Cast Lightning Bolt
    let result = apply(
        &mut state,
        GameAction::CastSpell {
            card_id: bolt_card_id,
            targets: vec![],
        },
    )
    .unwrap();

    assert!(
        matches!(result.waiting_for, WaitingFor::TargetSelection { .. }),
        "Casting spell with Any target should require target selection"
    );

    // Select player 1 as target
    let result = apply(
        &mut state,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Player(PlayerId(1))],
        },
    )
    .unwrap();
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "After selecting targets, should return to priority"
    );
    assert_eq!(state.stack.len(), 1, "Bolt should be on the stack");

    // Both players pass priority to resolve
    apply(&mut state, GameAction::PassPriority).unwrap();
    apply(&mut state, GameAction::PassPriority).unwrap();

    assert!(
        state.stack.is_empty(),
        "Stack should be empty after resolution"
    );
    assert_eq!(
        state.players[1].life, 17,
        "P1 should have 17 life after Lightning Bolt (20 - 3)"
    );
}

#[test]
fn test_smoke_game_combat_damage() {
    let db = load_test_db();

    let bears_face = db
        .get_face_by_name("Grizzly Bears")
        .expect("Grizzly Bears should be loaded");

    let mut state = engine::game::new_game(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    let bears_id = create_object_from_card_face(&mut state, bears_face, PlayerId(0));
    {
        let obj = state.objects.get_mut(&bears_id).unwrap();
        obj.zone = Zone::Battlefield;
        obj.entered_battlefield_turn = Some(1);
    }
    state.battlefield.push(bears_id);
    state.players[0].library.retain(|id| *id != bears_id);

    assert_eq!(state.players[1].life, 20);

    // Advance to combat
    apply(&mut state, GameAction::PassPriority).unwrap();
    apply(&mut state, GameAction::PassPriority).unwrap();

    for _ in 0..10 {
        if matches!(state.waiting_for, WaitingFor::DeclareAttackers { .. }) {
            break;
        }
        let _ = apply(&mut state, GameAction::PassPriority);
    }

    assert!(
        matches!(state.waiting_for, WaitingFor::DeclareAttackers { .. }),
        "Should be waiting for DeclareAttackers, got {:?}",
        state.waiting_for
    );

    apply(
        &mut state,
        GameAction::DeclareAttackers {
            attacks: vec![(
                bears_id,
                engine::game::combat::AttackTarget::Player(engine::types::player::PlayerId(1)),
            )],
        },
    )
    .unwrap();

    for _ in 0..10 {
        if matches!(state.waiting_for, WaitingFor::DeclareBlockers { .. }) {
            break;
        }
        let _ = apply(&mut state, GameAction::PassPriority);
    }

    if matches!(state.waiting_for, WaitingFor::DeclareBlockers { .. }) {
        apply(
            &mut state,
            GameAction::DeclareBlockers {
                assignments: vec![],
            },
        )
        .unwrap();
    }

    for _ in 0..20 {
        if state.players[1].life < 20 {
            break;
        }
        let _ = apply(&mut state, GameAction::PassPriority);
    }

    assert_eq!(
        state.players[1].life, 18,
        "P1 should have 18 life after Grizzly Bears combat damage (20 - 2)"
    );
}
