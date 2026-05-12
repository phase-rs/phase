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

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
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

    let card_id = runner.state().objects[&find_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: find_id,
            card_id,
            targets: vec![],
        })
        .expect("Treasured Find cast should be accepted");

    // Bears is the only legal target (sole card in P0's graveyard), so the
    // engine's `auto_select_targets_for_ability` short-circuits and skips
    // the `WaitingFor::TargetSelection` round-trip — the spell goes straight
    // to the stack with the auto-selected target. Submit the explicit target
    // only if the engine prompts; otherwise advance the stack directly.
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(bear_id)],
            })
            .expect("selecting graveyard card target should succeed");
    }

    runner.advance_until_stack_empty();

    // CR 608.2c (target half): the targeted graveyard card returns to hand.
    assert!(
        runner.state().players[0].hand.contains(&bear_id),
        "Grizzly Bears should be in hand after Treasured Find resolves; \
         hand={:?} graveyard={:?} exile={:?}",
        runner.state().players[0].hand,
        runner.state().players[0].graveyard,
        runner.state().exile,
    );
    assert!(
        !runner.state().players[0].graveyard.contains(&bear_id),
        "Grizzly Bears should have left the graveyard"
    );

    // CR 608.2c (self-exile half): Treasured Find itself exiles via the
    // chained ChangeZone sub-ability (NOT the post-resolution stack→graveyard
    // default).
    assert_eq!(
        runner.state().objects[&find_id].zone,
        Zone::Exile,
        "Treasured Find should be in exile (self-exile sub-ability), \
         not graveyard; current zone={:?}",
        runner.state().objects[&find_id].zone,
    );
    assert!(
        runner.state().exile.contains(&find_id),
        "Treasured Find should be tracked in the exile zone; exile={:?}",
        runner.state().exile,
    );
    assert!(
        !runner.state().players[0].graveyard.contains(&find_id),
        "Treasured Find must NOT end up in graveyard (#323 user-reported bug)"
    );
}
