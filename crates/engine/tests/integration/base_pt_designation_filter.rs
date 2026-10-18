//! Base-P/T designation as an object filter: "<creature filter> with base power
//! and toughness N/M" (Duskana, the Rage Mother — issue #6767; Bess, Soul
//! Nourisher; Andrios, Roaming Explorer).
//!
//! CR 208.4b: a check of a creature's base power and toughness sees its P/T
//! after characteristic-defining abilities (CR 613.4a) and setting effects
//! (CR 613.4b), but ignores counters and other modifying effects (CR 613.4c).
//! The filter lowers to two `PtValueScope::Base` exact comparisons; these tests
//! prove the runtime reads that value through every grammar seam the parser
//! wires: trigger subjects (CR 508.1m + CR 603.2 attack triggers, enter
//! triggers), effect targets, for-each counts, and static subjects
//! (CR 611.3a). "You control" binds to the ability source's controller
//! (CR 109.5).
//!
//! Every runtime test drives the real pipeline: `DeclareAttackers` through
//! `apply()` and trigger resolution, or a cast through `GameRunner::cast`. Every
//! card uses its verbatim Oracle text.

use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    Comparator, ContinuousModification, ControllerRef, Effect, FilterProp, PtStat, PtValueScope,
    QuantityExpr, QuantityRef, StaticCondition, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const DUSKANA_NAME: &str = "Duskana, the Rage Mother";
const DUSKANA_ORACLE: &str = "When Duskana enters, draw a card for each creature you control with base power and toughness 2/2.\nWhenever a creature you control with base power and toughness 2/2 attacks, it gets +3/+3 until end of turn.";

const BESS_NAME: &str = "Bess, Soul Nourisher";
const BESS_ORACLE: &str = "Whenever one or more other creatures you control with base power and toughness 1/1 enter, put a +1/+1 counter on Bess.\nWhenever Bess attacks, each other creature you control with base power and toughness 1/1 gets +X/+X until end of turn, where X is the number of +1/+1 counters on Bess.";

const ANDRIOS_NAME: &str = "Andrios, Roaming Explorer";
const ANDRIOS_ORACLE: &str = "Reach\nAs long as Andrios is attacking, tapped creatures you control with base power and toughness 4/3 have base power and toughness 16/9.\n{T}: Add {W}{U}{B}{R}{G}. Creature spells you spend this mana to cast have their base power and toughness become 4/3.";

const KUDO_NAME: &str = "Kudo, King Among Bears";
const KUDO_ORACLE: &str =
    "Other creatures have base power and toughness 2/2 and are Bears in addition to their other types.";

/// Current (post-layer) power and toughness of `id`.
fn pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let o = &runner.state().objects[&id];
    (
        o.power.expect("creature has power"),
        o.toughness.expect("creature has toughness"),
    )
}

/// CR 508.1m: declare `attackers` against P1, order this controller's triggers,
/// return the number of stack entries (reach guard), then resolve the stack.
fn attack(runner: &mut GameRunner, attackers: &[ObjectId]) -> usize {
    runner.advance_to_combat();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|a| (*a, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("attackers are declared");
    drain_order_triggers_with_identity(runner.state_mut());
    let entries = runner.state().stack.len();
    runner.advance_until_stack_empty();
    entries
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn base_eq(stat: PtStat, value: i32) -> FilterProp {
    FilterProp::PtComparison {
        stat,
        scope: PtValueScope::Base,
        comparator: Comparator::EQ,
        value: QuantityExpr::Fixed { value },
    }
}

fn has_base_pt(tf: &TypedFilter, power: i32, toughness: i32) -> bool {
    tf.properties.contains(&base_eq(PtStat::Power, power))
        && tf
            .properties
            .contains(&base_eq(PtStat::Toughness, toughness))
}

/// R1 — CR 208.4b + CR 508.1m + CR 603.2c: Duskana's attack trigger fires once
/// per attacking creature you control whose BASE P/T is 2/2 (counters ignored)
/// and pumps that attacker.
#[test]
fn duskana_attack_pumps_only_base_two_two_attackers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, DUSKANA_NAME, 5, 5, DUSKANA_ORACLE);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let grown = scenario
        .add_creature(P0, "Grown Bear", 2, 2)
        .with_plus_counters(1)
        .id();
    let squirrel = scenario
        .add_creature(P0, "Squirrel", 1, 1)
        .with_plus_counters(1)
        .id();
    let ogre = scenario.add_creature(P0, "Ogre", 3, 3).id();
    let mut runner = scenario.build();

    // Hostile pre-attack guard: Squirrel's CURRENT P/T is 2/2, Grown's is 3/3.
    assert_eq!(pt(&mut runner, squirrel), (2, 2));
    assert_eq!(pt(&mut runner, grown), (3, 3));

    let entries = attack(&mut runner, &[bear, grown, squirrel, ogre]);
    assert_eq!(
        entries, 2,
        "CR 508.1m + CR 603.2c: one trigger per attacker with base P/T 2/2 (Bear, Grown Bear)"
    );
    assert_eq!(
        (
            pt(&mut runner, bear),
            pt(&mut runner, grown),
            pt(&mut runner, squirrel),
            pt(&mut runner, ogre),
        ),
        ((5, 5), (6, 6), (2, 2), (3, 3)),
        "CR 208.4b: counters do not change base P/T; each trigger pumps its own attacker"
    );
}

/// R2 — CR 109.5: "a creature you control" binds to the trigger source's
/// controller; the opponent's Duskana does not trigger on P0's attacker.
#[test]
fn duskana_you_control_scopes_to_trigger_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, DUSKANA_NAME, 5, 5, DUSKANA_ORACLE);
    scenario.add_creature_from_oracle(P1, DUSKANA_NAME, 5, 5, DUSKANA_ORACLE);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    let entries = attack(&mut runner, &[bear]);
    assert_eq!(
        entries, 1,
        "CR 109.5: only the attacking player's Duskana triggers"
    );
    assert_eq!(
        pt(&mut runner, bear),
        (5, 5),
        "CR 109.5: one +3/+3, not one per Duskana"
    );
}

/// R3 — CR 208.4b + CR 613.4b: base P/T is the value after layer-7b setting
/// effects. Kudo sets the printed 3/3 Ogre's base P/T to 2/2, so it qualifies.
#[test]
fn duskana_reads_layer7b_set_base_pt_kudo() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, DUSKANA_NAME, 5, 5, DUSKANA_ORACLE);
    scenario.add_creature_from_oracle(P0, KUDO_NAME, 2, 2, KUDO_ORACLE);
    let ogre = scenario.add_creature(P0, "Ogre", 3, 3).id();
    let mut runner = scenario.build();

    assert_eq!(
        pt(&mut runner, ogre),
        (2, 2),
        "reach guard: CR 613.4b — Kudo sets the Ogre's base P/T to 2/2"
    );
    let entries = attack(&mut runner, &[ogre]);
    assert_eq!(entries, 1, "CR 208.4b: a layer-7b set base 2/2 qualifies");
    assert_eq!(pt(&mut runner, ogre), (5, 5));
}

/// R4 — CR 208.4b + CR 109.5: Duskana's enter trigger draws one card per
/// creature its controller controls with base P/T 2/2, counted at resolution.
#[test]
fn duskana_etb_draws_per_base_two_two_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
    let dusk = scenario
        .add_creature_to_hand_from_oracle(P0, DUSKANA_NAME, 5, 5, DUSKANA_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.add_creature(P0, "Bear", 2, 2);
    scenario
        .add_creature(P0, "Grown Bear", 2, 2)
        .with_plus_counters(1);
    scenario
        .add_creature(P0, "Squirrel", 1, 1)
        .with_plus_counters(1);
    scenario.add_creature(P0, "Ogre", 3, 3);
    scenario.add_creature(P1, "Enemy Bear", 2, 2);
    let mut runner = scenario.build();

    let out = runner.cast(dusk).resolve();
    out.assert_zone(&[dusk], Zone::Battlefield);
    assert_eq!(
        out.hand_drawn(P0),
        2,
        "CR 208.4b + CR 109.5: Bear and Grown Bear (base 2/2 with a counter) count; \
         the base-1/1 Squirrel, the 3/3, Duskana itself and the opponent's 2/2 do not"
    );
}

/// R5 — CR 208.4b + CR 603.2: Bess's enter trigger fires for another creature
/// you control with base P/T 1/1 and not for a 2/2.
#[test]
fn bess_enter_counts_only_base_one_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bess = scenario
        .add_creature_from_oracle(P0, BESS_NAME, 1, 1, BESS_ORACLE)
        .id();
    let soldier = scenario
        .add_creature_to_hand(P0, "Soldier", 1, 1)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner
        .cast(soldier)
        .resolve()
        .assert_zone(&[soldier], Zone::Battlefield);
    assert_eq!(
        plus_counters(&runner, bess),
        1,
        "CR 208.4b: a base-1/1 creature entering puts a counter on Bess"
    );

    runner
        .cast(bear)
        .resolve()
        .assert_zone(&[bear], Zone::Battlefield);
    assert_eq!(
        plus_counters(&runner, bess),
        1,
        "CR 208.4b: a base-2/2 creature entering does not trigger Bess"
    );
}

/// R5b — CR 208.4b + CR 613.4b + CR 611.3c: the entering creature's base P/T is read after
/// layer-7b setting effects. Under Kudo a printed 1/1 enters with base P/T 2/2,
/// so Bess does not trigger; the first Soldier, cast before Kudo, does.
#[test]
fn bess_enter_reads_layer7b_set_base_pt_of_entering_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bess = scenario
        .add_creature_from_oracle(P0, BESS_NAME, 1, 1, BESS_ORACLE)
        .id();
    let first = scenario
        .add_creature_to_hand(P0, "Soldier", 1, 1)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let kudo = scenario
        .add_creature_to_hand_from_oracle(P0, KUDO_NAME, 2, 2, KUDO_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let second = scenario
        .add_creature_to_hand(P0, "Soldier", 1, 1)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner
        .cast(first)
        .resolve()
        .assert_zone(&[first], Zone::Battlefield);
    assert_eq!(
        plus_counters(&runner, bess),
        1,
        "reach guard: a printed base-1/1 creature triggers Bess"
    );

    runner
        .cast(kudo)
        .resolve()
        .assert_zone(&[kudo], Zone::Battlefield);
    runner
        .cast(second)
        .resolve()
        .assert_zone(&[second], Zone::Battlefield);
    assert_eq!(
        pt(&mut runner, second),
        (2, 2),
        "reach guard: CR 613.4b — Kudo sets the entering Soldier's base P/T to 2/2"
    );
    assert_eq!(
        plus_counters(&runner, bess),
        1,
        "CR 208.4b + CR 611.3c: a creature entering with a layer-7b set base 2/2 does not trigger Bess"
    );
}

/// R6 — CR 208.4b: Bess's attack pump applies only to other creatures you
/// control with base P/T 1/1, with X = the +1/+1 counters on Bess.
#[test]
fn bess_attack_pumps_only_base_one_one_others() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bess = scenario
        .add_creature_from_oracle(P0, BESS_NAME, 1, 1, BESS_ORACLE)
        .with_plus_counters(2)
        .id();
    let soldier = scenario.add_creature(P0, "Soldier", 1, 1).id();
    let grown = scenario
        .add_creature(P0, "Grown Soldier", 1, 1)
        .with_plus_counters(1)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let enemy = scenario.add_creature(P1, "Enemy Soldier", 1, 1).id();
    let mut runner = scenario.build();

    let entries = attack(&mut runner, &[bess]);
    assert_eq!(entries, 1, "reach guard: Bess's attack trigger fires");
    assert_eq!(
        (
            pt(&mut runner, soldier),
            pt(&mut runner, grown),
            pt(&mut runner, bear),
            pt(&mut runner, enemy),
        ),
        ((3, 3), (4, 4), (2, 2), (1, 1)),
        "CR 208.4b + CR 109.5: only your other base-1/1 creatures get +2/+2"
    );
}

/// R7 — CR 208.4b + CR 613.4b + CR 611.3a: while Andrios attacks, tapped
/// creatures you control whose base P/T is 4/3 have base P/T 16/9. Untapped,
/// differently-based, and counter-made 4/3 creatures are unaffected.
#[test]
fn andrios_static_targets_tapped_base_four_three() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let andrios = scenario
        .add_creature_from_oracle(P0, ANDRIOS_NAME, 4, 3, ANDRIOS_ORACLE)
        .id();
    let tapped_43 = scenario.add_creature(P0, "Tapped Scout", 4, 3).id();
    let untapped_43 = scenario.add_creature(P0, "Untapped Scout", 4, 3).id();
    let tapped_bear = scenario.add_creature(P0, "Tapped Bear", 2, 2).id();
    let grown = scenario
        .add_creature(P0, "Grown Scout", 3, 2)
        .with_plus_counters(1)
        .id();
    let mut runner = scenario.build();
    for id in [tapped_43, tapped_bear, grown] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }

    // Reach guard: before Andrios attacks the static is off.
    assert_eq!(pt(&mut runner, tapped_43), (4, 3));
    assert_eq!(pt(&mut runner, grown), (4, 3));

    attack(&mut runner, &[andrios]);
    assert_eq!(
        (
            pt(&mut runner, andrios),
            pt(&mut runner, tapped_43),
            pt(&mut runner, untapped_43),
            pt(&mut runner, tapped_bear),
            pt(&mut runner, grown),
        ),
        ((16, 9), (16, 9), (4, 3), (2, 2), (4, 3)),
        "CR 208.4b + CR 613.4b + CR 508.1f: only tapped creatures you control with base 4/3 \
         (Andrios, tapped by attacking, and the tapped 4/3) become 16/9"
    );
}

/// R8 — CR 208.4b + CR 208.3: an Or type-disjunction's base-P/T designation
/// binds whole to every creature disjunct; a base 2/3 creature satisfies neither
/// leg. Synthetic text (no printed card has this shape — latent generic grammar,
/// PR #9653 review). Reverting the whole-group distribution leaves the plain
/// creature leg with base power only, so the base 2/3 Wall matches.
#[test]
fn or_disjunction_base_pt_designation_rejects_base_two_three() {
    let parsed = parse_oracle_text(
        "Destroy target creature or artifact creature with base power and toughness 2/2.",
        "Designation Probe",
        &[],
        &["Instant".to_owned()],
        &[],
    );
    assert!(
        parsed.parse_warnings.is_empty(),
        "no swallowed clause: {:?}",
        parsed.parse_warnings
    );
    let target = parsed
        .abilities
        .iter()
        .find_map(|a| match a.effect.as_ref() {
            Effect::Destroy { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("a Destroy spell ability");
    assert!(
        matches!(target, TargetFilter::Or { .. }),
        "expected an Or target, got {target:?}"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let wall = scenario.add_creature(P0, "Wall", 2, 3).id();
    let grown = scenario
        .add_creature(P0, "Grown Bear", 2, 2)
        .with_plus_counters(1)
        .id();
    let construct = scenario
        .add_creature(P0, "Construct", 2, 2)
        .as_artifact_creature()
        .id();
    let golem = scenario
        .add_creature(P0, "Golem", 2, 3)
        .as_artifact_creature()
        .id();
    let mut runner = scenario.build();

    // Hostile guard: Grown Bear's CURRENT P/T is 3/3, its base P/T 2/2.
    assert_eq!(pt(&mut runner, grown), (3, 3));

    let state = runner.state();
    let ctx = FilterContext::from_source(state, bear);
    let matches = |id: ObjectId| matches_target_filter(state, id, &target, &ctx);
    assert!(
        matches(bear),
        "CR 208.4b: a base 2/2 creature satisfies the creature leg"
    );
    assert!(
        matches(grown),
        "CR 208.4b: counters do not change base P/T, so base 2/2 still matches"
    );
    assert!(
        matches(construct),
        "CR 208.4b: a base 2/2 artifact creature satisfies the designation"
    );
    assert!(
        !matches(wall),
        "CR 208.4b: a base 2/3 creature must not satisfy base power and toughness 2/2"
    );
    assert!(
        !matches(golem),
        "CR 208.4b: a base 2/3 artifact creature must not satisfy base power and toughness 2/2"
    );
}

/// S1 (SHAPE) — Duskana parses with no warnings: an Attacks trigger whose
/// subject carries both base-scope props, and a dynamic draw count.
#[test]
fn duskana_parses_attack_filter_and_dynamic_draw_shape() {
    let parsed = parse_oracle_text(
        DUSKANA_ORACLE,
        DUSKANA_NAME,
        &[],
        &["Creature".to_owned()],
        &["Bear".to_owned()],
    );
    assert!(
        parsed.parse_warnings.is_empty(),
        "no swallowed clause: {:?}",
        parsed.parse_warnings
    );

    let attack = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Attacks)
        .expect("an Attacks trigger");
    match attack.valid_card.as_ref() {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
            assert!(has_base_pt(tf, 2, 2), "valid_card: {tf:?}");
        }
        other => panic!("expected a Typed valid_card, got {other:?}"),
    }

    let draw_count = parsed
        .triggers
        .iter()
        .find_map(|t| match t.execute.as_deref().map(|a| a.effect.as_ref()) {
            Some(Effect::Draw { count, .. }) => Some(count.clone()),
            _ => None,
        })
        .expect("an enter trigger that draws");
    match draw_count {
        QuantityExpr::Ref {
            qty:
                QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(tf),
                },
        } => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(has_base_pt(&tf, 2, 2), "draw count filter: {tf:?}");
        }
        other => panic!("expected an ObjectCount draw count, got {other:?}"),
    }
}

/// S2 (SHAPE) — Andrios's static affects tapped creatures you control with base
/// 4/3 and sets base P/T 16/9 while Andrios is attacking.
#[test]
fn andrios_static_shape() {
    let parsed = parse_oracle_text(
        ANDRIOS_ORACLE,
        ANDRIOS_NAME,
        &["Reach".to_owned()],
        &["Artifact".to_owned(), "Creature".to_owned()],
        &["Wizard".to_owned()],
    );
    let def = parsed
        .statics
        .iter()
        .find(|s| s.condition == Some(StaticCondition::SourceIsAttacking))
        .expect("the attacking-gated static");
    match def.affected.as_ref() {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.contains(&FilterProp::Tapped));
            assert!(has_base_pt(tf, 4, 3), "affected: {tf:?}");
        }
        other => panic!("expected a Typed affected filter, got {other:?}"),
    }
    assert_eq!(
        def.modifications,
        vec![
            ContinuousModification::SetPower { value: 16 },
            ContinuousModification::SetToughness { value: 9 },
        ]
    );
}
