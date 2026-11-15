//! Regression: a library-search delivery must not double-collect the
//! `GameEvent::ZoneChanged` occurrences a logical zone-change owner already
//! collected.
//!
//! Cracking a fetchland made the fetched land's ETB (Undercity Sewers' surveil)
//! and every landfall observer (Kazandu Mammoth) fire TWICE, while the same land
//! merely *played* fired once. The same occurrence reached
//! `state.deferred_triggers` from two collectors:
//!
//!   1. the logical zone-change owner — `change_zone::resolve` /
//!      `zone_pipeline::move_objects_simultaneously_then` →
//!      `triggers::complete_logical_zone_trigger_collection`;
//!   2. `engine_resolution_choices::collect_search_observer_triggers`, which
//!      re-scanned the raw action slice and collected again with no filter but
//!      `PhaseChanged`.
//!
//! `triggers.rs`'s per-event CR 603.2 dedup (`registered_this_event`) is a
//! `HashSet` allocated *inside* the event loop, so it cannot see across two
//! collection passes.
//!
//! CR 603.2c: "An ability triggers only once each time its trigger event occurs.
//! However, it can trigger repeatedly if one event contains multiple
//! occurrences." Row `two_land_search_delivery_fires_landfall_twice` pins the
//! second sentence so the fix is not over-applied into a blanket suppression.
//!
//! HARNESS NOTE — after a search choice settles, its observer triggers must be
//! dispatched before either player receives priority (CR 603.3). H1, H2, H5,
//! H6, N2, and N5 inspect that immediate post-choice state; H3, H4, and N3
//! reach the same result through other zone-change paths. N1 and N4 assert the
//! corresponding no-trigger cases.

use engine::ai_support::validated_candidate_actions_for_semantic_owner;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// Effective P/T (post-layers), read off the materialized `GameObject` fields
/// the layer pipeline writes during `apply`.
fn power_toughness(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let obj = runner
        .state()
        .objects
        .get(&id)
        .expect("object must still be present");
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

fn add_mana(runner: &mut GameRunner, green: usize, black: usize, colorless: usize) {
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for _ in 0..green {
        pool.add(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));
    }
    for _ in 0..black {
        pool.add(ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]));
    }
    for _ in 0..colorless {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

/// The validated activation index for Misty Rainforest's real printed ability,
/// derived the same way `prospective_fetchland_mana.rs` derives it.
fn misty_ability_index(state: &GameState, misty: ObjectId) -> usize {
    validated_candidate_actions_for_semantic_owner(state, P0)
        .into_iter()
        .find_map(|candidate| match candidate.action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            } if source_id == misty => Some(ability_index),
            _ => None,
        })
        .expect("Misty Rainforest's printed activated ability must be a validated root candidate")
}

/// CR 603.3: search-delivery observers must be dispatched before priority.
fn assert_observers_were_dispatched(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. }
        ),
        "the search choice must settle into trigger dispatch, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "all search-delivery observers must leave the deferred queue before priority"
    );
    assert!(
        !runner.state().stack.is_empty()
            || matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "the delivery's observers must be on the stack or awaiting their mandatory order"
    );
}

// ---------------------------------------------------------------------------
// H1 — reported symptom 1: landfall fires ONCE on a cracked fetch
// ---------------------------------------------------------------------------

#[test]
fn fetchland_crack_fires_landfall_observer_once() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let misty = scenario.add_real_card(P0, "Misty Rainforest", Zone::Battlefield, db);
    let mammoth = scenario.add_real_card(P0, "Kazandu Mammoth", Zone::Battlefield, db);
    // A basic Forest is the ONLY card Misty's filter can find, and it has no ETB
    // trigger — so exactly ONE observer is dispatched and no `OrderTriggers` or
    // surveil prompt entangles the assertion.
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let ability_index = misty_ability_index(runner.state(), misty);

    assert_eq!(
        power_toughness(&runner, mammoth),
        (3, 3),
        "Kazandu Mammoth's printed body is 3/3 before any landfall"
    );

    runner.activate(misty, ability_index).resolve();
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("selecting the searched Forest must succeed");

    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Battlefield,
        "the fetch must actually have delivered the Forest"
    );
    assert_observers_were_dispatched(&runner);
    runner.advance_until_stack_empty();

    // 3/3 base, +2/+2 exactly once. 7/7 is the double collection.
    assert_eq!(
        power_toughness(&runner, mammoth),
        (5, 5),
        "CR 603.2c: one land entering is ONE occurrence, so landfall fires once"
    );
}

// ---------------------------------------------------------------------------
// H2 — reported symptom 2: the fetched land's own ETB fires ONCE
// ---------------------------------------------------------------------------

#[test]
fn fetchland_fetched_land_etb_trigger_fires_once() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let misty = scenario.add_real_card(P0, "Misty Rainforest", Zone::Battlefield, db);
    // No Kazandu Mammoth here: Undercity Sewers' own "When this land enters,
    // surveil 1" is the single observer, so exactly one trigger is dispatched.
    let sewers = scenario.add_real_card(P0, "Undercity Sewers", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let ability_index = misty_ability_index(runner.state(), misty);

    runner.activate(misty, ability_index).resolve();
    runner
        .act(GameAction::SelectCards {
            cards: vec![sewers],
        })
        .expect("selecting the searched land must succeed");

    assert_eq!(
        runner.state().objects[&sewers].zone,
        Zone::Battlefield,
        "the fetch must actually have delivered Undercity Sewers"
    );
    assert_observers_were_dispatched(&runner);

    // Count stack objects directly after the choice, before the surveil trigger
    // resolves into its prompt.
    assert!(
        !runner.state().stack.is_empty(),
        "the search choice must have dispatched the fetched land's ETB"
    );
    let surveil_copies = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == sewers)
        .count();
    assert_eq!(
        surveil_copies, 1,
        "CR 603.2c: the fetched land's ETB must reach the stack exactly once"
    );
}

// ---------------------------------------------------------------------------
// H3 — control: a land PLAYED (one collector, never two) still fires once
// ---------------------------------------------------------------------------

#[test]
fn played_land_fires_landfall_observer_once() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mammoth = scenario.add_real_card(P0, "Kazandu Mammoth", Zone::Battlefield, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Hand, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("playing a land in the precombat main phase must succeed");

    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Battlefield,
        "the played land must have entered the battlefield"
    );
    // `handle_play_land` calls `zone_pipeline::deliver` directly and allocates
    // no logical zone-change group, so the landfall trigger reaches the stack
    // in-action — no priority pass is needed here.
    runner.advance_until_stack_empty();

    assert_eq!(
        power_toughness(&runner, mammoth),
        (5, 5),
        "a played land has exactly one collector and must stay at one firing"
    );
}

// ---------------------------------------------------------------------------
// H4 — pause/resume through a search delivery still fires landfall once
// ---------------------------------------------------------------------------

#[test]
fn fetch_pauses_on_optional_replacement_then_fires_landfall_once() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let misty = scenario.add_real_card(P0, "Misty Rainforest", Zone::Battlefield, db);
    let mammoth = scenario.add_real_card(P0, "Kazandu Mammoth", Zone::Battlefield, db);
    // Breeding Pool's "As this land enters, you may pay 2 life" surfaces a real
    // `ReplacementChoice` mid-delivery, so the owner pauses and resumes.
    let pool = scenario.add_real_card(P0, "Breeding Pool", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let ability_index = misty_ability_index(runner.state(), misty);

    let outcome = runner
        .activate(misty, ability_index)
        .search_first_legal()
        .resolve();

    // `AbilityActivation::resolve` hard-codes `replacement_choice: None`, so the
    // driver breaks and leaves the prompt live for us to answer by hand.
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice { .. }
        ),
        "Breeding Pool's MayCost must surface a real replacement pause, got {:?}",
        outcome.final_waiting_for()
    );
    // For an optional replacement the candidate vec is exactly
    // `[accept, decline]`; index 1 declines (enters tapped, no life paid).
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining the optional replacement must be accepted");

    assert_eq!(
        runner.state().objects[&pool].zone,
        Zone::Battlefield,
        "the paused delivery must still have completed"
    );
    // MEASURED, not assumed: this row is NOT a park path. The replacement pause
    // resumes through `effects/mod.rs`'s parked-`ChangeZone` drain, which drains
    // `deferred_triggers` and then collects + dispatches the resumed slice
    // INLINE — so the landfall observer is already on the stack here and the
    // parked queue is empty. That is the structural difference the plan's §4d
    // calls out, and it is why no priority pass belongs in this sequence.
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "the resumed-ChangeZone drain dispatches inline; nothing may remain parked"
    );
    let landfall_copies = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == mammoth)
        .count();
    assert_eq!(
        landfall_copies, 1,
        "the landfall observer must reach the stack exactly once on the \
         pause/resume route"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        power_toughness(&runner, mammoth),
        (5, 5),
        "a paused-then-resumed delivery must still fire landfall exactly once"
    );
}

// ---------------------------------------------------------------------------
// H5 — the single-basic partition fast path
// ---------------------------------------------------------------------------

#[test]
fn cultivate_fast_path_fires_landfall_once() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mammoth = scenario.add_real_card(P0, "Kazandu Mammoth", Zone::Battlefield, db);
    let cultivate = scenario.add_real_card(P0, "Cultivate", Zone::Hand, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    // A nonbasic so exactly one basic is findable and the fast path is taken.
    scenario.add_real_card(P0, "Mishra's Factory", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(&mut runner, 1, 0, 2);

    runner.cast(cultivate).resolve();
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("selecting Cultivate's battlefield basic must succeed");

    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Battlefield,
        "the fast path must have delivered the single basic"
    );
    assert_observers_were_dispatched(&runner);
    runner.advance_until_stack_empty();

    assert_eq!(
        power_toughness(&runner, mammoth),
        (5, 5),
        "the single-basic partition path must fire landfall exactly once"
    );
}

// ---------------------------------------------------------------------------
// H6 — the explicit `SearchPartitionChoice` route
// ---------------------------------------------------------------------------

#[test]
fn cultivate_partition_fires_landfall_once() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mammoth = scenario.add_real_card(P0, "Kazandu Mammoth", Zone::Battlefield, db);
    let cultivate = scenario.add_real_card(P0, "Cultivate", Zone::Hand, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    let mountain = scenario.add_real_card(P0, "Mountain", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(&mut runner, 1, 0, 2);

    // `search_first_legal` submits both basics at the `SearchChoice`; the
    // partition prompt then parks for an explicit pick.
    runner.cast(cultivate).search_first_legal().resolve();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::SearchPartitionChoice { .. }
        ),
        "two findable basics must reach a SearchPartitionChoice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("the partition pick must resolve");

    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Battlefield,
        "the primary basic must reach the battlefield"
    );
    assert_eq!(
        runner.state().objects[&mountain].zone,
        Zone::Hand,
        "the rest basic must reach the hand — exactly ONE land entered"
    );
    assert_observers_were_dispatched(&runner);
    runner.advance_until_stack_empty();

    assert_eq!(
        power_toughness(&runner, mammoth),
        (5, 5),
        "the explicit partition path must fire landfall exactly once"
    );
}

// ---------------------------------------------------------------------------
// N1 — a non-battlefield search destination is delivered and swallows nothing
// ---------------------------------------------------------------------------

#[test]
fn search_to_hand_delivers_and_fires_no_landfall() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mammoth = scenario.add_real_card(P0, "Kazandu Mammoth", Zone::Battlefield, db);
    let journey = scenario.add_real_card(P0, "Journey of Discovery", Zone::Hand, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    let mountain = scenario.add_real_card(P0, "Mountain", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(&mut runner, 1, 0, 2);

    // Journey of Discovery is modal + entwine; mode 0 is the
    // `ChangeZone { Library -> Hand }` half.
    runner.cast(journey).modes(&[0]).resolve();
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest, mountain],
        })
        .expect("selecting Journey of Discovery's basics must succeed");

    // The positive reach-guard: the search really delivered. Without it the
    // (3,3) assertion below could pass on a fail-to-find.
    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Hand,
        "mode 0 must put the found basics into HAND"
    );
    assert_eq!(
        runner.state().objects[&mountain].zone,
        Zone::Hand,
        "mode 0 must put the found basics into HAND"
    );

    assert_eq!(
        power_toughness(&runner, mammoth),
        (3, 3),
        "no land ENTERED, so landfall must not fire at all"
    );
}

// ---------------------------------------------------------------------------
// N2 — CR 603.2c sentence 2: two lands in ONE logical group fire landfall TWICE
// ---------------------------------------------------------------------------

#[test]
fn two_land_search_delivery_fires_landfall_twice() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mammoth = scenario.add_real_card(P0, "Kazandu Mammoth", Zone::Battlefield, db);
    // Harrow is an Instant with "sacrifice a land" as an additional cost.
    let harrow = scenario.add_real_card(P0, "Harrow", Zone::Hand, db);
    let spare = scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(&mut runner, 1, 0, 2);

    runner.cast(harrow).sacrifice_with(&[spare]).resolve();
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest, island],
        })
        .expect("selecting Harrow's basics must succeed");

    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Battlefield,
        "both basics must have been delivered"
    );
    assert_eq!(
        runner.state().objects[&island].zone,
        Zone::Battlefield,
        "both basics must have been delivered"
    );
    assert_observers_were_dispatched(&runner);
    // `advance_until_stack_empty` drains the CR 603.3b `OrderTriggers` prompt
    // internally; both dispatched triggers are pumps with no further prompt.
    runner.advance_until_stack_empty();

    assert_eq!(
        power_toughness(&runner, mammoth),
        (7, 7),
        "CR 603.2c sentence 2: TWO lands entering is TWO occurrences, so the \
         fix must not become a blanket zone-change suppression"
    );
}

// ---------------------------------------------------------------------------
// N3 — CR 603.7b fence: the leaves-the-battlefield delayed family must survive
//      a targeted `Effect::ChangeZone`
// ---------------------------------------------------------------------------

#[test]
fn aura_exiled_via_targeted_change_zone_fires_delayed_sacrifice() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    let aura = scenario.add_real_card(P0, "Animate Dead", Zone::Hand, db);
    let exiler = scenario.add_real_card(P0, "Introduction to Annihilation", Zone::Hand, db);
    // Introduction to Annihilation's `SequentialSibling` is "Its controller
    // draws a card". Without a library P0 decks out (CR 704.5b) and the game
    // ends before the delayed sacrifice can be observed.
    for _ in 0..5 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(&mut runner, 0, 1, 1);

    runner.cast(aura).target_object(creature).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "Animate Dead's ETB must have reanimated the creature, which is what \
         creates the WhenLeavesPlayFiltered delayed trigger"
    );

    add_mana(&mut runner, 0, 0, 5);
    // `Effect::ChangeZone { destination: Exile }` on a targeted nonland
    // permanent — the targeted `change_zone::resolve` path that allocates a
    // logical zone-change group and completes it without a paired `mark_`.
    runner.cast(exiler).target_object(aura).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Exile,
        "the targeted Effect::ChangeZone must actually have exiled the Aura"
    );
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Graveyard,
        "CR 603.7b: a delayed triggered ability triggers the next time its \
         trigger event occurs — claiming the Aura's ZoneChanged in the consumed \
         ledger would hide it from check_delayed_triggers and the reanimated \
         creature would never be sacrificed"
    );
}

// ---------------------------------------------------------------------------
// N4 — fail-to-find: an empty delivery slice still short-circuits cleanly
// ---------------------------------------------------------------------------

#[test]
fn fetch_with_no_legal_target_parks_nothing() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let misty = scenario.add_real_card(P0, "Misty Rainforest", Zone::Battlefield, db);
    let mammoth = scenario.add_real_card(P0, "Kazandu Mammoth", Zone::Battlefield, db);
    // Nothing Misty's "Forest or Island card" filter can find.
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let ability_index = misty_ability_index(runner.state(), misty);
    let life_before = runner.state().players[0].life;

    runner
        .activate(misty, ability_index)
        .search_first_legal()
        .resolve();

    // Positive reach-guards: the activation cost `{T}, Pay 1 life, Sacrifice
    // this land` was really paid, so the ability really ran. (A "waiting_for is
    // not SearchChoice" guard would be satisfied by a never-accepted activation
    // and does not discriminate.)
    assert_eq!(
        runner.state().objects[&misty].zone,
        Zone::Graveyard,
        "the sacrifice half of the activation cost must have been paid"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before - 1,
        "the pay-1-life half of the activation cost must have been paid"
    );

    assert!(
        runner.state().deferred_triggers.is_empty(),
        "a fail-to-find delivers nothing, so no trigger may be deferred"
    );
    assert_eq!(
        power_toughness(&runner, mammoth),
        (3, 3),
        "no land entered, so landfall must not fire"
    );
}

// ---------------------------------------------------------------------------
// N5 — CR 400.7 + CR 603.2c: the two occurrences dispatched by ONE search delivery
//      carry DISTINCT `turn_zone_change_index` values, each agreeing with the
//      ledger row `restrictions::record_zone_change` wrote for it.
//
//      SCOPE — this row is a SENTINEL ON ONE EMIT PATH, NOT A CENSUS.
//      Its Harrow fixture traverses:
//        change_zone::resolve -> execute_zone_move_with_applied_terminal
//        -> deliver_replaced_zone_change -> zones::move_to_zone (ordinary arm)
//        -> resolve_and_apply_zone_change (zones.rs:826)
//        -> zones.rs:1199 -> emit at zones.rs:1362.
//      A regression at the other three production emit sites — zones.rs:1430
//      (`from: None` entries), the `from != Library` path in
//      `move_to_library_at_index`, or merge.rs:689 (CR 730.3c split) — is NEVER
//      EXECUTED by this fixture and will NOT turn it red. Only the same-library
//      reorder path is not an emit site: it creates neither a `ZoneChanged` event
//      nor a ledger row, as covered by `within_library_reposition_does_not_create_a_zone_change`
//      in game/zones.rs.
//
//      WHY IT IS NONETHELESS LOAD-BEARING: the `TurnRecordIndexMismatch` guard
//      at zones.rs:946-953 validates the COMMAND's `turn_zone_change_index`
//      field against the allocator, never the RECORD's copy — and the record's
//      copy is what becomes the event (zones.rs:1199 -> :1362). So the emitted
//      index has no production guard on any path.
//      `GameObject::snapshot_for_zone_change` seeds `turn_zone_change_index: 0`
//      (game_object.rs:1983), so "an emit site forgot to stamp it" is the
//      natural failure mode this row detects, not a contrived one.
//
//      It deliberately does NOT pin the equality link: raw field reads bypass
//      `PartialEq`. That link is pinned at the authority layer by
//      `occurrence_exact_witness_consumes_the_occurrence_its_witness_names`.
//
//      NO PRIORITY PASS — DELIBERATE. This row reads the trigger events after
//      the mandatory CR 603.3 dispatch but before the triggered abilities
//      resolve, so each event's occurrence index remains observable on its
//      stack entry.
// ---------------------------------------------------------------------------

#[test]
fn dispatched_delivery_records_carry_distinct_occurrence_indices() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Kazandu Mammoth", Zone::Battlefield, db);
    // Harrow is an Instant with "sacrifice a land" as an additional cost.
    let harrow = scenario.add_real_card(P0, "Harrow", Zone::Hand, db);
    let spare = scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(&mut runner, 1, 0, 2);

    runner.cast(harrow).sacrifice_with(&[spare]).resolve();
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest, island],
        })
        .expect("selecting Harrow's basics must succeed");

    // Reach-guards: the delivery really happened and its observers have been
    // dispatched before priority.
    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Battlefield,
        "both basics must have been delivered"
    );
    assert_eq!(
        runner.state().objects[&island].zone,
        Zone::Battlefield,
        "both basics must have been delivered"
    );
    assert_observers_were_dispatched(&runner);

    // CR 603.3b requires the controller to order the two simultaneous landfall
    // triggers before either is put on the stack. This is not a priority pass
    // and does not resolve either trigger, so their event contexts remain
    // observable below.
    if let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() {
        let order = (0..triggers.len()).collect();
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("ordering the simultaneous landfall triggers must succeed");
    }

    // Every `ZoneChanged` carried by the dispatched trigger stack entries,
    // paired with the occurrence index its record was stamped with.
    let dispatched: Vec<(ObjectId, usize)> = runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            engine::types::game_state::StackEntryKind::TriggeredAbility {
                trigger_event:
                    Some(engine::types::events::GameEvent::ZoneChanged {
                        object_id, record, ..
                    }),
                ..
            } => Some((*object_id, record.turn_zone_change_index)),
            _ => None,
        })
        .collect();

    let indices_for = |land: ObjectId| -> Vec<usize> {
        dispatched
            .iter()
            .filter(|(id, _)| *id == land)
            .map(|(_, index)| *index)
            .collect()
    };
    let forest_indices = indices_for(forest);
    let island_indices = indices_for(island);

    // Third reach-guard: a record was located for EACH land, so the assertions
    // below cannot pass vacuously on an empty iterator.
    assert!(
        !forest_indices.is_empty(),
        "the dispatched trigger must carry a ZoneChanged for the delivered Forest"
    );
    assert!(
        !island_indices.is_empty(),
        "the dispatched trigger must carry a ZoneChanged for the delivered Island"
    );
    assert!(
        forest_indices
            .iter()
            .all(|index| *index == forest_indices[0])
            && island_indices
                .iter()
                .all(|index| *index == island_indices[0]),
        "N observers of ONE occurrence contribute N copies of the SAME value, so \
         every copy for one object must carry the same occurrence index"
    );

    // CR 603.2c sentence 2: two lands entering is TWO occurrences, and
    // `restrictions::record_zone_change` allocates a distinct index for each.
    assert_ne!(
        forest_indices[0], island_indices[0],
        "CR 400.7 + CR 603.2c: two distinct occurrences in ONE delivery must \
         carry DISTINCT turn_zone_change_index values, or the trigger-context \
         witness could cross-consume them"
    );

    // Each emitted event's index must agree with the ledger row the allocator
    // wrote for it — the two-storage contract. An emit site that ships the `0`
    // placeholder instead of the allocator's value breaks this.
    for (land, index) in [(forest, forest_indices[0]), (island, island_indices[0])] {
        let record = runner
            .state()
            .zone_changes_this_turn
            .get(index)
            .unwrap_or_else(|| panic!("the emitted index {index} must address a real ledger row"));
        assert_eq!(
            record.object_id, land,
            "the index carried by the emitted event must address THAT object's \
             own ledger row (zone_changes_this_turn[{index}])"
        );
    }
}
