//! Integration smoke tests for the Oracle text card loading pipeline.
//!
//! Validates that MTGJSON metadata loaded through `CardDatabase::from_mtgjson()`
//! works correctly, and that loaded cards function through the engine's `apply()`
//! pipeline for spell casting and combat.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityCost, AbilityKind, Effect, ManaContribution, ManaProduction, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card::CardLayout;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn load_test_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        CardDatabase::from_mtgjson(&data.join("mtgjson/test_fixture.json"))
            .expect("CardDatabase::from_mtgjson should succeed")
    })
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
            &*a.effect,
            Effect::Mana {
                produced: ManaProduction::Fixed { colors,
                    contribution: ManaContribution::Base,
                }, ..
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
            && matches!(&*a.effect, Effect::Attach { .. })
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

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forest_id = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let bolt_id = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);

    let mut runner = scenario.build();

    assert!(
        !runner.state().objects[&forest_id].abilities.is_empty(),
        "Forest game object should have a mana ability"
    );
    assert_eq!(runner.life(P1), 20, "P1 starts at 20 life");

    let bolt_card_id = runner.state().objects[&bolt_id].card_id;

    // Add red mana to P0's pool
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .add(ManaUnit::new(ManaType::Red, forest_id, false, vec![]));

    // Cast Lightning Bolt
    let result = runner
        .act(GameAction::CastSpell {
            object_id: bolt_id,
            card_id: bolt_card_id,
            targets: vec![],
        })
        .unwrap();

    assert!(
        matches!(result.waiting_for, WaitingFor::TargetSelection { .. }),
        "Casting spell with Any target should require target selection"
    );

    // Select player 1 as target
    let result = runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1)],
        })
        .unwrap();
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "After selecting targets, should return to priority"
    );
    assert_eq!(runner.state().stack.len(), 1, "Bolt should be on the stack");

    // Both players pass priority to resolve
    runner.pass_both_players();

    assert!(
        runner.state().stack.is_empty(),
        "Stack should be empty after resolution"
    );
    assert_eq!(
        runner.life(P1),
        17,
        "P1 should have 17 life after Lightning Bolt (20 - 3)"
    );
}

#[test]
fn test_smoke_game_combat_damage() {
    let db = load_test_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears_id = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);

    let mut runner = scenario.build();

    assert_eq!(runner.life(P1), 20);

    // Advance from PreCombatMain to DeclareAttackers
    runner.pass_both_players();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "Should be waiting for DeclareAttackers, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(bears_id, AttackTarget::Player(P1))],
        })
        .unwrap();

    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .unwrap();
    }

    // Pass priority through combat damage resolution
    for _ in 0..20 {
        if runner.life(P1) < 20 {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }

    assert_eq!(
        runner.life(P1),
        18,
        "P1 should have 18 life after Grizzly Bears combat damage (20 - 2)"
    );
}

// ---------------------------------------------------------------------------
// Bug 3 diagnostic: Omen cards (TDM layout="adventure") must prompt
// AdventureCastChoice when the back face is a castable Sorcery/Instant.
// ---------------------------------------------------------------------------

#[test]
fn sagu_wildling_loads_both_faces_from_export() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        eprintln!("skipping: client/public/card-data.json not generated");
        return;
    }
    let db = CardDatabase::from_export(&path).expect("export should load");
    let sagu = db
        .get_face_by_name("Sagu Wildling")
        .expect("Sagu Wildling must be exported");
    let roost = db
        .get_face_by_name("Roost Seek")
        .expect("Roost Seek must be exported");
    assert_eq!(
        sagu.scryfall_oracle_id, roost.scryfall_oracle_id,
        "omen faces must share oracle_id"
    );

    let layout_kind = sagu
        .scryfall_oracle_id
        .as_deref()
        .and_then(|id| db.get_layout_kind(id));
    assert!(
        layout_kind.is_some(),
        "layout_index must map Sagu Wildling's oracle_id to a LayoutKind"
    );
}

#[test]
fn sagu_wildling_cast_from_hand_prompts_adventure_choice() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        eprintln!("skipping: client/public/card-data.json not generated");
        return;
    }
    // Cache the parsed CardDatabase across test runs of this function — loading
    // the real client/public/card-data.json is expensive. `OnceLock` gives us
    // `&'static CardDatabase` without leaking a fresh allocation per invocation.
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    let db = DB.get_or_init(|| {
        CardDatabase::from_export(&path).expect("export should load")
    });

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sagu_id = scenario.add_real_card(P0, "Sagu Wildling", Zone::Hand, db);
    let mut runner = scenario.build();

    // Mirror production: engine-wasm calls rehydrate after deck load.
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Give P0 enough mana to cast the adventure face ({G}) — not the creature ({4}{G}).
    let dummy = engine::types::identifiers::ObjectId(0);
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .add(ManaUnit::new(ManaType::Green, dummy, false, vec![]));

    let obj = runner.state().objects.get(&sagu_id).expect("sagu present");
    assert!(
        obj.back_face.is_some(),
        "back_face must be populated by rehydrate; got None. \
         This is Bug 3 — runtime can't see the omen spell half."
    );
    let back = obj.back_face.as_ref().unwrap();
    assert_eq!(back.name, "Roost Seek", "back face should be Roost Seek");

    let card_id = obj.card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: sagu_id,
            card_id,
            targets: vec![],
        })
        .expect("cast should be accepted");
    assert!(
        matches!(result.waiting_for, WaitingFor::AdventureCastChoice { .. }),
        "Expected AdventureCastChoice when casting Omen card with only adventure-face mana, got {:?}",
        result.waiting_for
    );
}
