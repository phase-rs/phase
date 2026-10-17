//! Effect-carried conditional entry gates read the entrant as it would exist on
//! the battlefield (CR 614.12), and What Must Be Done's anaphoric "It enters with
//! two additional +1/+1 counters on it if it's a creature" rider is an entry
//! replacement (CR 614.1c), not a later instruction (CR 608.2c).
//!
//! CR 614.12: a replacement that modifies how a permanent enters the battlefield
//! checks the permanent as it would exist there — its own static abilities that
//! would apply once it is there, and continuous effects that already exist and
//! would apply to it. So:
//!
//! * Recommission returning Sol Ring under March of the Machines: Sol Ring would
//!   be a 1/1 artifact creature on the battlefield, so "if a creature enters
//!   this way" holds and it enters with one +1/+1 counter.
//! * Recommission returning Heliod, Sun-Crowned with devotion to white below
//!   five: Heliod's own static says it isn't a creature there, so no counter.
//! * What Must Be Done (mode 2) applies its rider as the entry, under the same
//!   projected reading.
//!
//! Built via the `/card-test` recipe: `GameScenario` + `GameRunner::cast(..)
//! .resolve()` + `CastOutcome` deltas, on verbatim Oracle text. Every negative
//! assertion is paired with a positive reach-guard in the same test.
//!
//! REVERT DISCRIMINATORS:
//! * Restore the pre-move `matches_target_filter` read in
//!   `effects::change_zone::enter_with_counters_for_object` and
//!   `recommission_reads_march_projection` gets 0 counters (expected 1),
//!   `recommission_reads_entrants_own_isnt_a_creature_static` gets 1 counter
//!   (expected 0), and the What Must Be Done March / Opalescence rows get 0
//!   counters (expected 2).
//! * Revert the anaphoric-rider conversion in
//!   `parser::oracle_effect::parse_effect_chain_ir_body` and the What Must Be
//!   Done rows fail their slot premise and their "no later `PutCounter`" check.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, EffectKind, QuantityExpr, TargetFilter, TypeFilter};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Recommission {1}{W} Sorcery — verbatim Oracle text.
const RECOMMISSION: &str = "Return target artifact or creature card with mana value 3 or less \
     from your graveyard to the battlefield. If a creature enters this way, it enters with an \
     additional +1/+1 counter on it.";

/// What Must Be Done {3}{W}{W} Sorcery — verbatim Oracle text.
const WHAT_MUST_BE_DONE: &str = "Choose one \u{2014}\n\
     \u{2022} Let the World Burn \u{2014} Destroy all artifacts and creatures.\n\
     \u{2022} Release Juno \u{2014} Return target historic permanent card from your graveyard \
     to the battlefield. It enters with two additional +1/+1 counters on it if it's a creature. \
     (Artifacts, legendaries, and Sagas are historic.)";

/// March of the Machines {3}{U} Enchantment — verbatim Oracle text.
const MARCH_OF_THE_MACHINES: &str = "Each noncreature artifact is an artifact creature with \
     power and toughness each equal to its mana value. (Equipment that's a creature can't equip \
     a creature.)";

/// Opalescence {2}{W}{W} Enchantment — verbatim Oracle text.
const OPALESCENCE: &str = "Each other non-Aura enchantment is a creature in addition to its \
     other types and has base power and base toughness each equal to its mana value.";

/// Sol Ring {1} Artifact — verbatim Oracle text.
const SOL_RING: &str = "{T}: Add {C}{C}.";

/// Sanctum of Tranquil Light {W} Legendary Enchantment — Shrine — verbatim Oracle text.
const SANCTUM_OF_TRANQUIL_LIGHT: &str = "{5}{W}: Tap target creature. This ability costs {1} \
     less to activate for each Shrine you control.";

/// Heliod, Sun-Crowned {2}{W} Legendary Enchantment Creature — God 5/5 —
/// verbatim Oracle text.
const HELIOD_SUN_CROWNED: &str = "Indestructible\n\
     As long as your devotion to white is less than five, Heliod isn't a creature.\n\
     Whenever you gain life, put a +1/+1 counter on target creature or enchantment you control.\n\
     {1}{W}: Another target creature gains lifelink until end of turn.";

fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(kind, ObjectId(0), false, vec![]); n]
}

fn generic_and_white(generic: u32, white: usize) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: vec![ManaCostShard::White; white],
    }
}

/// Stage `id` (built through `add_creature_to_graveyard`) as a card whose core
/// types are exactly `core_types` and that has no printed power/toughness unless
/// it is a creature card.
fn set_core_types(runner: &mut GameRunner, id: ObjectId, core_types: &[CoreType]) {
    let obj = runner
        .state_mut()
        .objects
        .get_mut(&id)
        .expect("fixture present");
    obj.card_types.core_types = core_types.to_vec();
    obj.base_card_types = obj.card_types.clone();
    if !core_types.contains(&CoreType::Creature) {
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
    }
}

fn add_recommission(scenario: &mut GameScenario) -> ObjectId {
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Recommission", false, RECOMMISSION)
        .with_mana_cost(generic_and_white(1, 1))
        .id();
    scenario.with_mana_pool(P0, {
        let mut pool = mana(ManaType::White, 1);
        pool.extend(mana(ManaType::Colorless, 1));
        pool
    });
    spell
}

fn add_what_must_be_done(scenario: &mut GameScenario) -> ObjectId {
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "What Must Be Done", false, WHAT_MUST_BE_DONE)
        .with_mana_cost(generic_and_white(3, 2))
        .id();
    scenario.with_mana_pool(P0, {
        let mut pool = mana(ManaType::White, 2);
        pool.extend(mana(ManaType::Colorless, 3));
        pool
    });
    spell
}

/// A graveyard Sol Ring (a noncreature artifact card, mana value 1).
fn add_graveyard_sol_ring(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Sol Ring", 0, 0)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .from_oracle_text(SOL_RING)
        .id()
}

/// A graveyard Heliod, Sun-Crowned (core types fixed up after `build`).
fn add_graveyard_heliod(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Heliod, Sun-Crowned", 5, 5)
        .as_legendary()
        .with_subtypes(vec!["God"])
        .with_mana_cost(generic_and_white(2, 1))
        .from_oracle_text(HELIOD_SUN_CROWNED)
        .id()
}

fn put_counter_resolved(events: &[GameEvent]) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::PutCounter,
                ..
            }
        )
    })
}

/// The `conditional_enter_with_counters` slot of the spell's `index`-th ability.
fn conditional_slot(
    runner: &GameRunner,
    spell: ObjectId,
    index: usize,
) -> Vec<(TargetFilter, CounterType, QuantityExpr)> {
    let abilities = &runner.state().objects[&spell].abilities;
    match abilities.get(index).map(|a| a.effect.as_ref()) {
        Some(Effect::ChangeZone {
            destination: Zone::Battlefield,
            conditional_enter_with_counters,
            ..
        }) => conditional_enter_with_counters.clone(),
        other => panic!("premise: ability {index} must be a battlefield ChangeZone, got {other:?}"),
    }
}

fn creature_gate(count: i32) -> Vec<(TargetFilter, CounterType, QuantityExpr)> {
    vec![(
        TargetFilter::Typed(engine::types::ability::TypedFilter::new(
            TypeFilter::Creature,
        )),
        CounterType::Plus1Plus1,
        QuantityExpr::Fixed { value: count },
    )]
}

fn recommission_on_sol_ring(with_march: bool) -> (engine::game::scenario::CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_recommission(&mut scenario);
    let sol_ring = add_graveyard_sol_ring(&mut scenario);
    if with_march {
        scenario
            .add_enchantment_from_oracle(P0, "March of the Machines", MARCH_OF_THE_MACHINES)
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Blue],
            });
    }
    let mut runner = scenario.build();
    set_core_types(&mut runner, sol_ring, &[CoreType::Artifact]);

    // Premise: the gate is the effect-carried "if a creature enters this way" slot.
    assert_eq!(conditional_slot(&runner, spell, 0), creature_gate(1));

    let outcome = runner.cast(spell).target_objects(&[sol_ring]).resolve();
    (outcome, sol_ring)
}

/// V-P1: Recommission returning Sol Ring under March of the Machines. On the
/// battlefield Sol Ring would be a 1/1 artifact creature (March), so the gate
/// holds and it enters with one +1/+1 counter: 2/2 (CR 614.12). The sibling
/// without March proves the gate is not unconditional.
#[test]
fn recommission_reads_march_projection() {
    let (outcome, sol_ring) = recommission_on_sol_ring(true);
    outcome.assert_zone(&[sol_ring], Zone::Battlefield);
    outcome.assert_counters(sol_ring, CounterType::Plus1Plus1, 1);
    outcome.assert_power_toughness(sol_ring, 2, 2);
    assert!(
        !put_counter_resolved(outcome.events()),
        "the counter must ride the entry, not a later PutCounter: {:?}",
        outcome.events()
    );

    // Sibling: no March — Sol Ring is a noncreature artifact on the battlefield.
    let (outcome, sol_ring) = recommission_on_sol_ring(false);
    outcome.assert_zone(&[sol_ring], Zone::Battlefield);
    outcome.assert_counters(sol_ring, CounterType::Plus1Plus1, 0);
    assert!(!put_counter_resolved(outcome.events()));
}

fn recommission_on_heliod(
    devotion_anchors: usize,
) -> (engine::game::scenario::CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_recommission(&mut scenario);
    let heliod = add_graveyard_heliod(&mut scenario);
    for i in 0..devotion_anchors {
        // CR 700.5: each {W}{W} permanent adds two to P0's devotion to white.
        scenario
            .add_enchantment_from_oracle(P0, &format!("Devotion Anchor {i}"), "")
            .with_mana_cost(generic_and_white(0, 2));
    }
    let mut runner = scenario.build();
    set_core_types(
        &mut runner,
        heliod,
        &[CoreType::Enchantment, CoreType::Creature],
    );
    assert_eq!(conditional_slot(&runner, spell, 0), creature_gate(1));

    let outcome = runner.cast(spell).target_objects(&[heliod]).resolve();
    (outcome, heliod)
}

/// V-P2: Recommission returning Heliod, Sun-Crowned. With devotion to white
/// below five (Heliod's own {W} only), Heliod's own static says it isn't a
/// creature on the battlefield (CR 614.12 "its own static abilities"), so no
/// counter. With two {W}{W} permanents, devotion is 1 + 4 = 5 (CR 700.5),
/// Heliod is a creature there, and it gets the counter (positive control).
#[test]
fn recommission_reads_entrants_own_isnt_a_creature_static() {
    let (outcome, heliod) = recommission_on_heliod(0);
    outcome.assert_zone(&[heliod], Zone::Battlefield);
    outcome.assert_counters(heliod, CounterType::Plus1Plus1, 0);

    let (outcome, heliod) = recommission_on_heliod(2);
    outcome.assert_zone(&[heliod], Zone::Battlefield);
    outcome.assert_counters(heliod, CounterType::Plus1Plus1, 1);
}

/// Cast What Must Be Done choosing Release Juno (mode index 1) on `target`.
fn cast_release_juno(
    mut runner: GameRunner,
    spell: ObjectId,
    target: ObjectId,
) -> engine::game::scenario::CastOutcome {
    // Premise: the rider is the effect-carried entry gate, not a later instruction.
    assert_eq!(conditional_slot(&runner, spell, 1), creature_gate(2));
    assert!(
        runner.state().objects[&spell].abilities[1]
            .sub_ability
            .is_none(),
        "the rider must be folded into the entry, not left as a sub-ability"
    );
    runner
        .cast(spell)
        .modes(&[1])
        .target_objects(&[target])
        .resolve()
}

/// V-L1 (full card): What Must Be Done mode 2 lowers to a battlefield
/// `ChangeZone` whose entry gate is `[(Creature, +1/+1, 2)]`, with no sub and no
/// swallowed-clause warning.
#[test]
fn what_must_be_done_parses_rider_as_entry_gate() {
    let parsed = parse_oracle_text(
        WHAT_MUST_BE_DONE,
        "What Must Be Done",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let release_juno = parsed
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::ChangeZone { .. }))
        .unwrap_or_else(|| panic!("mode 2 must parse to a ChangeZone: {parsed:#?}"));
    let Effect::ChangeZone {
        destination,
        conditional_enter_with_counters,
        ..
    } = release_juno.effect.as_ref()
    else {
        unreachable!()
    };
    assert_eq!(*destination, Zone::Battlefield);
    assert_eq!(*conditional_enter_with_counters, creature_gate(2));
    assert!(release_juno.sub_ability.is_none(), "{release_juno:#?}");
    assert!(
        !parsed
            .parse_warnings
            .iter()
            .any(|w| matches!(w, OracleDiagnostic::SwallowedClause { .. })),
        "the represented rider must not be reported swallowed: {:?}",
        parsed.parse_warnings
    );
}

/// V-L8: What Must Be Done returning Sol Ring under March of the Machines: the
/// projected Sol Ring is a creature, so it enters with two +1/+1 counters (3/3),
/// applied as the entry — no later `PutCounter` resolves.
#[test]
fn what_must_be_done_reads_march_projection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_what_must_be_done(&mut scenario);
    let sol_ring = add_graveyard_sol_ring(&mut scenario);
    scenario
        .add_enchantment_from_oracle(P0, "March of the Machines", MARCH_OF_THE_MACHINES)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Blue],
        });
    let mut runner = scenario.build();
    set_core_types(&mut runner, sol_ring, &[CoreType::Artifact]);

    let outcome = cast_release_juno(runner, spell, sol_ring);
    outcome.assert_zone(&[sol_ring], Zone::Battlefield);
    outcome.assert_counters(sol_ring, CounterType::Plus1Plus1, 2);
    outcome.assert_power_toughness(sol_ring, 3, 3);
    assert!(
        !put_counter_resolved(outcome.events()),
        "the rider must ride the entry, not resolve as a later PutCounter: {:?}",
        outcome.events()
    );
}

/// V-L9: What Must Be Done returning Sanctum of Tranquil Light under
/// Opalescence: the projected Sanctum is a 1/1 enchantment creature, so it
/// enters with two +1/+1 counters.
#[test]
fn what_must_be_done_reads_opalescence_projection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_what_must_be_done(&mut scenario);
    let sanctum = scenario
        .add_creature_to_graveyard(P0, "Sanctum of Tranquil Light", 0, 0)
        .as_legendary()
        .with_subtypes(vec!["Shrine"])
        .with_mana_cost(generic_and_white(0, 1))
        .from_oracle_text(SANCTUM_OF_TRANQUIL_LIGHT)
        .id();
    scenario
        .add_enchantment_from_oracle(P0, "Opalescence", OPALESCENCE)
        .with_mana_cost(generic_and_white(2, 2));
    let mut runner = scenario.build();
    set_core_types(&mut runner, sanctum, &[CoreType::Enchantment]);

    let outcome = cast_release_juno(runner, spell, sanctum);
    outcome.assert_zone(&[sanctum], Zone::Battlefield);
    outcome.assert_counters(sanctum, CounterType::Plus1Plus1, 2);
    outcome.assert_power_toughness(sanctum, 3, 3);
    assert!(!put_counter_resolved(outcome.events()));
}

/// V-L10: What Must Be Done returning Heliod, Sun-Crowned with devotion to white
/// below five: Heliod isn't a creature on the battlefield, so no counters.
/// (V-L11's Memnite row is the positive creature control.)
#[test]
fn what_must_be_done_reads_gods_own_static() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_what_must_be_done(&mut scenario);
    let heliod = add_graveyard_heliod(&mut scenario);
    let mut runner = scenario.build();
    set_core_types(
        &mut runner,
        heliod,
        &[CoreType::Enchantment, CoreType::Creature],
    );

    let outcome = cast_release_juno(runner, spell, heliod);
    outcome.assert_zone(&[heliod], Zone::Battlefield);
    outcome.assert_counters(heliod, CounterType::Plus1Plus1, 0);
}

/// V-L11: What Must Be Done returning Memnite (a 1/1 artifact creature,
/// historic by CR 700.6) → two counters, 3/3; returning a noncreature artifact
/// with no type-changing effect → no counters, and it still enters.
#[test]
fn what_must_be_done_plain_creature_and_noncreature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_what_must_be_done(&mut scenario);
    let memnite = scenario
        .add_creature_to_graveyard(P0, "Memnite", 1, 1)
        .with_subtypes(vec!["Construct"])
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![],
        })
        .id();
    let mut runner = scenario.build();
    set_core_types(
        &mut runner,
        memnite,
        &[CoreType::Artifact, CoreType::Creature],
    );
    let outcome = cast_release_juno(runner, spell, memnite);
    outcome.assert_zone(&[memnite], Zone::Battlefield);
    outcome.assert_counters(memnite, CounterType::Plus1Plus1, 2);
    outcome.assert_power_toughness(memnite, 3, 3);
    assert!(!put_counter_resolved(outcome.events()));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_what_must_be_done(&mut scenario);
    let sol_ring = add_graveyard_sol_ring(&mut scenario);
    let mut runner = scenario.build();
    set_core_types(&mut runner, sol_ring, &[CoreType::Artifact]);
    let outcome = cast_release_juno(runner, spell, sol_ring);
    outcome.assert_zone(&[sol_ring], Zone::Battlefield);
    outcome.assert_counters(sol_ring, CounterType::Plus1Plus1, 0);
}
