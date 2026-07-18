//! L02 BB7 — cast-context / alternative-cost gates (3 Standard cards).
//!
//! Cards (Oracle text verified verbatim vs `data/card-data.json`):
//!   * Molten Exhale (TDM, {1}{R} Sorcery) — "You may cast this spell as though
//!     it had flash if you behold a Dragon as an additional cost to cast it." The
//!     casting option (`AsThoughHadFlash` + a payable `Behold` cost) already fully
//!     represents + enforces the clause; the fix is a `swallow_check` exemption so
//!     the false-positive `SwallowedClause{Condition_If}` stops firing (CR 601.2f
//!     additional-cost surface). No parser/engine change.
//!   * Blasphemous Edict (FDN, {3}{B}{B} Sorcery) — "You may pay {B} rather than
//!     pay this spell's mana cost if there are thirteen or more creatures on the
//!     battlefield." A new `parse_quantity_ref` "<type> on the battlefield" arm
//!     lets the alt-cost condition parse to the runtime-supported
//!     `QuantityComparison{ObjectCount}` (CR 118.9), unblocking the fail-closed
//!     self-alt-cost path.
//!   * Sandman's Quicksand (SPM, {1}{B}{B} Sorcery, Mayhem {3}{B}) — "All
//!     creatures get -2/-2 until end of turn. If this spell's mayhem cost was
//!     paid, creatures your opponents control get -2/-2 until end of turn
//!     instead." The new `CastVariantPaid::Mayhem` leaf + a cast-time stamp +
//!     the Emerge→(Emerge|Mayhem) membership widen make the instead-swap fire
//!     (CR 702.187b + CR 608.2c).
//!
//! Parse tests drive `parse_oracle_text` / the condition authority
//! `parse_inner_condition`; runtime tests drive the real cast pipeline
//! (`GameRunner::cast(..).resolve()` + `can_cast_object_now`).

use engine::game::casting::can_cast_object_now;
use engine::game::layers::evaluate_layers;
use engine::game::restrictions::{record_card_discarded, record_discard};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::parser::oracle_nom::condition::parse_inner_condition;
use engine::types::ability::{
    AbilityCost, Comparator, ParsedCondition, QuantityExpr, QuantityRef, SpellCastingOptionKind,
    StaticCondition, TargetFilter, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, CastingVariant, StackEntryKind};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// ── Verbatim Oracle text ─────────────────────────────────────────────────────

const MOLTEN_EXHALE: &str = "You may cast this spell as though it had flash if you behold a Dragon as an additional cost to cast it. (To behold a Dragon, choose a Dragon you control or reveal a Dragon card from your hand.)\nMolten Exhale deals 4 damage to target creature or planeswalker.";

const BLASPHEMOUS_EDICT: &str = "You may pay {B} rather than pay this spell's mana cost if there are thirteen or more creatures on the battlefield.\nEach player sacrifices thirteen creatures of their choice.";

const SANDMANS_QUICKSAND: &str = "Mayhem {3}{B} (You may cast this card from your graveyard for {3}{B} if you discarded it this turn. Timing rules still apply.)\nAll creatures get -2/-2 until end of turn. If this spell's mayhem cost was paid, creatures your opponents control get -2/-2 until end of turn instead.";

const PLANAR_COLLAPSE: &str = "At the beginning of your upkeep, if there are four or more creatures on the battlefield, sacrifice this enchantment and destroy all creatures. They can't be regenerated.";

// ── Helpers ──────────────────────────────────────────────────────────────────

fn has_swallow(parsed: &ParsedAbilities, detector: &str) -> bool {
    parsed.parse_warnings.iter().any(|w| {
        format!("{w:?}").contains("SwallowedClause") && format!("{w:?}").contains(detector)
    })
}

fn black(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
        .collect()
}

/// {3}{B}: three generic (colorless) + one black.
fn three_and_black() -> Vec<ManaUnit> {
    let mut pool = black(1);
    for _ in 0..3 {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    pool
}

// ══ Parse fidelity ════════════════════════════════════════════════════════════

/// Molten Exhale parse: the casting option is `AsThoughHadFlash` with a payable
/// cost (the beheld additional cost) and NO `SwallowedClause{Condition_If}`.
/// Revert-probe: revert the `swallow_check.rs` `|| "as an additional cost"`
/// broaden → Condition_If reappears (the casting option, which represents the
/// clause, is unchanged — proving the swallow-check exemption is the sole cause).
#[test]
fn molten_exhale_parse_flash_option_no_condition_if() {
    let parsed = parse_oracle_text(
        MOLTEN_EXHALE,
        "Molten Exhale",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let opt = parsed
        .casting_options
        .first()
        .expect("Molten Exhale must emit a casting option");
    assert_eq!(
        opt.kind,
        SpellCastingOptionKind::AsThoughHadFlash,
        "the flash-timing permission must be represented as AsThoughHadFlash"
    );
    assert!(
        opt.cost.is_some(),
        "the beheld additional cost must ride the casting option (cost: Some), got {:?}",
        opt.cost
    );
    assert!(
        !has_swallow(&parsed, "Condition_If"),
        "Molten Exhale's represented flash-if-additional-cost clause must not swallow a Condition_If: {:?}",
        parsed.parse_warnings
    );
}

/// Blasphemous Edict parse: the {B} alternative cost is gated on the
/// runtime-supported `QuantityComparison{ObjectCount(Creature) >= 13}` (NOT the
/// battlefield-blind `ZoneCoreTypeCardCountAtLeast`), with NO Condition_If.
/// Revert-probe: remove the `parse_type_count_on_battlefield` arm → the option's
/// condition fails to parse and the whole option is dropped → Condition_If.
#[test]
fn blasphemous_edict_parse_alt_cost_object_count_gate() {
    let parsed = parse_oracle_text(
        BLASPHEMOUS_EDICT,
        "Blasphemous Edict",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let opt = parsed
        .casting_options
        .first()
        .expect("Blasphemous Edict must emit an alternative-cost option");
    assert_eq!(opt.kind, SpellCastingOptionKind::AlternativeCost);
    assert!(
        matches!(&opt.cost, Some(AbilityCost::Mana { .. })),
        "the alternative cost must be a mana cost ({{B}}), got {:?}",
        opt.cost
    );
    let Some(ParsedCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 13 },
    }) = &opt.condition
    else {
        panic!(
            "expected condition QuantityComparison{{ObjectCount >= 13}}, got {:?}",
            opt.condition
        );
    };
    let TargetFilter::Typed(typed) = filter else {
        panic!("expected a Typed creature filter, got {filter:?}");
    };
    assert!(
        typed.type_filters.contains(&TypeFilter::Creature),
        "the counted objects must be creatures, got {:?}",
        typed.type_filters
    );
    assert!(
        !has_swallow(&parsed, "Condition_If"),
        "Blasphemous Edict's recognized alt-cost gate must not swallow a Condition_If: {:?}",
        parsed.parse_warnings
    );
}

/// Sandman's Quicksand parse: the "instead" clause lowers to a `sub_ability`
/// carrying `ConditionInstead{ CastVariantPaid{ Mayhem } }` (the Adipose-Offspring
/// Route-1 shape), and NO Condition_If. Revert-probe: revert the `conditions.rs`
/// membership filter to Emerge-only → the mayhem phrase is unrecognized →
/// `sub_ability.condition == None` (falls back to an unconditional sibling) +
/// Condition_If.
#[test]
fn sandman_parse_condition_instead_mayhem_no_condition_if() {
    let parsed = parse_oracle_text(
        SANDMANS_QUICKSAND,
        "Sandman's Quicksand",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let sub_condition = parsed
        .abilities
        .iter()
        .find_map(|a| a.sub_ability.as_ref().and_then(|s| s.condition.as_ref()))
        .expect("Sandman's second sentence must attach a conditional sub_ability");
    let dbg = format!("{sub_condition:?}");
    assert!(
        dbg.contains("ConditionInstead") && dbg.contains("Mayhem"),
        "the sub_ability condition must be ConditionInstead{{CastVariantPaid{{Mayhem}}}}, got {dbg}"
    );
    assert!(
        !has_swallow(&parsed, "Condition_If"),
        "Sandman's recognized mayhem-instead modal must not swallow a Condition_If: {:?}",
        parsed.parse_warnings
    );
}

/// Class-level combinator: the new bare "<type> on the battlefield" arm covers
/// the type-filter axis (creatures / artifacts) as a GE count, and does NOT
/// shadow the `parse_no_on_battlefield` `== 0` form. Revert-probe: remove the
/// `parse_type_count_on_battlefield` arm → the two "N or more" cases return Err.
#[test]
fn battlefield_type_count_combinator_and_no_shadow() {
    // (a) the Blasphemous form → ObjectCount(Creature) >= 13, fully consumed.
    let (rest, cond) =
        parse_inner_condition("there are thirteen or more creatures on the battlefield")
            .expect("the 'N or more <type> on the battlefield' arm must parse");
    assert!(rest.trim().is_empty(), "must fully consume, left: {rest:?}");
    let StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 13 },
    } = &cond
    else {
        panic!("expected ObjectCount(Creature) >= 13, got {cond:?}");
    };
    let TargetFilter::Typed(typed) = filter else {
        panic!("expected a Typed filter, got {filter:?}");
    };
    assert!(typed.type_filters.contains(&TypeFilter::Creature));

    // (b) type-filter axis coverage: the artifact sibling.
    let (rest, cond) = parse_inner_condition("there are two or more artifacts on the battlefield")
        .expect("the artifact sibling must parse");
    assert!(rest.trim().is_empty());
    let StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 2 },
    } = &cond
    else {
        panic!("expected ObjectCount(Artifact) >= 2, got {cond:?}");
    };
    let TargetFilter::Typed(typed) = filter else {
        panic!("expected a Typed filter, got {filter:?}");
    };
    assert!(typed.type_filters.contains(&TypeFilter::Artifact));

    // (c) no shadowing of `parse_no_on_battlefield`: the "no" form stays `== 0`.
    let (rest, cond) = parse_inner_condition("there are no creatures on the battlefield")
        .expect("the 'no <type> on the battlefield' form must still parse");
    assert!(rest.trim().is_empty());
    assert!(
        matches!(
            cond,
            StaticCondition::QuantityComparison {
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
                ..
            }
        ),
        "'there are no creatures on the battlefield' must stay the `== 0` form, got {cond:?}"
    );
}

// ══ Runtime ═══════════════════════════════════════════════════════════════════

/// Seed `n` vanilla creatures split across both players' battlefields (the count
/// is controller-agnostic, so split proves the "any controller" semantics).
fn seed_creatures(scenario: &mut GameScenario, n: usize) {
    for i in 0..n {
        let owner = if i % 2 == 0 { P0 } else { P1 };
        scenario.add_creature(owner, "Grizzly Bears", 2, 2);
    }
}

/// Build a Blasphemous Edict cast scenario with `n` creatures on the battlefield
/// and a mana pool of exactly {B} — insufficient for the printed {3}{B}{B}, so
/// the ONLY way the spell is castable is via the conditional {B} alternative.
fn blasphemous_scenario(n: usize) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let edict = scenario
        .add_spell_to_hand_from_oracle(P0, "Blasphemous Edict", false, BLASPHEMOUS_EDICT)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        })
        .id();
    seed_creatures(&mut scenario, n);
    scenario.with_mana_pool(P0, black(1));
    (scenario, edict)
}

/// B1 (positive) — with 13 creatures on the battlefield the conditional {B}
/// alternative cost is available, so Blasphemous Edict is castable even though its
/// printed cost is unpayable. Paired with the negative below (12 creatures →
/// unavailable), the difference proves the `option.condition` gate is load-bearing.
#[test]
fn blasphemous_alt_cost_available_at_thirteen_creatures() {
    let (scenario, edict) = blasphemous_scenario(13);
    let runner = scenario.build();
    assert!(
        can_cast_object_now(runner.state(), P0, edict),
        "13 creatures on the battlefield → the {{B}} alternative cost must be offered (castable)"
    );
}

/// B1 (negative) — with only 12 creatures the condition is unsatisfied, so the
/// {B} alternative is NOT offered; with the printed {3}{B}{B} unpayable (only {B}
/// in pool), the spell is not castable. Non-vacuous: the positive above is
/// castable at 13 on an otherwise identical board. Revert-probe: emitting the
/// option unconditionally (or dropping the condition) would make this castable at
/// 12 — the gate is what keeps it uncastable.
#[test]
fn blasphemous_alt_cost_unavailable_at_twelve_creatures() {
    let (scenario, edict) = blasphemous_scenario(12);
    let runner = scenario.build();
    assert!(
        !can_cast_object_now(runner.state(), P0, edict),
        "12 creatures → the {{B}} alternative must be withheld and the printed cost is unpayable"
    );
}

/// Put Sandman's Quicksand in P0's graveyard, discarded this turn (so its
/// intrinsic Mayhem grants a graveyard cast), with {3}{B} floating.
fn sandman_in_graveyard() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let sandman = scenario
        .add_spell_to_graveyard(P0, "Sandman's Quicksand", false)
        .from_oracle_text(SANDMANS_QUICKSAND)
        .id();
    scenario.with_mana_pool(P0, three_and_black());
    (scenario, sandman)
}

/// Cast `spell` through the real reducer (`GameAction::CastSpell`, auto mana
/// payment), capture the on-stack `CastingVariant` (the reach-guard proving WHICH
/// cast method was used), then drain the stack and materialize layers. Returns
/// the cast variant.
fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId) -> CastingVariant {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Sandman's Quicksand must begin casting");
    let variant = match &runner.state().stack[0].kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => *casting_variant,
        other => panic!("expected a Spell on the stack, got {other:?}"),
    };
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());
    variant
}

fn pt(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let o = &runner.state().objects[&id];
    (o.power.unwrap_or(0), o.toughness.unwrap_or(0))
}

/// S1 — mayhem cast → OPPONENTS ONLY. Cast Sandman from the graveyard via its
/// mayhem alternative cost (reach-guard: the on-stack `CastingVariant::Mayhem`);
/// on resolution only P1's (opponent's) creatures get -2/-2, while P0's
/// (controller's) creatures are untouched. Paired with S2 (hard cast shrinks BOTH
/// sides) the difference proves the cast-time Mayhem stamp + the `ConditionInstead`
/// swap are load-bearing. Revert-probe: remove the `casting_costs.rs` Mayhem stamp
/// → `cast_variant_paid` stays unset → the swap does not fire → the base "all
/// creatures -2/-2" runs and P0's creature ALSO shrinks to 1/1 (flipping the P0
/// assertion below).
#[test]
fn sandman_mayhem_cast_shrinks_opponents_only() {
    let (mut scenario, sandman) = sandman_in_graveyard();
    let mine = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let theirs = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();

    // Mayhem's discarded-this-turn gate (CR 702.187b).
    record_discard(runner.state_mut(), P0);
    record_card_discarded(runner.state_mut(), sandman);

    let variant = cast_and_resolve(&mut runner, sandman);

    // Reach-guard: the spell really was cast via Mayhem (not some fallback path).
    assert_eq!(
        variant,
        CastingVariant::Mayhem,
        "reach-guard: Sandman must have been cast using CastingVariant::Mayhem"
    );
    assert_eq!(
        runner.state().objects[&sandman].zone,
        Zone::Graveyard,
        "reach-guard: the sorcery must have resolved (into the graveyard)"
    );
    assert_eq!(
        pt(&runner, theirs),
        (1, 1),
        "opponent's creature must get -2/-2 (3/3 → 1/1) under the mayhem instead-clause"
    );
    assert_eq!(
        pt(&runner, mine),
        (3, 3),
        "controller's own creature must be UNTOUCHED under the mayhem instead-clause (opponents only)"
    );
}

/// S2 / S3 — hard cast → SYMMETRIC, and exactly -2/-2 (never -4/-4). Cast Sandman
/// for its printed {1}{B}{B} (reach-guard: the on-stack `CastingVariant::Normal`);
/// mayhem was not paid, so the base clause runs and ALL creatures (both sides) get
/// -2/-2. This is the non-vacuity partner of S1: P0's creature shrinks HERE but not
/// under mayhem, proving the S1 "untouched" result is a real veto. The exact (1,1)
/// on the opponent (not -4/-4) confirms the sub does not independently
/// double-resolve as a continuation (Q-Sandman-3b guard).
#[test]
fn sandman_hard_cast_shrinks_all_creatures_symmetrically() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let sandman = scenario
        .add_spell_to_hand_from_oracle(P0, "Sandman's Quicksand", false, SANDMANS_QUICKSAND)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        })
        .id();
    let mine = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let theirs = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    scenario.with_mana_pool(P0, {
        let mut pool = black(2);
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
        pool
    });
    let mut runner = scenario.build();

    let variant = cast_and_resolve(&mut runner, sandman);

    assert_ne!(
        variant,
        CastingVariant::Mayhem,
        "reach-guard: the hand cast must NOT be a mayhem cast (mayhem paid = false)"
    );
    assert_eq!(
        pt(&runner, mine),
        (1, 1),
        "hard cast: controller's own creature must get -2/-2 (base clause, symmetric)"
    );
    assert_eq!(
        pt(&runner, theirs),
        (1, 1),
        "hard cast: opponent's creature must get EXACTLY -2/-2 (not -4/-4 — no double resolution)"
    );
}

/// Drive Planar Collapse's upkeep trigger through the production path with
/// `n` creatures on the battlefield (the enchantment itself is NOT a creature),
/// returning `(planar_collapse_still_on_battlefield, creatures_remaining)`.
fn run_planar_collapse_upkeep(n: usize) -> (bool, usize) {
    let mut scenario = GameScenario::new();
    // Start at Untap so advancing into Upkeep fires the synthesized phase
    // trigger (CR 503.1a + CR 603.2).
    scenario.at_phase(Phase::Untap).with_life(P0, 20);
    let pc = scenario
        .add_creature(P0, "Planar Collapse", 0, 0)
        .as_enchantment()
        .from_oracle_text(PLANAR_COLLAPSE)
        .id();
    for _ in 0..n {
        scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    }
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.resolve_top();

    let pc_on = runner.state().battlefield.contains(&pc);
    let creatures = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|o| o.card_types.core_types.contains(&CoreType::Creature))
        .count();
    (pc_on, creatures)
}

/// N1 — Planar Collapse's upkeep intervening-if is enforced at RESOLUTION
/// (CR 603.4: the condition is re-checked as the ability resolves). The
/// trigger-level `TriggerCondition` is null (same as shipping cards like Armored
/// Kincaller); enforcement rides the ability's `AbilityCondition::QuantityCheck`
/// (ObjectCount(Creature) >= 4) that BB7's `parse_type_count_on_battlefield` arm
/// now produces — sufficient to kill the pre-BB7 unconditional-destroy bug.
///
/// Both poles run on near-identical boards (3 vs 4 vanilla creatures); the
/// enchantment doesn't count itself, so 3 is below and 4 is at the threshold.
/// Discriminating by construction: the pre-BB7 always-fire bug wipes at 3
/// (fails FIZZLE); an inverted condition would not wipe at 4 (fails FIRE).
#[test]
fn planar_collapse_upkeep_trigger_gates_on_creature_count() {
    // FIZZLE — 3 creatures (< 4): the resolution-time re-check is false, so the
    // whole effect is skipped (CR 603.4: ability removed from the stack, does
    // nothing).
    let (pc_on, creatures) = run_planar_collapse_upkeep(3);
    assert!(
        pc_on,
        "3 creatures: condition 3<4 false at resolution → Planar Collapse must survive"
    );
    assert_eq!(
        creatures, 3,
        "3 creatures: destroy-all must be skipped → all 3 creatures survive"
    );

    // FIRE — 4 creatures (>= 4): the condition holds, so the enchantment is
    // sacrificed and every creature is destroyed.
    let (pc_on, creatures) = run_planar_collapse_upkeep(4);
    assert!(
        !pc_on,
        "4 creatures: condition 4>=4 true → Planar Collapse must be sacrificed"
    );
    assert_eq!(
        creatures, 0,
        "4 creatures: destroy-all must wipe every creature on the battlefield"
    );
}
