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
//! CR 614.12 + CR 700.5: the conditions those statics read count the population
//! as it is before the entry. Heliod, Sun-Crowned ruling (2020-01-24): "Because
//! replacement effects are considered before the God is on the battlefield, the
//! mana symbols in its mana cost won't be counted." So Heliod returned with four
//! other white symbols is not a creature as it would enter (no counter), while an
//! enters observer still sees it enter as a creature (the ruling's second half):
//! after entry its own {W} makes devotion five. The same holds for every
//! population-reading condition (Rusted Relic's metalcraft, Starfield of Nyx's
//! enchantment count, including a copy of it or a command-zone carrier of its
//! static) and for the controller the entrant enters under (CR 110.2a + CR 109.5).
//!
//! Built via the `/card-test` recipe: `GameScenario` + `GameRunner::cast(..)
//! .resolve()` + `CastOutcome` deltas, on verbatim Oracle text. Every negative
//! assertion is paired with a positive reach-guard in the same test.
//!
//! REVERT DISCRIMINATORS:
//! * Restore the pre-move `matches_target_filter` read in
//!   `effects::change_zone::enter_with_counters_for_object` and
//!   `recommission_reads_march_projection` gets 0 counters (expected 1),
//!   `recommission_reads_entrants_own_isnt_a_creature_static` gets 1 counter in
//!   its (0, 0) and (2, 0) cases (expected 0: the graveyard card is a creature),
//!   and the What Must Be Done March / Opalescence rows get 0 counters
//!   (expected 2).
//! * Revert the anaphoric-rider conversion in
//!   `parser::oracle_effect::parse_effect_chain_ir_body` and the What Must Be
//!   Done rows fail their slot premise and their "no later `PutCounter`" check.
//! * Drop the pre-entry population pin
//!   (`layers::pin_entry_population_conditions`, called from
//!   `filter::BattlefieldEntrantProjection::new`) and the projection counts the
//!   entrant: `recommission_reads_entrants_own_isnt_a_creature_static` and
//!   `recommission_heliod_four_devotion_still_entered_as_a_creature` get 1
//!   counter at devotion four (expected 0),
//!   `release_juno_rusted_relic_counts_pre_entry_artifacts` gets 2 counters with
//!   two other artifacts (expected 0),
//!   `release_juno_sanctum_counts_pre_entry_enchantments` gets 2 counters at four
//!   enchantments (expected 0), and the stolen-entrant rows get 1 counter
//!   (expected 0). Key the pin's "you" to the owner and the stolen-entrant rows
//!   also fail.
//! * Drop the pin's copy-payload carrier and
//!   `release_juno_sanctum_counts_copied_starfields_pre_entry_enchantments` gets
//!   2 counters (expected 0); write the pin to `base_static_definitions` only and
//!   `release_juno_sanctum_counts_command_zone_static_pre_entry` gets 2
//!   (expected 0).
//! * Restore the single-type-only slice match in
//!   `parser::oracle_effect::conditions::card_type_condition_as_entry_this_way`
//!   and `release_juno_any_of_gate_is_an_entry_replacement` fails: the entry gate
//!   stays empty and the rider survives as a later `PutCounter`, so Elemental
//!   Bond draws nothing for Memnite.
//! * Drop the pin's `GrantStaticAbility` arm (`EntryPopulationPin::grants`) and
//!   `release_juno_granted_shrine_condition_counts_pre_entry_shrines` gets 2
//!   counters with Sanctum as the only Shrine (expected 0), and
//!   `release_juno_granted_condition_binds_each_recipients_controller` gets 2 in
//!   its P1-Shrine-only and no-Shrine rows (expected 0). Pin the granted inner
//!   with the host's controller instead of each recipient's and the
//!   P1-Shrine-only row gets 2 (expected 0) and the P0-Shrine-only row gets 0
//!   (expected 2). `release_juno_granted_starfield_condition_counts_pre_entry_enchantments`
//!   and `release_juno_granted_starfield_condition_binds_the_recipients_controller`
//!   fail too: their zero-counter rows get 2, and with a host-keyed pin the
//!   opposite-controller rows flip.

use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    ContinuousModification, Effect, EffectKind, QuantityExpr, TargetFilter, TypeFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
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

/// Rusted Relic {4} Artifact — verbatim Oracle text.
const RUSTED_RELIC: &str = "Metalcraft \u{2014} This artifact is a 5/5 Golem artifact creature \
     as long as you control three or more artifacts.";

/// Starfield of Nyx {4}{W} Enchantment — verbatim Oracle text.
const STARFIELD_OF_NYX: &str = "At the beginning of your upkeep, you may return target \
     enchantment card from your graveyard to the battlefield.\n\
     As long as you control five or more enchantments, each other non-Aura enchantment you \
     control is a creature in addition to its other types and has base power and base toughness \
     each equal to its mana value.";

/// Copy Enchantment {2}{U} Enchantment — verbatim Oracle text.
const COPY_ENCHANTMENT: &str =
    "You may have this enchantment enter as a copy of any enchantment on the battlefield.";

/// Elemental Bond {2}{G} Enchantment — verbatim Oracle text.
const ELEMENTAL_BOND: &str =
    "Whenever a creature you control with power 3 or greater enters, draw a card.";

/// Synthetic (no printed card): a gated put from an opponent's graveyard under
/// the caster's control, so owner and controller differ.
const STEAL_CREATURE_WITH_GATE: &str = "Put target creature card from an opponent's graveyard \
     onto the battlefield under your control. If a creature enters this way, it enters with an \
     additional +1/+1 counter on it.";

/// Synthetic: [`STEAL_CREATURE_WITH_GATE`] naming an artifact card.
const STEAL_ARTIFACT_WITH_GATE: &str = "Put target artifact card from an opponent's graveyard \
     onto the battlefield under your control. If a creature enters this way, it enters with an \
     additional +1/+1 counter on it.";

/// Synthetic: What Must Be Done with Release Juno's rider widened to two types,
/// so the anaphoric gate names several card types.
const MULTI_TYPE_RELEASE_JUNO: &str = "Choose one \u{2014}\n\
     \u{2022} Let the World Burn \u{2014} Destroy all artifacts and creatures.\n\
     \u{2022} Release Juno \u{2014} Return target historic permanent card from your graveyard \
     to the battlefield. It enters with two additional +1/+1 counters on it if it's an artifact \
     or creature.";

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

/// A main-phase board with a stocked P0 library (draws are counted).
fn board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D"]);
    scenario.with_library_top(P1, &["Card E", "Card F", "Card G", "Card H"]);
    scenario
}

/// CR 700.5: permanents `player` controls adding `ww_anchors` × {W}{W} and
/// `w_anchors` × {W} to that player's devotion to white.
fn add_devotion_anchors(
    scenario: &mut GameScenario,
    player: PlayerId,
    ww_anchors: usize,
    w_anchors: usize,
) {
    for i in 0..ww_anchors {
        scenario
            .add_enchantment_from_oracle(player, &format!("Devotion Anchor WW {i}"), "")
            .with_mana_cost(generic_and_white(0, 2));
    }
    for i in 0..w_anchors {
        scenario
            .add_enchantment_from_oracle(player, &format!("Devotion Anchor W {i}"), "")
            .with_mana_cost(generic_and_white(0, 1));
    }
}

fn is_creature(outcome: &CastOutcome, id: ObjectId) -> bool {
    outcome.state().objects[&id]
        .card_types
        .core_types
        .contains(&CoreType::Creature)
}

/// Recommission returning Heliod, Sun-Crowned with P0 devotion anchors
/// (`ww_anchors` × {W}{W}, `w_anchors` × {W}) and, optionally, Elemental Bond.
fn recommission_on_heliod(
    ww_anchors: usize,
    w_anchors: usize,
    with_bond: bool,
) -> (CastOutcome, ObjectId) {
    let mut scenario = board();
    let spell = add_recommission(&mut scenario);
    let heliod = add_graveyard_heliod(&mut scenario);
    add_devotion_anchors(&mut scenario, P0, ww_anchors, w_anchors);
    if with_bond {
        scenario.add_enchantment_from_oracle(P0, "Elemental Bond", ELEMENTAL_BOND);
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

/// V-P2 + B1: Recommission returning Heliod, Sun-Crowned. CR 614.12 checks Heliod
/// as it would exist on the battlefield, with its own static, and CR 700.5's
/// devotion is counted before the entry (Heliod ruling 2020-01-24: its own {W}
/// "won't be counted"). With no anchors (pre-entry devotion 0) and with two
/// {W}{W} anchors (pre-entry devotion 4) Heliod isn't a creature as it would
/// enter, so no counter. With one more {W} anchor (pre-entry devotion 5) it is a
/// creature there and gets the counter (positive control).
#[test]
fn recommission_reads_entrants_own_isnt_a_creature_static() {
    let (outcome, heliod) = recommission_on_heliod(0, 0, false);
    outcome.assert_zone(&[heliod], Zone::Battlefield);
    outcome.assert_counters(heliod, CounterType::Plus1Plus1, 0);
    assert!(
        !is_creature(&outcome, heliod),
        "devotion 1 after entry: not a creature"
    );

    // Pre-entry devotion 4: the entrant's own {W} is not counted.
    let (outcome, heliod) = recommission_on_heliod(2, 0, false);
    outcome.assert_zone(&[heliod], Zone::Battlefield);
    outcome.assert_counters(heliod, CounterType::Plus1Plus1, 0);
    assert!(
        is_creature(&outcome, heliod),
        "reach: after entry its own {{W}} makes devotion five, so Heliod is a creature"
    );
    assert!(!put_counter_resolved(outcome.events()));

    // Pre-entry devotion 5: a creature as it would enter.
    let (outcome, heliod) = recommission_on_heliod(2, 1, false);
    outcome.assert_zone(&[heliod], Zone::Battlefield);
    outcome.assert_counters(heliod, CounterType::Plus1Plus1, 1);
    assert!(is_creature(&outcome, heliod));
}

/// B1, Heliod ruling (b) (2020-01-24): "When a God enters the battlefield, your
/// devotion to its color (including the mana symbols in the mana cost of the God
/// itself) will determine if a creature entered the battlefield or not for
/// abilities that trigger whenever a creature enters the battlefield." With
/// pre-entry devotion 4, Recommission's gate sees no creature (no counter), but
/// Elemental Bond sees a 5/5 creature enter and draws (CR 603.10 + CR 603.6a).
/// A pre-entry pin leaking into the live game would make Heliod a noncreature
/// and Bond draw nothing.
#[test]
fn recommission_heliod_four_devotion_still_entered_as_a_creature() {
    let (outcome, heliod) = recommission_on_heliod(2, 0, true);
    outcome.assert_zone(&[heliod], Zone::Battlefield);
    outcome.assert_counters(heliod, CounterType::Plus1Plus1, 0);
    assert!(is_creature(&outcome, heliod));
    outcome.assert_power_toughness(heliod, 5, 5);
    outcome.assert_hand_drawn(P0, 1);
}

/// Cast What Must Be Done choosing Release Juno (mode index 1) on `target`.
fn cast_release_juno(
    runner: GameRunner,
    spell: ObjectId,
    target: ObjectId,
) -> engine::game::scenario::CastOutcome {
    cast_release_juno_gated(runner, spell, target, creature_gate(2))
}

/// [`cast_release_juno`] for a Release Juno text whose folded entry gate is `gate`.
fn cast_release_juno_gated(
    mut runner: GameRunner,
    spell: ObjectId,
    target: ObjectId,
    gate: Vec<(TargetFilter, CounterType, QuantityExpr)>,
) -> engine::game::scenario::CastOutcome {
    // Premise: the rider is the effect-carried entry gate, not a later instruction.
    assert_eq!(conditional_slot(&runner, spell, 1), gate);
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

/// `count` mana-value-1 enchantments `player` controls (CR 202.3), named with
/// `prefix`. Mana value 1 keeps them alive as 1/1s if Starfield of Nyx animates
/// them.
fn add_enchantment_anchors(
    scenario: &mut GameScenario,
    player: PlayerId,
    prefix: &str,
    count: usize,
) {
    for i in 0..count {
        scenario
            .add_enchantment_from_oracle(player, &format!("{prefix} {i}"), "")
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![],
            });
    }
}

/// A graveyard Sanctum of Tranquil Light (core types fixed up after `build`).
fn add_graveyard_sanctum(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Sanctum of Tranquil Light", 0, 0)
        .as_legendary()
        .with_subtypes(vec!["Shrine"])
        .with_mana_cost(generic_and_white(0, 1))
        .from_oracle_text(SANCTUM_OF_TRANQUIL_LIGHT)
        .id()
}

/// A graveyard Rusted Relic owned by `owner` (core types fixed up after `build`).
fn add_graveyard_rusted_relic(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    scenario
        .add_creature_to_graveyard(owner, "Rusted Relic", 0, 0)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![],
        })
        .from_oracle_text(RUSTED_RELIC)
        .id()
}

/// How many of P0's battlefield anchors named with `prefix` are creatures.
fn creature_anchors(state: &engine::types::game_state::GameState, prefix: &str) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &state.objects[id];
            obj.name.starts_with(prefix) && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .count()
}

/// B1 class (own static, `ObjectCount` comparison): Release Juno returning
/// Rusted Relic. CR 614.12: the condition counts the artifacts P0 controls before
/// the entry. With two other artifacts, P0 controls two before Rusted Relic
/// enters, so metalcraft is off as it would enter and the "if it's a creature"
/// rider adds nothing.
/// After entry Rusted Relic is the third artifact, so it is a 5/5 creature.
/// With three other artifacts metalcraft already holds: two counters, 7/7.
#[test]
fn release_juno_rusted_relic_counts_pre_entry_artifacts() {
    for (others, counters, pt) in [(2usize, 0, (5, 5)), (3, 2, (7, 7))] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = add_what_must_be_done(&mut scenario);
        let relic = add_graveyard_rusted_relic(&mut scenario, P0);
        for i in 0..others {
            scenario.add_artifact_from_oracle(P0, &format!("Artifact Anchor {i}"), "");
        }
        let mut runner = scenario.build();
        set_core_types(&mut runner, relic, &[CoreType::Artifact]);

        let outcome = cast_release_juno(runner, spell, relic);
        outcome.assert_zone(&[relic], Zone::Battlefield);
        outcome.assert_counters(relic, CounterType::Plus1Plus1, counters);
        assert!(
            is_creature(&outcome, relic),
            "reach ({others} others): metalcraft holds after entry"
        );
        outcome.assert_power_toughness(relic, pt.0, pt.1);
        assert!(!put_counter_resolved(outcome.events()));
    }
}

/// B1 class (another object's group static): Release Juno returning Sanctum of
/// Tranquil Light under Starfield of Nyx. With Starfield and three other
/// enchantments, P0 controls four enchantments before Sanctum enters, so
/// Starfield's static is off as Sanctum would enter: no counters. After entry
/// Sanctum is the fifth enchantment, so it is a 1/1 creature (mana value 1).
/// With four other enchantments the condition already holds: two counters, 3/3.
#[test]
fn release_juno_sanctum_counts_pre_entry_enchantments() {
    for (others, counters, pt) in [(3usize, 0, (1, 1)), (4, 2, (3, 3))] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = add_what_must_be_done(&mut scenario);
        let sanctum = add_graveyard_sanctum(&mut scenario);
        scenario
            .add_enchantment_from_oracle(P0, "Starfield of Nyx", STARFIELD_OF_NYX)
            .with_mana_cost(generic_and_white(4, 1));
        add_enchantment_anchors(&mut scenario, P0, "Enchantment Anchor", others);
        let mut runner = scenario.build();
        set_core_types(&mut runner, sanctum, &[CoreType::Enchantment]);

        let outcome = cast_release_juno(runner, spell, sanctum);
        outcome.assert_zone(&[sanctum], Zone::Battlefield);
        outcome.assert_counters(sanctum, CounterType::Plus1Plus1, counters);
        assert!(
            is_creature(&outcome, sanctum),
            "reach ({others} others): five enchantments after entry"
        );
        outcome.assert_power_toughness(sanctum, pt.0, pt.1);
        assert!(!put_counter_resolved(outcome.events()));
    }
}

/// B1 class, a copied group static (CR 707.2 + CR 613.1a + CR 109.5): P0's Copy
/// Enchantment enters as a copy of P1's Starfield of Nyx, so the copy's static is
/// P0's ("you" is the copy's controller). With the copy and three other P0
/// enchantments, P0 controls four before Sanctum enters: no counters, and Sanctum
/// is a 1/1 creature after entry (five). With four others: two counters, 3/3.
/// The original Starfield is P1's, so only the copy can animate P0's Sanctum.
#[test]
fn release_juno_sanctum_counts_copied_starfields_pre_entry_enchantments() {
    for (others, counters, pt) in [(3usize, 0, (1, 1)), (4, 2, (3, 3))] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = add_what_must_be_done(&mut scenario);
        let copier = scenario
            .add_spell_to_hand_from_oracle(P0, "Copy Enchantment", false, COPY_ENCHANTMENT)
            .as_enchantment()
            .id();
        let sanctum = add_graveyard_sanctum(&mut scenario);
        let starfield = scenario
            .add_enchantment_from_oracle(P1, "Starfield of Nyx", STARFIELD_OF_NYX)
            .with_mana_cost(generic_and_white(4, 1))
            .id();
        add_enchantment_anchors(&mut scenario, P0, "Enchantment Anchor", others);
        let mut runner = scenario.build();
        set_core_types(&mut runner, sanctum, &[CoreType::Enchantment]);

        let copied = runner
            .cast(copier)
            .accept_optional()
            .replacement_choice(0)
            .copy_target(starfield)
            .resolve();
        let copy = &copied.state().objects[&copier];
        assert_eq!(copy.zone, Zone::Battlefield, "reach: the copy entered");
        assert_eq!(copy.name, "Starfield of Nyx", "reach: it copied Starfield");
        assert_eq!(copy.controller, P0);
        assert!(
            copied
                .state()
                .transient_continuous_effects
                .iter()
                .any(|tce| tce
                    .modifications
                    .iter()
                    .any(|m| matches!(m, ContinuousModification::CopyValues { .. }))),
            "reach: the copy is a layer-1 copy effect"
        );
        assert_eq!(
            creature_anchors(copied.state(), "Enchantment Anchor"),
            if others + 1 >= 5 { others } else { 0 },
            "reach: the copy's static counts P0's enchantments"
        );

        let outcome = cast_release_juno(runner, spell, sanctum);
        outcome.assert_zone(&[sanctum], Zone::Battlefield);
        outcome.assert_counters(sanctum, CounterType::Plus1Plus1, counters);
        assert!(
            is_creature(&outcome, sanctum),
            "reach ({others} others): five P0 enchantments after entry"
        );
        outcome.assert_power_toughness(sanctum, pt.0, pt.1);
    }
}

/// B1 class, a command-zone carrier. Labelled synthetic: a P0 emblem (CR 113.6 +
/// CR 114.4: an emblem's static functions in the command zone) carrying Starfield
/// of Nyx's parsed static, cloned from a P1 donor. With four P0 enchantments
/// before Sanctum enters: no counters, and Sanctum is a 1/1 creature after entry
/// (five). With five: two counters, 3/3.
#[test]
fn release_juno_sanctum_counts_command_zone_static_pre_entry() {
    for (anchors, counters, pt) in [(4usize, 0, (1, 1)), (5, 2, (3, 3))] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = add_what_must_be_done(&mut scenario);
        let sanctum = add_graveyard_sanctum(&mut scenario);
        let donor = scenario
            .add_enchantment_from_oracle(P1, "Starfield of Nyx", STARFIELD_OF_NYX)
            .with_mana_cost(generic_and_white(4, 1))
            .id();
        add_enchantment_anchors(&mut scenario, P0, "Enchantment Anchor", anchors);
        let mut runner = scenario.build();
        set_core_types(&mut runner, sanctum, &[CoreType::Enchantment]);
        {
            let state = runner.state_mut();
            let statics = state.objects[&donor].static_definitions.clone();
            let base_statics = state.objects[&donor].base_static_definitions.clone();
            let emblem = engine::game::zones::create_object(
                state,
                engine::types::identifiers::CardId(9_001),
                P0,
                "Starfield Emblem".to_string(),
                Zone::Command,
            );
            let obj = state.objects.get_mut(&emblem).expect("emblem created");
            obj.is_emblem = true;
            obj.static_definitions = statics;
            obj.base_static_definitions = base_statics;
            engine::game::layers::mark_layers_full(state);
            engine::game::layers::flush_layers(state);
            assert!(
                state.command_zone.contains(&emblem),
                "reach: the emblem is in the command zone"
            );
            assert_eq!(
                creature_anchors(state, "Enchantment Anchor"),
                if anchors >= 5 { anchors } else { 0 },
                "reach: the emblem's static functions and counts P0's enchantments"
            );
        }

        let outcome = cast_release_juno(runner, spell, sanctum);
        outcome.assert_zone(&[sanctum], Zone::Battlefield);
        outcome.assert_counters(sanctum, CounterType::Plus1Plus1, counters);
        assert!(
            is_creature(&outcome, sanctum),
            "reach ({anchors} anchors): five P0 enchantments after entry"
        );
        outcome.assert_power_toughness(sanctum, pt.0, pt.1);
    }
}

/// B1 + CR 110.2a + CR 109.5: Heliod owned by P1 enters under P0's control, so
/// "your devotion" is P0's, counted before entry. P0 has devotion 4 (two {W}{W}
/// permanents) and P1 has 6: the projection counts P0's 4, so Heliod isn't a
/// creature as it would enter and gets no counter. After entry its {W} makes P0's
/// devotion five, so it is a creature. A pin keyed to the owner (P1, 6) would
/// give the counter.
#[test]
fn stolen_heliod_counts_new_controllers_pre_entry_devotion() {
    let mut scenario = board();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Steal Gate", false, STEAL_CREATURE_WITH_GATE)
        .id();
    let heliod = scenario
        .add_creature_to_graveyard(P1, "Heliod, Sun-Crowned", 5, 5)
        .as_legendary()
        .with_subtypes(vec!["God"])
        .with_mana_cost(generic_and_white(2, 1))
        .from_oracle_text(HELIOD_SUN_CROWNED)
        .id();
    add_devotion_anchors(&mut scenario, P0, 2, 0);
    add_devotion_anchors(&mut scenario, P1, 3, 0);
    let mut runner = scenario.build();
    set_core_types(
        &mut runner,
        heliod,
        &[CoreType::Enchantment, CoreType::Creature],
    );
    // Premise: the synthetic text carries the gate and the controller override.
    assert_eq!(conditional_slot(&runner, spell, 0), creature_gate(1));
    assert!(matches!(
        runner.state().objects[&spell].abilities[0].effect.as_ref(),
        Effect::ChangeZone {
            enters_under: Some(_),
            ..
        }
    ));

    let outcome = runner.cast(spell).target_objects(&[heliod]).resolve();
    outcome.assert_zone(&[heliod], Zone::Battlefield);
    assert_eq!(outcome.state().objects[&heliod].controller, P0);
    outcome.assert_counters(heliod, CounterType::Plus1Plus1, 0);
    assert!(
        is_creature(&outcome, heliod),
        "reach: P0's devotion is five after entry"
    );
}

/// B1 + CR 110.2a + CR 109.5: Rusted Relic owned by P1 enters under P0's control;
/// P0 controls two artifacts before entry and P1 three. Metalcraft counts P0's
/// two, so it is not a creature as it would enter: no counter. After entry it is
/// P0's third artifact, a 5/5 creature.
#[test]
fn stolen_rusted_relic_counts_new_controllers_pre_entry_artifacts() {
    let mut scenario = board();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Steal Gate", false, STEAL_ARTIFACT_WITH_GATE)
        .id();
    let relic = add_graveyard_rusted_relic(&mut scenario, P1);
    for i in 0..2 {
        scenario.add_artifact_from_oracle(P0, &format!("P0 Artifact {i}"), "");
    }
    for i in 0..3 {
        scenario.add_artifact_from_oracle(P1, &format!("P1 Artifact {i}"), "");
    }
    let mut runner = scenario.build();
    set_core_types(&mut runner, relic, &[CoreType::Artifact]);
    assert_eq!(conditional_slot(&runner, spell, 0), creature_gate(1));

    let outcome = runner.cast(spell).target_objects(&[relic]).resolve();
    outcome.assert_zone(&[relic], Zone::Battlefield);
    assert_eq!(outcome.state().objects[&relic].controller, P0);
    outcome.assert_counters(relic, CounterType::Plus1Plus1, 0);
    assert!(
        is_creature(&outcome, relic),
        "reach: P0 controls three artifacts after entry"
    );
    outcome.assert_power_toughness(relic, 5, 5);
}

/// C4 (CR 614.1c + CR 614.12): a multi-type anaphoric rider ("if it's an
/// artifact or creature") folds into an any-of entry gate, so the counters are
/// part of the entry. Memnite enters as a 3/3, so Elemental Bond draws, and no
/// later `PutCounter` resolves; Sol Ring (an artifact) also enters with two;
/// Sanctum of Tranquil Light (neither) gets none and still enters.
#[test]
fn release_juno_any_of_gate_is_an_entry_replacement() {
    let any_of_gate = || {
        vec![(
            TargetFilter::Typed(engine::types::ability::TypedFilter::new(TypeFilter::AnyOf(
                vec![TypeFilter::Artifact, TypeFilter::Creature],
            ))),
            CounterType::Plus1Plus1,
            QuantityExpr::Fixed { value: 2 },
        )]
    };
    let release = |scenario: &mut GameScenario| {
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Multi-Type Release Juno",
                false,
                MULTI_TYPE_RELEASE_JUNO,
            )
            .with_mana_cost(generic_and_white(3, 2))
            .id();
        scenario.with_mana_pool(P0, {
            let mut pool = mana(ManaType::White, 2);
            pool.extend(mana(ManaType::Colorless, 3));
            pool
        });
        spell
    };

    // Case 1: Memnite, a 1/1 artifact creature, with Elemental Bond.
    let mut scenario = board();
    scenario.add_enchantment_from_oracle(P0, "Elemental Bond", ELEMENTAL_BOND);
    let spell = release(&mut scenario);
    let memnite = scenario
        .add_creature_to_graveyard(P0, "Memnite", 1, 1)
        .as_artifact_creature()
        .with_subtypes(vec!["Construct"])
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![],
        })
        .id();
    let runner = scenario.build();
    let outcome = cast_release_juno_gated(runner, spell, memnite, any_of_gate());
    outcome.assert_zone(&[memnite], Zone::Battlefield);
    outcome.assert_counters(memnite, CounterType::Plus1Plus1, 2);
    outcome.assert_power_toughness(memnite, 3, 3);
    assert!(
        !put_counter_resolved(outcome.events()),
        "the counters ride the entry: {:?}",
        outcome.events()
    );
    outcome.assert_hand_drawn(P0, 1);

    // Case 2: Sol Ring, a noncreature artifact.
    let mut scenario = board();
    let spell = release(&mut scenario);
    let sol_ring = add_graveyard_sol_ring(&mut scenario);
    let mut runner = scenario.build();
    set_core_types(&mut runner, sol_ring, &[CoreType::Artifact]);
    let outcome = cast_release_juno_gated(runner, spell, sol_ring, any_of_gate());
    outcome.assert_zone(&[sol_ring], Zone::Battlefield);
    outcome.assert_counters(sol_ring, CounterType::Plus1Plus1, 2);
    assert!(!put_counter_resolved(outcome.events()));

    // Case 3: Sanctum of Tranquil Light, neither an artifact nor a creature.
    let mut scenario = board();
    let spell = release(&mut scenario);
    let sanctum = add_graveyard_sanctum(&mut scenario);
    let mut runner = scenario.build();
    set_core_types(&mut runner, sanctum, &[CoreType::Enchantment]);
    let outcome = cast_release_juno_gated(runner, spell, sanctum, any_of_gate());
    outcome.assert_zone(&[sanctum], Zone::Battlefield);
    outcome.assert_counters(sanctum, CounterType::Plus1Plus1, 0);
}

/// Synthetic (no printed card): a host granting every enchantment a
/// population-conditioned static. Sedge Sliver is the only printed grant of this
/// shape and it changes only power/toughness, which no entry gate observes.
const SHRINE_GRANT_HOST: &str = "Each enchantment has \"As long as you control a Shrine, this \
     enchantment is a 1/1 creature in addition to its other types.\"";

/// Release Juno returning P0's Sanctum of Tranquil Light (a Shrine) with
/// [`SHRINE_GRANT_HOST`] under `host_controller`, plus a Shrine anchor for P0
/// and/or P1.
fn release_juno_under_shrine_grant(
    host_controller: PlayerId,
    p0_shrine: bool,
    p1_shrine: bool,
) -> (CastOutcome, ObjectId, ObjectId) {
    let mut scenario = board();
    let spell = add_what_must_be_done(&mut scenario);
    let sanctum = add_graveyard_sanctum(&mut scenario);
    let host = scenario
        .add_enchantment_from_oracle(host_controller, "Shrine Grant Host", SHRINE_GRANT_HOST)
        .id();
    for (player, present) in [(P0, p0_shrine), (P1, p1_shrine)] {
        if present {
            scenario
                .add_enchantment_from_oracle(player, &format!("{player:?} Shrine"), "")
                .with_subtypes(vec!["Shrine"]);
        }
    }
    let mut runner = scenario.build();
    set_core_types(&mut runner, sanctum, &[CoreType::Enchantment]);
    // Premise: the host grants a static whose inner condition is population-reading.
    assert!(
        runner.state().objects[&host]
            .static_definitions
            .as_slice()
            .iter()
            .any(|def| def.modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::GrantStaticAbility { definition }
                    if definition.condition.is_some()
            ))),
        "premise: the host carries a conditioned GrantStaticAbility"
    );

    let outcome = cast_release_juno(runner, spell, sanctum);
    (outcome, sanctum, host)
}

/// CR 614.12 + CR 109.5: a granted static's population condition is
/// read before the entry, with the recipient's controller as "you". P0's host
/// grants every enchantment "As long as you control a Shrine, this enchantment
/// is a 1/1 creature". With Sanctum as P0's only Shrine, P0 controls no Shrine
/// before it enters, so it is not a creature as it would enter: no counters.
/// After entry Sanctum is P0's Shrine, so the live pass makes it and the host
/// 1/1 creatures. With another P0 Shrine the condition already holds: two
/// counters, 3/3.
#[test]
fn release_juno_granted_shrine_condition_counts_pre_entry_shrines() {
    for (other_shrine, counters, pt) in [(false, 0, (1, 1)), (true, 2, (3, 3))] {
        let (outcome, sanctum, host) = release_juno_under_shrine_grant(P0, other_shrine, false);
        outcome.assert_zone(&[sanctum], Zone::Battlefield);
        outcome.assert_counters(sanctum, CounterType::Plus1Plus1, counters);
        assert!(
            is_creature(&outcome, sanctum),
            "reach (other Shrine: {other_shrine}): P0 controls a Shrine after entry"
        );
        assert!(
            is_creature(&outcome, host),
            "live pass (other Shrine: {other_shrine}): the host is a creature too"
        );
        outcome.assert_power_toughness(sanctum, pt.0, pt.1);
        assert!(!put_counter_resolved(outcome.events()));
    }
}

/// CR 614.12 + CR 109.5: "you" in a granted static is the controller of the
/// object it is on, not the granting host's. P1's host grants the static to
/// P0's Sanctum, so Sanctum's condition counts P0's Shrines before entry:
/// * only P1 controls a Shrine → no counters (a pin keyed to the host's
///   controller would give two), and Sanctum is a 1/1 creature after entry;
/// * only P0 controls a Shrine → two counters, 3/3 (a host-keyed pin would give
///   none);
/// * no Shrines → no counters, and Sanctum is a 1/1 creature after entry.
#[test]
fn release_juno_granted_condition_binds_each_recipients_controller() {
    for (p0_shrine, p1_shrine, counters, pt) in [
        (false, true, 0, (1, 1)),
        (true, false, 2, (3, 3)),
        (false, false, 0, (1, 1)),
    ] {
        let (outcome, sanctum, _host) = release_juno_under_shrine_grant(P1, p0_shrine, p1_shrine);
        outcome.assert_zone(&[sanctum], Zone::Battlefield);
        assert_eq!(outcome.state().objects[&sanctum].controller, P0);
        outcome.assert_counters(sanctum, CounterType::Plus1Plus1, counters);
        assert!(
            is_creature(&outcome, sanctum),
            "reach (P0 Shrine: {p0_shrine}, P1 Shrine: {p1_shrine}): \
             P0 controls a Shrine after entry"
        );
        outcome.assert_power_toughness(sanctum, pt.0, pt.1);
        assert!(!put_counter_resolved(outcome.events()));
    }
}

/// Synthetic (no printed card): a host granting every enchantment Starfield of
/// Nyx's animation static, scoped to its recipient. Starfield's own text grants
/// nothing ("each other non-Aura enchantment you control"); this carrier gives the
/// same population condition and characteristics to each enchantment through
/// `GrantStaticAbility`, so the pin's grant arm reads it.
const STARFIELD_GRANT_HOST: &str = "Each enchantment has \"As long as you control five or more \
     enchantments, this enchantment is a creature in addition to its other types and has base \
     power and base toughness each equal to its mana value.\"";

/// Release Juno returning P0's Sanctum of Tranquil Light with
/// [`STARFIELD_GRANT_HOST`] under `host_controller`, plus `p0_anchors` P0 and
/// `p1_anchors` P1 mana-value-1 enchantments. The host's mana value 5 keeps it
/// alive as a 5/5 if its own grant animates it.
fn release_juno_under_starfield_grant(
    host_controller: PlayerId,
    p0_anchors: usize,
    p1_anchors: usize,
) -> (CastOutcome, ObjectId, ObjectId) {
    let mut scenario = board();
    let spell = add_what_must_be_done(&mut scenario);
    let sanctum = add_graveyard_sanctum(&mut scenario);
    let host = scenario
        .add_enchantment_from_oracle(
            host_controller,
            "Starfield Grant Host",
            STARFIELD_GRANT_HOST,
        )
        .with_mana_cost(generic_and_white(4, 1))
        .id();
    add_enchantment_anchors(&mut scenario, P0, "Enchantment Anchor", p0_anchors);
    add_enchantment_anchors(&mut scenario, P1, "P1 Anchor", p1_anchors);
    let mut runner = scenario.build();
    set_core_types(&mut runner, sanctum, &[CoreType::Enchantment]);
    // Premise: the host grants a static whose inner condition is population-reading.
    assert!(
        runner.state().objects[&host]
            .static_definitions
            .as_slice()
            .iter()
            .any(|def| def.modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::GrantStaticAbility { definition }
                    if definition.condition.is_some()
            ))),
        "premise: the host carries a conditioned GrantStaticAbility"
    );

    let outcome = cast_release_juno(runner, spell, sanctum);
    (outcome, sanctum, host)
}

/// CR 614.12 + CR 109.5: a granted Starfield-shaped static's enchantment count is
/// read before the entry, with the recipient's controller as "you". With P0's
/// host and three P0 anchors, P0 controls four enchantments before Sanctum enters
/// as the fifth, so Sanctum is not a creature as it would enter: no counters.
/// After entry P0 controls five, so Sanctum is a creature with base power and
/// toughness equal to its mana value, 1/1. With four anchors the condition
/// already holds before entry: two counters, 3/3.
#[test]
fn release_juno_granted_starfield_condition_counts_pre_entry_enchantments() {
    for (p0_anchors, counters, pt) in [(3usize, 0, (1, 1)), (4, 2, (3, 3))] {
        let (outcome, sanctum, _host) = release_juno_under_starfield_grant(P0, p0_anchors, 0);
        outcome.assert_zone(&[sanctum], Zone::Battlefield);
        outcome.assert_counters(sanctum, CounterType::Plus1Plus1, counters);
        assert!(
            is_creature(&outcome, sanctum),
            "reach ({p0_anchors} anchors): P0 controls five enchantments after entry"
        );
        outcome.assert_power_toughness(sanctum, pt.0, pt.1);
        assert!(!put_counter_resolved(outcome.events()));
    }
}

/// CR 614.12 + CR 109.5: "you" in the granted Starfield-shaped static is the
/// controller of the object it is on, not the granting host's. P1's host grants
/// it to P0's Sanctum, so Sanctum's condition counts P0's enchantments before
/// entry:
/// * P0 controls four and P1 five (host plus four) → no counters (a pin keyed to
///   the host's controller would read five and give two), and Sanctum is a 1/1
///   creature after entry;
/// * P0 controls five and P1 one (the host) → two counters, 3/3 (a host-keyed
///   pin would read one and give none).
#[test]
fn release_juno_granted_starfield_condition_binds_the_recipients_controller() {
    for (p0_anchors, p1_anchors, counters, pt) in [(4usize, 4usize, 0, (1, 1)), (5, 0, 2, (3, 3))] {
        let (outcome, sanctum, _host) =
            release_juno_under_starfield_grant(P1, p0_anchors, p1_anchors);
        outcome.assert_zone(&[sanctum], Zone::Battlefield);
        assert_eq!(outcome.state().objects[&sanctum].controller, P0);
        outcome.assert_counters(sanctum, CounterType::Plus1Plus1, counters);
        assert!(
            is_creature(&outcome, sanctum),
            "reach (P0 {p0_anchors}, P1 {p1_anchors}): P0 controls five enchantments \
             after entry"
        );
        outcome.assert_power_toughness(sanctum, pt.0, pt.1);
        assert!(!put_counter_resolved(outcome.events()));
    }
}
