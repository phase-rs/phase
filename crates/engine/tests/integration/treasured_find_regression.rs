//! Regression: GitHub issue #323 — Treasured Find ("Return target card from
//! your graveyard to your hand. Exile Treasured Find.").
//!
//! User report (Discord, 2026-05-09): the resolver was producing the inverse
//! of the printed effect. Diagnosis: `Effect::Bounce.destination` was silently
//! ignored by the single-target bounce resolver — only `BounceAll` honored it.
//! For Treasured Find specifically, the parser emits
//! `Bounce { target: Typed[Card, controller=You, InZone=Graveyard], destination: None }`
//! plus a chained `ChangeZone { target: SelfRef, destination: Exile }` for the
//! self-exile clause. With the resolver fix in `effects/bounce.rs`, the
//! `destination: None` default explicitly resolves to `Hand` (mirroring
//! `BounceAll`'s `unwrap_or(Zone::Hand)`), making the field meaningful for
//! future parser branches.
//!
//! End-to-end assertion: after casting Treasured Find with a card pre-loaded
//! into the controller's graveyard, the targeted card returns to hand and
//! Treasured Find itself ends up in exile (CR 608.2c — "the controller of
//! the spell or ability follows its instructions in the order written").

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// Issue #323: target returns to hand, Treasured Find self-exiles.
///
/// Pre-fix behavior: the bounce resolver hard-coded `Zone::Hand` and ignored
/// the `destination` field entirely; user-visible symptom was Treasured Find
/// landing in the graveyard instead of exile (the post-resolution
/// stack→graveyard fallback won the race against the silently-ignored
/// destination override). The architectural fix unifies single-target Bounce
/// with `BounceAll`'s `destination.unwrap_or(Zone::Hand)` pattern.
#[test]
fn treasured_find_returns_target_and_self_exiles() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let find_id = scenario.add_real_card(P0, "Treasured Find", Zone::Hand, db);
    // Any card in P0's graveyard is a legal target — use a vanilla creature
    // to avoid coupling the test to other parser features.
    let bear_id = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana(&mut runner, &[ManaType::Black, ManaType::Green]);

    // Bears is the only legal target (sole card in P0's graveyard). The driver
    // declares the intended graveyard target and matches it to the slot if the
    // engine prompts; if `auto_select_targets_for_ability` short-circuits, the
    // intent is harmlessly unused.
    let outcome = runner.cast(find_id).target_object(bear_id).resolve();

    // CR 608.2c (target half): the targeted graveyard card returns to hand.
    outcome.assert_zone(&[bear_id], Zone::Hand);

    // CR 608.2c (self-exile half): Treasured Find itself exiles via the
    // chained ChangeZone sub-ability (NOT the post-resolution stack→graveyard
    // default — the #323 user-reported bug).
    outcome.assert_zone(&[find_id], Zone::Exile);
}
