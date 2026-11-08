//! Per-acquisition usage budgets (CR 602.5b + CR 602.5c + CR 113.2c).
//!
//! A granted activated ability is a distinct instance per acquisition path:
//! the holder's printed copy and each provider-stamped grant carry independent
//! once-per-turn / once-per-game budgets, keyed by
//! [`engine::types::game_state::AbilityUsageKey`] (provenance stamp + rules
//! text), never by the shifting ability index. Real cards throughout via the
//! committed fixture; every activation drives the production
//! `GameRunner::activate` path with `flush_layers` recomputes.
//!
//! Revert detectors: restoring Tier-1 dedup collapses H1a to one holding;
//! restoring `get_or_insert` collapses H1b to one Marvin instance; restoring
//! the `(source, index)` usage key lets the H2 survivor inherit the used
//! budget (its activation is rejected).

use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::AbilityKind;
use engine::types::counter::CounterType;
use engine::types::game_state::{AbilityUsageKey, ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// CR 602.5c: the usage key for `holder`'s ability at `index`, derived the
/// same way the record and check paths derive it — through the production
/// constructor, so cost concretization and the body fingerprint match.
fn usage_key(
    state: &engine::types::game_state::GameState,
    holder: ObjectId,
    index: usize,
) -> AbilityUsageKey {
    engine::game::restrictions::ability_usage_key(state, holder, index)
        .expect("the tested slot resolves through the activation lookup")
}

/// Indices of `holder`'s Untap abilities (Quirion Ranger grants).
fn untap_indices(state: &engine::types::game_state::GameState, holder: ObjectId) -> Vec<usize> {
    state.objects[&holder]
        .abilities
        .iter()
        .enumerate()
        .filter(|(_, ability)| {
            ability.kind == AbilityKind::Activated
                && ability
                    .description
                    .as_deref()
                    .is_some_and(|text| text.contains("Untap"))
        })
        .map(|(index, _)| index)
        .collect()
}

/// The index of `holder`'s Untap instance stamped with `stamp`, if present —
/// the per-carrier acquisition (CR 113.2c), not a position.
fn carrier_instance(
    state: &engine::types::game_state::GameState,
    holder: ObjectId,
    stamp: ObjectIncarnationRef,
) -> Option<usize> {
    untap_indices(state, holder)
        .into_iter()
        .find(|index| state.objects[&holder].abilities[*index].granting_object == Some(stamp))
}

/// Give `id` a +1/+1 counter, qualifying it for the Cauldron grant.
fn counter(state: &mut engine::types::game_state::GameState, id: ObjectId) {
    state
        .objects
        .get_mut(&id)
        .expect("carrier on the battlefield")
        .counters
        .insert(CounterType::Plus1Plus1, 1);
}

/// Tap `id` so it is a legal Untap target.
fn tap(state: &mut engine::types::game_state::GameState, id: ObjectId) {
    state.objects.get_mut(&id).expect("tappable").tapped = true;
}

fn fund_green(scenario: &mut GameScenario, mana_units: usize) {
    scenario.with_mana_pool(
        P0,
        (0..mana_units)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
}

/// H1a: one creature holding BOTH its printed Quirion Ranger ability and the
/// Cauldron-granted copy of the same text activates EACH copy once in the
/// same turn — two holdings, two independent once-per-turn budgets.
#[test]
fn native_and_granted_ranger_copies_activate_independently() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cauldron = scenario.add_real_card(P0, "Agatha's Soul Cauldron", Zone::Battlefield, db);
    let ranger = scenario.add_real_card(P0, "Quirion Ranger", Zone::Battlefield, db);
    let exiled = scenario.add_real_card(P0, "Quirion Ranger", Zone::Exile, db);
    let bear_a = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let bear_b = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let forest1 = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let forest2 = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    counter(runner.state_mut(), ranger);
    runner.state_mut().exile_links.push(ExileLink {
        source_id: cauldron,
        exiled_id: exiled,
        kind: ExileLinkKind::TrackedBySource,
    });
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    tap(runner.state_mut(), bear_a);
    tap(runner.state_mut(), bear_b);

    // Reach-guard: exactly two Untap instances — the printed one (unstamped)
    // and the Cauldron grant (stamped with the exiled Ranger's incarnation).
    let indices = untap_indices(runner.state(), ranger);
    assert_eq!(
        indices.len(),
        2,
        "CR 113.2c: the countered Ranger must hold its printed Untap plus the Cauldron grant; got {:?}",
        runner.state().objects[&ranger].abilities
    );
    let exile_stamp = ObjectIncarnationRef::from_object(&runner.state().objects[&exiled]);
    let mut native = None;
    let mut granted = None;
    for index in indices {
        match runner.state().objects[&ranger].abilities[index].granting_object {
            None => native = Some(index),
            Some(stamp) => {
                assert_eq!(
                    stamp, exile_stamp,
                    "the grant must name its immediate provider (the exiled Ranger)"
                );
                granted = Some(index);
            }
        }
    }
    let (native, granted) = (
        native.expect("the printed instance"),
        granted.expect("the granted instance"),
    );

    // First instance through the driver: bounce Forest 1, untap bear A.
    runner
        .activate(ranger, native)
        .target_object(bear_a)
        .pay_with(&[forest1])
        .resolve();
    assert!(
        !runner.state().objects[&bear_a].tapped,
        "bear A must be untapped by the printed instance"
    );
    assert_eq!(
        runner.state().objects[&forest1].zone,
        Zone::Hand,
        "Forest 1 must pay the printed instance's bounce cost"
    );

    // Second instance: the granted copy's own budget is still unused.
    let granted_now = untap_indices(runner.state(), ranger)
        .into_iter()
        .find(|index| {
            runner.state().objects[&ranger].abilities[*index]
                .granting_object
                .is_some()
        })
        .expect("the granted instance survives the first activation");
    assert_eq!(
        granted_now, granted,
        "no recompute shifts the granted instance's index"
    );
    runner
        .activate(ranger, granted_now)
        .target_object(bear_b)
        .pay_with(&[forest2])
        .resolve();
    assert!(
        !runner.state().objects[&bear_b].tapped,
        "bear B must be untapped by the granted instance"
    );
    assert_eq!(
        runner.state().objects[&forest2].zone,
        Zone::Hand,
        "Forest 2 must pay the granted instance's bounce cost"
    );

    // Both per-instance budgets are consumed independently.
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&usage_key(runner.state(), ranger, native))
            .copied(),
        Some(1),
        "the printed instance's budget is spent"
    );
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&usage_key(runner.state(), ranger, granted_now))
            .copied(),
        Some(1),
        "the granted instance's budget is spent"
    );
}

/// H1b: two countered carriers share one exiled Ranger via the Cauldron and an
/// uncountered Marvin re-donates each carrier's copy with its own stamp — one
/// distinct instance per carrier, each activatable once. Removing one
/// carrier's counter drops exactly its instance while the other survives.
///
/// Known pre-existing gap (out of scope, parser/filter zone scoping): Marvin's
/// "creatures you control" provider filter carries no zone term, so the exiled
/// donor ALSO donates directly through Marvin's own expansion. The assertions
/// below therefore count carrier-stamped instances only and never pin a total,
/// so they discriminate the per-acquisition identity change and stay green
/// under a future zone fix.
#[test]
fn marvin_holds_one_instance_per_carrier() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cauldron = scenario.add_real_card(P0, "Agatha's Soul Cauldron", Zone::Battlefield, db);
    let carrier_a = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let carrier_b = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let marvin = scenario.add_real_card(P0, "Marvin, Murderous Mimic", Zone::Battlefield, db);
    let exiled = scenario.add_real_card(P0, "Quirion Ranger", Zone::Exile, db);
    let bear_a = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let bear_b = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let forest1 = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let forest2 = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    counter(runner.state_mut(), carrier_a);
    counter(runner.state_mut(), carrier_b);
    runner.state_mut().exile_links.push(ExileLink {
        source_id: cauldron,
        exiled_id: exiled,
        kind: ExileLinkKind::TrackedBySource,
    });
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    tap(runner.state_mut(), bear_a);
    tap(runner.state_mut(), bear_b);

    // Reach-guard: one carrier-stamped Untap instance per carrier.
    let stamp_a = ObjectIncarnationRef::from_object(&runner.state().objects[&carrier_a]);
    let stamp_b = ObjectIncarnationRef::from_object(&runner.state().objects[&carrier_b]);
    for (label, stamp) in [("A", stamp_a), ("B", stamp_b)] {
        assert!(
            carrier_instance(runner.state(), marvin, stamp).is_some(),
            "CR 113.2c: Marvin must hold carrier {label}'s Untap instance; got {:?}",
            runner.state().objects[&marvin].abilities
        );
    }

    // Both carrier instances activate once under their own once-per-turn
    // budgets, regardless of order.
    let index_a = carrier_instance(runner.state(), marvin, stamp_a).expect("carrier A's instance");
    let outcome_a = runner
        .activate(marvin, index_a)
        .target_object(bear_a)
        .pay_with(&[forest1])
        .resolve();
    assert!(
        !outcome_a.state().objects[&bear_a].tapped,
        "bear A must be untapped by carrier A's instance"
    );
    let index_b = carrier_instance(runner.state(), marvin, stamp_b).expect("carrier B's instance");
    let outcome_b = runner
        .activate(marvin, index_b)
        .target_object(bear_b)
        .pay_with(&[forest2])
        .resolve();
    assert!(
        !outcome_b.state().objects[&bear_b].tapped,
        "bear B must be untapped by carrier B's instance"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "both activations must resolve to Priority, got {:?}",
        runner.state().waiting_for
    );

    // Hostile: removing carrier A's counter drops exactly its instance.
    runner
        .state_mut()
        .objects
        .get_mut(&carrier_a)
        .expect("carrier A")
        .counters
        .remove(&CounterType::Plus1Plus1);
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    assert!(
        carrier_instance(runner.state(), marvin, stamp_a).is_none(),
        "losing carrier A's counter must drop exactly its instance; got {:?}",
        runner.state().objects[&marvin].abilities
    );
    assert!(
        carrier_instance(runner.state(), marvin, stamp_b).is_some(),
        "carrier B's instance must survive carrier A's removal; got {:?}",
        runner.state().objects[&marvin].abilities
    );
}

/// H1b control: a single carrier yields exactly one carrier-stamped Marvin
/// instance, stable across recomputes — no phantom split from re-expansion
/// (F3 pair: distinct-across-carriers but one-within-a-carrier). Totals are
/// not pinned (see the zone-gap note above).
#[test]
fn marvin_with_a_single_carrier_holds_one_instance() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cauldron = scenario.add_real_card(P0, "Agatha's Soul Cauldron", Zone::Battlefield, db);
    let carrier = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let marvin = scenario.add_real_card(P0, "Marvin, Murderous Mimic", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Quirion Ranger", Zone::Exile, db);
    let mut runner = scenario.build();
    counter(runner.state_mut(), carrier);
    let exiled = runner
        .state()
        .objects
        .iter()
        .find(|(_, obj)| obj.zone == Zone::Exile)
        .map(|(id, _)| *id)
        .expect("the exiled Ranger");
    runner.state_mut().exile_links.push(ExileLink {
        source_id: cauldron,
        exiled_id: exiled,
        kind: ExileLinkKind::TrackedBySource,
    });
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    let stamp = ObjectIncarnationRef::from_object(&runner.state().objects[&carrier]);
    assert!(
        carrier_instance(runner.state(), marvin, stamp).is_some(),
        "the single carrier must yield its Marvin instance; got {:?}",
        runner.state().objects[&marvin].abilities
    );
    // Idempotent re-expansion: a second pass neither splits nor multiplies.
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    let count = untap_indices(runner.state(), marvin)
        .into_iter()
        .filter(|index| {
            runner.state().objects[&marvin].abilities[*index].granting_object == Some(stamp)
        })
        .count();
    assert_eq!(
        count,
        1,
        "re-expansion must not split the single carrier's instance; got {:?}",
        runner.state().objects[&marvin].abilities
    );
}

/// H2: two graveyard Rangers grant Thranduil two Untap instances. After
/// activating instance #0, exiling donor #0 with the Ooze leaves the survivor
/// usable (it does not inherit #0's spent budget) while #0's spent entry
/// persists as an orphan.
#[test]
fn used_budget_survives_provider_removal_per_instance() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund_green(&mut scenario, 1);
    let thranduil = scenario.add_real_card(P0, "Thranduil, the Elvenking", Zone::Battlefield, db);
    let donor_a = scenario.add_real_card(P0, "Quirion Ranger", Zone::Graveyard, db);
    let donor_b = scenario.add_real_card(P0, "Quirion Ranger", Zone::Graveyard, db);
    let ooze = scenario.add_real_card(P0, "Scavenging Ooze", Zone::Battlefield, db);
    let bear_a = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let bear_b = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let forest1 = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let forest2 = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    tap(runner.state_mut(), bear_a);
    tap(runner.state_mut(), bear_b);

    // Reach-guard: one Untap instance per graveyard Ranger.
    let stamp_a = ObjectIncarnationRef::from_object(&runner.state().objects[&donor_a]);
    let stamp_b = ObjectIncarnationRef::from_object(&runner.state().objects[&donor_b]);
    let indices = untap_indices(runner.state(), thranduil);
    assert_eq!(
        indices.len(),
        2,
        "CR 113.2c: Thranduil must hold one Untap per graveyard Ranger; got {:?}",
        runner.state().objects[&thranduil].abilities
    );
    let first = indices
        .iter()
        .find(|index| {
            runner.state().objects[&thranduil].abilities[**index].granting_object == Some(stamp_a)
        })
        .copied()
        .expect("donor A's instance");
    let second = indices
        .iter()
        .find(|index| {
            runner.state().objects[&thranduil].abilities[**index].granting_object == Some(stamp_b)
        })
        .copied()
        .expect("donor B's instance");
    assert_ne!(
        first, second,
        "the two donors must grant distinct ability indices"
    );
    // The spent-budget oracle for donor A's instance, captured before removal.
    let spent_key = usage_key(runner.state(), thranduil, first);
    assert_eq!(
        spent_key.provenance,
        Some(stamp_a),
        "the recorded key must name donor A's incarnation"
    );

    // Activate donor A's instance: bounce Forest 1, untap bear A.
    runner
        .activate(thranduil, first)
        .target_object(bear_a)
        .pay_with(&[forest1])
        .resolve();
    assert!(
        !runner.state().objects[&bear_a].tapped,
        "bear A must be untapped by donor A's instance"
    );
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&spent_key)
            .copied(),
        Some(1),
        "donor A's instance budget is spent"
    );

    // Exile donor A with the Ooze, then recompute.
    let ooze_index = runner.state().objects[&ooze]
        .abilities
        .iter()
        .position(|ability| {
            ability.kind == AbilityKind::Activated
                && ability
                    .description
                    .as_deref()
                    .is_some_and(|text| text.contains("Exile target card"))
        })
        .expect("the Ooze exile ability");
    runner
        .activate(ooze, ooze_index)
        .target_object(donor_a)
        .resolve();
    assert_eq!(
        runner.state().objects[&donor_a].zone,
        Zone::Exile,
        "the Ooze must exile donor A from the graveyard"
    );
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    // Only donor B's instance remains — and it is still usable.
    let indices = untap_indices(runner.state(), thranduil);
    assert_eq!(
        indices.len(),
        1,
        "exiling donor A must drop exactly its instance; got {:?}",
        runner.state().objects[&thranduil].abilities
    );
    assert_eq!(
        runner.state().objects[&thranduil].abilities[indices[0]].granting_object,
        Some(stamp_b),
        "the survivor must be donor B's instance"
    );
    // The spent entry for donor A's instance persists (never cleared: the
    // zone change moved donor A, not Thranduil).
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&spent_key)
            .copied(),
        Some(1),
        "CR 602.5b: the used budget stays used after its provider leaves"
    );
    runner
        .activate(thranduil, indices[0])
        .target_object(bear_b)
        .pay_with(&[forest2])
        .resolve();
    assert!(
        !runner.state().objects[&bear_b].tapped,
        "bear B must be untapped by the surviving instance"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the survivor activation must resolve to Priority, got {:?}",
        runner.state().waiting_for
    );
    let survivor_key = usage_key(runner.state(), thranduil, indices[0]);
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&survivor_key)
            .copied(),
        Some(1),
        "the survivor's own budget is now spent"
    );
    // Negative: the survivor never inherited donor A's spent count — its key
    // differs by provenance, so the check above succeeded and this key is new.
    assert_ne!(
        survivor_key, spent_key,
        "per-instance keys must differ by provider"
    );
}
