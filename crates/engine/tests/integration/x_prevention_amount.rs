//! Announced-X prevention amounts (issue #10495) — "prevent the next X damage"
//! shields sized by the `{X}` announced at cast/activation time.
//!
//! Verbatim Oracle text under test (MTGJSON/Scryfall). Every card's exact text
//! is used; no paraphrases, no synthetic cards.
//!
//! The parser fix carries the announced X in `Effect::PreventDamage.amount_dynamic`
//! and `prevent_damage::resolve` materializes it through
//! `resolve_quantity_with_targets`, the `chosen_x`-aware resolver. These rows
//! drive the real cast/activation pipelines and then the real damage event
//! (`deal_damage::resolve`, exactly as `awe_strike_prevention.rs` does for the
//! producer side), and would fail with a fixed `Next(1)` shield.
//!
//! CR 107.3a + CR 107.3i + CR 107.3k + CR 615.1a + CR 615.3 + CR 615.7.

use std::io::Cursor;

use engine::database::CardDatabase;
use engine::game::effects::deal_damage;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, Effect, PreventionAmount, QuantityExpr, QuantityRef, ResolvedAbility,
    ShieldKind, TargetFilter, TargetRef,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ALABASTER_POTION: &str = "Choose one —\n• Target player gains X life.\n• Prevent the next X damage that would be dealt to any target this turn.";
const ATALYA: &str = "{X}, {T}: Choose one —\n• Prevent the next X damage that would be dealt to target creature this turn. Spend only white mana on X.\n• You gain X life. Spend only white mana on X.";
const PANACEA: &str =
    "{X}{X}, {T}: Prevent the next X damage that would be dealt to any target this turn.";
const VENGEFUL_ARCHON: &str = "Flying\n{X}: Prevent the next X damage that would be dealt to you this turn. If damage is prevented this way, this creature deals that much damage to target player or planeswalker.";
const TEMPER: &str = "Prevent the next X damage that would be dealt to target creature this turn. For each 1 damage prevented this way, put a +1/+1 counter on that creature.";
const BATTLEFIELD_MEDIC: &str =
    "{T}: Prevent the next X damage that would be dealt to target creature this turn, where X is the number of Clerics on the battlefield.";

fn mana(kind: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn damage_ability(source_id: ObjectId, target: TargetRef, amount: i32) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![target],
        source_id,
        P1,
    )
}

/// Alabaster Potion's printed cost, `{X}{W}{W}`. `add_spell_to_hand_from_oracle`
/// attaches no cost, so without this the pending cast has no X component and
/// the announced X is never prompted.
fn alabaster_potion_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::White, ManaCostShard::White],
        generic: 0,
    }
}

fn variable_x() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::Variable {
            name: "X".to_string(),
        },
    }
}

/// Prevention shields currently installed in the floating registry. Player
/// targets and untargeted shields land there; object-hosted shields do not.
fn prevention_shields(runner: &GameRunner) -> Vec<PreventionAmount> {
    runner
        .state()
        .pending_damage_replacements
        .iter()
        .filter_map(|shield| match shield.shield_kind {
            ShieldKind::Prevention { amount } => Some(amount),
            _ => None,
        })
        .collect()
}

fn assert_priority(outcome: &engine::game::scenario::Outcome) {
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution must return priority, got {:?}",
        outcome.final_waiting_for()
    );
}

/// R1: Alabaster Potion, X=3 — the shield survives two damage events (CR 615.7):
/// 2 prevented, then 1 prevented and 1 dealt, net P0 −1.
#[test]
fn alabaster_potion_x3_prevents_three_across_two_damage_events() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut pool = mana(ManaType::Colorless, 3);
    pool.extend(mana(ManaType::White, 2));
    scenario.with_mana_pool(P0, pool);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Alabaster Potion", true, ALABASTER_POTION)
        .with_mana_cost(alabaster_potion_cost())
        .id();

    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .modes(&[1])
        .x(3)
        .target_player(P0)
        .resolve();
    assert_priority(&outcome);
    assert_eq!(
        prevention_shields(&runner),
        vec![PreventionAmount::Next(3)],
        "X=3 must install a Next(3) shield"
    );

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(spell, TargetRef::Player(P0), 2),
        &mut events,
    )
    .expect("first damage event resolves");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 2, .. })),
        "first event must be fully prevented, got {events:?}"
    );
    assert_eq!(runner.life(P0), 20, "two prevented points leave P0 at 20");

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(spell, TargetRef::Player(P0), 2),
        &mut events,
    )
    .expect("second damage event resolves");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 1, .. })),
        "only the last shield point prevents on the second event, got {events:?}"
    );
    assert_eq!(
        runner.life(P0),
        19,
        "the shield's remaining one point prevents one, the other is dealt"
    );
}

/// R2: Alabaster Potion, X=0 — a zero shield is installed and prevents nothing.
#[test]
fn alabaster_potion_x0_installs_zero_shield_and_prevents_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::White, 2));
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Alabaster Potion", true, ALABASTER_POTION)
        .with_mana_cost(alabaster_potion_cost())
        .id();

    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .modes(&[1])
        .x(0)
        .target_player(P0)
        .resolve();
    assert_priority(&outcome);
    assert_eq!(
        prevention_shields(&runner),
        vec![PreventionAmount::Next(0)],
        "X=0 must install a Next(0) shield"
    );

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(spell, TargetRef::Player(P0), 2),
        &mut events,
    )
    .expect("damage event resolves");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "a zero shield prevents nothing, got {events:?}"
    );
    assert_eq!(runner.life(P0), 18, "both points are dealt");
}

/// R3: Panacea's activated `{X}{X}` uses the single announced X (CR 107.3a).
#[test]
fn panacea_activated_x2_prevents_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 4));
    let panacea = scenario
        .add_artifact_from_oracle(P0, "Panacea", PANACEA)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.activate(panacea, 0).x(2).target_player(P0).resolve();
    assert_priority(&outcome);
    assert_eq!(prevention_shields(&runner), vec![PreventionAmount::Next(2)]);

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(panacea, TargetRef::Player(P0), 2),
        &mut events,
    )
    .expect("damage event resolves");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 2, .. })),
        "the announced X=2 must prevent both points, got {events:?}"
    );
    assert_eq!(runner.life(P0), 20);
}

/// R4: Vengeful Archon's activated `{X}` shields the activator. The rider's
/// separate "prevented this way" assignment is not asserted (separate seam).
#[test]
fn vengeful_archon_activated_x2_prevents_two_to_you() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 2));
    let archon = scenario
        .add_creature_from_oracle(P0, "Vengeful Archon", 5, 5, VENGEFUL_ARCHON)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.activate(archon, 0).x(2).target_player(P1).resolve();
    assert_priority(&outcome);
    assert_eq!(
        prevention_shields(&runner),
        vec![PreventionAmount::Next(2)],
        "the \"to you\" shield must be installed on the activator"
    );

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(archon, TargetRef::Player(P0), 2),
        &mut events,
    )
    .expect("damage event resolves");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 2, .. })),
        "X=2 must prevent both points to the activator, got {events:?}"
    );
    assert_eq!(runner.life(P0), 20);
}

/// R5: Atalya's modal activated `{X}` prevention mode shields the chosen
/// creature. The mode's pre-existing "Spend only white mana on X" sibling is
/// unimplemented and is deliberately not asserted.
#[test]
fn atalya_modal_activated_x2_prevents_two_on_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::White, 2));
    let atalya = scenario
        .add_creature_from_oracle(P0, "Atalya, Samite Master", 2, 3, ATALYA)
        .id();
    let target = scenario.add_creature(P1, "Shielded", 2, 2).id();

    let mut runner = scenario.build();
    let outcome = runner
        .activate(atalya, 0)
        .modes(&[0])
        .x(2)
        .target_objects(&[target])
        .resolve();
    assert_priority(&outcome);

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(atalya, TargetRef::Object(target), 2),
        &mut events,
    )
    .expect("damage event resolves");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 2, .. })),
        "X=2 must prevent both points on the chosen creature, got {events:?}"
    );
    assert_eq!(runner.state().objects[&target].damage_marked, 0);
}

/// R5 sibling: Atalya's life mode installs no shield; the reach guard is the
/// life gain plus the absence of prevention.
#[test]
fn atalya_life_mode_installs_no_shield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::White, 2));
    let atalya = scenario
        .add_creature_from_oracle(P0, "Atalya, Samite Master", 2, 3, ATALYA)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.activate(atalya, 0).modes(&[1]).x(2).resolve();
    assert_priority(&outcome);
    assert_eq!(outcome.life_delta(P0), 2, "the life mode gains X life");
    assert!(
        prevention_shields(&runner).is_empty(),
        "the life mode must install no prevention shield"
    );
}

/// R6: a trailing "where X is the number of Clerics" binder DEFINES X and must
/// override the announced-word placeholder (CR 107.3c).
#[test]
fn battlefield_medic_where_x_binder_overrides_the_word_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let medic = scenario
        .add_creature_from_oracle(P0, "Battlefield Medic", 1, 2, BATTLEFIELD_MEDIC)
        .id();
    let target = scenario.add_creature(P1, "Shielded", 2, 2).id();
    // CR 107.3c: the binder counts the Clerics on the battlefield at
    // resolution. `with_subtypes` writes both the live and the base card
    // types, so the activation's layer flush keeps the subtype.
    for name in ["Cleric One", "Cleric Two"] {
        scenario
            .add_creature(P1, name, 1, 1)
            .with_subtypes(vec!["Cleric"])
            .id();
    }

    let mut runner = scenario.build();
    let outcome = runner
        .activate(medic, 0)
        .target_objects(&[target])
        .resolve();
    assert_priority(&outcome);

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(medic, TargetRef::Object(target), 2),
        &mut events,
    )
    .expect("damage event resolves");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 2, .. })),
        "the Cleric-count binder must yield a 2-point shield, got {events:?}"
    );
    assert_eq!(runner.state().objects[&target].damage_marked, 0);
}

fn walk_prevention_dynamic(ability: &AbilityDefinition, out: &mut Vec<Option<QuantityExpr>>) {
    if let Effect::PreventDamage { amount_dynamic, .. } = &*ability.effect {
        out.push(amount_dynamic.clone());
    }
    if let Some(sub) = &ability.sub_ability {
        walk_prevention_dynamic(sub, out);
    }
    for mode in &ability.mode_abilities {
        walk_prevention_dynamic(mode, out);
    }
}

fn face_prevention_dynamics(face: &CardFace) -> Vec<Option<QuantityExpr>> {
    let mut out = Vec::new();
    for ability in &face.abilities {
        walk_prevention_dynamic(ability, &mut out);
    }
    out
}

fn temper_face() -> CardFace {
    let parsed = parse_oracle_text(TEMPER, "Temper", &[], &["Instant".to_string()], &[]);
    CardFace {
        name: "Temper".to_string(),
        mana_cost: ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::X, ManaCostShard::White],
        },
        card_type: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Instant],
            subtypes: vec![],
        },
        oracle_text: Some(TEMPER.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    }
}

/// R7: the dynamic amount survives BOTH native export loaders, and the
/// round-tripped card's shield is still sized by the announced X.
#[test]
fn temper_export_round_trip_through_both_native_loaders_prevents_two() {
    let export = serde_json::json!({ "temper": temper_face() });
    let json = serde_json::to_vec(&export).expect("Temper face serializes");

    let reader_db = CardDatabase::from_export_reader(Cursor::new(json.clone()))
        .expect("from_export_reader accepts the synthetic export");
    let path =
        std::env::temp_dir().join(format!("phase-temper-export-{}.json", std::process::id()));
    std::fs::write(&path, &json).expect("temp export writes");
    let file_db = CardDatabase::from_export(&path).expect("from_export accepts the same export");
    let _ = std::fs::remove_file(&path);

    for (label, db) in [
        ("from_export_reader", &reader_db),
        ("from_export", &file_db),
    ] {
        let face = db
            .get_face_by_name("Temper")
            .unwrap_or_else(|| panic!("{label}: Temper must be loaded"));
        assert_eq!(
            face_prevention_dynamics(face),
            vec![Some(variable_x())],
            "{label}: the announced X must survive the loader"
        );
    }

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut pool = mana(ManaType::Colorless, 3);
    pool.extend(mana(ManaType::White, 1));
    scenario.with_mana_pool(P0, pool);
    let target = scenario.add_creature(P1, "Shielded", 2, 2).id();
    let spell = scenario.add_real_card(P0, "Temper", Zone::Hand, &reader_db);

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).x(2).target_objects(&[target]).resolve();
    assert_priority(&outcome);

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(spell, TargetRef::Object(target), 2),
        &mut events,
    )
    .expect("damage event resolves");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 2, .. })),
        "the round-tripped X=2 shield must prevent both points, got {events:?}"
    );
    assert_eq!(runner.state().objects[&target].damage_marked, 0);
}

/// R8: X is bound per cast, not globally — two casts with different X each keep
/// their own shield (CR 107.3i + CR 107.3k).
#[test]
fn announced_x_is_bound_per_cast_not_globally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut pool = mana(ManaType::Colorless, 4);
    pool.extend(mana(ManaType::White, 4));
    scenario.with_mana_pool(P0, pool);
    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "Alabaster Potion", true, ALABASTER_POTION)
        .with_mana_cost(alabaster_potion_cost())
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "Alabaster Potion", true, ALABASTER_POTION)
        .with_mana_cost(alabaster_potion_cost())
        .id();

    let mut runner = scenario.build();
    runner
        .cast(first)
        .modes(&[1])
        .x(1)
        .target_player(P0)
        .resolve();
    runner
        .cast(second)
        .modes(&[1])
        .x(3)
        .target_player(P1)
        .resolve();

    let mut shields = prevention_shields(&runner);
    shields.sort_by_key(|amount| match amount {
        PreventionAmount::Next(n) => *n,
        _ => u32::MAX,
    });
    assert_eq!(
        shields,
        vec![PreventionAmount::Next(1), PreventionAmount::Next(3)],
        "both casts must keep their own announced X"
    );

    // P0's 1-point shield: 2 damage → 1 prevented, 1 dealt.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(first, TargetRef::Player(P0), 2),
        &mut events,
    )
    .expect("P0 damage resolves");
    assert!(events
        .iter()
        .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 1, .. })));
    assert_eq!(runner.life(P0), 19);

    // P1's 3-point shield: 2 damage → fully prevented.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(second, TargetRef::Player(P1), 2),
        &mut events,
    )
    .expect("P1 damage resolves");
    assert!(events
        .iter()
        .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 2, .. })));
    assert_eq!(runner.life(P1), 20);
}

/// R9: each activation announces its own X (CR 107.3k): Panacea X=2 then X=0.
#[test]
fn panacea_reannounces_x_independently_per_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 4));
    let panacea = scenario
        .add_artifact_from_oracle(P0, "Panacea", PANACEA)
        .id();

    let mut runner = scenario.build();
    runner.activate(panacea, 0).x(2).target_player(P0).resolve();
    runner
        .state_mut()
        .objects
        .get_mut(&panacea)
        .expect("Panacea exists")
        .tapped = false;
    runner.activate(panacea, 0).x(0).target_player(P1).resolve();

    let mut shields = prevention_shields(&runner);
    shields.sort_by_key(|amount| match amount {
        PreventionAmount::Next(n) => *n,
        _ => u32::MAX,
    });
    assert_eq!(
        shields,
        vec![PreventionAmount::Next(0), PreventionAmount::Next(2)],
        "each activation must keep its own announced X"
    );

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(panacea, TargetRef::Player(P0), 2),
        &mut events,
    )
    .expect("P0 damage resolves");
    assert!(events
        .iter()
        .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 2, .. })));
    assert_eq!(runner.life(P0), 20);

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(panacea, TargetRef::Player(P1), 2),
        &mut events,
    )
    .expect("P1 damage resolves");
    assert!(!events
        .iter()
        .any(|e| matches!(e, GameEvent::DamagePrevented { .. })));
    assert_eq!(runner.life(P1), 18, "the X=0 activation prevents nothing");
}
