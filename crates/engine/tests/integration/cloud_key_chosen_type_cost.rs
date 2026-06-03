//! Reproduction + regression for issue #930: Cloud Key chosen-type cost reduction.
//!
//! Cloud Key: "As this artifact enters, choose artifact, creature, enchantment,
//! instant, or sorcery. Spells you cast of the chosen type cost {1} less to cast."
//!
//! On `main`, two coupled parser bugs make Cloud Key reduce the cost of EVERY
//! spell you cast, regardless of (or without) a chosen type:
//!   - The static `ReduceCost` filter parses to `Typed { type_filters: [Card] }`
//!     with no `FilterProp::IsChosenCardType`, so it matches all spells.
//!   - The ETB "choose artifact, creature, ..." parses to a `Labeled` choice
//!     instead of `ChoiceType::CardType`, so no `ChosenAttribute::CardType` is
//!     ever stored for `IsChosenCardType` to read.
//!
//! CR 601.2f: cost reductions apply only to spells matching the effect's filter.
//! A spell that is not of the chosen type must NOT be reduced.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::ChoiceType;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn load_db() -> Option<&'static CardDatabase> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        return None;
    }
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    Some(DB.get_or_init(|| CardDatabase::from_export(&path).expect("export should load")))
}

/// With Cloud Key on the battlefield and NO card type chosen, a non-artifact
/// spell (Cultivate, {2}{G}) must not receive any cost reduction.
///
/// On `main` this FAILS: Cloud Key's `ReduceCost` filter parses to all-`Card`
/// (no `IsChosenCardType`), so `display_spell_cost` reduces the generic from
/// 2 to 1 for every spell the controller casts.
#[test]
fn cloud_key_does_not_reduce_non_chosen_type_spell() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Cloud Key on the battlefield => its cost-reduction static is active.
    scenario.add_real_card(P0, "Cloud Key", Zone::Battlefield, db);
    // A {2}{G} non-artifact sorcery in hand.
    let cultivate = scenario.add_real_card(P0, "Cultivate", Zone::Hand, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let cost = engine::game::casting::display_spell_cost(runner.state(), P0, cultivate)
        .expect("Cultivate should have a displayable cost");

    let ManaCost::Cost { generic, .. } = cost else {
        panic!("expected ManaCost::Cost, got {cost:?}");
    };

    // CR 601.2f: Cloud Key reduces only spells of the CHOSEN type. With nothing
    // chosen — and Cultivate being a non-artifact sorcery regardless — the
    // generic component must stay 2. A reduced value proves the all-`Card`
    // filter / missing `IsChosenCardType` bug (issue #930).
    assert_eq!(
        generic, 2,
        "Cloud Key must NOT reduce a non-chosen-type spell (issue #930); \
         got generic={generic}, expected 2"
    );
}

/// End-to-end: casting Cloud Key surfaces a `CardType` ETB choice (issue #930
/// Bug B — the enumerated "choose artifact, creature, enchantment, instant, or
/// sorcery" must not fall to a `Labeled` choice). After choosing Artifact, an
/// artifact spell is reduced by {1} while a non-artifact spell is not (Bug A).
#[test]
fn cloud_key_reduces_only_the_chosen_card_type_after_etb_choice() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cloud_key = scenario.add_real_card(P0, "Cloud Key", Zone::Hand, db);
    let mind_stone = scenario.add_real_card(P0, "Mind Stone", Zone::Hand, db); // {2} artifact
    let cultivate = scenario.add_real_card(P0, "Cultivate", Zone::Hand, db); // {2}{G} sorcery

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // {3} generic to cast Cloud Key.
    {
        let pool = &mut runner.state_mut().players[0].mana_pool;
        for _ in 0..3 {
            pool.add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }
    }

    // Pool-funded cast through the canonical pipeline. Cloud Key's ETB choice
    // is a prompt the resolution driver does not auto-answer, so it halts there
    // and surfaces it via `final_waiting_for()` for the caller to inspect/drive.
    let outcome = runner.cast(cloud_key).resolve();

    // Bug B: the ETB choice must surface as a CardType choice keyed to Cloud Key.
    match outcome.final_waiting_for() {
        WaitingFor::NamedChoice {
            choice_type,
            source_id,
            ..
        } => {
            assert!(
                matches!(choice_type, ChoiceType::CardType),
                "Cloud Key ETB must be a CardType choice, got {choice_type:?}"
            );
            assert_eq!(*source_id, Some(cloud_key));
        }
        other => panic!("expected NamedChoice after Cloud Key ETB, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseOption {
            choice: "Artifact".to_string(),
        })
        .expect("ChooseOption(Artifact) must resolve");
    assert_eq!(
        runner.state().objects[&cloud_key].chosen_card_type(),
        Some(CoreType::Artifact),
        "the chosen card type must persist on Cloud Key"
    );

    // Bug A: the artifact spell (chosen type) is reduced {2} -> {1}; the
    // non-artifact spell ({2}{G} Cultivate) is unchanged.
    let artifact_cost = engine::game::casting::display_spell_cost(runner.state(), P0, mind_stone)
        .expect("Mind Stone should have a displayable cost");
    let ManaCost::Cost {
        generic: art_generic,
        ..
    } = artifact_cost
    else {
        panic!("expected ManaCost::Cost, got {artifact_cost:?}");
    };
    assert_eq!(
        art_generic, 1,
        "an artifact spell of the chosen type must be reduced from {{2}} to {{1}}"
    );

    let other_cost = engine::game::casting::display_spell_cost(runner.state(), P0, cultivate)
        .expect("Cultivate should have a displayable cost");
    let ManaCost::Cost {
        generic: other_generic,
        ..
    } = other_cost
    else {
        panic!("expected ManaCost::Cost, got {other_cost:?}");
    };
    assert_eq!(
        other_generic, 2,
        "a non-chosen-type spell must be unchanged"
    );
}
