//! Regression: end-to-end foretell pipeline against real card data
//! (Tergrid's Shadow). Issue #320 reported that the foretell special action
//! correctly exiled the card but no later-turn cast surfaced.
//!
//! These tests exercise the full pipeline against `card-data.json`:
//!   1. `Foretell` legal action surfaces from hand on the controller's turn.
//!   2. Submitting `Foretell` pays {2}, exiles face-down, grants the
//!      `Foretold` casting permission.
//!   3. `CastSpell` is rejected on the same turn (CR 702.143a "later turn").
//!   4. After turn advance, `CastSpell` is accepted, the spell is on the
//!      stack with `CastingVariant::Foretell`, and the foretell cost
//!      ({2}{B}{B}) is the announced mana cost — not the printed mana cost
//!      ({3}{B}{B}).
//!   5. `legal_actions` surfaces the cast-from-foretell-exile action when
//!      conditions are met (the action surface AI / UI consult).
//!   6. After resolution, "Each player sacrifices two creatures" fires for
//!      every player (covered by the symptom-2 fix in dd91a9b91, included
//!      here as the resolution check).

use std::path::Path;
use std::sync::OnceLock;

use engine::ai_support::legal_actions;
use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::CastingPermission;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastingVariant, StackEntryKind};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
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

fn add_mana_to(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
    mana: &[ManaType],
) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn foretell_cost_22bb() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        generic: 2,
    }
}

#[test]
fn tergrids_shadow_foretell_surfaces_in_legal_actions() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shadow = scenario.add_real_card(P0, "Tergrid's Shadow", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana_to(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);

    // CR 702.143a: Foretell special action must surface as a legal action
    // for the controller during their own turn.
    let actions = legal_actions(runner.state());
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, GameAction::Foretell { object_id, .. } if *object_id == shadow)),
        "Foretell should be a legal action for Tergrid's Shadow on its controller's turn"
    );
}

#[test]
fn tergrids_shadow_foretell_special_action_exiles_with_permission() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shadow = scenario.add_real_card(P0, "Tergrid's Shadow", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana_to(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);

    let card_id = runner.state().objects[&shadow].card_id;
    runner
        .act(GameAction::Foretell {
            object_id: shadow,
            card_id,
        })
        .expect("foretell special action should succeed");

    // CR 702.143a-b: After foretelling, the card is in exile, face-down,
    // marked foretold, and carries a `Foretold` casting permission.
    let obj = &runner.state().objects[&shadow];
    assert_eq!(obj.zone, Zone::Exile);
    assert!(runner.state().exile.contains(&shadow));
    assert!(obj.foretold);
    assert!(obj.face_down);
    assert!(matches!(
        obj.casting_permissions.as_slice(),
        [CastingPermission::Foretold { cost, turn_foretold }]
            if *cost == foretell_cost_22bb() && *turn_foretold == runner.state().turn_number
    ));

    // Mana paid: {2} consumed.
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

#[test]
fn tergrids_shadow_cannot_be_cast_from_foretell_exile_same_turn() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shadow = scenario.add_real_card(P0, "Tergrid's Shadow", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana_to(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);
    let card_id = runner.state().objects[&shadow].card_id;
    runner
        .act(GameAction::Foretell {
            object_id: shadow,
            card_id,
        })
        .expect("foretell special action should succeed");

    // Even with full mana available, the cast must be rejected on the same
    // turn (CR 702.143a — "Cast it on a later turn").
    add_mana_to(
        &mut runner,
        P0,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
            ManaType::Black,
        ],
    );

    let actions = legal_actions(runner.state());
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == shadow)),
        "Foretold card must not be castable on the same turn it was foretold"
    );
}

#[test]
fn tergrids_shadow_cast_from_foretell_exile_on_later_turn() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shadow = scenario.add_real_card(P0, "Tergrid's Shadow", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana_to(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);
    let card_id = runner.state().objects[&shadow].card_id;
    runner
        .act(GameAction::Foretell {
            object_id: shadow,
            card_id,
        })
        .expect("foretell special action should succeed");

    // CR 702.143a — "after the current turn has ended". Bump turn_number to
    // simulate a later turn (the actual turn pass is exercised elsewhere;
    // the cast pathway only consults `state.turn_number > turn_foretold`).
    runner.state_mut().turn_number += 1;

    // Pay the foretell cost {2}{B}{B}.
    add_mana_to(
        &mut runner,
        P0,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
            ManaType::Black,
        ],
    );

    // CR 702.143a: The cast surfaces in legal actions on a later turn.
    let actions = legal_actions(runner.state());
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == shadow)),
        "Foretold card must be castable on a later turn (got actions: {:?})",
        actions
            .iter()
            .filter(|a| matches!(
                a,
                GameAction::CastSpell { .. } | GameAction::Foretell { .. }
            ))
            .collect::<Vec<_>>()
    );

    runner
        .act(GameAction::CastSpell {
            object_id: shadow,
            card_id,
            targets: vec![],
        })
        .expect("casting from foretell exile on a later turn must succeed");

    // Stack entry uses CastingVariant::Foretell so resolution-time
    // bookkeeping (CR 702.143c "was foretold") is correct.
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == shadow)
        .expect("Tergrid's Shadow must be on the stack after foretell-cast");
    match entry.kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => {
            assert_eq!(casting_variant, CastingVariant::Foretell);
        }
        ref other => panic!("expected Spell entry, got {other:?}"),
    }

    // CR 702.143b: The cast paid the foretell cost, not the printed mana
    // cost. After paying {2}{B}{B} the pool is empty.
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

#[test]
fn tergrids_shadow_foretell_cast_resolves_each_player_sacrifices() {
    // End-to-end: foretell on turn N, advance to turn N+1, cast from exile,
    // resolve, and verify the two-player sac-two-creatures effect prompts
    // both players. This is the user's repro from #320 — they paid foretell,
    // expected to cast on a later turn, and the cast never surfaced.
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shadow = scenario.add_real_card(P0, "Tergrid's Shadow", Zone::Hand, db);
    // Each player needs two creatures so the resolve actually fires.
    for _ in 0..2 {
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
        scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Pay foretell special action {2}.
    add_mana_to(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);
    let card_id = runner.state().objects[&shadow].card_id;
    runner
        .act(GameAction::Foretell {
            object_id: shadow,
            card_id,
        })
        .expect("foretell special action should succeed");
    let foretold_turn = runner.state().turn_number;

    // Advance to a later turn.
    runner.state_mut().turn_number = foretold_turn + 1;

    // Pay foretell cost {2}{B}{B} and cast.
    add_mana_to(
        &mut runner,
        P0,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
            ManaType::Black,
        ],
    );
    // Cast from foretell exile (pool-funded foretell cost {2}{B}{B}) and
    // resolve through the canonical pipeline. Both players must sacrifice two
    // creatures (covered architecturally by dd91a9b91's player_scope sweep).
    let outcome = runner.cast(shadow).resolve();

    // After resolution each player has zero creatures (they had two each).
    let count_creatures = |player: engine::types::player::PlayerId| {
        outcome
            .state()
            .battlefield
            .iter()
            .filter(|id| {
                outcome.state().objects.get(id).is_some_and(|o| {
                    o.controller == player
                        && o.card_types
                            .core_types
                            .contains(&engine::types::card_type::CoreType::Creature)
                })
            })
            .count()
    };
    assert_eq!(
        count_creatures(P0),
        0,
        "P0 should have sacrificed both creatures (Tergrid's Shadow resolution)"
    );
    assert_eq!(
        count_creatures(P1),
        0,
        "P1 should have sacrificed both creatures (Tergrid's Shadow resolution)"
    );
}
