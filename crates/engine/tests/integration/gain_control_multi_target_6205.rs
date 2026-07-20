//! Issue #6205 — "gain control of up to N target …" dropped its target count.
//!
//! CR 115.1d + CR 601.2c: a spell or ability that says "up to N target …" offers up to N
//! target slots. `parse_targeted_action_ast`'s `gain control of ` branch called
//! `strip_optional_target_prefix` — which already parses the quantifier — but
//! bound the returned `MultiTargetSpec` to `_`, so the count never reached
//! `ParsedEffectClause.multi_target`. Every other "up to N target" shape kept
//! its count (Call of the Death-Dweller 2, Patch Up 3, The War in Heaven 3), so
//! the defect was specific to this one verb path. It spans 7 cards, of which
//! three were genuinely under-targeted:
//!
//!   * The Super Hero Civil War — "Gain control of up to two target creatures
//!     with total mana value 6 or less" parsed max = 1 (only one selectable).
//!   * Jace, Ingenious Mind-Mage — "Gain control of up to three target
//!     creatures" parsed no count at all.
//!   * Domineering Will — "target player gains control of up to three target
//!     nonattacking creatures"; `GiveControl` shares these parse paths, so it
//!     was capped at one as well. Parse-level only: the give-control path does
//!     not surface creature target slots at runtime for an unrelated reason,
//!     documented with the runtime section at the bottom of this file.
//!
//! The remaining four ("up to one target": Pyreswipe Hawk, Rangers of Ithilien,
//! Scroll of Isildur, Jon Irenicus) already selected correctly and only change
//! representation — from an implicit "optional (up to)" marker to an explicit
//! 0..=1 range. `up_to_one_stays_optional_with_an_explicit_upper_bound` pins
//! that their optionality (min 0) survives that move.
//!
//! The `TotalManaValue` target constraint was always parsed correctly; only the
//! count was lost, which is why the bug reads as "can only select one creature".
//!
//! Revert-proof: restoring the discarded binding drops `multi_target` back to
//! `None`/1 and the count assertions below fail.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, QuantityExpr};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Parse `oracle` and return the `(min, max)` of the `multi_target` spec on the
/// first ability or trigger whose effect transfers control — `GainControl` (the
/// controller takes it) or `GiveControl` ("target player gains control of …").
/// Both lower through the same `gain control of ` parse paths, so both are part
/// of this class.
fn control_target_range(
    oracle: &str,
    name: &str,
    types: &[&str],
    subtypes: &[&str],
) -> Option<(u32, Option<u32>)> {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &subtypes);

    let is_control =
        |e: &Effect| matches!(e, Effect::GainControl { .. } | Effect::GiveControl { .. });
    let fixed = |q: &QuantityExpr| match q {
        QuantityExpr::Fixed { value } => Some(*value as u32),
        _ => None,
    };
    let range = |spec: &Option<engine::types::ability::MultiTargetSpec>| {
        spec.as_ref().map(|s| {
            (
                fixed(&s.min).unwrap_or(u32::MAX),
                s.max.as_ref().and_then(fixed),
            )
        })
    };

    for ability in &parsed.abilities {
        if is_control(&ability.effect) {
            return range(&ability.multi_target);
        }
    }
    for trigger in &parsed.triggers {
        if let Some(exec) = trigger.execute.as_ref() {
            if is_control(&exec.effect) {
                return range(&exec.multi_target);
            }
        }
    }
    None
}

/// Just the max, for the cases where optionality is not the point.
fn gain_control_max(oracle: &str, name: &str, types: &[&str], subtypes: &[&str]) -> Option<u32> {
    control_target_range(oracle, name, types, subtypes).and_then(|(_, max)| max)
}

#[test]
fn saga_chapter_gain_control_keeps_up_to_two() {
    // The Super Hero Civil War, chapter I. The trailing duration clause and the
    // `total mana value` constraint both survive today; only the count was lost.
    let max = gain_control_max(
        "I — Gain control of up to two target creatures with total mana value 6 or less \
         for as long as this Saga remains on the battlefield.",
        "The Super Hero Civil War",
        &["Enchantment"],
        &["Saga"],
    );
    assert_eq!(
        max,
        Some(2),
        "\"up to two target creatures\" must offer 2 slots, got {max:?}"
    );
}

#[test]
fn loyalty_gain_control_keeps_up_to_three() {
    // Jace, Ingenious Mind-Mage — the same verb path, a different count, and no
    // constraint clause: pins that the fix is the verb path, not the constraint.
    // Verbatim ultimate line (Scryfall Oracle), including the U+2212 minus.
    let max = gain_control_max(
        "−9: Gain control of up to three target creatures.",
        "Jace, Ingenious Mind-Mage",
        &["Planeswalker"],
        &["Jace"],
    );
    assert_eq!(
        max,
        Some(3),
        "\"up to three target creatures\" must offer 3 slots, got {max:?}"
    );
}

#[test]
fn single_target_gain_control_declares_no_count() {
    // Discriminating guard: the common single-target form must NOT acquire a
    // count, so the fix cannot be "always attach a multi_target".
    let max = gain_control_max(
        "Gain control of target creature until end of turn.",
        "Act of Treason",
        &["Sorcery"],
        &[],
    );
    assert_eq!(
        max, None,
        "single-target gain control must not declare a count, got {max:?}"
    );
}

#[test]
fn up_to_one_stays_optional_with_an_explicit_upper_bound() {
    // The largest affected subgroup (Pyreswipe Hawk, Rangers of Ithilien, Scroll
    // of Isildur). These already targeted correctly; the fix moves them from an
    // implicit "optional (up to)" marker to an EXPLICIT 0..=1 range, so the risk
    // here is losing optionality, not losing the count. CR 601.2c: "up to one"
    // permits choosing zero targets, so min must stay 0.
    let range = control_target_range(
        "Whenever you expend 6, gain control of up to one target artifact \
         for as long as you control this creature.",
        "Pyreswipe Hawk",
        &["Creature"],
        &["Bird"],
    );
    assert_eq!(
        range,
        Some((0, Some(1))),
        "\"up to one target\" must stay optional (min 0) with max 1, got {range:?}"
    );
}

#[test]
fn give_control_keeps_up_to_three() {
    // Domineering Will — "target player gains control of up to three target
    // nonattacking creatures". The same `gain control of ` parse paths serve
    // `GiveControl`, so this card was capped at one target too; it is a third
    // genuinely-broken member of the class, not just a representation change.
    let max = gain_control_max(
        "Target player gains control of up to three target nonattacking creatures \
         until end of turn. Untap those creatures. They block this turn if able.",
        "Domineering Will",
        &["Instant"],
        &[],
    );
    assert_eq!(
        max,
        Some(3),
        "\"up to three target\" give-control must offer 3 slots, got {max:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// RUNTIME regression — the parser tests above assert the AST shape; this one
// asserts the production consequence. `collect_target_slots`
// (`game/ability_utils.rs`) pushes ONE `TargetSelectionSlot` per unit of the
// ability's `multi_target` bound, so a dropped count means the pipeline offers
// exactly one slot and only one creature can ever change controller — which is
// the defect as reported (#6205). The witness selects DISTINCT legal targets
// through the ordinary `WaitingFor::TargetSelection` → `GameAction::ChooseTarget`
// flow (CR 601.2c, one target per slot in written order) and asserts every
// selected creature actually changed controller (CR 613.1b, Layer 2).
//
// Revert-proof: restore the discarded `multi_target` binding and only one slot
// is created, so the driver's second and third declared targets are never
// consumed and `assert_all_controlled_by` fails on creatures 2 and 3.
// ─────────────────────────────────────────────────────────────────────────────

/// Read an object's post-layer controller (CR 613.1b: control-changing effects
/// are applied in Layer 2, so the resolved `controller` field is the authority).
fn controller_of(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner
        .state()
        .objects
        .get(&id)
        .unwrap_or_else(|| panic!("object {id:?} must still exist"))
        .controller
}

/// Assert every listed creature is controlled by `expected`, naming the first
/// one that is not — the "only one target was selectable" failure lands here.
fn assert_all_controlled_by(
    runner: &GameRunner,
    creatures: &[ObjectId],
    expected: PlayerId,
    context: &str,
) {
    let actual: Vec<PlayerId> = creatures
        .iter()
        .map(|&c| controller_of(runner, c))
        .collect();
    assert!(
        actual.iter().all(|&c| c == expected),
        "{context}: all {} selected creatures must change controller to {expected:?}, got {actual:?}. \
         A creature still under its original controller means the target slot for it was never \
         offered — the dropped `multi_target` count (#6205).",
        creatures.len()
    );
}

#[test]
fn jace_ultimate_steals_three_creatures_through_target_selection() {
    // Jace, Ingenious Mind-Mage's "−9: Gain control of up to three target
    // creatures." — the `GainControl` runtime witness. A loyalty ability IS an
    // activated ability (CR 606.1 + CR 602.2b), so it goes on the stack and its
    // targets are chosen at announcement (CR 601.2c) exactly like a spell's.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Three distinct opponent creatures — distinct so a slot that is never
    // offered shows up as that specific creature staying with P1.
    let first = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let second = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let third = scenario.add_creature(P1, "Air Elemental", 4, 4).id();

    // Verbatim Oracle line, so the ability takes the real parse branch.
    let jace = scenario
        .add_creature_from_oracle(
            P0,
            "Jace, Ingenious Mind-Mage",
            0,
            0,
            "−9: Gain control of up to three target creatures.",
        )
        .id();

    let mut runner = scenario.build();
    {
        // Make Jace a real planeswalker with enough loyalty to pay −9, so the
        // LOYALTY activation path is the one under test (CR 606.3).
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&jace).expect("jace");
        obj.card_types.core_types = vec![CoreType::Planeswalker];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.loyalty = Some(9);
        obj.counters.insert(CounterType::Loyalty, 9);
    }

    // Pre-state guard: all three start with the opponent, so the assertion
    // below cannot pass vacuously.
    assert_all_controlled_by(&runner, &[first, second, third], P1, "before activation");

    // The driver answers one slot per `ChooseTarget`, consuming one declared
    // object per slot — so it can only place all three if all three slots exist.
    runner
        .activate(jace, 0)
        .target_objects(&[first, second, third])
        .resolve();

    assert_all_controlled_by(
        &runner,
        &[first, second, third],
        P0,
        "CR 601.2c: \"up to three target creatures\" must offer three selectable slots",
    );
}

// NOTE — why there is no `GiveControl` RUNTIME witness here.
//
// `GiveControl`'s count is pinned at parse level (`give_control_keeps_up_to_three`)
// but cannot be pinned at runtime yet, and the reason is NOT this PR's defect: the
// RECIPIENT slot is missing. `ability_needs_companion_target_player_slot` defers to
// `effect_references_target_player`, which inspects only the effect's target FILTER
// (`effect_bound_filter_matches`) and never `GiveControl`'s separate `recipient`
// field. So "target player gains control of …" surfaces its creature slots but no
// player slot, the recipient never resolves, and `gain_control::resolve_give`
// installs no `ChangeController` effect at all.
//
// Measured, not assumed. Building the target slots for the same ability with and
// without `multi_target` gives 3 creature slots vs 1 creature slot — and NO player
// slot in either case. That proves two things: the missing recipient slot is
// pre-existing rather than introduced here, and the count this PR restores does
// reach real slot generation. A runtime witness fails identically with and without
// the "nonattacking" clause, which rules out the target filter as the cause.
//
// (Separately: "nonattacking" parses to `Subtype("Attacking")` — no such creature
// subtype exists, so Domineering Will has a second, independent filter gap on top
// of the recipient one. Also out of scope here.)
//
// It is deliberately not fixed here. `GiveControl`'s `recipient` is not always a
// chosen target — "an opponent gains control of that creature" and the
// `ScopedPlayer` / `SpecificPlayer` recipients resolve at resolution time via
// `unique_recipient_from_filter`. So teaching the companion-slot check to fire on
// any `GiveControl` would manufacture a spurious player slot for that whole
// non-targeted class, which is the same hazard the `Sacrifice`,
// `Bounce { AtResolution }` and `UnattachAll` carve-outs in
// `extract_target_filter_from_effect` exist to prevent. Doing it correctly means
// discriminating a TARGETED recipient from a resolution-time one — a separate
// design change, tracked as follow-up work rather than smuggled into a parser
// count fix.
