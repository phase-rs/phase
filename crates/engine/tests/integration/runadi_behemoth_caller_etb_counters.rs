//! Runadi, Behemoth Caller — RUNTIME witness for the ETB-counter replacement
//! and its downstream haste consequence (issue #6492).
//!
//! Oracle (verified via Scryfall, card j22/44):
//!   "Whenever you cast a creature spell with mana value 5 or greater, that
//!   creature enters with X additional +1/+1 counters on it, where X is its
//!   mana value minus 4."
//!   "Creatures you control with three or more +1/+1 counters on them have
//!   haste."
//!   "{T}: Add {G}."
//!
//! Pre-fix, the composite "its mana value minus 4" clause failed to parse
//! (the combinator only supported atomic quantity refs), misrouting the
//! ability to the self-ETB fallback with `valid_card: SelfRef` — Runadi would
//! try to put counters on HERSELF, not the cast creature, and the cast
//! creature would enter with 0 counters regardless of its mana value.
//!
//! CR 603.1 + CR 603.3: "Whenever you cast ..." is a TRIGGERED ability — it
//! goes on the stack and resolves independently of Runadi's continued
//! presence. The first ability is modeled as a `SpellCast` trigger whose
//! resolution installs a floating (not object-hosted), one-shot `ChangeZone`
//! replacement, so the entering-with-counters effect survives Runadi leaving
//! the battlefield between the trigger resolving and the cast creature
//! resolving (maintainer review on issue #6492 / PR #6735).
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 614.1c: "[this permanent] enters with ..." is a replacement effect.
//!   - CR 202.3: mana value.
//!   - CR 122.1a: a +1/+1 counter adds 1 to power and 1 to toughness.
//!   - CR 603.3b: a resolving triggered ability functions independently of
//!     its source once it exists on the stack.
//!
//! Discrimination: a mana-value-8 creature must enter with 4 counters (8-4)
//! and gain Haste from the second ability's 3-or-more threshold; a
//! mana-value-4 creature must enter with 0 counters (filter excludes it) and
//! no Haste; a mana-value-8 creature must still enter with 4 counters even
//! when Runadi leaves the battlefield after the qualifying spell is cast but
//! before it resolves.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const RUNADI: &str = "Whenever you cast a creature spell with mana value 5 or greater, that creature enters with X additional +1/+1 counters on it, where X is its mana value minus 4.\nCreatures you control with three or more +1/+1 counters on them have haste.\n{T}: Add {G}.";

/// Cast a green creature of the given mana value (shards: GG, generic = mv -
/// 2) while Runadi is on P0's battlefield. Returns `(counters on the
/// entrant, entrant has Haste)`.
fn cast_creature_with_runadi(name: &str, mana_value: u32) -> (u32, bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Runadi, Behemoth Caller", 1, 3, RUNADI);

    let generic = mana_value.saturating_sub(2);
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, name, 1, 1, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic,
        })
        .id();

    scenario.with_mana_pool(
        P0,
        (0..generic)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, Vec::new()))
            .chain((0..2).map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, Vec::new())))
            .collect(),
    );

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();

    let entered = outcome
        .find_object(|o| o.name == name && o.zone == Zone::Battlefield)
        .expect("cast creature must have entered the battlefield");

    let counters = outcome.counters(entered, CounterType::Plus1Plus1);

    // Force a full layers re-evaluation to read the haste static's current
    // grant — mirrors the established convention (see
    // `frostcliff_siege_anchor_word_modes.rs`) since not every action reliably
    // bumps `layers_dirty` on its own; this test cares about the counters
    // (the actual bug) driving the static's condition, not about dirty-bit
    // plumbing.
    let state = runner.state_mut();
    state.layers_dirty.mark_full();
    evaluate_layers(state);
    let has_haste = runner
        .state()
        .objects
        .get(&entered)
        .is_some_and(|obj| obj.keywords.contains(&Keyword::Haste));
    (counters, has_haste)
}

#[test]
fn runadi_grants_mv_minus_4_counters_and_downstream_haste_at_mv8() {
    // MV 8: X = 8 - 4 = 4 counters, crossing the "three or more" haste
    // threshold on the SAME creature.
    assert_eq!(
        cast_creature_with_runadi("Test Behemoth", 8),
        (4, true),
        "an MV8 creature must enter with 4 counters and gain haste from the \
         3-or-more threshold; (0, false) means the ETB-counter replacement \
         never fired (issue #6492 regression)"
    );
}

#[test]
fn runadi_grants_exactly_one_counter_at_mv5_threshold() {
    // MV 5: X = 5 - 4 = 1 counter — below the haste threshold.
    assert_eq!(
        cast_creature_with_runadi("Test Whelp", 5),
        (1, false),
        "an MV5 creature (the exact threshold) must enter with exactly 1 \
         counter and not yet have haste"
    );
}

#[test]
fn runadi_grants_no_counters_below_mv5_threshold() {
    // MV 4: below the "mana value 5 or greater" filter — the replacement
    // must not apply at all (proves the Cmc filter still gates correctly and
    // the fix didn't turn this into an unconditional counter grant, and
    // doubles as "no qualifying spell → no floating replacement installed"
    // coverage: the trigger never fires, so nothing is affected).
    assert_eq!(
        cast_creature_with_runadi("Test Sprite", 4),
        (0, false),
        "an MV4 creature must NOT receive any counters (mana value filter \
         excludes it) and must not have haste"
    );
}

/// CR 603.1 + CR 603.3b + CR 614.1c/614.12: Runadi's ability is a TRIGGERED
/// ability — once her trigger exists on the stack (queued the moment the
/// qualifying spell is cast), it resolves independently of whether Runadi
/// herself is still around. Removing her from the battlefield after the cast
/// commits (her trigger has been created and stacked above the spell) but
/// before the whole stack resolves must NOT prevent the entering creature
/// from getting its counters — the floating replacement the trigger installs
/// is source-independent, unlike an object-hosted static replacement, which
/// would have vanished with Runadi (the pre-review design this test guards
/// against regressing to).
#[test]
fn runadi_leaving_before_spell_resolves_still_grants_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let runadi = scenario
        .add_creature_from_oracle(P0, "Runadi, Behemoth Caller", 1, 3, RUNADI)
        .id();

    let mana_value = 8u32;
    let generic = mana_value.saturating_sub(2);
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Behemoth", 1, 1, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..generic)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, Vec::new()))
            .chain((0..2).map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, Vec::new())))
            .collect(),
    );

    let mut runner = scenario.build();
    let mut commit = runner.cast(spell).commit();
    // Runadi leaves the battlefield WHILE the spell — and her own trigger,
    // already queued by the cast — are still unresolved on the stack.
    commit.state_mut().objects.get_mut(&runadi).unwrap().zone = Zone::Graveyard;
    let outcome = commit.resolve();

    let entered = outcome
        .find_object(|o| o.name == "Test Behemoth" && o.zone == Zone::Battlefield)
        .expect("cast creature must have entered the battlefield");
    assert_eq!(
        outcome.counters(entered, CounterType::Plus1Plus1),
        4,
        "the entering creature must still get its mana-value-minus-4 counters \
         even though Runadi left the battlefield before the spell resolved — \
         the floating replacement her trigger installs must not depend on her \
         continued presence (issue #6492 maintainer review)"
    );
}
