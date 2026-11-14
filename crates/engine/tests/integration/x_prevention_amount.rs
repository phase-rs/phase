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
//! CR 107.3a + CR 107.3c + CR 107.3i + CR 107.3k + CR 608.2h + CR 615.1a + CR 615.3 + CR 615.7.

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
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
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
const SERRAS_HYMN: &str = "At the beginning of your upkeep, you may put a verse counter on this enchantment.\nSacrifice this enchantment: Prevent the next X damage that would be dealt this turn to any number of targets, divided as you choose, where X is the number of verse counters on this enchantment.";

const MURDER: &str = "Destroy target creature.";

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

/// Prevention shields hosted on battlefield objects. A creature-targeted
/// shield is installed in the target's `replacement_definitions`
/// (`prevent_damage::host_on_targets`), not in `pending_damage_replacements`,
/// so `prevention_shields` alone cannot see it.
fn hosted_prevention_shields(runner: &GameRunner) -> Vec<PreventionAmount> {
    runner
        .state()
        .objects
        .values()
        .flat_map(|object| object.replacement_definitions.iter_all())
        .filter_map(|replacement| match replacement.shield_kind {
            ShieldKind::Prevention { amount } => Some(amount),
            _ => None,
        })
        .collect()
}

/// Clerics on the battlefield, read from live state. A reach guard for the
/// Battlefield Medic board before and after the mid-stack change, not the
/// expected shield (that is derived from the card's text).
fn clerics_on_battlefield(runner: &GameRunner) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|&&id| {
            state.objects[&id]
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Cleric")
        })
        .count()
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
        .add_creature_from_oracle(P0, "Vengeful Archon", 7, 7, VENGEFUL_ARCHON)
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
    // Reach guard for the life-mode sibling's hosted-shield absence assertion:
    // the prevention mode's shield is hosted on the chosen creature.
    assert_eq!(
        hosted_prevention_shields(&runner),
        vec![PreventionAmount::Next(2)],
        "the prevention mode hosts its X=2 shield on the chosen creature"
    );

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
        "the life mode must install no player-scoped prevention shield"
    );
    assert!(
        hosted_prevention_shields(&runner).is_empty(),
        "the life mode must install no prevention shield on any object"
    );
}

/// R6: a trailing "where X is the number of Clerics" binder DEFINES X and must
/// override the announced-word placeholder (CR 107.3c).
#[test]
fn battlefield_medic_where_x_binder_overrides_the_word_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let medic = scenario
        .add_creature_from_oracle(P0, "Battlefield Medic", 1, 1, BATTLEFIELD_MEDIC)
        .with_subtypes(vec!["Human", "Cleric"])
        .id();
    let target = scenario.add_creature(P1, "Shielded", 2, 2).id();
    // CR 107.3c: the binder counts the Clerics on the battlefield at
    // resolution. `with_subtypes` writes both the live and the base card
    // types, so the activation's layer flush keeps the subtype. The board is
    // the printed Human Cleric Medic itself (it counts itself), two Clerics
    // under P1, and the non-Cleric 2/2 target: X = 3, while a binder that
    // counted every creature would read 4.
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

    // 4 damage against the X = 3 shield: 3 prevented, 1 marked.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(medic, TargetRef::Object(target), 4),
        &mut events,
    )
    .expect("damage event resolves");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 3, .. })),
        "the Cleric-count binder must yield a 3-point shield (the Medic and two other Clerics), got {events:?}"
    );
    assert_eq!(
        runner.state().objects[&target].damage_marked,
        1,
        "4 damage minus the 3-point shield leaves 1 marked on the target"
    );
}

/// R11: the where-X count changes while the Medic's ability is on the stack.
/// CR 107.3c: an X defined by the ability's text "may change while that spell
/// or ability is on the stack"; CR 608.2h: information from the game is
/// "determined only once, when the effect is applied". Murder destroys one of
/// the three Clerics in response (CR 701.8a), so the shield is 2 — a count
/// fixed at activation would install 3.
#[test]
fn battlefield_medic_where_x_count_is_read_at_resolution_not_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Murder's printed {1}{B}{B}.
    let mut pool = mana(ManaType::Colorless, 1);
    pool.extend(mana(ManaType::Black, 2));
    scenario.with_mana_pool(P0, pool);
    let medic = scenario
        .add_creature_from_oracle(P0, "Battlefield Medic", 1, 1, BATTLEFIELD_MEDIC)
        .with_subtypes(vec!["Human", "Cleric"])
        .id();
    let target = scenario.add_creature(P1, "Shielded", 2, 2).id();
    let doomed = scenario
        .add_creature(P1, "Cleric One", 1, 1)
        .with_subtypes(vec!["Cleric"])
        .id();
    scenario
        .add_creature(P1, "Cleric Two", 1, 1)
        .with_subtypes(vec!["Cleric"])
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    assert_eq!(
        clerics_on_battlefield(&runner),
        3,
        "reach: the Medic and two other Clerics are on the battlefield at activation"
    );

    // CR 602.2b: activate through the public action path. `activate(..)`
    // drives straight through resolution, so it cannot open the response window.
    runner
        .act(GameAction::ActivateAbility {
            source_id: medic,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Battlefield Medic's ability activates");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection {
            player,
            target_slots,
            ..
        } => {
            assert_eq!(*player, P0, "the activator chooses the target");
            assert!(
                target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(target)),
                "the non-Cleric 2/2 is a legal target"
            );
        }
        other => panic!("the Medic's ability must ask for its creature target, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("the 2/2 is a legal target");

    // CR 117.3c: the activator receives priority with the ability on the stack.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "the activator holds priority over the ability, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.source_id)
            .collect::<Vec<_>>(),
        vec![medic],
        "reach: the Medic's ability is the only object on the stack"
    );
    let mut installed = prevention_shields(&runner);
    installed.extend(hosted_prevention_shields(&runner));
    assert!(
        installed.is_empty(),
        "no shield exists before the ability resolves"
    );

    // In response, Murder destroys one Cleric. CR 405.5: the top object
    // resolves first, leaving the Medic's ability on the stack.
    runner.cast(murder).target_object(doomed).commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "Murder waits above the Medic's ability"
    );
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == murder),
        "reach: Murder is on the stack"
    );
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&doomed].zone,
        Zone::Graveyard,
        "reach: Murder destroyed the Cleric"
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.source_id)
            .collect::<Vec<_>>(),
        vec![medic],
        "reach: the Medic's ability is still waiting on the stack"
    );
    assert_eq!(
        clerics_on_battlefield(&runner),
        2,
        "reach: two Clerics remain when the ability resolves"
    );
    let mut installed = prevention_shields(&runner);
    installed.extend(hosted_prevention_shields(&runner));
    assert!(
        installed.is_empty(),
        "still no shield before the ability resolves"
    );

    runner.resolve_top();
    assert!(
        runner.state().stack.is_empty(),
        "the Medic's ability resolved"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return priority, got {:?}",
        runner.state().waiting_for
    );
    let mut installed = prevention_shields(&runner);
    installed.extend(hosted_prevention_shields(&runner));
    assert_eq!(
        installed,
        vec![PreventionAmount::Next(2)],
        "X is the two Clerics left when the effect is applied, not the three at activation"
    );

    // 3 damage against the 2-point shield: 2 prevented, 1 marked (CR 615.7).
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(medic, TargetRef::Object(target), 3),
        &mut events,
    )
    .expect("damage event resolves");
    let prevented: u32 = events
        .iter()
        .filter_map(|e| match e {
            GameEvent::DamagePrevented { amount, .. } => Some(*amount),
            _ => None,
        })
        .sum();
    assert_eq!(
        prevented, 2,
        "the 2-point shield prevents two, got {events:?}"
    );
    assert_eq!(
        runner.state().objects[&target].damage_marked,
        1,
        "3 damage minus the 2-point shield leaves 1 marked on the target"
    );
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
/// their own shield (CR 107.3a + CR 107.3i).
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

/// R10: Serra's Hymn sacrifices itself as a cost, so at resolution the source
/// is in the graveyard with no counters. The where-X count of verse counters
/// must come from last-known information (CR 608.2h), not the graveyard object.
#[test]
fn serras_hymn_sacrificed_source_counts_its_verse_counters_by_lki() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hymn = scenario
        .add_enchantment_from_oracle(P0, "Serra's Hymn", SERRAS_HYMN)
        .id();
    scenario.with_counter(hymn, CounterType::Generic("verse".to_string()), 3);
    let target = scenario.add_creature(P0, "Shielded", 2, 2).id();

    let mut runner = scenario.build();
    // Reach guards: the Hymn starts on the battlefield holding 3 verse counters.
    assert_eq!(runner.state().objects[&hymn].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&hymn]
            .counters
            .get(&CounterType::Generic("verse".to_string()))
            .copied()
            .unwrap_or(0),
        3,
        "the Hymn must carry 3 verse counters before it is sacrificed"
    );

    let outcome = runner
        .activate(hymn, 0)
        .pay_with(&[hymn])
        .target_objects(&[target])
        .resolve();
    assert_priority(&outcome);
    // Reach guard: the sacrifice cost really moved the source out of play, so
    // resolution could only read its counters through last-known information.
    assert_eq!(runner.state().objects[&hymn].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&hymn]
            .counters
            .get(&CounterType::Generic("verse".to_string()))
            .copied()
            .unwrap_or(0),
        0,
        "the graveyard object carries no counters"
    );

    let mut installed = prevention_shields(&runner);
    installed.extend(hosted_prevention_shields(&runner));
    assert_eq!(
        installed,
        vec![PreventionAmount::Next(3)],
        "X is the 3 verse counters the Hymn had when it was sacrificed"
    );

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(hymn, TargetRef::Object(target), 5),
        &mut events,
    )
    .expect("damage event resolves");
    let prevented: u32 = events
        .iter()
        .filter_map(|e| match e {
            GameEvent::DamagePrevented { amount, .. } => Some(*amount),
            _ => None,
        })
        .sum();
    assert_eq!(
        prevented, 3,
        "all three shield points prevent, got {events:?}"
    );
    assert_eq!(
        runner.state().objects[&target].damage_marked,
        2,
        "the remaining two of five damage is dealt"
    );
}
