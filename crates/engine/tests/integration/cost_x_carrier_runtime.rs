//! t96 — RUNTIME PROBES for the cost-X channel. Measures, never assumes.
//!
//! The charter's premise is that an unrewritten `QuantityRef::Variable{"X"}` in a cost-X
//! ability "fabricates 0". That is a claim about the RUNTIME, and the runtime has a fallback
//! chain the parser cannot see (`game/quantity.rs`, `resolve_ref`):
//!
//!     Variable{"X"}  ->  ability.chosen_x
//!                    ->  current_trigger_event -> extract_source_from_event -> obj.cost_x_paid
//!                    ->  0
//!
//! So a residual X fabricates ONLY where BOTH links are dead. These probes drive the real
//! cast/activate pipeline and read the resulting board, so each MEASURES which case it is
//! rather than predicting it.
//!
//! HARNESS NOTE (learned the hard way): `add_card_to_hand` builds a name-only object
//! "without rules text" — a probe built on it is VACUOUS and reads 0 for everything, which
//! looks exactly like a fabrication. Every card below is therefore synthesized from its
//! VERBATIM Oracle text (pool export, `cargo export-cards`) via the `*_from_oracle` builders,
//! and the control below exists precisely to catch a regression back into that vacuum.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn add_mana(runner: &mut engine::game::scenario::GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// Battlefield objects whose name contains `needle`, as (name, power, toughness).
fn named_on_battlefield(
    runner: &engine::game::scenario::GameRunner,
    needle: &str,
) -> Vec<(String, i32, i32)> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.name.contains(needle))
        .map(|o| {
            (
                o.name.clone(),
                o.power.unwrap_or(0),
                o.toughness.unwrap_or(0),
            )
        })
        .collect()
}

fn zone_size(runner: &engine::game::scenario::GameRunner, zone: Zone) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == zone)
        .count()
}

// ─────────────────────────────────────────────────────────────────────────────
// CONTROL — is the cost_x_paid fallback link actually LIVE?
// ─────────────────────────────────────────────────────────────────────────────

/// CR 107.3m + CR 107.3e — Hydroid Krasis, `{X}{G}{U}`:
/// "When you cast this spell, you gain half X life and draw half X cards. Round down each time."
///
/// A `SpellCast` trigger. Its own `chosen_x` is None (a trigger has no cost of its own), so it
/// can only work by riding the second link: `current_trigger_event` -> the Krasis spell object
/// -> `cost_x_paid`, stamped by `finalize_cast`. Cast for X=4 it must gain half X = 2 life.
///
/// THIS IS THE NON-VACUITY CONTROL FOR THE WHOLE FILE. If it reads 0, the harness is not
/// really casting anything and every "fabrication" verdict below is void.
#[test]
fn control_spellcast_trigger_reads_cast_x_through_the_fallback() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = {
        let mut b = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Hydroid Krasis",
            0,
            0,
            "When you cast this spell, you gain half X life and draw half X cards. Round down \
             each time.\nFlying, trample",
        );
        b.with_mana_cost(cost(
            vec![ManaCostShard::X, ManaCostShard::Green, ManaCostShard::Blue],
            0,
        ));
        b.id()
    };
    let mut runner = scenario.build();

    let life_before = runner.state().players[0].life;
    add_mana(&mut runner, ManaType::Green, 4);
    add_mana(&mut runner, ManaType::Blue, 4);

    runner.cast(spell).x(4).resolve();

    let gained = runner.state().players[0].life - life_before;
    assert_eq!(
        gained, 2,
        "CONTROL: the Variable(\"X\") -> current_trigger_event -> cost_x_paid fallback must be \
         LIVE. Hydroid Krasis cast for X=4 gains half X = 2 life. A 0 here means the harness is \
         vacuous (see the HARNESS NOTE at the top of this file) and every fabrication verdict in \
         this file is void. MEASURED: {gained}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// GATED-ETB — the walk RUNS on these, but leaves some slots unrewritten.
// Does the fallback save them, or do they fabricate?
// ─────────────────────────────────────────────────────────────────────────────

/// Hugs, Grisly Guardian, `{X}{R}{R}{G}{G}`:
/// "When Hugs enters, exile the top X cards of your library. Until the end of your next turn,
/// you may play those cards."
///
/// A self-ETB trigger — the cost-X walk's gate PASSES — but the effect is a variant the walk
/// does NOT enumerate, so its `count` keeps a bare `Variable("X")`. Discriminates: does an
/// unenumerated ARM on a gated trigger fabricate, or does the ZoneChanged -> permanent ->
/// `cost_x_paid` fallback bind it anyway (making the missing arm a normalization gap, not a
/// bug)? The answer decides whether a totality guard on this walk may red these faces at all.
#[test]
fn gated_etb_unenumerated_effect_arm() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..8 {
        scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
    }
    let spell = {
        let mut b = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Hugs, Grisly Guardian",
            5,
            5,
            "Trample\nWhen Hugs enters, exile the top X cards of your library. Until the end of \
             your next turn, you may play those cards.",
        );
        b.with_mana_cost(cost(
            vec![
                ManaCostShard::X,
                ManaCostShard::Red,
                ManaCostShard::Red,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
            0,
        ));
        b.id()
    };
    let mut runner = scenario.build();

    let exiled_before = zone_size(&runner, Zone::Exile);
    add_mana(&mut runner, ManaType::Red, 5);
    add_mana(&mut runner, ManaType::Green, 5);

    runner.cast(spell).x(3).resolve();

    let exiled = zone_size(&runner, Zone::Exile) - exiled_before;
    assert_eq!(
        exiled, 3,
        "Hugs cast for X=3 must exile the top 3 cards. 0 = the unenumerated `count` arm \
         FABRICATED. 3 = the cost_x_paid fallback bound it, so the missing arm is a \
         NORMALIZATION gap rather than a live bug. MEASURED: {exiled}"
    );
}

/// Broodlord, `{3}{X}{G}`:
/// "When this creature enters, distribute X +1/+1 counters among any number of other target
/// creatures you control."
///
/// Also a gated self-ETB, but the X lives in `multi_target.max` — a SIBLING FIELD of `effect`
/// that `rewrite_cost_x_in_ability` never visits at all. `multi_target` is consumed during
/// TARGET SELECTION, before `current_trigger_event` is set for resolution, so this is the slot
/// most likely to fabricate even where the fallback rescues `effect` slots.
#[test]
fn gated_etb_multi_target_sibling_field() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let spell = {
        let mut b = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Broodlord",
            4,
            4,
            "When this creature enters, distribute X +1/+1 counters among any number of other \
             target creatures you control.",
        );
        b.with_mana_cost(cost(vec![ManaCostShard::X, ManaCostShard::Green], 3));
        b.id()
    };
    let mut runner = scenario.build();

    add_mana(&mut runner, ManaType::Green, 9);

    runner.cast(spell).x(2).target_object(bear).resolve();

    let counters = runner
        .state()
        .objects
        .get(&bear)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0);
    assert_eq!(
        counters, 2,
        "Broodlord cast for X=2 must distribute 2 +1/+1 counters onto the Bears. 0 means \
         `multi_target.max` FABRICATED — the walk never visits that sibling field. MEASURED: \
         {counters}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// UNGATED — the walk NEVER runs. X comes from an ACTIVATED ability's cost
// (CR 107.3k), which nothing stamps anywhere.
// ─────────────────────────────────────────────────────────────────────────────

/// CR 107.3k — Hydra Broodmaster, "{X}{X}{G}: Monstrosity X."
/// "When this creature becomes monstrous, create X X/X green Hydra creature tokens."
///
/// The trigger's X is the X of the ACTIVATED ABILITY that caused the event. CR 107.3k
/// (grep-verified): "If an object's activated ability has an {X} ... in its activation cost, the
/// value of X for that ability is INDEPENDENT of any other values of X chosen for that object."
/// So this X cannot ride `cost_x_paid` (the CR 107.3m *cast* channel) — that would be the wrong
/// X by rule, not merely a missing one.
///
/// This is the SAME defect as Shark Typhoon's cycling trigger on a different trigger mode,
/// which is what makes it a CLASS rather than a Shark Typhoon special case.
///
/// IGNORED — and it is IGNORED BECAUSE IT FAILS, not because it is unimportant. The engine
/// carrier for an activated ability's announced X does not exist yet (task #96, commit 2). The
/// assertion below is the CORRECT expectation and is deliberately left in place, red, as the
/// ready-made red-first witness: whoever lands the carrier deletes this `#[ignore]` and watches
/// it go 0 -> 2 tokens. Do NOT "fix" it by weakening the assertion to 0 — that would pin the
/// fabrication as expected behaviour, which is exactly the defect class this work exists to kill.
#[test]
#[ignore = "t96 commit 2: the activated-ability X carrier (CR 107.3k) is not built yet — this \
            witness is the red-first artifact for it, not a bug in the test"]
fn ungated_monstrosity_trigger_reads_the_activated_ability_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hydra = {
        let b = scenario.add_creature_from_oracle(
            P0,
            "Hydra Broodmaster",
            7,
            7,
            "{X}{X}{G}: Monstrosity X.\nWhen this creature becomes monstrous, create X X/X green \
             Hydra creature tokens.",
        );
        b.id()
    };
    let mut runner = scenario.build();

    add_mana(&mut runner, ManaType::Green, 10);

    runner.activate(hydra, 0).x(2).resolve();

    let tokens: Vec<_> = named_on_battlefield(&runner, "Hydra")
        .into_iter()
        .filter(|(n, _, _)| !n.contains("Broodmaster"))
        .collect();

    assert_eq!(
        tokens.len(),
        2,
        "CR 107.3k: Monstrosity X=2 must create X=2 Hydra tokens. 0 tokens means the activated \
         ability's announced X was DROPPED — nothing carries it to the trigger. MEASURED: \
         {tokens:?}"
    );
    assert!(
        tokens.iter().all(|(_, p, t)| (*p, *t) == (2, 2)),
        "CR 107.3k: each Hydra token is X/X = 2/2. MEASURED: {tokens:?}"
    );
}
