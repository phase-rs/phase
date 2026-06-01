//! Integration tests for The Ur-Dragon's Eminence ability — a card-class
//! regression for cost-reduction statics that function from the command zone.
//!
//! Oracle text (relevant fragment):
//!   Eminence — As long as The Ur-Dragon is in the command zone or on the
//!   battlefield, other Dragon spells you cast cost {1} less to cast.
//!
//! CR references:
//!   - CR 113.6b — an ability that "expressly mentions a zone its source
//!     object is in" functions in that zone. The Eminence static lists both
//!     Battlefield and Command via the typed disjunction in its `condition`,
//!     so `populate_active_zones_from_condition` seeds
//!     `active_zones = [Battlefield, Command]`.
//!   - CR 207.2c — "Eminence" is an ability word with no rules meaning; the
//!     condition clause is what makes the static function from the command
//!     zone (the ability-word strip leaves the static intact).
//!   - CR 408 — the command zone.
//!   - CR 601.2f — cost-reduction effects apply during cost-determination.
//!
//! What this exercises end-to-end:
//!   1. A Dragon-creature card in hand is cast while The Ur-Dragon is in the
//!      command zone (NOT on the battlefield). The {3} base cost is reduced
//!      to {2} so only 2 of 3 available lands are tapped.
//!   2. The same scenario with The Ur-Dragon on the battlefield — same
//!      reduction (Eminence functions from either listed zone).
//!   3. The same scenario with The Ur-Dragon in the graveyard — NO reduction
//!      (graveyard is not a listed zone; CR 113.6b denies function).
//!   4. Casting a Dragon equal to the Eminence source from the command zone —
//!      the `Another` filter prevents self-reduction.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    ControllerRef, FilterProp, StaticCondition, StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::Zone;

/// Build The Ur-Dragon's Eminence static — typed `ReduceCost {1}` over
/// non-self Dragon spells the controller casts, gated by a typed Or-
/// disjunction that lists both Battlefield and Command in its `active_zones`.
///
/// This mirrors what `parse_static_line` produces from the printed Oracle
/// text (see the `static_eminence_*` parser tests in `oracle_static.rs`).
/// Built directly here so the integration test does not couple to the
/// parser's exact output shape — it asserts the casting-pipeline contract.
fn build_eminence_static() -> StaticDefinition {
    // Dragon-spell filter: subtype Dragon, controlled by You, Another.
    let dragon_filter = TargetFilter::Typed(
        TypedFilter::card()
            .controller(ControllerRef::You)
            .subtype("Dragon".to_string())
            .properties(vec![FilterProp::Another]),
    );
    StaticDefinition::new(StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        amount: ManaCost::generic(1),
        spell_filter: Some(dragon_filter),
        dynamic_count: None,
    })
    .affected(TargetFilter::Typed(
        TypedFilter::card().controller(ControllerRef::You),
    ))
    // CR 113.6b: typed Or-disjunction — Eminence functions from either zone.
    .condition(StaticCondition::Or {
        conditions: vec![
            StaticCondition::SourceInZone {
                zone: Zone::Battlefield,
            },
            StaticCondition::SourceInZone {
                zone: Zone::Command,
            },
        ],
    })
    // CR 113.6b: declare both functional zones so the casting cost-modifier
    // scan visits The Ur-Dragon's static regardless of the zone it lives in.
    .active_zones(vec![Zone::Battlefield, Zone::Command])
}

/// Configure a 3/3 Dragon creature in P0's hand with mana cost {3}.
fn add_dragon_spell(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_creature_to_hand(P0, name, 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 3,
        })
        .with_subtypes(vec!["Dragon"])
        .id()
}

fn cast_dragon_and_count_tapped(
    runner: &mut GameRunner,
    spell_id: ObjectId,
    lands: &[ObjectId],
) -> usize {
    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed when sufficient mana is available");
    lands
        .iter()
        .filter(|id| runner.state().objects[id].tapped)
        .count()
}

/// CR 113.6b: With The Ur-Dragon in the command zone, the Eminence static
/// functions per CR 113.6b's "expressly mentions a zone" rule and reduces a
/// Dragon spell's cost during cost-determination (CR 601.2f). Only 2 lands
/// tap to pay the reduced {2} cost.
#[test]
fn eminence_cost_reduction_active_from_command_zone() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Three lands — enough to pay the BASE {3} or the reduced {2}.
    let lands: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    // The Ur-Dragon: spawn on the battlefield, attach the Eminence static,
    // then move to the command zone via `with_commander` (CR 408).
    let ur_id = scenario
        .add_creature(P0, "The Ur-Dragon", 10, 10)
        .with_subtypes(vec!["Dragon", "Avatar"])
        .with_static_definition(build_eminence_static())
        .id();
    scenario.with_commander(ur_id);
    let spell_id = add_dragon_spell(&mut scenario, "Dragonling");

    let mut runner = scenario.build();
    let tapped = cast_dragon_and_count_tapped(&mut runner, spell_id, &lands);
    assert_eq!(
        tapped, 2,
        "CR 113.6b: Eminence in command zone must reduce {{3}} → {{2}} — only 2 lands tap"
    );
}

/// CR 113.6b: With The Ur-Dragon on the battlefield, the Eminence static
/// functions normally (battlefield is the CR 113.6 default and is also one
/// of the two listed zones in `active_zones`).
#[test]
fn eminence_cost_reduction_active_from_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lands: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    scenario
        .add_creature(P0, "The Ur-Dragon", 10, 10)
        .with_subtypes(vec!["Dragon", "Avatar"])
        .with_static_definition(build_eminence_static());
    let spell_id = add_dragon_spell(&mut scenario, "Dragonling");

    let mut runner = scenario.build();
    let tapped = cast_dragon_and_count_tapped(&mut runner, spell_id, &lands);
    assert_eq!(
        tapped, 2,
        "CR 113.6b: Eminence on battlefield must reduce {{3}} → {{2}}"
    );
}

/// CR 113.6b: A zone outside `active_zones` (graveyard) must NOT activate the
/// static — the full {3} is paid. This is the negative case the active_zones
/// gate guards against (without per-static zone gating an Eminence static
/// would happily reduce from anywhere).
#[test]
fn eminence_cost_reduction_inactive_from_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lands: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    // Put The Ur-Dragon directly into the graveyard so it carries the static
    // from a zone OUTSIDE `active_zones = [Battlefield, Command]`.
    scenario
        .add_creature_to_graveyard(P0, "The Ur-Dragon", 10, 10)
        .with_subtypes(vec!["Dragon", "Avatar"])
        .with_static_definition(build_eminence_static());
    let spell_id = add_dragon_spell(&mut scenario, "Dragonling");

    let mut runner = scenario.build();
    let tapped = cast_dragon_and_count_tapped(&mut runner, spell_id, &lands);
    assert_eq!(
        tapped, 3,
        "CR 113.6b: graveyard is not a listed zone — no reduction, all 3 lands tap"
    );
}

/// `FilterProp::Another` on the spell filter must exclude the Eminence
/// source's own card from matching. The most direct exercise of this rule is
/// the commander-cast pattern: The Ur-Dragon lives in the command zone (so
/// the Eminence static IS active — `active_zones` is satisfied), and the
/// spell-being-cast is the SAME object id as the static's source. The
/// `Another` filter must reject it and the {3} base cost must be paid in
/// full (3 lands tap), not reduced to {2} (2 lands tap).
///
/// Without this isolation (e.g. casting a *different* Dragon from hand with
/// The Ur-Dragon nowhere active) the `active_zones = [Battlefield, Command]`
/// gate already denies cost-reduction — the test would pass even if the
/// `Another` filter were removed. CR 113.6b's zone gate and CR 109.1's
/// identity-based "another" semantics are independent rules; this test
/// pins the latter by holding the former constant (static IS active).
#[test]
fn eminence_does_not_reduce_its_own_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lands: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    // Spawn The Ur-Dragon on the battlefield with the Eminence static,
    // then move it to the command zone (CR 408) via `with_commander`.
    // The static carries `active_zones = [Battlefield, Command]`, so it
    // functions from the command zone per CR 113.6b — the cost-determination
    // scan will visit it and evaluate the spell filter.
    let ur_id = scenario
        .add_creature(P0, "The Ur-Dragon", 10, 10)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 3,
        })
        .with_subtypes(vec!["Dragon", "Avatar"])
        .with_static_definition(build_eminence_static())
        .id();
    scenario.with_commander(ur_id);

    let mut runner = scenario.build();
    // CR 408 + CR 903.8: casting from the command zone requires a format
    // whose `command_zone` flag is set. The default scenario uses
    // `FormatConfig::standard()` (no command zone); flip just that single
    // flag so this test stays a focused isolation of `FilterProp::Another`
    // without taking on the wider `FormatConfig::commander()` semantics
    // (starting life 40, singleton, commander damage threshold, etc.).
    runner.state_mut().format_config.command_zone = true;

    let card_id = runner.state().objects[&ur_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: ur_id,
            card_id,
            targets: vec![],
        })
        .expect("commander cast from command zone should succeed with {3} available");
    let tapped = lands
        .iter()
        .filter(|id| runner.state().objects[id].tapped)
        .count();
    assert_eq!(
        tapped, 3,
        "CR 109.1: Eminence's `Another` filter must exclude its own source — the spell-being-cast and the static source share an object id, so the {{3}} cost must be paid in full (3 lands tap), not reduced to {{2}}"
    );
}
