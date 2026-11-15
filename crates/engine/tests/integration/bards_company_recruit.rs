//! Bard's Company — the generated card-data trigger must run through the public
//! cast/resolve pipeline and inspect the actual discarded card's hand-time type.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BARDS_COMPANY: &str = "Bard's Company";

fn resolve_recruit(discard_land: bool, db: &engine::database::card_db::CardDatabase) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );
    let bards_company = scenario.add_real_card(P0, BARDS_COMPANY, Zone::Hand, db);
    let discarded = if discard_land {
        scenario.add_land_to_hand(P0, "Recruit land").id()
    } else {
        scenario.add_spell_to_hand(P0, "Recruit nonland", true).id()
    };
    scenario.with_library_top(P0, &["Recruit draw"]);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.cast(bards_company).resolve();
    runner.resolve_top();
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::DiscardChoice { cards, .. } if cards.contains(&discarded)),
        "Bard's Company's ETB Recruit trigger must ask for a discard, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![discarded],
        })
        .expect("chosen Recruit discard resolves");
    runner
}

fn human_soldiers(runner: &GameRunner) -> Vec<&engine::game::game_object::GameObject> {
    runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token
                && object.zone == Zone::Battlefield
                && object.base_power == Some(1)
                && object.base_toughness == Some(1)
                && object.color == vec![ManaColor::White]
                && object.card_types.core_types.contains(&CoreType::Creature)
                && object.card_types.subtypes.contains(&"Human".to_string())
                && object.card_types.subtypes.contains(&"Soldier".to_string())
        })
        .collect()
}

#[test]
fn bards_company_recruit_creates_a_human_soldier_only_for_nonland_discards() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    // The shared loader uses the committed, generated subset so this remains
    // fast and visible in CI. Regenerate it from the real export after a
    // serialized Recruit parser change.
    if db.get_face_by_name(BARDS_COMPANY).is_none() {
        eprintln!(
            "skipping: Bard's Company is not in integration_cards.json.gz — add it once the authoritative card export includes it"
        );
        return;
    }

    let nonland = resolve_recruit(false, db);
    assert_eq!(
        human_soldiers(&nonland).len(),
        1,
        "discarding a nonland must create exactly one 1/1 white Human Soldier"
    );

    let land = resolve_recruit(true, db);
    assert!(
        human_soldiers(&land).is_empty(),
        "discarding a land must not create Recruit's contingent token"
    );
}
