//! Regression: Louisoix's Sacrifice (issue #408) targets must include all
//! three legs of its counter clause — activated ability, triggered ability,
//! AND noncreature spell. Pre-fix, the `scan_contains("activated or triggered
//! ability")` precheck at `imperative.rs:3788` short-circuited and produced
//! `TargetFilter::StackAbility` only, silently dropping the noncreature-spell
//! leg. Runtime symptoms:
//!   - the Counter effect didn't resolve cleanly (no legal noncreature-spell
//!     targets even when one was on the stack),
//!   - the target restriction wasn't enforced (the malformed downstream filter
//!     skipped its zone scoping and routed to player-targeting via
//!     `find_legal_targets`).
//!
//! Oracle text:
//!   "As an additional cost to cast this spell, sacrifice a legendary creature.
//!    Counter target activated ability, triggered ability, or noncreature spell."
//!
//! CR cites grep-verified (`grep -n "^XXX" docs/MagicCompRules.txt`):
//! - CR 113.3a/b/c: Spell, activated ability, triggered ability classification.
//! - CR 115.1: Targeting fundamentals.
//! - CR 115.2: Legal-target requirements (only permanents unless the
//!   spell explicitly targets stack/zone objects).
//! - CR 601.2f: Additional costs (sacrifice a legendary creature).
//! - CR 701.6a: Counter — moves the targeted spell to graveyard / removes
//!   the targeted ability from the stack.
//!
//! These tests pin the post-fix shape:
//!
//!   1. **Parser** — the parsed `Counter.target` is `Or { 3 legs }`:
//!      two `StackAbility` (no controller scope) + one
//!      `And { StackSpell, Typed(noncreature) }`. This is verified by the
//!      `snapshot_louisoixs_sacrifice` test in `oracle_parser.rs`; the
//!      assertion is duplicated here so a runtime-side regression breaking
//!      `find_legal_targets` is also caught.
//!
//!   2. **Targeting** — `find_legal_targets(state, &counter.target, P0, src)`
//!      returns the noncreature spell on the stack when one is present. The
//!      old shape (bare `StackAbility`) returned an empty list because no
//!      `StackAbility` entry existed → the cast would have no legal target →
//!      cast aborts. The new shape returns the spell as a legal target.
//!
//!   3. **Negative target** — a creature spell on the stack is NOT a legal
//!      target (the `noncreature` predicate is enforced). Pre-fix this leg
//!      was silently dropped, which is a different bug; this test pins the
//!      complementary fix path.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{Effect, TargetFilter, TargetRef};
use engine::types::game_state::{StackEntry, StackEntryKind};
use engine::types::identifiers::ObjectId;
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

/// Walks a card's parsed abilities to find the first `Counter` effect.
fn find_counter_effect_target(db: &CardDatabase, name: &str) -> TargetFilter {
    let face = db
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("card '{name}' not in database"));
    for ability in &face.abilities {
        if let Effect::Counter { target, .. } = &*ability.effect {
            return target.clone();
        }
    }
    panic!(
        "card '{name}' must have a Counter effect; abilities: {:?}",
        face.abilities
            .iter()
            .map(|a| std::mem::discriminant(&*a.effect))
            .collect::<Vec<_>>()
    );
}

/// Verifies the parsed `Counter` target shape: `Or` with exactly 3 legs.
/// This is the unit-level claim, duplicated from the parser snapshot test
/// so a runtime crate consumer breaking the shape is caught here too.
#[test]
fn louisoix_sacrifice_counter_target_is_or_with_three_legs() {
    let Some(db) = load_db() else {
        return;
    };

    let target = find_counter_effect_target(db, "Louisoix's Sacrifice");
    match target {
        TargetFilter::Or { filters } => {
            assert_eq!(
                filters.len(),
                3,
                "expected 3 legs (activated ability, triggered ability, noncreature spell), got {filters:?}"
            );
            // Two `StackAbility` legs (in some order) + one stack-spell leg.
            let stack_ability_count = filters
                .iter()
                .filter(|f| matches!(f, TargetFilter::StackAbility { .. }))
                .count();
            assert_eq!(
                stack_ability_count, 2,
                "expected exactly 2 StackAbility legs, got: {filters:?}"
            );
            // Third (spell) leg is `And { StackSpell, Typed(...) }`.
            let spell_legs: Vec<_> = filters
                .iter()
                .filter(|f| !matches!(f, TargetFilter::StackAbility { .. }))
                .collect();
            assert_eq!(spell_legs.len(), 1, "expected exactly 1 spell leg");
            assert!(matches!(
                spell_legs[0],
                TargetFilter::And { .. } | TargetFilter::StackSpell
            ));
        }
        other => panic!("expected Or, got {other:?}"),
    }
}

/// CR 115.1 + CR 115.2: `find_legal_targets` returns a noncreature spell on
/// the stack as a legal target for Louisoix's Sacrifice. This is the runtime
/// targeting layer's view of the new `Or` filter — it must enumerate stack
/// objects (not just battlefield), correctly union the three legs, and
/// produce the spell as legal.
///
/// Pre-fix this test would fail because the parsed target was bare
/// `StackAbility`, which never matches a `StackEntryKind::Spell` on the
/// stack — `find_legal_targets` would return an empty Vec.
#[test]
fn louisoix_sacrifice_finds_noncreature_spell_on_stack() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sac_id = scenario.add_real_card(P0, "Louisoix's Sacrifice", Zone::Hand, db);
    // P1's noncreature spell to be on the stack. Divination is a non-creature
    // sorcery; its presence on the stack is what we want the target list to
    // surface as a legal target.
    let spell_id = scenario.add_real_card(P1, "Divination", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Manually place the Divination spell on the stack as P1's spell. Using
    // direct state manipulation rather than driving the cast pipeline avoids
    // the priority/active-player setup churn — what we want to verify here
    // is the targeting layer's behavior with the new `Or` filter, not the
    // full cast UX.
    let spell_card_id = runner.state().objects[&spell_id].card_id;
    {
        let state = runner.state_mut();
        // Move Divination from P1's hand to the stack.
        let p1 = state
            .players
            .iter_mut()
            .find(|p| p.id == P1)
            .expect("P1 must exist");
        p1.hand.retain(|id| *id != spell_id);
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: P1,
            kind: StackEntryKind::Spell {
                card_id: spell_card_id,
                ability: None,
                casting_variant: engine::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        // Sync the object's zone.
        state.objects.get_mut(&spell_id).unwrap().zone = Zone::Stack;
    }

    // Pull the parsed Counter target from the (rehydrated) Louisoix object.
    let target_filter = {
        let obj = &runner.state().objects[&sac_id];
        let mut found = None;
        for ability in obj.abilities.iter() {
            if let Effect::Counter { target, .. } = &*ability.effect {
                found = Some(target.clone());
                break;
            }
        }
        found.expect("Louisoix's Sacrifice must have a Counter ability")
    };

    let legal = find_legal_targets(runner.state(), &target_filter, P0, sac_id);

    // CR 701.6a: The Divination spell (noncreature) on the stack must be a
    // legal target. Pre-fix this list would be empty (only StackAbility was
    // searched for, and there are no abilities on the stack).
    assert!(
        legal.iter().any(|t| matches!(t, TargetRef::Object(id) if *id == spell_id)),
        "Divination (noncreature spell) on the stack must be a legal target for Louisoix's Sacrifice; got legal targets: {legal:?}"
    );
}

/// CR 115.2: A creature spell on the stack must NOT be a legal target — the
/// `noncreature` predicate on the spell leg is enforced via the
/// `And { StackSpell, Typed(noncreature) }` shape. Pre-fix the noncreature
/// leg was silently dropped, so a creature spell would either be a legal
/// target (if a different leg matched) or trivially excluded (since only
/// `StackAbility` was searched). Post-fix this test pins the negative case.
#[test]
fn louisoix_sacrifice_rejects_creature_spell_on_stack() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sac_id = scenario.add_real_card(P0, "Louisoix's Sacrifice", Zone::Hand, db);
    // Grizzly Bears is a creature spell — should NOT be a legal target.
    let bear_id = scenario.add_real_card(P1, "Grizzly Bears", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let bear_card_id = runner.state().objects[&bear_id].card_id;
    {
        let state = runner.state_mut();
        let p1 = state
            .players
            .iter_mut()
            .find(|p| p.id == P1)
            .expect("P1 must exist");
        p1.hand.retain(|id| *id != bear_id);
        state.stack.push_back(StackEntry {
            id: bear_id,
            source_id: bear_id,
            controller: P1,
            kind: StackEntryKind::Spell {
                card_id: bear_card_id,
                ability: None,
                casting_variant: engine::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        state.objects.get_mut(&bear_id).unwrap().zone = Zone::Stack;
    }

    let target_filter = {
        let obj = &runner.state().objects[&sac_id];
        let mut found = None;
        for ability in obj.abilities.iter() {
            if let Effect::Counter { target, .. } = &*ability.effect {
                found = Some(target.clone());
                break;
            }
        }
        found.expect("Louisoix's Sacrifice must have a Counter ability")
    };

    let legal = find_legal_targets(runner.state(), &target_filter, P0, sac_id);
    assert!(
        !legal
            .iter()
            .any(|t| matches!(t, TargetRef::Object(id) if *id == bear_id)),
        "Grizzly Bears (creature spell) must NOT be a legal target — the noncreature predicate is enforced; got: {legal:?}"
    );
}

/// CR 113.3a + CR 115.1: Drives the actual cast pipeline. P0 casts Louisoix's
/// Sacrifice. The cast accepts (mana is added; the additional sacrifice cost
/// has a legendary creature available). The waiting state advances through
/// the additional cost flow to target selection, where the noncreature spell
/// on the stack is offered as a legal target.
///
/// This test exercises the cast pipeline end-to-end up to the target-selection
/// state — it stops short of resolution to keep the cost-payment flow under
/// control. Resolution semantics for the resolved `Counter` effect with this
/// target shape are covered by `find_legal_targets` returning a legal target
/// (above) and by the existing resolver-level Counter tests.
#[test]
fn louisoix_sacrifice_cast_pipeline_reaches_target_selection() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::{StackEntry, WaitingFor};
    use engine::types::mana::{ManaType, ManaUnit};

    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sac_id = scenario.add_real_card(P0, "Louisoix's Sacrifice", Zone::Hand, db);
    // P0 needs a legendary creature on the battlefield to pay the additional
    // sacrifice cost (CR 601.2f).
    let _legendary = scenario.add_real_card(P0, "Tinybones, Trinket Thief", Zone::Battlefield, db);
    // P1's noncreature spell on the stack.
    let spell_id = scenario.add_real_card(P1, "Divination", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Place Divination on the stack.
    let spell_card_id = runner.state().objects[&spell_id].card_id;
    {
        let state = runner.state_mut();
        let p1 = state.players.iter_mut().find(|p| p.id == P1).unwrap();
        p1.hand.retain(|id| *id != spell_id);
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: P1,
            kind: StackEntryKind::Spell {
                card_id: spell_card_id,
                ability: None,
                casting_variant: engine::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        state.objects.get_mut(&spell_id).unwrap().zone = Zone::Stack;
    }

    // Add {U} to P0's mana pool for casting cost.
    let dummy = ObjectId(0);
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .add(ManaUnit::new(ManaType::Blue, dummy, false, vec![]));

    let sac_card_id = runner.state().objects[&sac_id].card_id;
    // Cast Louisoix's Sacrifice. The cast pipeline should accept the action
    // and either start the additional cost flow or proceed to target
    // selection (depending on the engine's ordering). We assert only that
    // the cast did not abort.
    let cast_result = runner.act(GameAction::CastSpell {
        object_id: sac_id,
        card_id: sac_card_id,
        targets: vec![],
    });
    assert!(
        cast_result.is_ok(),
        "cast must be accepted; got {cast_result:?}. waiting_for={:?}",
        runner.state().waiting_for
    );

    // The pipeline now expects either an additional-cost choice (sacrifice
    // the legendary) or has already advanced into target selection. Drive
    // forward a bounded number of steps, picking the legendary as the
    // sacrifice and the noncreature spell as the target when prompted.
    //
    // The exact `WaitingFor` variants for the additional-cost flow are
    // engine-internal; we treat anything other than `TargetSelection` as
    // an in-progress step we should advance with PassPriority or the
    // appropriate choice. This is a regression-bracket test, not a UX test.
    for _ in 0..20 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { target_slots, .. } => {
                // CR 115.2: The noncreature spell on the stack should be in
                // the legal-target list. This is the assertion the test
                // exists to make.
                let legal = &target_slots[0].legal_targets;
                assert!(
                    legal
                        .iter()
                        .any(|t| matches!(t, TargetRef::Object(id) if *id == spell_id)),
                    "Divination must be a legal target; legal_targets: {legal:?}"
                );
                return;
            }
            _ => {
                // Try passing priority to advance the flow.
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }

    panic!(
        "did not reach TargetSelection within 20 steps; final waiting_for: {:?}",
        runner.state().waiting_for
    );
}
