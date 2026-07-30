//! CR 508.1d + CR 611.2c: true directing-carrier attribution for grafted
//! `MustAttackPlayer` combat requirements.
//!
//! A `MustAttackPlayer` requirement is never an intrinsic printed static — it is
//! always grafted onto its carrier creature by a directing object (an
//! `Effect::ForceAttack` / `Encore` / mass-coerce source) through an
//! `AddStaticMode` transient continuous effect. Before this change the combat
//! source collector could only attribute the requirement to the creature itself
//! (a DEFERRED comment). Now the layer materialization stamps the directing
//! object's id onto `StaticDefinition.source_object` (gated to the
//! attribution-mode class), and the combat producer surfaces that directing id
//! in `CombatRequirement.sources`.
//!
//! These tests drive the REAL pipeline: graft via
//! `add_transient_continuous_effect` (exactly as `effects/force_attack.rs`
//! does), recompute via `evaluate_layers`, then read the production authority
//! `attacker_constraints_for_active_player` (what `turns.rs` uses to populate the
//! `DeclareAttackers` waiting payload).

use std::sync::Arc;

use engine::game::combat::{
    attacker_constraints_for_active_player, get_valid_attacker_ids, CombatRequirement,
};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::static_abilities::object_crew_power_contribution;
use engine::types::ability::{ContinuousModification, Duration, StaticDefinition, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{CrewAction, CrewContributionKind, StaticMode};

/// Graft a `MustAttackPlayer { player }` requirement onto `creature` from the
/// directing object `source`, exactly as `Effect::ForceAttack` resolves it. The
/// stamp reads `effect.source_id` (= `source`), so the materialized static gains
/// `source_object == Some(source)`.
fn graft_must_attack_player(
    runner: &mut GameRunner,
    source: ObjectId,
    creature: ObjectId,
    player: PlayerId,
) {
    runner.state_mut().add_transient_continuous_effect(
        source,
        P0,
        Duration::UntilEndOfCombat,
        TargetFilter::SpecificObject { id: creature },
        vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::MustAttackPlayer { player },
        }],
        None,
    );
}

/// Install a functioning intrinsic static (set both live + base so it survives
/// `evaluate_layers`, which resets live to base each pass).
fn set_intrinsic_static(runner: &mut GameRunner, obj: ObjectId, def: StaticDefinition) {
    let object = runner.state_mut().objects.get_mut(&obj).unwrap();
    object.static_definitions = vec![def.clone()].into();
    object.base_static_definitions = Arc::new(vec![def]);
}

fn refresh(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

/// 7.1a — a grafted `MustAttackPlayer` requirement attributes the DIRECTING
/// object, not the creature. REVERT-FAIL: without the `source_object` stamp in
/// `layers.rs` (or the carrier threading in `combat.rs`), the carrier falls back
/// to the creature, so `sources == [creature]` and both the `contains(source)`
/// and `!contains(creature)` assertions fail.
#[test]
fn grafted_must_attack_player_attributes_directing_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let source = scenario.add_creature(P0, "Directing Source", 0, 1).id();
    let creature = scenario.add_creature(P0, "Forced Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;

    graft_must_attack_player(&mut runner, source, creature, P1);
    refresh(&mut runner);

    let valid = get_valid_attacker_ids(runner.state());
    assert!(
        valid.contains(&creature),
        "reach-guard: the forced creature is an eligible attacker"
    );

    let constraints = attacker_constraints_for_active_player(runner.state(), &valid);
    let Some(CombatRequirement::MustAttack { players, sources }) = constraints.get(&creature)
    else {
        panic!(
            "expected a MustAttack requirement for the forced creature, got {:?}",
            constraints.get(&creature)
        );
    };
    assert_eq!(players, &vec![P1], "the required defending player surfaces");
    assert!(
        sources.contains(&source),
        "the directing object is attributed as the requirement source"
    );
    assert!(
        !sources.contains(&creature),
        "the creature itself is NOT the source — the stamp fired (this is the whole change)"
    );
    // 7.4 drift pin: for the single attackable directive, its player is in
    // `players` iff its carrier is in `sources` (one scan feeds both).
    assert!(
        players.contains(&P1) == sources.contains(&source),
        "players and sources derive from one scan — no drift"
    );
}

/// 7.1b (paired reach-guard for 7.1a's negative) — an intrinsic generic
/// `MustAttack` (Curse of the Nightly Hunt class) attributes the CREATURE itself,
/// proving the producer is live and 7.1a's `!contains(creature)` is non-vacuous.
#[test]
fn generic_must_attack_attributes_the_creature_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let creature = scenario.add_creature(P0, "Berserker", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;

    set_intrinsic_static(
        &mut runner,
        creature,
        StaticDefinition::new(StaticMode::MustAttack).affected(TargetFilter::SelfRef),
    );
    refresh(&mut runner);

    let valid = get_valid_attacker_ids(runner.state());
    assert!(valid.contains(&creature), "reach-guard: eligible attacker");
    let constraints = attacker_constraints_for_active_player(runner.state(), &valid);
    assert_eq!(
        constraints.get(&creature),
        Some(&CombatRequirement::MustAttack {
            players: vec![],
            sources: vec![creature],
        }),
        "a generic must-attack creature is its own source (carrier fallback)"
    );
}

/// 7.1c — two distinct directing sources forcing the SAME creature to attack the
/// SAME player retain BOTH ids in `sources` (full-def dedup keeps both because
/// their `source_object` differs), while `players` deduplicates to one entry.
/// REVERT-FAIL: without per-source `source_object`, the two grafts collapse to
/// one def and `sources` carries a single (creature-fallback) id.
#[test]
fn two_sources_forcing_same_player_surface_both_and_dedup_players() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let s1 = scenario.add_creature(P0, "Lure One", 0, 1).id();
    let s2 = scenario.add_creature(P0, "Lure Two", 0, 1).id();
    let creature = scenario.add_creature(P0, "Doubly Forced", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;

    graft_must_attack_player(&mut runner, s1, creature, P1);
    graft_must_attack_player(&mut runner, s2, creature, P1);
    refresh(&mut runner);

    let valid = get_valid_attacker_ids(runner.state());
    let constraints = attacker_constraints_for_active_player(runner.state(), &valid);
    let Some(CombatRequirement::MustAttack { players, sources }) = constraints.get(&creature)
    else {
        panic!("expected MustAttack, got {:?}", constraints.get(&creature));
    };
    assert!(
        sources.contains(&s1) && sources.contains(&s2),
        "both directing sources are attributed ({sources:?})"
    );
    assert_eq!(
        players,
        &vec![P1],
        "the multi-source multiplicity lives in `sources`, not `players` (deduped set)"
    );
    // 7.4 drift pin for the multi-source case.
    for src in [s1, s2] {
        assert!(
            players.contains(&P1) == sources.contains(&src),
            "each attackable directive's player∈players iff its carrier∈sources"
        );
    }
}

/// 7.1d — after the directing object leaves the battlefield the requirement
/// persists (CR 611.2c: the continuous effect runs for its duration) and its
/// departed id is surfaced without panic (the collector never dereferences it).
#[test]
fn departed_directing_source_id_is_surfaced_without_panic() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let source = scenario.add_creature(P0, "Ephemeral Lure", 0, 1).id();
    let creature = scenario.add_creature(P0, "Forced Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;

    graft_must_attack_player(&mut runner, source, creature, P1);
    refresh(&mut runner);

    // The directing object leaves the battlefield; its UntilEndOfCombat effect is
    // NOT pruned by source departure (only by duration), so the requirement stays.
    runner.state_mut().objects.remove(&source);
    runner.state_mut().battlefield.retain(|&id| id != source);
    refresh(&mut runner);

    let valid = get_valid_attacker_ids(runner.state());
    let constraints = attacker_constraints_for_active_player(runner.state(), &valid);
    let Some(CombatRequirement::MustAttack { players, sources }) = constraints.get(&creature)
    else {
        panic!(
            "the requirement must persist after the source departs (CR 611.2c), got {:?}",
            constraints.get(&creature)
        );
    };
    assert_eq!(players, &vec![P1], "the requirement persists");
    assert!(
        sources.contains(&source),
        "the departed directing id is still surfaced (no panic, no silent drop)"
    );
}

/// 7.6 — the stamp is SCOPED to the attribution-mode class: a sibling
/// non-attribution mode (`CrewContribution`) grafted from two distinct sources
/// keeps `source_object == None`, so the two identical defs still collapse and
/// crew math is byte-identical to today (`base + 1`, NOT `base + 2`). REVERT-FAIL:
/// an UNCONDITIONAL stamp (dropping the `static_mode_carries_directing_source`
/// gate) splits the two crew grafts and yields `base + 2`. Discriminating
/// positive: the same two-source pattern with `MustAttackPlayer` (7.1c) DOES
/// split — together they prove the gate splits attribution modes and only those.
#[test]
fn source_object_stamp_scoped_to_attribution_modes() {
    let mut scenario = GameScenario::new();
    let s1 = scenario.add_creature(P0, "Crew Buff One", 0, 1).id();
    let s2 = scenario.add_creature(P0, "Crew Buff Two", 0, 1).id();
    let creature = scenario.add_creature(P0, "Crewer", 2, 2).id();
    let mut runner = scenario.build();

    for src in [s1, s2] {
        runner.state_mut().add_transient_continuous_effect(
            src,
            P0,
            Duration::Permanent,
            TargetFilter::SpecificObject { id: creature },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::CrewContribution {
                    kind: CrewContributionKind::PowerDelta { delta: 1 },
                    actions: vec![CrewAction::Crew],
                },
            }],
            None,
        );
    }
    refresh(&mut runner);

    assert_eq!(
        object_crew_power_contribution(runner.state(), creature, CrewAction::Crew),
        3,
        "two identical CrewContribution grafts collapse (source_object stays None): base 2 + 1, NOT + 2"
    );
}

/// 7.3 — the new `source_object` field is serde-defaulted: pre-existing
/// serialized statics (which never wrote the field) decode to `None`, and a
/// stamped value round-trips. Covers the `PersistedGameState` WASM restore
/// boundary. REVERT-FAIL: removing `#[serde(default)]` turns the omitted-field
/// blob into a decode error.
#[test]
fn static_definition_source_object_serde_default() {
    // Omitted field (legacy blob) decodes to None.
    let without = StaticDefinition::new(StaticMode::MustAttack);
    let json = serde_json::to_string(&without).unwrap();
    assert!(
        !json.contains("source_object"),
        "a None source_object is skipped on serialize (legacy-blob shape)"
    );
    let decoded: StaticDefinition = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.source_object, None);

    // A stamped value round-trips.
    let stamped = StaticDefinition::new(StaticMode::MustAttackPlayer {
        player: PlayerId(1),
    })
    .source_object(ObjectId(7));
    let round: StaticDefinition =
        serde_json::from_str(&serde_json::to_string(&stamped).unwrap()).unwrap();
    assert_eq!(round.source_object, Some(ObjectId(7)));
}
