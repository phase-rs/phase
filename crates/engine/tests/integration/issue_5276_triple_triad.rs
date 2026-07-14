//! Issue #5276: Triple Triad exiles the cards but does not allow you to play
//! them.
//!
//! Oracle (verified against the dispatched issue text):
//! "At the beginning of your upkeep, each player exiles the top card of
//! their library. Until end of turn, you may play the card you own exiled
//! this way and each other card exiled this way with lesser mana value than
//! it without paying their mana costs."
//!
//! Root cause: the comparative clause "... and each other card exiled this
//! way with lesser mana value than it ..." has no dedicated parser path.
//! `try_parse_cast_effect` fell through every anaphor/typed-filter branch to
//! Branch 3's bare fallback, producing `Effect::CastFromZone { target:
//! TargetFilter::Any, .. }` — a permission grant not scoped to the exiled
//! cards at all, so the "may play" permission never actually let you cast
//! anything (CR 608.2c + CR 406.6 + CR 118.9).
//!
//! Fix: a dedicated combinator (`try_parse_own_exiled_and_compared_others`)
//! recognizes this compound clause and builds
//! `TargetFilter::Or[ own-exiled-card, other-exiled-cards-with-lesser-mv ]`,
//! where the per-grantee comparator threshold is a new `ObjectScope::
//! OwnExiledThisWay` (re-derived per read from the exile-link set and the
//! resolving ability's current controller, CR 608.2c + CR 108.3).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::CastingPermission;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const TRIPLE_TRIAD_ORACLE: &str = "At the beginning of your upkeep, each player exiles the top card of their library. Until end of turn, you may play the card you own exiled this way and each other card exiled this way with lesser mana value than it without paying their mana costs.";

/// True when `state.objects[id]` carries a zero-cost `ExileWithAltCost`
/// permission granted to `grantee` — the "may play ... without paying its
/// mana cost" permission shape this clause must produce (CR 118.9 + CR 611.2a).
fn has_free_exile_cast_permission(
    state: &engine::types::game_state::GameState,
    id: engine::types::identifiers::ObjectId,
    grantee: PlayerId,
) -> bool {
    state.objects[&id].casting_permissions.iter().any(|p| {
        matches!(
            p,
            CastingPermission::ExileWithAltCost {
                cost,
                granted_to: Some(g),
                ..
            } if *cost == ManaCost::zero() && *g == grantee
        )
    })
}

/// Triple Triad's controller (P0) must be able to play BOTH the card they
/// own exiled this way AND any other exiled card with strictly lesser mana
/// value — but NOT an exiled card with equal or greater mana value.
#[test]
fn triple_triad_grants_own_card_and_only_lesser_mv_others() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    // Start at Untap so advancing into Upkeep fires the synthesized phase
    // trigger (CR 503.1a + CR 603.2), mirroring the Dreadhorde Invasion
    // upkeep-trigger regression test.
    scenario.at_phase(Phase::Untap);

    scenario
        .add_creature(P0, "Triple Triad", 0, 0)
        .as_enchantment()
        .from_oracle_text(TRIPLE_TRIAD_ORACLE);

    // P0's own top card: mana value 4 — the per-grantee comparator threshold.
    let p0_card = scenario
        .add_spell_to_library_top(P0, "P0 Own Card", false)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    // P1's top card: mana value 2 — LESSER than P0's own (4), must become
    // playable by P0.
    let p1_lesser = scenario
        .add_spell_to_library_top(P1, "P1 Lesser Card", false)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    // P2's top card: mana value 5 — GREATER than P0's own (4), must NOT
    // become playable by P0.
    let p2_greater = scenario
        .add_spell_to_library_top(P2, "P2 Greater Card", false)
        .with_mana_cost(ManaCost::generic(5))
        .id();

    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    // Resolve the upkeep trigger (ExileTop for each player -> the
    // comparative CastFromZone grant).
    runner.resolve_top();

    // Precondition: all three cards actually left their libraries and
    // landed in Exile (the exile half of the bug report was never broken,
    // but pinning it here catches a regression that silently stopped
    // exiling instead of just dropping the permission).
    for &id in &[p0_card, p1_lesser, p2_greater] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Exile,
            "each player's top card must be exiled at Triple Triad's upkeep trigger"
        );
    }

    assert!(
        has_free_exile_cast_permission(runner.state(), p0_card, P0),
        "P0 must be able to play the card they own exiled this way, got permissions {:?}",
        runner.state().objects[&p0_card].casting_permissions
    );
    assert!(
        has_free_exile_cast_permission(runner.state(), p1_lesser, P0),
        "P0 must be able to play another player's exiled card with LESSER mana value than their own, got permissions {:?}",
        runner.state().objects[&p1_lesser].casting_permissions
    );
    assert!(
        !has_free_exile_cast_permission(runner.state(), p2_greater, P0),
        "P0 must NOT be able to play another player's exiled card with GREATER mana value than their own, got permissions {:?}",
        runner.state().objects[&p2_greater].casting_permissions
    );

    // Negative control: P1 and P2 never receive any casting permission on
    // these cards — only Triple Triad's controller (P0) does ("you may
    // play", not "each player may play").
    for &id in &[p0_card, p1_lesser, p2_greater] {
        assert!(
            !has_free_exile_cast_permission(runner.state(), id, P1),
            "P1 must not receive a Triple Triad casting permission on {id:?}"
        );
        assert!(
            !has_free_exile_cast_permission(runner.state(), id, P2),
            "P2 must not receive a Triple Triad casting permission on {id:?}"
        );
    }
}
