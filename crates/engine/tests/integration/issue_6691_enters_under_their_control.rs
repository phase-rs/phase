//! Issue #6691 — CR 110.2a: a battlefield-destination zone change that states
//! `"under their control"` must put the permanent under the named player's
//! control, not under the resolving player's.
//!
//! CR 110.2a: "If an effect instructs a player to
//! put an object onto the battlefield, that object enters the battlefield under
//! that player's control **unless the effect states otherwise**." Jailbreak
//! states otherwise: it returns a permanent card from an OPPONENT's graveyard
//! "under their control".
//!
//! CR 400.1 + CR 400.3 + CR 404.1: a card in a graveyard
//! is in ITS OWNER's graveyard, so "an opponent's graveyard" identifies that
//! opponent as the card's owner (CR 108.3). The parser therefore binds
//! the anaphor to `ControllerRef::ParentTargetOwner`.
//!
//! SCOPE OF THIS TEST — read before trusting it as a regression guard.
//!
//! This test does NOT discriminate the fix at the runtime layer, and that was
//! measured, not assumed: with `enters_under` stripped from the fixture entry
//! (i.e. the pre-fix AST) this test still passes. On a graveyard → battlefield
//! move the engine already resolves the new object's controller to its OWNER,
//! and for Jailbreak the owner IS the player the clause names, so the correct
//! binding and the unfixed default coincide. Staging the graveyard card with a
//! divergent controller does not separate them either — the zone move
//! reassigns it.
//!
//! What this test therefore is: a runtime REACH + NON-REGRESSION guard. It
//! proves the bound `ControllerRef::ParentTargetOwner` resolves cleanly through
//! `resolve_enters_under_player` (rather than raising `InvalidParam`, which is
//! exactly what a wrong binding such as `Opponent` or `TargetPlayer` would do
//! here), and that the card still resolves end-to-end.
//!
//! The DISCRIMINATING test for this change is the parser-level assertion in
//! `oracle_effect/tests.rs::jailbreak_enters_under_their_control_binds_to_the_owner`,
//! which asserts `enters_under == Some(ParentTargetOwner)` and fails with the
//! fix reverted (the clause is dropped and the field is `None`). The
//! fixture-integrity guard below is also revert-sensitive.
//!
//! HOSTILE FIXTURE: a THREE-player game where the victim is owned by P1 and P2
//! also holds a graveyard permanent. A two-player game cannot discriminate
//! `ParentTargetOwner` from several wrong bindings (seat-order fallbacks, "the
//! only opponent"), so the third seat is load-bearing.

use engine::game::rehydrate_game_from_card_db;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// CR 110.2a: Jailbreak's returned permanent enters under its OWNER's control.
///
/// NOT revert-failing at the runtime layer — see the module docstring. This is
/// the reach + non-regression half; the discriminating half is the parser test.
#[test]
fn jailbreak_returns_the_permanent_under_its_owners_control() {
    let Some(db) = load_db() else {
        eprintln!("skipping: integration card fixture not available");
        return;
    };

    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let p2 = PlayerId(2);

    let jailbreak = scenario.add_real_card(P0, "Jailbreak", Zone::Hand, db);
    // The victim: owned by P1, sitting in P1's graveyard.
    let victim = scenario.add_real_card(P1, "Grizzly Bears", Zone::Graveyard, db);
    // A decoy permanent card in a DIFFERENT opponent's graveyard. If the
    // binding were "an opponent" (a class, CR 102.2) rather than the
    // moved card's owner, the seat this resolves to would be ambiguous.
    let _decoy = scenario.add_real_card(p2, "Grizzly Bears", Zone::Graveyard, db);
    // Jailbreak costs {1}{W}; seed the pool so the cast is about the effect,
    // not about mana.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(9_999), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);

    // FIXTURE-INTEGRITY GUARD. `Effect::ChangeZone.enters_under` is
    // `skip_serializing_if = "Option::is_none"`, so a fixture entry captured
    // from a PRE-FIX `card-data.json` deserializes as `enters_under: None` —
    // i.e. the unfixed AST — and every assertion below would then be measuring
    // the old behaviour while appearing to pass. Fail loudly instead.
    {
        let face = db
            .get_face_by_name("Jailbreak")
            .expect("Jailbreak present in the integration fixture");
        let dump = format!("{face:?}");
        assert!(
            dump.contains("ParentTargetOwner"),
            "stale fixture: Jailbreak's parsed ability must carry \
             enters_under: Some(ParentTargetOwner); regenerate \
             crates/engine/tests/fixtures/integration_cards.json.gz"
        );
    }

    let outcome = runner
        .cast(jailbreak)
        .target_object(victim)
        // Jailbreak's delayed "up to one target" trigger is optional; P0's
        // graveyard is empty so it has no legal target either way.
        .decline_optional()
        .resolve();
    let state = outcome.state();

    // Reach guard (foot-gun #6): the move must actually have HAPPENED. Without
    // this, a fizzle or an illegal-target failure would make the controller
    // assertion below pass vacuously.
    assert!(
        state.objects.contains_key(&victim),
        "the returned card must still be a live object"
    );
    assert_eq!(
        zone_of(state, victim),
        Some(Zone::Battlefield),
        "Jailbreak must actually return the card to the battlefield"
    );
    // Reach guard: ownership is what LICENSES the binding, so pin it.
    assert_eq!(
        state.objects[&victim].owner, P1,
        "fixture invariant: the victim must be OWNED by P1"
    );

    // CR 110.2a resolution guard (see the module docstring for why this is a
    // reach guard and not a revert-failing discriminator).
    assert_eq!(
        state.objects[&victim].controller, P1,
        "CR 110.2a: the bound ParentTargetOwner must RESOLVE (not raise \
         InvalidParam) and yield the card's owner P1 — not the caster P0, \
         not the third seat P2"
    );
}

fn zone_of(state: &engine::types::game_state::GameState, id: ObjectId) -> Option<Zone> {
    state.objects.get(&id).map(|o| o.zone)
}
