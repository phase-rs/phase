//! Regression: `speed::has_max_speed` re-entrancy / stack-overflow guard.
//!
//! ## The bug (pre-guard)
//!
//! `has_max_speed` -> `can_increase_speed_beyond_4` scans
//! `active_static_definitions`, which evaluates each static's CR 604.1
//! functioning condition. A `StaticCondition::HasMaxSpeed` condition maps
//! (layers.rs) back to `has_max_speed`, re-entering `can_increase_speed_beyond_4`
//! -> the scan -> the same condition -> infinite recursion -> stack overflow.
//! This fires on any board where the controller has a HasMaxSpeed-gated static
//! (e.g. Racers' Scoreboard) and was hit driving a saved 4-player Commander game
//! through `apply(PassPriority)` (`resolve_bench` repro: "thread 'main' has
//! overflowed its stack / fatal runtime error: stack overflow").
//!
//! The recursion-regression tests above the coverage-promotion section drive
//! either `evaluate_layers` or `can_increase_speed_beyond_4`; both paths
//! evaluate a HasMaxSpeed-gated static through `active_static_definitions`,
//! the recursing seam. The coverage-promotion tests below exercise their
//! named production consumers. These are runtime pipeline tests rather than
//! parsed-AST shape tests.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 702.179e: A player has max speed if their speed is 4.
//!   - CR 613.8b: a dependency loop is broken; values are taken without circular
//!     contribution (rules basis for the base-cap re-entry answer).
//!   - CR 604.1: a static's functioning condition is re-evaluated continuously.
//!
//! ## Coverage-promotion runtime evidence (below)
//!
//! `game::coverage::static_condition_feature` classifies
//! `StaticCondition::HasMaxSpeed` as `Handled` (it was previously
//! misclassified `Unhandled` despite this file already proving the runtime
//! arm terminates and applies). The tests below drive the four distinct
//! PRODUCTION consumers of that runtime arm through verbatim-Oracle
//! Standard-legal cards, so the coverage promotion has real pipeline
//! evidence rather than resting on the classifier flip alone:
//!   - Gastal Raider: `functioning_abilities::active_static_definitions` via
//!     `evaluate_layers` (continuous P/T + granted keyword).
//!   - Hazoret, Godseeker: `combat::creature_cant_attack` +
//!     `combat::can_block_pair` (negated combat restriction).
//!   - Lightwheel Enhancements: `casting::graveyard_permission_sources` via
//!     the real `GameRunner::cast` pipeline (off-zone cast permission).
//!   - Racers' Scoreboard: `casting::evaluate_cost_mod_static_condition` via
//!     the real cast pipeline, measuring exact mana consumed (cost
//!     reduction).
//!
//! CR references for the new evidence (verified against
//! docs/MagicCompRules.txt):
//!   - CR 702.178a-b: "Max speed — [Ability]" grants `[Ability]` as long as
//!     the source's controller/owner has max speed, from whatever zone the
//!     granted ability functions from.
//!   - CR 108.4a + CR 109.5: a card with no controller (e.g. in a graveyard)
//!     substitutes its owner wherever "you"/"your" is read.
//!   - CR 508.1c, CR 509.1b: attack/block legality restrictions.
//!   - CR 601.2f: total-cost cost modification, scoped by caster.
//!   - CR 613.1f: the ability-adding layer for granted menace.
//!   - CR 613.1g, CR 613.4c: the P/T-modification layer for +1/+1.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::combat::{can_block_pair, creature_cant_attack};
use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::speed::{has_max_speed, increase_speed};
use engine::types::ability::{
    ContinuousModification, StaticCondition, StaticDefinition, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

/// The power a Racers'-Scoreboard-shaped HasMaxSpeed anthem grants to creatures
/// the controller owns while that player has max speed. Observable, so the test
/// can confirm the gated static is ACTIVE (or not) after layer derivation.
const ANTHEM_BONUS: i32 = 1;

/// Build a board for `P0` and return `(runner, gomif_id, beater_id)`.
///
/// `beater` is a vanilla 2/2 creature `P0` controls (the anthem's subject).
/// A Racers'-Scoreboard-shaped enchantment `P0` controls carries a `Continuous`
/// `AddPower`/`AddToughness` anthem gated by `StaticCondition::HasMaxSpeed` —
/// the static whose condition re-enters `has_max_speed`.
/// When `with_gomif` is true, `P0` also controls a Gomif-shaped permanent: a
/// `StaticDefinition { mode: SpeedCanIncreaseBeyondFour, condition: None }`.
fn build(
    with_gomif: bool,
) -> (
    engine::game::scenario::GameRunner,
    Option<ObjectId>,
    ObjectId,
) {
    let mut scenario = GameScenario::new();

    let beater = scenario.add_vanilla(P0, 2, 2);

    // Racers'-Scoreboard-shaped HasMaxSpeed-gated anthem.
    let scoreboard_static = StaticDefinition::new(StaticMode::Continuous)
        .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
        .modifications(vec![
            ContinuousModification::AddPower {
                value: ANTHEM_BONUS,
            },
            ContinuousModification::AddToughness {
                value: ANTHEM_BONUS,
            },
        ])
        .condition(StaticCondition::HasMaxSpeed);
    scenario
        .add_creature(P0, "Scoreboard", 0, 0)
        .with_static_definition(scoreboard_static);

    let gomif = with_gomif.then(|| {
        // Gomif-shaped: unconditionally allows speed to exceed 4.
        let gomif_static = StaticDefinition::new(StaticMode::SpeedCanIncreaseBeyondFour);
        scenario
            .add_creature(P0, "Gomif", 0, 0)
            .with_static_definition(gomif_static)
            .id()
    });

    let runner = scenario.build();
    (runner, gomif, beater)
}

fn set_speed(runner: &mut engine::game::scenario::GameRunner, player: PlayerId, speed: Option<u8>) {
    for p in runner.state_mut().players.iter_mut() {
        if p.id == player {
            p.speed = speed;
        }
    }
}

/// Drive the layer system and read `beater`'s effective power. This is the path
/// that evaluates the HasMaxSpeed-gated anthem's condition via
/// `active_static_definitions` — the recursing path. Returns the power computed
/// by the engine after derivation.
fn derived_power(runner: &mut engine::game::scenario::GameRunner, beater: ObjectId) -> i32 {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    runner
        .state()
        .objects
        .get(&beater)
        .expect("beater exists")
        .power
        .expect("creature has power")
}

/// TERMINATION: with the HasMaxSpeed-gated static present and `P0` at speed 4,
/// driving `evaluate_layers` (which evaluates the static's condition through the
/// recursing path) COMPLETES instead of overflowing the stack, `has_max_speed`
/// returns true, and the gated anthem applies (2/2 -> 3/3).
#[test]
fn layer_derivation_terminates_with_hasmaxspeed_gated_static() {
    let (mut runner, _gomif, beater) = build(false);
    set_speed(&mut runner, P0, Some(4));

    // Drives active_static_definitions condition-eval -> has_max_speed -> ...
    // This overflows the stack on the pre-guard code.
    let power = derived_power(&mut runner, beater);

    assert!(
        has_max_speed(runner.state(), P0),
        "speed 4 with no beyond-4 static is exactly max speed"
    );
    assert_eq!(
        power,
        2 + ANTHEM_BONUS,
        "HasMaxSpeed-gated anthem must apply at speed 4"
    );
}

/// SEMANTICS: speed 4 -> max speed true; speed 3 -> false (cap is 4 with no
/// beyond-4 static). At speed 3 the gated anthem is inactive, so the beater
/// stays 2/2; at speed 4 it becomes 3/3.
#[test]
fn max_speed_semantics_without_beyond_four() {
    let (mut runner, _gomif, beater) = build(false);

    set_speed(&mut runner, P0, Some(3));
    assert!(
        !has_max_speed(runner.state(), P0),
        "speed 3 is below max speed"
    );
    assert_eq!(
        derived_power(&mut runner, beater),
        2,
        "below max speed the HasMaxSpeed anthem must be inactive"
    );

    set_speed(&mut runner, P0, Some(4));
    assert!(has_max_speed(runner.state(), P0), "speed 4 is max speed");
    assert_eq!(
        derived_power(&mut runner, beater),
        2 + ANTHEM_BONUS,
        "at max speed the HasMaxSpeed anthem must be active"
    );
}

/// INTERACTION (discriminating case): `P0` at speed 5 WITH Gomif's
/// `SpeedCanIncreaseBeyondFour` static AND the HasMaxSpeed-gated anthem.
/// `has_max_speed` must be true (speed >= 4 with a beyond-4 static), AND the
/// gated anthem must be observably ACTIVE (beater 2/2 -> 3/3) — proving the
/// guard's base-cap re-entry answer did NOT corrupt the consumed result. The
/// real layer pass re-evaluates HasMaxSpeed through the unguarded outer call.
#[test]
fn beyond_four_speed_keeps_hasmaxspeed_static_active() {
    let (mut runner, gomif, beater) = build(true);
    assert!(gomif.is_some(), "Gomif static present for this case");
    set_speed(&mut runner, P0, Some(5));

    assert!(
        has_max_speed(runner.state(), P0),
        "speed 5 WITH a SpeedCanIncreaseBeyondFour static is max speed (>= 4)"
    );
    assert_eq!(
        derived_power(&mut runner, beater),
        2 + ANTHEM_BONUS,
        "the HasMaxSpeed-gated anthem must be active at speed 5 when a beyond-4 \
         static is present — the guard must not corrupt the consumed result"
    );
}

/// WITHOUT Gomif: `increase_speed` from 4 cannot exceed the default cap of 4,
/// and a speed of 5 set directly (no beyond-4 static) is NOT max speed (the cap
/// is exactly 4). Both paths run through `can_increase_speed_beyond_4` and must
/// terminate.
#[test]
fn no_beyond_four_static_caps_increase_and_max_speed_at_four() {
    let (mut runner, _gomif, _beater) = build(false);

    // increase_speed runs can_increase_speed_beyond_4; from 4 it stays 4.
    set_speed(&mut runner, P0, Some(4));
    let mut events = Vec::new();
    increase_speed(runner.state_mut(), P0, 1, &mut events);
    let speed_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .and_then(|p| p.speed);
    assert_eq!(
        speed_after,
        Some(4),
        "without a beyond-4 static, increase_speed is capped at 4"
    );

    // Speed 5 set directly with no beyond-4 static: cap is exactly 4, so 5 is
    // NOT max speed.
    set_speed(&mut runner, P0, Some(5));
    assert!(
        !has_max_speed(runner.state(), P0),
        "speed 5 with no beyond-4 static is not max speed (cap is exactly 4)"
    );
}

// ---------------------------------------------------------------------------
// Coverage-promotion runtime evidence: Gastal Raider (continuous P/T + keyword)
// ---------------------------------------------------------------------------

const GASTAL_RAIDER_ORACLE: &str = "Start your engines!\nWhen this creature enters, target opponent reveals their hand. You choose an instant or sorcery card from it. That player discards that card.\nMax speed — This creature gets +1/+1 and has menace.";
// Lowercased to match `database::synthesis::prepare_oracle_parser_input`,
// which lowercases every MTGJSON keyword name before it reaches the parser
// as a hint (several hint checks compare case-sensitively against a
// lowercase literal).
const GASTAL_RAIDER_KEYWORDS: &[&str] = &["max speed", "start your engines!"];

/// CR 613.1f + CR 613.1g + CR 613.4c + CR 702.178a: Gastal Raider's "Max speed — This
/// creature gets +1/+1 and has menace" is a continuous P/T + keyword grant
/// gated by `StaticCondition::HasMaxSpeed`, driven here through the real
/// `functioning_abilities::active_static_definitions` -> `evaluate_layers`
/// path (not asserted into existence). Owned by P1 but controlled by P0 with
/// OPPOSITE speeds at each row, so a gate that read the wrong player (owner
/// instead of controller, or vice versa) flips both assertions together —
/// see the Identity/Provenance contract: `static_def_applies` passes the
/// source's controller (CR 108.4/109.5 — "you" on a permanent is its
/// controller).
#[test]
fn gastal_raider_continuous_pt_and_menace_track_controller_speed() {
    let mut scenario = GameScenario::new();
    let gastal = scenario
        .add_creature(P1, "Gastal Raider", 2, 1)
        .from_oracle_text_with_keywords(GASTAL_RAIDER_KEYWORDS, GASTAL_RAIDER_ORACLE)
        .controlled_by(P0)
        .id();
    let mut runner = scenario.build();

    // Controller (P0) below max speed, owner (P1) AT max speed: the gated
    // static must stay inactive — an owner read would wrongly activate it.
    set_speed(&mut runner, P0, Some(3));
    set_speed(&mut runner, P1, Some(4));
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    {
        let obj = runner.state().objects.get(&gastal).expect("gastal exists");
        assert_eq!(obj.power, Some(2), "controller below max speed: base power");
        assert_eq!(
            obj.toughness,
            Some(1),
            "controller below max speed: base toughness"
        );
    }
    assert!(
        !object_has_effective_keyword_kind(runner.state(), gastal, KeywordKind::Menace),
        "controller below max speed: menace must not be granted"
    );

    // REACH GUARD: controller (P0) AT max speed, owner (P1) below — the
    // gated static must be observably active.
    set_speed(&mut runner, P0, Some(4));
    set_speed(&mut runner, P1, Some(3));
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    {
        let obj = runner.state().objects.get(&gastal).expect("gastal exists");
        assert_eq!(
            obj.power,
            Some(3),
            "controller at max speed: +1/+1 must apply"
        );
        assert_eq!(
            obj.toughness,
            Some(2),
            "controller at max speed: +1/+1 must apply"
        );
    }
    assert!(
        object_has_effective_keyword_kind(runner.state(), gastal, KeywordKind::Menace),
        "controller at max speed: menace must be granted (reach guard)"
    );
}

// ---------------------------------------------------------------------------
// Coverage-promotion runtime evidence: Hazoret, Godseeker (negated combat
// restriction)
// ---------------------------------------------------------------------------

const HAZORET_ORACLE: &str = "Indestructible, haste\nStart your engines! (If you have no speed, it starts at 1. It increases once on each of your turns when an opponent loses life. Max speed is 4.)\n{1}, {T}: Target creature with power 2 or less can't be blocked this turn.\nHazoret can't attack or block unless you have max speed.";
const HAZORET_KEYWORDS: &[&str] = &["haste", "indestructible", "start your engines!"];

/// CR 508.1c + CR 509.1b + CR 702.179e: Hazoret's "can't attack or block
/// unless you have max speed" is `Not(HasMaxSpeed)`-gated
/// `CantAttackOrBlock`, driven through the real production combat-legality
/// entry points `combat::creature_cant_attack` and `combat::can_block_pair`.
/// Owned by P1 but controlled by P0 with OPPOSITE speeds, exactly like the
/// Gastal Raider row above, so both consumers must move together with the
/// CONTROLLER's speed, not the owner's.
#[test]
fn hazoret_combat_restriction_tracks_controller_speed() {
    let mut scenario = GameScenario::new();
    let hazoret = scenario
        .add_creature(P1, "Hazoret, Godseeker", 5, 3)
        .from_oracle_text_with_keywords(HAZORET_KEYWORDS, HAZORET_ORACLE)
        .controlled_by(P0)
        .id();
    let attacker = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    // Controller (P0) below max speed: the restriction functions, so Hazoret
    // can neither attack nor block.
    set_speed(&mut runner, P0, Some(3));
    set_speed(&mut runner, P1, Some(4));
    assert!(
        creature_cant_attack(runner.state(), hazoret),
        "controller below max speed: Hazoret can't attack"
    );
    assert!(
        !can_block_pair(runner.state(), hazoret, attacker),
        "controller below max speed: Hazoret can't block"
    );

    // REACH GUARD: controller (P0) AT max speed: the restriction stops
    // functioning (CR 604.1), so Hazoret may do both.
    set_speed(&mut runner, P0, Some(4));
    set_speed(&mut runner, P1, Some(3));
    assert!(
        !creature_cant_attack(runner.state(), hazoret),
        "controller at max speed: Hazoret may attack (reach guard)"
    );
    assert!(
        can_block_pair(runner.state(), hazoret, attacker),
        "controller at max speed: Hazoret may block (reach guard)"
    );
}

// ---------------------------------------------------------------------------
// Coverage-promotion runtime evidence: Lightwheel Enhancements (off-zone cast
// permission)
// ---------------------------------------------------------------------------

const LIGHTWHEEL_ORACLE: &str = "Enchant creature or Vehicle\nStart your engines! (If you have no speed, it starts at 1. It increases once on each of your turns when an opponent loses life. Max speed is 4.)\nEnchanted permanent gets +1/+1 and has vigilance.\nMax speed — You may cast this card from your graveyard.";
// Lowercase "enchant" is load-bearing here, not cosmetic:
// `extract_granted_keyword_list`'s multi-type Enchant gate
// (`oracle_keyword.rs`) checks `mtgjson_keyword_names.iter().any(|n| n ==
// "enchant")` case-sensitively. Passing MTGJSON's printed "Enchant" instead
// silently drops the "Enchant creature or Vehicle" keyword grant.
const LIGHTWHEEL_KEYWORDS: &[&str] = &["enchant", "max speed", "start your engines!"];

fn floating_generic(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn lightwheel_scenario(
    p0_speed: Option<u8>,
    p1_speed: Option<u8>,
) -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_vanilla(P0, 2, 2);
    let lightwheel = scenario
        .add_spell_to_graveyard(P0, "Lightwheel Enhancements", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .from_oracle_text_with_keywords(LIGHTWHEEL_KEYWORDS, LIGHTWHEEL_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    set_speed(&mut runner, P0, p0_speed);
    set_speed(&mut runner, P1, p1_speed);
    (runner, lightwheel, host)
}

/// CR 108.4a + CR 604.1 + CR 702.178a-b: Lightwheel's "Max speed — You may
/// cast this card from your graveyard" is a `GraveyardCastPermission` static
/// gated by `HasMaxSpeed`, driven through the real
/// `casting::graveyard_permission_sources` (via
/// `spell_objects_available_to_cast`) and the full `GameRunner::cast`
/// pipeline. A graveyard card has no controller (CR 108.4), so the gate
/// reads its OWNER (P0); P0 at max speed with P1 below authorizes the cast.
#[test]
fn lightwheel_owner_at_max_speed_may_cast_it_from_the_graveyard() {
    let (mut runner, lightwheel, host) = lightwheel_scenario(Some(4), Some(3));
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&lightwheel),
        "owner at max speed: the graveyard cast permission must be offered"
    );

    let outcome = runner.cast(lightwheel).target_object(host).resolve();
    outcome.assert_zone(&[lightwheel], Zone::Battlefield);
}

/// The mirror of the row above: owner (P0) below max speed, opponent (P1) AT
/// max speed. CR 108.4a's owner substitution must not let the OPPONENT's
/// speed authorize the cast — the card stays in the graveyard.
#[test]
fn lightwheel_owner_below_max_speed_cannot_cast_it_from_the_graveyard() {
    let (mut runner, lightwheel, host) = lightwheel_scenario(Some(3), Some(4));
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&lightwheel),
        "owner below max speed: the opponent's max speed must not authorize the cast"
    );

    let result = runner.cast(lightwheel).target_object(host).try_resolve();
    let message = match &result {
        Err(err) => format!("{err:?}"),
        Ok(_) => "cast unexpectedly succeeded".to_string(),
    };
    assert!(
        result.is_err(),
        "owner below max speed: the cast must be rejected, got: {message}"
    );
    assert!(
        message.contains("castable"),
        "the refusal should name the castable-zone admission gate, got {message}"
    );
    assert_eq!(
        runner.state().objects[&lightwheel].zone,
        Zone::Graveyard,
        "a rejected cast leaves the card in the graveyard"
    );
}

// ---------------------------------------------------------------------------
// Coverage-promotion runtime evidence: Racers' Scoreboard (cost reduction)
// ---------------------------------------------------------------------------

const RACERS_SCOREBOARD_ORACLE: &str = "Start your engines! (If you have no speed, it starts at 1. It increases once on each of your turns when an opponent loses life. Max speed is 4.)\nWhen this artifact enters, draw two cards, then discard a card.\nMax speed — Spells you cast cost {1} less to cast.";
const RACERS_SCOREBOARD_KEYWORDS: &[&str] = &["max speed", "start your engines!"];

/// CR 601.2f + CR 604.1 + CR 702.178a: Racers' Scoreboard's "Max speed —
/// Spells you cast cost {1} less to cast" is a `HasMaxSpeed`-gated cost
/// modifier, driven through the real cast pipeline
/// (`casting::evaluate_cost_mod_static_condition`), measuring the EXACT mana
/// left in the pool after a successful cast rather than asserting a computed
/// cost into existence.
#[test]
fn racers_scoreboard_charges_full_cost_below_max_speed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_artifact_from_oracle(P0, "Racers' Scoreboard", RACERS_SCOREBOARD_ORACLE)
        .from_oracle_text_with_keywords(RACERS_SCOREBOARD_KEYWORDS, RACERS_SCOREBOARD_ORACLE);
    let spell = scenario
        .add_spell_to_hand(P0, "Test Sorcery", false)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.with_mana_pool(P0, floating_generic(4));
    let mut runner = scenario.build();
    set_speed(&mut runner, P0, Some(3));
    set_speed(&mut runner, P1, Some(4));

    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "controller below max speed: the full {{4}} cost must be paid"
    );
}

/// REACH GUARD for the row above: controller (P0) AT max speed pays the
/// reduced {{3}} cost, leaving exactly one mana of the four floated.
#[test]
fn racers_scoreboard_reduces_controllers_spells_at_max_speed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_artifact_from_oracle(P0, "Racers' Scoreboard", RACERS_SCOREBOARD_ORACLE)
        .from_oracle_text_with_keywords(RACERS_SCOREBOARD_KEYWORDS, RACERS_SCOREBOARD_ORACLE);
    let spell = scenario
        .add_spell_to_hand(P0, "Test Sorcery", false)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.with_mana_pool(P0, floating_generic(4));
    let mut runner = scenario.build();
    set_speed(&mut runner, P0, Some(4));
    set_speed(&mut runner, P1, Some(3));

    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(
        outcome.mana_pool_total(P0),
        1,
        "controller at max speed: the reduced {{3}} cost must consume exactly \
         three of the four floated mana"
    );
}

/// HOSTILE/sibling row: P1 is the active player and caster, at max speed;
/// P0 controls Racers' Scoreboard. `CostModifierCasterScope::You::admits`
/// (CR 109.5 + CR 601.2f) admits only the source's OWN controller (P0), so P1's max
/// speed must not discount P1's own spell — the full {{4}} cost is paid.
/// Without this row, a modifier that widened `You` to "any player at max
/// speed" would pass every assertion above (both rows share P0 as both
/// controller and caster).
#[test]
fn racers_scoreboard_does_not_discount_a_noncontrolling_caster_at_max_speed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_artifact_from_oracle(P0, "Racers' Scoreboard", RACERS_SCOREBOARD_ORACLE)
        .from_oracle_text_with_keywords(RACERS_SCOREBOARD_KEYWORDS, RACERS_SCOREBOARD_ORACLE);
    let spell = scenario
        .add_spell_to_hand(P1, "Test Sorcery", false)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.with_mana_pool(P1, floating_generic(4));
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    set_speed(&mut runner, P0, Some(3));
    set_speed(&mut runner, P1, Some(4));

    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(
        outcome.mana_pool_total(P1),
        0,
        "P1's max speed must not discount a spell when P0 controls the \
         Scoreboard (CostModifierCasterScope::You admits only the source's \
         own controller)"
    );
}
