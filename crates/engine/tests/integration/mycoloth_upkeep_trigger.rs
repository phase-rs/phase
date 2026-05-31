//! Runtime regression for issue #1284 — Mycoloth's beginning-of-upkeep trigger
//! does not create Saproling tokens.
//!
//! Mycoloth: "Devour 2. At the beginning of your upkeep, create a 1/1 green
//! Saproling creature token for each +1/+1 counter on this creature."
//!
//! Root cause hypothesis: the parse is correct (CountersOn(Source, P1P1)), but
//! either (a) the trigger does not fire at upkeep, or (b) the counter quantity
//! resolves to 0 at effect time despite counters being present.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
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

/// Issue #1284: Mycoloth enters with 3 +1/+1 counters; at the beginning of its
/// controller's upkeep it should create 3 Saproling tokens.
#[test]
fn mycoloth_upkeep_creates_saproling_tokens_equal_to_counters() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    let mycoloth = scenario.add_real_card(P0, "Mycoloth", Zone::Battlefield, db);
    // Seed 3 +1/+1 counters on Mycoloth.
    scenario.with_counter(mycoloth, CounterType::Plus1Plus1, 3);
    // Stock library so Draw step doesn't deck out.
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Precondition: Mycoloth has 3 +1/+1 counters.
    let mycoloth_obj = runner.state().objects.get(&mycoloth).unwrap();
    assert_eq!(
        mycoloth_obj
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        3,
        "precondition: Mycoloth must have 3 +1/+1 counters"
    );

    // Count tokens before advancing.
    let _token_count_before = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .map(|obj| obj.is_token)
                .unwrap_or(false)
        })
        .count();

    // Advance through Untap → Upkeep (trigger fires + resolves) → Draw → PreCombatMain.
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    // Count Saproling tokens after the upkeep trigger resolves.
    let saproling_tokens: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .map(|obj| {
                    obj.is_token
                        && obj
                            .card_types
                            .subtypes
                            .iter()
                            .any(|s| s.eq_ignore_ascii_case("Saproling"))
                })
                .unwrap_or(false)
        })
        .copied()
        .collect();

    assert_eq!(
        saproling_tokens.len(),
        3,
        "Mycoloth must create 3 Saproling tokens (one for each +1/+1 counter). \
         Tokens found: {:?}",
        saproling_tokens
            .iter()
            .map(|id| {
                let obj = runner.state().objects.get(id).unwrap();
                (obj.name.clone(), obj.power, obj.toughness)
            })
            .collect::<Vec<_>>()
    );
}
