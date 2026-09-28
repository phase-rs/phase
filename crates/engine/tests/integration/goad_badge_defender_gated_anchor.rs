//! CR 508.1c beats CR 508.1d, on the DEFENDER-ANCHORED half — plus the three
//! per-pairing axes `defending_player_controls_combat_anchor.rs` leaves
//! unreached.
//!
//! A `CantAttack` gated on `StaticCondition::DefendingPlayerControls` cannot be
//! answered by a CREATURE-LEVEL query: CR 506.2 + CR 508.5 determine the
//! defending player relative to an attacking creature and the target it is
//! declared against, and a "can this creature attack at all?" question carries
//! neither. `static_abilities::static_ability_match_applies` therefore DEFERS
//! such a static to the per-pairing authority `combat::attacker_can_attack_target`,
//! which does carry a target. That deferral is correct — but it also makes
//! `combat::creature_cant_attack_gated` answer `false` for every defender-gated
//! creature, and that bool was the whole CR 508.1d "if able" gate inside
//! `creature_must_attack_with_attackable_targets_gated`. Requirement/display and
//! legality were therefore two predicates that could disagree; they now share one
//! pairability authority (`combat::legal_attack_targets_iter`), whose LIST view
//! (`legal_attack_targets_for_attacker`) is the payload's per-attacker map and
//! whose short-circuiting EXISTENTIAL view
//! (`attacker_has_legal_attack_target`) is the "if able" gate.
//!
//! ROW 1 (`goaded_defender_gated_creature_badge_agrees_with_enforcement`) is the
//! regression pin for the resulting display/enforcement split. ROWS 2-4 are
//! additive coverage that already held before it:
//!   * ROW 2 — CR 508.5a: the defending player is individually determined for
//!     each attacking creature (3-player; main's anchor file is entirely
//!     two-player, where CR 506.2 fixes one defending player for the phase).
//!   * ROW 3 — CR 508.5a again, from the other side: ONE static definition, two
//!     recipients attacking two different defenders in the SAME combat, with
//!     opposite verdicts. Proves the anchor binds per RECIPIENT, not per carrier.
//!   * ROW 4 — a REMOTE carrier (`affected != SelfRef`) across its zone of
//!     function (CR 113.6b) and CR 702.26b phasing.
//!
//! Fixture discipline, inherited from `defending_player_controls_combat_anchor.rs`:
//! a SUCCESSFUL `declare_attackers` advances past the step, so every positive
//! control that succeeds runs on a CLONE, or runs last. A REJECTED declaration
//! leaves the step open and may be followed by another.

use std::collections::HashSet;

use engine::game::combat::{AttackTarget, CombatRequirement};
use engine::game::game_object::{PhaseOutCause, PhaseStatus};
use engine::game::perf_counters;
use engine::game::phasing::phase_out_object;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{StaticCondition, StaticDefinition, TargetFilter};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Verbatim Hammerhead Shark Oracle text (MTGJSON AtomicCards); 2/3.
const HAMMERHEAD_SHARK: &str =
    "This creature can't attack unless defending player controls an Island.";
/// Verbatim Tanglewalker Oracle text (MTGJSON AtomicCards); 2/2.
const TANGLEWALKER: &str =
    "Each creature you control can't be blocked as long as defending player controls an artifact land.";

// --- shared reach-guard helpers ---------------------------------------------

/// CR 506.2 + CR 508.5: does this condition tree carry a leaf whose answer
/// depends on WHICH player is the defending player? The test-local mirror of
/// `StaticCondition::needs_defending_player_anchor` (crate-private), used only
/// as a fixture reach-guard — never as a primary claim.
fn mentions_defending_player_controls(condition: &StaticCondition) -> bool {
    match condition {
        StaticCondition::DefendingPlayerControls { .. } => true,
        StaticCondition::Not { condition } => mentions_defending_player_controls(condition),
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            conditions.iter().any(mentions_defending_player_controls)
        }
        _ => false,
    }
}

/// THE reach-guard every row in this file depends on: the fixture card really
/// parsed to exactly ONE static of `mode` whose condition is defender-anchored.
/// Without it a permissive verdict is equally compatible with "the Oracle text
/// never produced a restriction at all".
fn assert_one_defender_gated_static(
    runner: &GameRunner,
    carrier: ObjectId,
    mode: &StaticMode,
    label: &str,
) {
    let obj = &runner.state().objects[&carrier];
    let matching: Vec<&StaticDefinition> = obj
        .static_definitions
        .iter_unchecked()
        .filter(|def| &def.mode == mode)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "REACH-GUARD ({label}): {} must carry exactly ONE {mode:?} static, or the \
         row says nothing about whether it applies; got {:?}",
        obj.name,
        obj.static_definitions
            .iter_unchecked()
            .map(|def| def.mode.clone())
            .collect::<Vec<_>>()
    );
    let condition = matching[0].condition.as_ref().unwrap_or_else(|| {
        panic!(
            "REACH-GUARD ({label}): {}'s {mode:?} static must be CONDITIONAL",
            obj.name
        )
    });
    assert!(
        mentions_defending_player_controls(condition),
        "REACH-GUARD ({label}): CR 506.2 + CR 508.5 — {}'s {mode:?} static must be \
         gated on the DEFENDING PLAYER's board for this row to exercise the \
         anchor; got {condition:?}",
        obj.name
    );
}

/// REACH-GUARD: the fixture land really is a Land with the named subtype and is
/// controlled by the intended player — otherwise a refusal is equally
/// compatible with "the board was never staged".
fn assert_land_with_subtype(
    runner: &GameRunner,
    land: ObjectId,
    controller: PlayerId,
    subtype: &str,
    label: &str,
) {
    let obj = &runner.state().objects[&land];
    assert!(
        obj.card_types.core_types.contains(&CoreType::Land)
            && obj.card_types.subtypes.iter().any(|s| s == subtype),
        "REACH-GUARD ({label}): the fixture land must be a Land with the {subtype} \
         subtype; got {:?} / {:?}",
        obj.card_types.core_types,
        obj.card_types.subtypes
    );
    assert_eq!(
        obj.controller, controller,
        "REACH-GUARD ({label}): the fixture land must be controlled by {controller:?}"
    );
}

fn advance_to_declare_attackers(runner: &mut GameRunner, label: &str) {
    runner.advance_to_combat();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "fixture must reach the declare-attackers step ({label}); got {:?}",
        runner.state().waiting_for.variant_name()
    );
}

/// The three declare-attackers payload views this file asserts against, read as
/// one borrow so the runner stays free afterwards.
struct AttackersPayload {
    valid: Vec<ObjectId>,
    constraints: std::collections::HashMap<ObjectId, CombatRequirement>,
    legal_targets: std::collections::HashMap<ObjectId, Vec<AttackTarget>>,
}

fn attackers_payload(runner: &GameRunner) -> AttackersPayload {
    let WaitingFor::DeclareAttackers {
        valid_attacker_ids,
        attacker_constraints,
        valid_attack_targets_by_attacker,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "must be at the declare-attackers step to read its payload; got {:?}",
            runner.state().waiting_for.variant_name()
        );
    };
    AttackersPayload {
        valid: valid_attacker_ids.clone(),
        constraints: attacker_constraints.clone(),
        legal_targets: valid_attack_targets_by_attacker
            .as_ref()
            .expect("the engine-authoritative per-attacker target map must be populated")
            .clone(),
    }
}

// ===========================================================================
// ROW 1 — the goad pin: display must agree with enforcement.
// ===========================================================================

/// CR 508.1c beats CR 508.1d: "a 'can't attack' restriction overrides an
/// 'attacks if able' requirement ... Enforcement must agree with display"
/// (`combat::creature_must_attack_with_attackable_targets_gated`'s own words).
///
/// A GOADED (CR 701.15b) Hammerhead Shark on a board where the defending player
/// controls no Island can attack NOBODY: its per-attacker legal-target list is
/// empty and declaring it is refused. Enforcement agrees — the empty
/// declaration is legal. Before the fix the badge did not: the "if able" gate
/// consulted `creature_cant_attack_gated`, whose creature-level query DEFERS a
/// defender-anchored gate and answers `false`, so the payload said `MustAttack`
/// about a creature the engine was happy to leave at home.
///
/// The three PRIMARY assertions on the production `DeclareAttackers` path are
/// labelled in the gate-unmet arm below: the per-attacker target map is EMPTY,
/// there is NO false `MustAttack` state, and an EMPTY declaration is ACCEPTED.
///
/// This is NOT the missing-`CantAttack`-badge case
/// `attacker_constraints_for_active_player` books as a known follow-up (that one
/// is a badge that is merely ABSENT, and this row deliberately asserts nothing
/// about it). This one emitted a positively WRONG badge in its place.
///
/// The Island arm is the paired positive control: same card, same goad, a
/// defender who DOES satisfy the gate — the requirement is real, the badge is
/// emitted, and the empty declaration is refused.
#[test]
fn goaded_defender_gated_creature_badge_agrees_with_enforcement() {
    for defender_island in [false, true] {
        let label = if defender_island {
            "Island (positive control)"
        } else {
            "no Island (the pin)"
        };

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let shark = scenario
            .add_creature_from_oracle(P0, "Hammerhead Shark", 2, 3, HAMMERHEAD_SHARK)
            .id();
        // Positive control: an unrestricted, UN-goaded co-attacker, so every
        // verdict below is specific to the shark rather than a dead step.
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let island = defender_island.then(|| scenario.add_basic_land(P1, ManaColor::Blue));
        let mut runner = scenario.build();

        assert_one_defender_gated_static(&runner, shark, &StaticMode::CantAttack, label);
        match island {
            Some(land) => assert_land_with_subtype(&runner, land, P1, "Island", label),
            // REACH-GUARD: the gate is genuinely UNMET — P1 controls nothing at
            // all, so "no Island" is a property of the board, not an accident of
            // how the Island was staged.
            None => {
                let p1_permanents: Vec<&str> = runner
                    .state()
                    .battlefield
                    .iter()
                    .filter_map(|id| runner.state().objects.get(id))
                    .filter(|o| o.controller == P1)
                    .map(|o| o.name.as_str())
                    .collect();
                assert!(
                    p1_permanents.is_empty(),
                    "REACH-GUARD ({label}): the gate must be unmet — P1 must \
                     control nothing; got {p1_permanents:?}"
                );
            }
        }

        // CR 701.15b: goad the shark, and ONLY the shark.
        runner
            .state_mut()
            .objects
            .get_mut(&shark)
            .unwrap()
            .goaded_by
            .insert(P1);
        // REACH-GUARD: the designation really landed, and the control creature
        // really did NOT get one (its badge-free-ness below must mean
        // "un-goaded", not "goad silently failed on both").
        assert!(
            runner.state().objects[&shark].goaded_by.contains(&P1),
            "REACH-GUARD ({label}): the shark must actually be goaded by P1"
        );
        assert!(
            runner.state().objects[&bear].goaded_by.is_empty(),
            "REACH-GUARD ({label}): the control bear must NOT be goaded"
        );

        advance_to_declare_attackers(&mut runner, label);
        let AttackersPayload {
            valid,
            constraints,
            legal_targets,
        } = attackers_payload(&runner);

        // REACH-GUARD (both arms): the unrestricted, un-goaded bear is offered
        // and badge-free.
        assert!(
            valid.contains(&bear) && !constraints.contains_key(&bear),
            "REACH-GUARD ({label}): the un-goaded bear must be offered and \
             badge-free; got valid={valid:?} constraints={constraints:?}"
        );
        // REACH-GUARD (both arms): the shark IS offered as a candidate — the
        // creature-level query deferred rather than refusing. This is the
        // documented state `attacker_constraints_for_active_player` books as a
        // follow-up; asserting it here keeps the row's claim about the BADGE and
        // not about candidacy.
        assert!(
            valid.contains(&shark),
            "REACH-GUARD ({label}): the defender-gated shark must still be offered \
             as a candidate (the creature-level query defers); got {valid:?}"
        );

        let shark_targets = legal_targets.get(&shark).cloned().unwrap_or_default();
        let badge = constraints.get(&shark);

        if defender_island {
            // PREMISE (positive control): the gate admits P1.
            assert!(
                shark_targets.contains(&AttackTarget::Player(P1)),
                "PREMISE ({label}): CR 506.2 + CR 508.1c — the shark must be able \
                 to attack the Island-controlling defender; got {shark_targets:?}"
            );
            // PRIMARY: the requirement is real, so the badge IS emitted.
            assert!(
                matches!(badge, Some(CombatRequirement::MustAttack { .. })),
                "CR 508.1d + CR 701.15b ({label}): a goaded creature that CAN \
                 attack must be badged MustAttack; got {badge:?}"
            );
            // PRIMARY: enforcement agrees — leaving it at home is illegal.
            let mut empty = GameRunner::from_state(runner.state().clone());
            assert!(
                empty.declare_attackers(&[]).is_err(),
                "CR 508.1d ({label}): the goaded shark can attack, so declaring no \
                 attackers must be refused"
            );
            // Positive control, run LAST because it advances past the step.
            assert!(
                runner
                    .declare_attackers(&[(shark, AttackTarget::Player(P1))])
                    .is_ok(),
                "CR 506.2 + CR 508.1c ({label}): the shark must be a legal \
                 attacker against the Island-controlling defender"
            );
        } else {
            // PRIMARY 1 of 3 — EMPTY TARGET MAP. The production
            // `WaitingFor::DeclareAttackers` payload's per-attacker map gives the
            // gated creature no selectable defender at all.
            assert!(
                shark_targets.is_empty(),
                "CR 508.1b + CR 508.1c ({label}): the per-attacker target map must \
                 be EMPTY for the defender-gated creature; got {shark_targets:?}"
            );
            // PRIMARY 2 of 3 — NO FALSE `MustAttack` STATE. Scope-tight: this row
            // claims only that "attacks if able" must not be displayed, never that
            // some other badge must appear in its place.
            assert!(
                !matches!(badge, Some(CombatRequirement::MustAttack { .. })),
                "CR 508.1c beats CR 508.1d ({label}): the goaded shark can attack \
                 NOBODY, so the payload must carry no false MustAttack state; got \
                 {badge:?}"
            );
            // SUPPORTING: the pairing really is refused downstream — the empty map
            // above is a legality verdict, not an unpopulated field. A rejected
            // declaration leaves the step open, so this runs on the live runner.
            assert!(
                runner
                    .declare_attackers(&[(shark, AttackTarget::Player(P1))])
                    .is_err(),
                "CR 508.1c ({label}): declaring the shark against the only defender \
                 must be REFUSED"
            );
            // PRIMARY 3 of 3 — EMPTY DECLARATION ACCEPTED. Enforcement's answer,
            // which display must match.
            let mut empty = GameRunner::from_state(runner.state().clone());
            assert!(
                empty.declare_attackers(&[]).is_ok(),
                "CR 508.1c beats CR 508.1d ({label}): the goaded shark is not ABLE \
                 to attack, so an EMPTY declaration must be ACCEPTED"
            );
            // Positive control, run LAST because it advances past the step.
            assert!(
                runner
                    .declare_attackers(&[(bear, AttackTarget::Player(P1))])
                    .is_ok(),
                "REACH-GUARD ({label}): the unrestricted bear must be a legal \
                 attacker — the step is alive"
            );
        }
    }
}

// ===========================================================================
// ROW 2 — CR 508.5a: the defending player is determined PER attacking creature.
// ===========================================================================

/// CR 508.5a: "the appropriate defending player is individually determined for
/// each of those attacking creatures." Main's anchor file is entirely
/// two-player, where CR 506.2 fixes ONE defending player for the whole combat
/// phase and a fix that resolved the anchor to "the nonactive player" would pass
/// every row in it. Three players, one Island, one creature: the SAME Hammerhead
/// Shark must be legal against P1 and refused against P2.
#[test]
fn hammerhead_shark_binds_the_defending_player_per_attacking_creature() {
    const LABEL: &str = "3-player per-defender binding";

    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let shark = scenario
        .add_creature_from_oracle(P0, "Hammerhead Shark", 2, 3, HAMMERHEAD_SHARK)
        .id();
    // Positive control: unrestricted, so the shark's exclusions below are about
    // its GATE and not about a defender being unattackable.
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let island = scenario.add_basic_land(P1, ManaColor::Blue);
    let mountain = scenario.add_basic_land(P2, ManaColor::Red);
    let mut runner = scenario.build();

    assert_one_defender_gated_static(&runner, shark, &StaticMode::CantAttack, LABEL);
    assert_land_with_subtype(&runner, island, P1, "Island", LABEL);
    // REACH-GUARD: P2's land is a real land that is NOT an Island — the arms
    // differ by the gate's subtype, not by "P2 has no board".
    assert_land_with_subtype(&runner, mountain, P2, "Mountain", LABEL);
    assert!(
        !runner.state().objects[&mountain]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Island"),
        "REACH-GUARD ({LABEL}): P2's land must NOT be an Island"
    );

    advance_to_declare_attackers(&mut runner, LABEL);
    let AttackersPayload {
        valid,
        legal_targets,
        ..
    } = attackers_payload(&runner);

    assert!(
        valid.contains(&bear) && valid.contains(&shark),
        "REACH-GUARD ({LABEL}): both creatures must be offered as candidates; got {valid:?}"
    );
    let shark_targets: HashSet<AttackTarget> = legal_targets
        .get(&shark)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let bear_targets: HashSet<AttackTarget> = legal_targets
        .get(&bear)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();
    // PAIRED CONTROL: P2 is attackable at all — by the unrestricted bear.
    assert!(
        bear_targets.contains(&AttackTarget::Player(P1))
            && bear_targets.contains(&AttackTarget::Player(P2)),
        "REACH-GUARD ({LABEL}): the unrestricted bear must be able to attack BOTH \
         opponents; got {bear_targets:?}"
    );
    // PRIMARY, per-pairing map: one creature, two defending players, two answers.
    assert!(
        shark_targets.contains(&AttackTarget::Player(P1)),
        "CR 508.5a ({LABEL}): P1 controls the Island, so that pairing satisfies \
         the gate; got {shark_targets:?}"
    );
    assert!(
        !shark_targets.contains(&AttackTarget::Player(P2)),
        "CR 508.5a ({LABEL}): P2 controls no Island, so that pairing must be \
         refused — \"defending player\" means ONE specific defending player; got \
         {shark_targets:?}"
    );

    // PRIMARY, real action. The refused declaration leaves the step open, so the
    // accepted one that ends it runs second.
    assert!(
        runner
            .declare_attackers(&[(shark, AttackTarget::Player(P2))])
            .is_err(),
        "CR 508.5a ({LABEL}): declaring the shark against P2, who controls no \
         Island, must be refused"
    );
    assert!(
        runner
            .declare_attackers(&[
                (shark, AttackTarget::Player(P1)),
                (bear, AttackTarget::Player(P2)),
            ])
            .is_ok(),
        "CR 508.5a ({LABEL}): the SAME shark attacking P1, who does control an \
         Island, must be legal — in the same declaration as the bear attacking P2"
    );
}

// ===========================================================================
// ROW 3 — ONE static definition, two recipients, two defenders, two verdicts.
// ===========================================================================

/// CR 508.5a from the other side. Tanglewalker's single `CantBeBlocked`
/// definition is scoped `Each creature you control` — ONE definition, several
/// recipients. Two of P0's creatures attack two DIFFERENT defending players in
/// the same combat, and that one definition must give them opposite verdicts.
/// A carrier-keyed anchor (or any "the defending player" singleton) cannot do
/// this; a recipient-keyed one must.
///
/// Tanglewalker itself never attacks, so the carrier has no attack pairing of
/// its own to be coincidentally right from. Both holders are tried so the
/// verdict cannot be an artifact of player-id ordering.
#[test]
fn one_static_definition_binds_each_recipient_to_its_own_defending_player() {
    for artifact_land_holder in [P1, P2] {
        let other_holder = if artifact_land_holder == P1 { P2 } else { P1 };
        let label = format!("artifact land held by {artifact_land_holder:?}");

        let mut scenario = GameScenario::new_n_player(3, 11);
        scenario.at_phase(Phase::PreCombatMain);
        let tanglewalker = scenario
            .add_creature_from_oracle(P0, "Tanglewalker", 2, 2, TANGLEWALKER)
            .id();
        let gated_attacker = scenario.add_creature(P0, "Gated Attacker", 2, 2).id();
        let plain_attacker = scenario.add_creature(P0, "Plain Attacker", 2, 2).id();
        let artifact_land = scenario
            .add_land_from_oracle(artifact_land_holder, "Seat of the Synod", "{T}: Add {U}.")
            .as_artifact()
            .id();
        let plain_land = scenario.add_basic_land(other_holder, ManaColor::Blue);
        let blocker_gated = scenario
            .add_creature(artifact_land_holder, "Wall A", 0, 6)
            .id();
        let blocker_plain = scenario.add_creature(other_holder, "Wall B", 0, 6).id();
        let mut runner = scenario.build();

        assert_one_defender_gated_static(&runner, tanglewalker, &StaticMode::CantBeBlocked, &label);
        // REACH-GUARD: the definition is REMOTE — scoped to a filter, not to the
        // carrier itself — which is what makes "one definition, two recipients"
        // the thing under test.
        let affected = runner.state().objects[&tanglewalker]
            .static_definitions
            .iter_unchecked()
            .find(|def| def.mode == StaticMode::CantBeBlocked)
            .and_then(|def| def.affected.clone());
        assert!(
            affected.is_some() && !matches!(affected, Some(TargetFilter::SelfRef)),
            "REACH-GUARD ({label}): Tanglewalker's definition must be scoped to a \
             filter covering OTHER creatures, not to itself; got {affected:?}"
        );
        // REACH-GUARD: one defender's land really is [Land, Artifact] and the
        // other's really is not — the two arms differ only in that.
        let artifact_types = &runner.state().objects[&artifact_land].card_types.core_types;
        assert!(
            artifact_types.contains(&CoreType::Land) && artifact_types.contains(&CoreType::Artifact),
            "REACH-GUARD ({label}): the artifact land must be [Land, Artifact]; got {artifact_types:?}"
        );
        assert!(
            !runner.state().objects[&plain_land]
                .card_types
                .core_types
                .contains(&CoreType::Artifact),
            "REACH-GUARD ({label}): the other defender's land must NOT be an artifact"
        );

        advance_to_declare_attackers(&mut runner, &label);
        runner
            .declare_attackers(&[
                (gated_attacker, AttackTarget::Player(artifact_land_holder)),
                (plain_attacker, AttackTarget::Player(other_holder)),
            ])
            .expect("both attackers are unrestricted — only BLOCKING is gated here");

        // REACH-GUARD: exactly the intended pairings are live, and the CARRIER is
        // not among them (CR 508.5 anchors on the attacking creature; if
        // Tanglewalker were itself attacking, a carrier-keyed anchor could answer
        // correctly by accident).
        let combat = runner
            .state()
            .combat
            .as_ref()
            .expect("combat state must exist after a declaration");
        assert!(
            !combat.attackers.iter().any(|a| a.object_id == tanglewalker),
            "REACH-GUARD ({label}): Tanglewalker must NOT be attacking"
        );
        assert!(
            combat.attackers.iter().any(
                |a| a.object_id == gated_attacker && a.defending_player == artifact_land_holder
            ),
            "REACH-GUARD ({label}): the gated attacker must be attacking the \
             artifact-land holder"
        );
        assert!(
            combat
                .attackers
                .iter()
                .any(|a| a.object_id == plain_attacker && a.defending_player == other_holder),
            "REACH-GUARD ({label}): the plain attacker must be attacking the other \
             defender"
        );

        // PRIMARY: the same definition, opposite verdicts, keyed by recipient.
        assert!(
            engine::game::combat::has_cant_be_blocked_static(runner.state(), gated_attacker),
            "CR 508.5a ({label}): the recipient attacking the ARTIFACT-LAND \
             defender must be unblockable"
        );
        assert!(
            !engine::game::combat::has_cant_be_blocked_static(runner.state(), plain_attacker),
            "CR 508.5a ({label}): the SAME definition must NOT restrict the \
             recipient attacking the defender who controls no artifact land"
        );

        // PRIMARY, through the block-legality map each defender actually sees.
        let gated_map = engine::game::combat::get_valid_block_targets_for_player(
            runner.state(),
            artifact_land_holder,
        );
        let plain_map =
            engine::game::combat::get_valid_block_targets_for_player(runner.state(), other_holder);
        assert!(
            plain_map
                .get(&blocker_plain)
                .is_some_and(|attackers| attackers.contains(&plain_attacker)),
            "PAIRED CONTROL ({label}): the defender with no artifact land must be \
             able to block; got {plain_map:?}"
        );
        assert!(
            gated_map
                .get(&blocker_gated)
                .is_none_or(|attackers| !attackers.contains(&gated_attacker)),
            "CR 508.5a ({label}): the artifact-land defender must have no legal \
             block against its attacker; got {gated_map:?}"
        );
    }
}

// ===========================================================================
// ROW 4 — a REMOTE defender-gated carrier across zone of function and phasing.
// ===========================================================================

/// CR 113.6b + CR 702.26b + CR 508.1c, with the restriction carried by a
/// DIFFERENT object than the creature it restricts (`affected != SelfRef`).
/// This is the only row that reaches the per-pairing door's own
/// `game_functioning_statics` sweep, so it is where the carrier's zone of
/// function and phased-out status are actually consulted.
///
/// The gate itself is TRUE in every arm — P1 controls the named land throughout
/// — so the row turns on the CARRIER's functioning state alone. The
/// battlefield/phased-in arm is the paired positive control: same board, same
/// gate, restriction applies.
///
/// The CR 113.6b arm keeps the carrier ON the battlefield and retunes its
/// definition to `active_zones = [Graveyard]`, rather than moving the carrier
/// into the graveyard. That is deliberate: `game_functioning_statics` sweeps
/// only `battlefield ∪ command_zone`, so a graveyard-RESIDENT carrier is never
/// visited at all and such an arm would pass without ever consulting
/// `active_zones`. Keeping the carrier in the swept set is what makes
/// `static_functions_in_zone` — the CR 113.6b authority — the thing under test.
#[test]
fn remote_defender_gated_cant_attack_follows_its_carriers_zone_and_phasing() {
    // (label, active_zones for the carrier's definition, phase the carrier out?,
    //  may the restricted creature legally attack?)
    let arms: [(&str, Vec<Zone>, bool, bool); 3] = [
        (
            "carrier on the battlefield, battlefield-scoped (positive control)",
            Vec::new(),
            false,
            false,
        ),
        (
            "carrier on the battlefield, definition functions only from the graveyard",
            vec![Zone::Graveyard],
            false,
            true,
        ),
        ("carrier phased out", Vec::new(), true, true),
    ];

    for (label, active_zones, phased_out, attack_legal) in arms {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let restricted = scenario.add_creature(P0, "Restrained Bear", 2, 2).id();
        // Positive control: never restricted, never phased out.
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let land = scenario.add_basic_land(P1, ManaColor::Green);
        // A REMOTE `CantAttack` scoped to one other creature and gated on the
        // defending player controlling that land (CR 506.2 + CR 508.5).
        let mut gate = StaticDefinition::new(StaticMode::CantAttack)
            .affected(TargetFilter::SpecificObject { id: restricted })
            .condition(StaticCondition::DefendingPlayerControls {
                filter: TargetFilter::SpecificObject { id: land },
            });
        if !active_zones.is_empty() {
            // CR 113.6b: an ability that states which zones it functions in
            // functions ONLY from those zones — so a graveyard-scoped definition
            // does not function while its carrier is on the battlefield.
            gate = gate.active_zones(active_zones.clone());
        }
        let carrier = scenario
            .add_creature(P0, "Watchful Sentry", 1, 1)
            .with_static_definition(gate)
            .id();
        let mut runner = scenario.build();

        assert_one_defender_gated_static(&runner, carrier, &StaticMode::CantAttack, label);
        assert_land_with_subtype(&runner, land, P1, "Forest", label);
        // REACH-GUARD: the restriction is REMOTE — the restricted creature carries
        // no attack restriction of its own, so every verdict below is about the
        // carrier.
        assert!(
            runner.state().objects[&restricted]
                .static_definitions
                .iter_unchecked()
                .all(|def| !matches!(
                    def.mode,
                    StaticMode::CantAttack | StaticMode::CantAttackOrBlock
                )),
            "REACH-GUARD ({label}): the restricted creature must carry NO attack \
             restriction of its own"
        );
        // REACH-GUARD: the definition's declared zones of function match the arm.
        assert_eq!(
            runner.state().objects[&carrier]
                .static_definitions
                .iter_unchecked()
                .find(|def| def.mode == StaticMode::CantAttack)
                .map(|def| def.active_zones.clone())
                .unwrap_or_default(),
            active_zones,
            "REACH-GUARD ({label}): the carrier's definition must declare the arm's \
             zones of function"
        );

        if phased_out {
            // CR 702.26b, through the production authority — not a hand-set field.
            let mut events: Vec<GameEvent> = Vec::new();
            phase_out_object(
                runner.state_mut(),
                carrier,
                PhaseOutCause::Directly,
                &mut events,
            );
        }
        // REACH-GUARD: the carrier is on the battlefield in every arm, and its
        // phase status matches the arm.
        assert_eq!(
            runner.state().objects[&carrier].zone,
            Zone::Battlefield,
            "REACH-GUARD ({label}): the carrier must be on the battlefield"
        );
        assert_eq!(
            matches!(
                runner.state().objects[&carrier].phase_status,
                PhaseStatus::PhasedOut {
                    cause: PhaseOutCause::Directly
                }
            ),
            phased_out,
            "REACH-GUARD ({label}): CR 702.26b — the carrier's phase status must \
             match the arm; got {:?}",
            runner.state().objects[&carrier].phase_status
        );

        advance_to_declare_attackers(&mut runner, label);

        // PAIRED CONTROL, on a clone because a successful declaration ends the
        // step: the unrestricted bear can always attack, in every arm.
        let mut control = GameRunner::from_state(runner.state().clone());
        assert!(
            control
                .declare_attackers(&[(bear, AttackTarget::Player(P1))])
                .is_ok(),
            "PAIRED CONTROL ({label}): the unrestricted bear must be a legal attacker"
        );

        // PRIMARY: the per-pairing door consults the carrier's functioning state.
        let declared = runner.declare_attackers(&[(restricted, AttackTarget::Player(P1))]);
        assert_eq!(
            declared.is_ok(),
            attack_legal,
            "CR 113.6b + CR 702.26b + CR 508.1c ({label}): the declaration must be \
             legal exactly when the carrier's restriction is NOT functioning; got \
             {declared:?}"
        );
    }
}

// ===========================================================================
// ROW 5 — the "if able" gate is EXISTENTIAL: it must short-circuit, not sweep.
// ===========================================================================

/// The CR 508.1d "if able" gate (ROW 1's fix) asks the shared pairability
/// authority whether ANY legal pairing exists. That question is answered by
/// `combat::attacker_has_legal_attack_target`, the short-circuiting view of
/// `combat::legal_attack_targets_iter` — NOT by collecting and sorting the
/// creature's whole legal-target list and testing it for emptiness.
///
/// The distinction is not cosmetic. `attacker_constraints_for_active_player`
/// reaches this gate once per must-attack creature and the AI's
/// mandatory-attacker filter reaches it once per candidate, on every combat,
/// with NO list consumer on either path. A collecting existential spends one
/// `attacker_can_attack_target` evaluation per defender in the universe — plus
/// an allocation and a sort — to learn one bit.
///
/// MEASUREMENT, not inspection: `pairability_evaluations` counts every
/// per-(attacker, defender) evaluation of `combat::attacker_can_attack_target`,
/// the one predicate both views of the sweep share. It is bumped inside that
/// function, so the cost cannot be hidden by moving it to another caller.
///
/// The row is revert-failing: restore the collecting
/// `legal_attack_targets_for_attacker(..).is_empty()` form of the gate and both
/// arms rise from `goaded` to `goaded * universe` (4 and 8 against the asserted
/// universe of 4), because every creature here is legal against the FIRST
/// defender the sweep reaches and every pairing after it is wasted.
///
/// The two arms are each other's control. One goaded creature costing 1 is
/// equally compatible with a dead counter that only fires once, or with a
/// universe of size 1; two goaded creatures costing exactly 2 on the SAME board
/// is not. The cost scales with the number of creatures asking the question and
/// is independent of the universe — which is what "short-circuits" means.
#[test]
fn must_attack_gate_short_circuits_on_the_first_legal_pairing() {
    for goaded in [1usize, 2] {
        let label = format!("existential if-able gate, {goaded} goaded");

        // Three players and a planeswalker apiece, so the defender universe is
        // strictly larger than the number of askers and a short-circuit is
        // distinguishable from a sweep. Nothing on this board restricts
        // attacking: every pairing is legal, so the FIRST one the sweep reaches
        // settles the existential question.
        let mut scenario = GameScenario::new_n_player(3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let bears: Vec<ObjectId> = (0..goaded)
            .map(|i| {
                scenario
                    .add_creature(P0, &format!("Grizzly Bears {i}"), 2, 2)
                    .id()
            })
            .collect();
        scenario.add_planeswalker_from_oracle(P1, "Jace Beleren", "Jace", 3, "");
        scenario.add_planeswalker_from_oracle(P2, "Chandra Nalaar", "Chandra", 6, "");
        let mut runner = scenario.build();

        // CR 701.15b: goad every bear — they are the only creatures on the
        // attacking team, so they are the only creatures that reach the gate.
        for &bear in &bears {
            runner
                .state_mut()
                .objects
                .get_mut(&bear)
                .unwrap()
                .goaded_by
                .insert(P1);
        }

        advance_to_declare_attackers(&mut runner, &label);

        // REACH-GUARD: the defender universe really is bigger than the number of
        // askers, so the counts below are short-circuits rather than restatements
        // of a tiny board.
        let universe = engine::game::combat::get_valid_attack_targets(runner.state());
        assert_eq!(
            universe.len(),
            4,
            "REACH-GUARD ({label}): CR 506.3 — two opponents and two planeswalkers \
             must all be attackable; got {universe:?}"
        );

        // REACH-GUARD: the gate is actually REACHED. A creature carrying no
        // requirement returns early, long before the existential gate, and would
        // also spend 0 evaluations — so a passing count would prove nothing.
        let payload = attackers_payload(&runner);
        for &bear in &bears {
            assert!(
                matches!(
                    payload.constraints.get(&bear),
                    Some(CombatRequirement::MustAttack { .. })
                ),
                "REACH-GUARD ({label}): CR 701.15b — every goaded bear must carry a \
                 MustAttack requirement, or the gate under measurement is never \
                 reached; got {:?}",
                payload.constraints.get(&bear)
            );
            // REACH-GUARD: the bear really can attack SOMETHING, so "1
            // evaluation" below is the sweep stopping at a LEGAL first pairing
            // rather than a board on which nothing is legal.
            //
            // This map is deliberately NOT used as the list view's control: the
            // payload publishes the CR 508.1d SOLVER's requirement-aware
            // selectable set, which is narrower than the raw pairability list
            // (goad is satisfied only by attacking a PLAYER other than P1, so
            // the two planeswalkers are dropped here even though every one of
            // the four pairings is legal under CR 508.1b). The two arms of this
            // row are each other's control instead.
            assert!(
                payload
                    .legal_targets
                    .get(&bear)
                    .is_some_and(|t| !t.is_empty()),
                "REACH-GUARD ({label}): the goaded bear must have at least one \
                 selectable defender; got {:?}",
                payload.legal_targets.get(&bear)
            );
        }
        assert_eq!(
            payload.valid.len(),
            goaded,
            "REACH-GUARD ({label}): the goaded bears must be the ONLY eligible \
             attackers, so every counted evaluation below is attributable to them; \
             got {:?}",
            payload.valid
        );

        // PRIMARY: the display/requirement path, which has no list consumer at
        // all — every evaluation it spends is spent answering "if able".
        let valid_attacker_ids = engine::game::combat::get_valid_attacker_ids(runner.state());
        perf_counters::reset();
        let constraints = engine::game::combat::attacker_constraints_for_active_player(
            runner.state(),
            &valid_attacker_ids,
        );
        let measured = perf_counters::attack_declaration_solver_snapshot();
        assert_eq!(
            constraints.len(),
            goaded,
            "REACH-GUARD ({label}): the measured call must have produced one badge \
             per goaded bear; got {constraints:?}"
        );
        assert_eq!(
            measured.pairability_evaluations,
            goaded as u64,
            "CR 508.1d ({label}): the \"if able\" gate asks whether ANY legal \
             pairing exists, so each asker must stop at the first one — {goaded} \
             evaluation(s) total, NOT one per defender in a universe of {}",
            universe.len()
        );
    }
}

// ===========================================================================
// ROW 5 — CONTAINMENT: the PROHIBITION polarity's published state is UNCHANGED
// by the permission authority.
// ===========================================================================

/// Badge emission for the deferred-anchor PROHIBITION polarity is a pre-existing
/// engine follow-up, whose defining comment sits inside
/// `combat::attacker_constraints_for_active_player`'s declare-attackers badge
/// walk. That gap belongs to the OPPOSITE polarity from the defender-anchored
/// PERMISSION class, and the permission work must neither create, widen nor
/// repair it.
///
/// The gap is defined by TWO observables, and this row reads BOTH plus
/// membership, because asserting the badge half alone would leave the
/// legal-target half — the half a permission-side change is most likely to move
/// — unguarded. All three are published together by
/// `combat::build_declare_attackers_waiting_for`, so the row is a single
/// published snapshot rather than three independent probes.
///
/// BASE VALUES ARE RECORDED, NOT INHERITED. Read at commit
/// `032c71408dbe9eb2a08707dc2e214066147f7c26` — the last commit before the
/// permission authority landed — on this exact board:
///
/// | observable | `gated` (deferred-anchor `CantAttack`) |
/// |---|---|
/// | `valid_attacker_ids` | PRESENT |
/// | `attacker_constraints` | NO entry (no `CantAttack` badge) |
/// | `valid_attack_targets_by_attacker` | EMPTY |
///
/// PAIRED POSITIVE CONTROLS, SAME FIXTURE: `badged` carries an UNCONDITIONAL
/// `CantAttack` — it is ABSENT from the eligible set AND carries the badge, so
/// both surfaces are proven to be read on this board and the badge-ABSENCE
/// assertion above cannot pass vacuously. `bear` is unrestricted — offered and
/// badge-free — so the eligibility surface is proven live in the other direction.
///
/// FIXTURE CONSTRUCTION: every static is attached with
/// `GameScenario::with_static_definition`, which pushes into BOTH
/// `static_definitions` and `base_static_definitions`. A bare
/// `obj.static_definitions.push(..)` is FORBIDDEN in this file: a
/// scenario-built object's `base_static_definitions` may already be non-empty,
/// in which case the one-shot live->base back-fill declines to copy and the
/// push is wiped by the FIRST layers flush. Each attached static's survival is
/// self-checked after `build()`, before the payload is read.
#[test]
fn deferred_anchor_prohibition_published_state_is_unchanged_by_the_permission_authority() {
    const LABEL: &str = "C2.7 containment";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The land the deferred anchor names. P1 is the defending player, and P1
    // DOES control it — so at every pairing the prohibition APPLIES and the
    // published target list is empty, while the creature-level query still
    // defers and offers the creature.
    let land = scenario.add_basic_land(P1, ManaColor::Green);
    let gated = scenario
        .add_creature(P0, "Anchored Prohibition", 2, 2)
        .with_static_definition(
            StaticDefinition::new(StaticMode::CantAttack)
                .affected(TargetFilter::SelfRef)
                .condition(StaticCondition::DefendingPlayerControls {
                    filter: TargetFilter::SpecificObject { id: land },
                }),
        )
        .id();
    // POSITIVE CONTROL for BOTH the badge surface and the eligibility surface:
    // an UNCONDITIONAL `CantAttack` is answerable at creature level, so this
    // creature is filtered OUT of the eligible set and lands in the `else if`
    // branch that emits the badge.
    let badged = scenario
        .add_creature(P0, "Plainly Restrained", 2, 2)
        .with_static_definition(
            StaticDefinition::new(StaticMode::CantAttack).affected(TargetFilter::SelfRef),
        )
        .id();
    // POSITIVE CONTROL in the other direction: unrestricted, offered, badge-free.
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    // REACH-GUARDS, after `build()` (and therefore after the first layers flush).
    assert_one_defender_gated_static(&runner, gated, &StaticMode::CantAttack, LABEL);
    assert_land_with_subtype(&runner, land, P1, "Forest", LABEL);
    let badged_statics: Vec<&StaticDefinition> = runner.state().objects[&badged]
        .static_definitions
        .iter_unchecked()
        .filter(|def| def.mode == StaticMode::CantAttack)
        .collect();
    assert_eq!(
        badged_statics.len(),
        1,
        "REACH-GUARD ({LABEL}): the control creature must carry exactly ONE \
         CantAttack static after the flush; got {:?}",
        runner.state().objects[&badged]
            .static_definitions
            .iter_unchecked()
            .map(|def| def.mode.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        badged_statics[0].condition.is_none(),
        "REACH-GUARD ({LABEL}): the control's CantAttack must be UNCONDITIONAL, \
         or it defers like `gated` and proves nothing about the badge surface"
    );

    advance_to_declare_attackers(&mut runner, LABEL);
    let AttackersPayload {
        valid,
        constraints,
        legal_targets,
    } = attackers_payload(&runner);

    // --- The controls FIRST, so nothing below can pass vacuously. ---
    assert!(
        valid.contains(&bear) && !constraints.contains_key(&bear),
        "CONTROL ({LABEL}): the unrestricted creature must be OFFERED and \
         BADGE-FREE — the eligibility surface is live; got valid={valid:?} \
         constraints={constraints:?}"
    );
    assert!(
        !valid.contains(&badged),
        "CONTROL ({LABEL}): an UNCONDITIONAL CantAttack must remove its creature \
         from the eligible set — the eligibility surface really filters; got \
         {valid:?}"
    );
    assert!(
        matches!(
            constraints.get(&badged),
            Some(CombatRequirement::CantAttack { .. })
        ),
        "CONTROL ({LABEL}): ... and must emit the CantAttack badge — the badge \
         surface is being read on this board, so the badge-ABSENCE assertion \
         below is not vacuous; got {:?}",
        constraints.get(&badged)
    );

    // --- The three base observables, UNCHANGED. ---
    assert!(
        valid.contains(&gated),
        "C2.7 ({LABEL}): base observable 1 of 3 — the deferred-anchor PROHIBITION \
         creature is OFFERED (the creature-level query DEFERS). Mapping \
         `CantAttack` to `Permission`, or returning `Some(true)` for a \
         prohibition, moves this; got {valid:?}"
    );
    // #9265: this observable INVERTED, deliberately. It previously asserted the
    // deferred-anchor creature carries NO badge — "the pre-existing display gap,
    // held UNCHANGED". That gap is now closed: a creature that is eligible but has
    // an empty legal-target set carries a `CantAttack` badge so the UI can explain
    // the refusal it was already enforcing.
    //
    // The row still earns its place. It pins that the badge is attributed, not just
    // present — an unattributed badge tells the player nothing about WHY — and the
    // `valid.contains(&gated)` assertion above still holds, so the creature remains
    // OFFERED. Eligibility and display are still separate observables; only the
    // display half moved.
    assert!(
        matches!(
            constraints.get(&gated),
            Some(CombatRequirement::CantAttack { .. })
        ),
        "C2.7 ({LABEL}): base observable 2 of 3 — the deferred-anchor creature now \
         carries a CantAttack badge (#9265 closed the display gap); got {:?}",
        constraints.get(&gated)
    );
    // An `if let` here would be a GUARD, not an assertion: a badge of another shape
    // would skip the block and the row would pass having checked nothing. Destructure
    // with `else { panic! }` so the shape is asserted on the way in.
    let Some(CombatRequirement::CantAttack { sources }) = constraints.get(&gated) else {
        panic!(
            "C2.7 ({LABEL}): expected a CantAttack badge on the deferred-anchor \
             creature before its sources can be read; got {:?}",
            constraints.get(&gated)
        );
    };
    // EXACT, not `!is_empty()`. A nonempty check passes when the badge names some
    // unrelated carrier, which is precisely the attribution defect this row exists
    // to catch — `bear` and `badged` are both on this board carrying statics of
    // their own, so a mode-only attribution has candidates to wrongly offer.
    assert_eq!(
        sources.as_slice(),
        &[gated],
        "C2.7 ({LABEL}): the badge must name EXACTLY the creature whose own \
         anchored prohibition explains the refusal — not merely some carrier; \
         got {sources:?} (gated={gated:?} badged={badged:?} bear={bear:?})"
    );
    let gated_targets = legal_targets.get(&gated).cloned().unwrap_or_default();
    assert!(
        gated_targets.is_empty(),
        "C2.7 ({LABEL}): base observable 3 of 3 — its published legal-target list \
         is EMPTY, because the per-pairing authority refuses every pairing. This \
         is the half a permission-side change is most likely to move, which is why \
         the badge half is not asserted alone; got {gated_targets:?}"
    );
}

/// ROW 5 — CR 702.3b + CR 508.1c: the no-legal-target badge must name the
/// definition that actually explains the refusal, not every object that happens
/// to carry a matching MODE.
///
/// `static_abilities::functioning_static_carriers` answers exactly one question:
/// "which objects carry a functioning definition of this mode?" It does not ask
/// whose `affected` filter reaches the badged creature, and it does not ask whether
/// the definition is the per-pairing kind. Attribution that stops there hands the
/// player a tooltip pointing at an unrelated permanent.
///
/// THE BOARD. `defender` is a Defender creature that is ELIGIBLE — a remote
/// unconditional permission grants it the ability to attack despite CR 702.3b — but
/// has NO legal target, because its own defending-player-anchored `CantAttack`
/// applies at every pairing. Two decoy permission carriers sit beside it, and each
/// fails a DIFFERENT conjunct of the attribution test:
///
///  * `remote_uncond` — reaches `defender` through its `affected` filter, but its
///    permission is UNCONDITIONAL. An unconditional permission holds for every
///    pairing, so it is never the reason a pairing was refused; the refusal here
///    comes from the prohibition. Fails the ANCHOR conjunct.
///  * `remote_excluding` — carries a properly ANCHORED permission, but its
///    `affected` filter names `decoy`, not `defender`. Fails the AFFECTED conjunct.
///
/// THE ASSERTION IS AN EXACT SET. `!sources.is_empty()` would pass on this board
/// with either decoy wrongly included, which is the whole defect; so would
/// `sources.contains(&defender)`. Only equality catches an over-broad source list.
///
/// WHY BOTH DECOYS ARE REQUIRED. Each conjunct needs its own witness. A fixture
/// carrying only `remote_excluding` is satisfied by an affected-filter check alone
/// and says nothing about the anchor test; a fixture carrying only `remote_uncond`
/// says nothing about the affected test. Dropping either decoy leaves half the
/// attribution rule unpinned.
#[test]
fn no_legal_target_badge_names_only_the_definition_that_explains_the_refusal() {
    const LABEL: &str = "badge attribution";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P1 is the defending player and DOES control this land, so the anchored
    // prohibition applies at every pairing and the legal-target list empties.
    let land = scenario.add_basic_land(P1, ManaColor::Green);
    let decoy = scenario.add_creature(P0, "Decoy Walker", 1, 1).id();
    let defender = scenario
        .add_creature(P0, "Anchored Sentinel", 0, 4)
        .with_keyword(engine::types::keywords::Keyword::Defender)
        .with_static_definition(
            StaticDefinition::new(StaticMode::CantAttack)
                .affected(TargetFilter::SelfRef)
                .condition(StaticCondition::DefendingPlayerControls {
                    filter: TargetFilter::SpecificObject { id: land },
                }),
        )
        .id();
    // DECOY 1 — reaches `defender`, but UNCONDITIONAL: fails the anchor conjunct.
    // It is also what makes `defender` eligible at all, so this object is load
    // bearing twice over and cannot be dropped from the fixture.
    let remote_uncond = scenario
        .add_creature(P0, "Unconditional Banner", 1, 1)
        .with_static_definition(
            StaticDefinition::new(StaticMode::CanAttackWithDefender)
                .affected(TargetFilter::SpecificObject { id: defender }),
        )
        .id();
    // DECOY 2 — properly ANCHORED, but names `decoy`: fails the affected conjunct.
    let remote_excluding = scenario
        .add_creature(P0, "Misdirected Standard", 1, 1)
        .with_static_definition(
            StaticDefinition::new(StaticMode::CanAttackWithDefender)
                .affected(TargetFilter::SpecificObject { id: decoy })
                .condition(StaticCondition::DefendingPlayerControls {
                    filter: TargetFilter::SpecificObject { id: land },
                }),
        )
        .id();
    let mut runner = scenario.build();

    // --- REACH-GUARDS: the board really has the shape the claims above assume. ---
    assert_land_with_subtype(&runner, land, P1, "Forest", LABEL);
    assert!(
        runner.state().objects[&defender].has_keyword(&engine::types::keywords::Keyword::Defender),
        "REACH-GUARD ({LABEL}): the badged creature must actually carry Defender, \
         or the permission arm of the attribution is never consulted"
    );
    assert_one_defender_gated_static(&runner, defender, &StaticMode::CantAttack, LABEL);
    // Both decoys must really be FUNCTIONING carriers of the permission mode —
    // otherwise they are filtered out upstream and never test attribution at all.
    for (id, name) in [
        (remote_uncond, "remote_uncond"),
        (remote_excluding, "remote_excluding"),
    ] {
        assert!(
            runner.state().objects[&id]
                .static_definitions
                .iter_unchecked()
                .any(|def| def.mode == StaticMode::CanAttackWithDefender),
            "REACH-GUARD ({LABEL}): {name} must carry a CanAttackWithDefender \
             definition, or it is not a decoy the attribution could wrongly name"
        );
    }
    // And they must differ on the conjunct each one is here to witness.
    assert!(
        runner.state().objects[&remote_uncond]
            .static_definitions
            .iter_unchecked()
            .find(|def| def.mode == StaticMode::CanAttackWithDefender)
            .is_some_and(|def| def.condition.is_none()),
        "REACH-GUARD ({LABEL}): remote_uncond's permission must be UNCONDITIONAL, \
         or it does not witness the anchor conjunct"
    );
    assert!(
        runner.state().objects[&remote_excluding]
            .static_definitions
            .iter_unchecked()
            .find(|def| def.mode == StaticMode::CanAttackWithDefender)
            .is_some_and(|def| def
                .condition
                .as_ref()
                .is_some_and(mentions_defending_player_controls)),
        "REACH-GUARD ({LABEL}): remote_excluding's permission must be ANCHORED, \
         or it does not witness the affected conjunct"
    );

    advance_to_declare_attackers(&mut runner, LABEL);
    let AttackersPayload {
        valid,
        constraints,
        legal_targets,
    } = attackers_payload(&runner);

    // --- The preconditions, ASSERTED. The badge only fires for a creature that is
    // --- eligible AND has no legal target; if either half moved, an exact-set
    // --- assertion below would be checking a badge that arrived for another reason.
    assert!(
        valid.contains(&defender),
        "PRECONDITION ({LABEL}): the Defender must be ELIGIBLE — the unconditional \
         remote permission grants it CR 702.3b relief; got valid={valid:?}"
    );
    let defender_targets = legal_targets.get(&defender).cloned().unwrap_or_default();
    assert!(
        defender_targets.is_empty(),
        "PRECONDITION ({LABEL}): its legal-target list must be EMPTY — the anchored \
         prohibition refuses every pairing; got {defender_targets:?}"
    );

    let Some(CombatRequirement::CantAttack { sources }) = constraints.get(&defender) else {
        panic!(
            "({LABEL}): expected a CantAttack badge on the eligible, target-less \
             Defender; got {:?}",
            constraints.get(&defender)
        );
    };
    assert_eq!(
        sources.as_slice(),
        &[defender],
        "({LABEL}): the badge must name EXACTLY the creature whose own anchored \
         prohibition refuses every pairing. Naming remote_uncond={remote_uncond:?} \
         means the ANCHOR conjunct was dropped (an unconditional permission cannot \
         explain a refused pairing); naming remote_excluding={remote_excluding:?} \
         means the AFFECTED conjunct was dropped (its permission names \
         decoy={decoy:?}, not this creature); got {sources:?}"
    );
}
