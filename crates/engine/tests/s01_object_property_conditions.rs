//! GROUP A1 (S01) — reflexive object-property conditions driven through the real
//! parse + resolution pipeline. Three Oracle shapes, each a class:
//!
//!   1. Yarus, Roar of the Old Gods — "..., then turn it face up" (CR 708.7): the
//!      face-toggle clause splits off so the dies-trigger gates `ChangeZone`
//!      (return face down) on "if it's a permanent card" and chains `TurnFaceUp`.
//!   2. Amalia Benavides Aguirre — "destroy all other creatures if its power is
//!      exactly 20" (CR 201.5 + CR 208.1): the possessive "its" names the source
//!      (Amalia), so the gate is `QuantityCheck{Power{Source}, EQ, 20}`.
//!   3. Reptilian Recruiter — "If that creature's power is 2 or less or if you
//!      control another Lizard, gain control ..." (CR 115.1 + CR 608.2c): an
//!      " or if " disjunction whose first disjunct binds the chosen TARGET's
//!      power (not the source), the second a controlled-Lizard presence count.
//!
//! Runtime tests drive `GameRunner::cast(..).resolve()` (the real cast +
//! trigger-resolution pipeline) and assert the gated effect fires ONLY when its
//! condition holds — the negative case is the discriminator. Reverting the
//! recognizer makes the condition `null` (fires unconditionally) or mis-scopes
//! the subject (Source vs Target), and the discriminating assertion flips.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, Comparator, Effect, FilterProp, ObjectScope, PtStat,
    PtValueScope, QuantityExpr, QuantityRef, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verified identical to the engine's authoritative card data (MTGJSON AtomicCards).
const YARUS: &str = "Other creatures you control have haste.\n\
     Whenever one or more face-down creatures you control deal combat damage to a player, draw a card.\n\
     Whenever a face-down creature you control dies, return it to the battlefield face down under its owner's control if it's a permanent card, then turn it face up.";
const AMALIA: &str = "Ward—Pay 3 life.\n\
     Whenever you gain life, Amalia Benavides Aguirre explores. Then destroy all other creatures if its power is exactly 20.";
const REPTILIAN: &str = "Trample\n\
     When this creature enters, choose target creature. If that creature's power is 2 or less or if you control another Lizard, gain control of that creature until end of turn, untap it, and it gains haste until end of turn.";

fn ct() -> Vec<String> {
    vec!["Creature".to_string()]
}

/// Depth-first walk of an ability + its `sub_ability`/`else_ability` chain,
/// returning the first node whose top-level effect discriminant matches `pred`.
fn find_node<'a>(
    def: &'a AbilityDefinition,
    pred: &dyn Fn(&Effect) -> bool,
) -> Option<&'a AbilityDefinition> {
    if pred(&def.effect) {
        return Some(def);
    }
    def.sub_ability
        .as_deref()
        .and_then(|s| find_node(s, pred))
        .or_else(|| def.else_ability.as_deref().and_then(|s| find_node(s, pred)))
}

fn first_execute(p: &engine::parser::oracle::ParsedAbilities) -> &AbilityDefinition {
    p.triggers
        .iter()
        .find_map(|t| t.execute.as_deref())
        .expect("trigger with an execute ability")
}

// ===========================================================================
// PARSER (production `parse_oracle_text`) — structural gates. Each assertion
// flips when the corresponding recognizer is reverted.
// ===========================================================================

/// Yarus — the dies-trigger must (a) gate the return on "it's a permanent card"
/// (`TargetMatchesFilter{Permanent}`) and (b) chain `TurnFaceUp`. Reverting the
/// "turn <obj> face up" clause-starter leaves "then turn it face up" glued to the
/// return clause: the split never happens, so neither the condition nor the
/// TurnFaceUp sub-ability is produced.
#[test]
fn yarus_dies_trigger_gates_return_and_chains_turn_face_up() {
    let p = parse_oracle_text(
        YARUS,
        "Yarus, Roar of the Old Gods",
        &[],
        &ct(),
        &["Centaur".into(), "Druid".into()],
    );
    let dies = p
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find(|e| matches!(*e.effect, Effect::ChangeZone { .. }))
        .expect("Yarus must have a ChangeZone (return-to-battlefield) dies trigger");

    // (a) The return is gated on "if it's a permanent card" → permanent filter.
    match dies.condition.as_ref() {
        Some(AbilityCondition::TargetMatchesFilter {
            filter: TargetFilter::Typed(tf),
            use_lki,
        }) => {
            assert!(
                !use_lki,
                "present-tense \"it's a permanent card\" reads live state"
            );
            assert!(
                tf.type_filters
                    .iter()
                    .any(|f| format!("{f:?}").contains("Permanent")),
                "return must be gated on a Permanent card-type filter, got {tf:?}"
            );
        }
        other => panic!("Yarus return must carry a permanent-card gate, got {other:?}"),
    }

    // (b) The "then turn it face up" tail splits off into a TurnFaceUp sub-ability.
    assert!(
        find_node(dies, &|e| matches!(e, Effect::TurnFaceUp { .. })).is_some(),
        "Yarus must chain a TurnFaceUp sub-ability; got chain:\n{dies:#?}"
    );
}

/// Amalia — the DestroyAll must be gated on the SOURCE's power being exactly 20.
/// The `scope == Source` assertion is the CostPaidObject-regression discriminator;
/// reverting the `exactly` arm drops the gate to `null` entirely.
#[test]
fn amalia_destroy_all_gated_on_source_power_exactly_20() {
    let p = parse_oracle_text(
        AMALIA,
        "Amalia Benavides Aguirre",
        &[],
        &ct(),
        &["Vampire".into(), "Scout".into()],
    );
    let destroy = find_node(first_execute(&p), &|e| {
        matches!(e, Effect::DestroyAll { .. })
    })
    .expect("Amalia must produce a DestroyAll");
    assert_eq!(
        destroy.condition,
        Some(AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Source,
                },
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 20 },
        }),
        "DestroyAll must be gated on Power{{Source}} EQ 20 (Source scope, not CostPaidObject)"
    );
}

/// Reptilian — the GainControl chain must be gated on the " or if " disjunction:
/// `Or[ TargetMatchesFilter{PtComparison{Power,Current,LE,2}}, QuantityCheck{
/// ObjectCount GE 1} ]`. The first disjunct binds the chosen TARGET's power
/// (CR 115.1) — NOT the source. Reverting `parse_or_if_disjunction` drops the gate
/// to `null`; reverting the possessive-P/T arm mis-scopes disjunct A to
/// `Power{CostPaidObject}` (a QuantityCheck, not a TargetMatchesFilter).
#[test]
fn reptilian_gain_control_gated_on_or_if_disjunction() {
    let p = parse_oracle_text(
        REPTILIAN,
        "Reptilian Recruiter",
        &[],
        &ct(),
        &["Lizard".into(), "Warrior".into()],
    );
    let gain = find_node(first_execute(&p), &|e| {
        matches!(e, Effect::GainControl { .. })
    })
    .expect("Reptilian must produce a GainControl");
    let conditions = match gain.condition.as_ref() {
        Some(AbilityCondition::Or { conditions }) => conditions,
        other => panic!("GainControl must be gated on an Or disjunction, got {other:?}"),
    };
    assert_eq!(
        conditions.len(),
        2,
        "two disjuncts: power-≤2 OR another-Lizard"
    );

    // Disjunct A: TARGET's current power ≤ 2 (Target scope via TargetMatchesFilter).
    match &conditions[0] {
        AbilityCondition::TargetMatchesFilter {
            filter: TargetFilter::Typed(tf),
            use_lki,
        } => {
            assert!(!use_lki);
            assert!(
                tf.properties.contains(&FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 2 },
                }),
                "disjunct A must be the target's current power ≤ 2, got {tf:?}"
            );
        }
        other => panic!("disjunct A must be a target P/T comparison, got {other:?}"),
    }

    // Disjunct B: control ≥ 1 other Lizard (object-count presence).
    assert!(
        matches!(
            &conditions[1],
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. }
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        ),
        "disjunct B must be ObjectCount ≥ 1, got {:?}",
        conditions[1]
    );
}

// ===========================================================================
// RUNTIME (production cast/resolve pipeline) — behavioral discriminators.
// ===========================================================================

fn controller_of(state: &GameState, obj: ObjectId) -> PlayerId {
    state.objects.get(&obj).expect("object exists").controller
}

/// Reptilian A-branch — target power 2, no other Lizard. Disjunct A (target power
/// ≤ 2) fires; P0 gains control. Reptilian's own power is 4, so a Source-scoped
/// reading (4 ≤ 2 = false) would NOT fire — control change PROVES Target scope.
#[test]
fn reptilian_a_branch_target_power_le_2_gains_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let recruiter = scenario
        .add_creature_to_hand_from_oracle(P0, "Reptilian Recruiter", 4, 2, REPTILIAN)
        .id();
    let victim = scenario.add_creature(P1, "Power Two", 2, 2).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(recruiter).target_object(victim).resolve();

    assert_eq!(
        controller_of(outcome.state(), victim),
        P0,
        "target power 2 satisfies disjunct A (TARGET power ≤ 2) → P0 gains control"
    );
}

/// Reptilian B-branch — target power 5 (disjunct A false), but P0 controls ANOTHER
/// Lizard (disjunct B true) → P0 gains control.
#[test]
fn reptilian_b_branch_other_lizard_gains_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let recruiter = scenario
        .add_creature_to_hand_from_oracle(P0, "Reptilian Recruiter", 4, 2, REPTILIAN)
        .id();
    scenario
        .add_creature(P0, "Pet Lizard", 1, 1)
        .with_subtypes(vec!["Lizard"]);
    let victim = scenario.add_creature(P1, "Power Five", 5, 5).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(recruiter).target_object(victim).resolve();

    assert_eq!(
        controller_of(outcome.state(), victim),
        P0,
        "another controlled Lizard satisfies disjunct B → P0 gains control"
    );
}

/// Reptilian negative (discriminator) — target power 5, NO other Lizard. Both
/// disjuncts false → P0 does NOT gain control. With a null gate (reverted
/// recognizer) the GainControl would fire unconditionally and this would fail.
#[test]
fn reptilian_negative_no_disjunct_no_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let recruiter = scenario
        .add_creature_to_hand_from_oracle(P0, "Reptilian Recruiter", 4, 2, REPTILIAN)
        .id();
    let victim = scenario.add_creature(P1, "Power Five", 5, 5).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(recruiter).target_object(victim).resolve();

    assert_eq!(
        controller_of(outcome.state(), victim),
        P1,
        "neither disjunct holds → P0 must NOT gain control (gate must be live)"
    );
}

// ---------------------------------------------------------------------------
// G2 — TRIGGER-referential sibling. "Whenever a creature dies, if that creature's
// power is 2 or less, draw a card." The "that creature" anaphor has NO chosen
// target, so `TargetMatchesFilter` must fall back to the TriggeringSource (the
// dead creature) — proving the CostPaidObject→Target scope flip still resolves
// against trigger subjects, not just chosen targets.
// ---------------------------------------------------------------------------

const DEATHWATCHER: &str =
    "Whenever a creature dies, if that creature's power is 2 or less, draw a card.";
const DOOM: &str = "Destroy target creature.";

/// A power-2 death satisfies the TriggeringSource-resolved "≤ 2" gate → draw.
#[test]
fn trigger_referential_power_le_2_draws_via_triggering_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Deathwatcher", 3, 3, DEATHWATCHER);
    let victim = scenario.add_creature(P0, "Small One", 2, 2).id();
    scenario.add_spell_to_library_top(P0, "Draw Filler", true);
    let doom = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", true, DOOM)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(doom).target_object(victim).resolve();

    outcome.assert_zone(&[victim], Zone::Graveyard);
    assert_eq!(
        outcome.hand_drawn(P0),
        1,
        "power-2 death must satisfy the TriggeringSource-resolved gate and draw a card"
    );
}

/// A power-3 death fails the ≤2 gate; no draw. Together with the positive this
/// proves the trigger-referential gate is live (not null) after the scope flip.
#[test]
fn trigger_referential_power_gt_2_does_not_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Deathwatcher", 3, 3, DEATHWATCHER);
    let victim = scenario.add_creature(P0, "Big One", 3, 3).id();
    scenario.add_spell_to_library_top(P0, "Draw Filler", true);
    let doom = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", true, DOOM)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(doom).target_object(victim).resolve();

    outcome.assert_zone(&[victim], Zone::Graveyard);
    assert_eq!(
        outcome.hand_drawn(P0),
        0,
        "power-3 death must NOT satisfy the ≤2 gate (gate must be live)"
    );
}

// ---------------------------------------------------------------------------
// Amalia — runtime Source-scope discriminator. Amalia explores (top card is a
// land → no counter, power unchanged), then destroys all OTHER creatures iff her
// power is exactly 20. A controller life-gain fires the trigger.
// ---------------------------------------------------------------------------

const GAIN_LIFE: &str = "You gain 1 life.";

fn amalia_runner(amalia_power: i32) -> (ObjectId, ObjectId, GameRunner) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Amalia Benavides Aguirre", amalia_power, 2, AMALIA);
    // Explore reveals the top card. A LAND goes to hand with NO +1/+1 counter, so
    // Amalia's power is unchanged when the destroy gate is evaluated. The generic
    // library card is untyped by default, so type it Land explicitly.
    let land = scenario.add_card_to_library_top(P0, "Forest");
    let bystander = scenario.add_creature(P1, "Bystander", 3, 3).id();
    let gain = scenario
        .add_spell_to_hand_from_oracle(P0, "Gain Life", true, GAIN_LIFE)
        .id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
    }
    (bystander, gain, runner)
}

/// Amalia power EXACTLY 20 → the life-gain trigger explores (top land, power
/// stays 20) then destroys all other creatures. The bystander dies.
#[test]
fn amalia_destroys_others_when_power_exactly_20() {
    let (bystander, gain, mut runner) = amalia_runner(20);
    let outcome = runner.cast(gain).resolve();
    outcome.assert_zone(&[bystander], Zone::Graveyard);
}

/// Amalia power 19 (discriminator) → gate fails; the bystander survives. The
/// Source-scope positive+negative pair is the discriminator a CostPaidObject
/// 0-resolution cannot reproduce (that would be false in BOTH cases).
#[test]
fn amalia_spares_others_when_power_not_20() {
    let (bystander, gain, mut runner) = amalia_runner(19);
    let outcome = runner.cast(gain).resolve();
    outcome.assert_zone(&[bystander], Zone::Battlefield);
}
