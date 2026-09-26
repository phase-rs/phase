//! Runtime proof that `StaticCondition::DefendingPlayerControls` is evaluated
//! CORRECTLY through the real combat pipeline, on both the census axis (CR
//! 109.2 + CR 108.4 + CR 110.1) and the anchor-resolution axis (CR 506.2 + CR
//! 508.1c + CR 508.5).
//!
//! Every test here drives `GameRunner::declare_attackers` /
//! `GameRunner::declare_blockers` — the real `GameAction` pipeline — never
//! `layers::evaluate_condition*` directly and never a parsed-AST shape as a
//! primary claim.
//!
//! Two reachability foot-guns shape every fixture:
//!   * a *successful* `declare_attackers`/`declare_blockers` ADVANCES past the
//!     step, so a reach-guard declaration must run BEFORE the assertion that
//!     succeeds, or against a cloned state;
//!   * the declare-blockers step AUTO-SUBMITS when no legal block exists
//!     anywhere (`combat.rs:6773` → `combat.rs:6337` →
//!     `game/engine.rs:9434-9441`), so a block-side fixture whose true arm
//!     must reach `WaitingFor::DeclareBlockers` needs an unrestricted
//!     co-attacker (T6) or must assert the auto-pass itself as the observable
//!     (T3).
//!
//! Scope note (documentation only — no behavior implied to be missing): the
//! `needs_defending_player_anchor` deferral gate at
//! `static_abilities.rs:823` and `combat.rs:3441` matches
//! `CantAttack | CantAttackOrBlock` and fires only on the ATTACK path
//! (`context.attack_target.is_none()`), which is everything this file
//! exercises. The block-legality collector
//! (`combat.rs:1434-1450` — `collect_blocker_restriction_statics`) filters
//! `CantBlock | CantAttackOrBlock` with no equivalent anchor check, so a
//! hypothetical `CantAttackOrBlock` gated on `DefendingPlayerControls` would
//! still be evaluated unanchored on the block half. No printed card pairs
//! `CantAttackOrBlock` with `DefendingPlayerControls` today, so this file has
//! no fixture for it and the block half stays unreached by any current card.

use engine::game::combat::AttackTarget;
use engine::game::combat::CombatRequirement;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ChosenAttribute;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Hazy Homunculus Oracle text (MTGJSON AtomicCards).
const HAZY: &str =
    "This creature can't be blocked as long as defending player controls an untapped land.";

/// T6 — `hazy_homunculus_controls_census_is_battlefield_scoped` (PRIMARY, Unit
/// 1 — the live bug).
///
/// CR 109.2 + CR 108.4 + CR 110.1: "defending player controls an untapped
/// land" means a permanent on the battlefield. Before this fix the census
/// swept `state.objects.values()`, which also counts the defending player's
/// graveyard, hand, and library — a card with no controller at all (CR
/// 108.4). Arm A3 plants an untapped Island in P1's graveyard while P1's only
/// BATTLEFIELD land is tapped, so the pre-fix census wrongly finds a match.
///
/// Arms:
///   * A1 — untapped land on the battlefield: restriction applies (`Err`).
///   * A2 — tapped land on the battlefield, nothing off-board: restriction
///     inactive (`Ok`) — the vacuity guard.
///   * A3 — tapped battlefield land + untapped graveyard Island: restriction
///     must stay inactive (`Ok`) — THE LIVE BUG this test guards.
#[test]
fn hazy_homunculus_controls_census_is_battlefield_scoped() {
    // (arm name, battlefield land tapped?, plant an untapped Island in P1's graveyard?)
    let arms = [
        ("A1", false, false),
        ("A2", true, false),
        ("A3", true, true),
    ];

    for (arm, battlefield_land_tapped, graveyard_island) in arms {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        // P0: Hazy plus an unrestricted co-attacker — required so the
        // declare-blockers step is reached at all (the auto-pass foot-gun).
        let hazy = scenario
            .add_creature_from_oracle(P0, "Hazy Homunculus", 1, 1, HAZY)
            .id();
        let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

        // P1: two 0/4 walls and a battlefield land.
        let wall = scenario.add_creature(P1, "Wall A", 0, 4).id();
        let wall2 = scenario.add_creature(P1, "Wall B", 0, 4).id();
        let land = scenario.add_basic_land(P1, ManaColor::Blue);

        if graveyard_island {
            scenario.add_land_to_graveyard(P1, "Island");
        }

        let mut runner = scenario.build();

        // REACH-GUARD (A3 only): the graveyard object is really a Land, in
        // P1's graveyard, untapped, controlled by P1 — otherwise an A3 `Ok`
        // is also compatible with "the fixture never planted the card".
        if graveyard_island {
            let gy_obj = runner
                .state()
                .objects
                .values()
                .find(|o| o.zone == Zone::Graveyard && o.controller == P1)
                .expect("A3 must plant a graveyard card controlled by P1");
            assert!(
                gy_obj.card_types.core_types.contains(&CoreType::Land),
                "the planted graveyard card must be a Land; got {:?}",
                gy_obj.card_types.core_types
            );
            assert!(
                !gy_obj.tapped,
                "the planted graveyard card must be untapped"
            );
        }

        if battlefield_land_tapped {
            runner.state_mut().objects.get_mut(&land).unwrap().tapped = true;
        }
        // REACH-GUARD: the battlefield land's tapped state matches the arm.
        assert_eq!(
            runner.state().objects[&land].tapped,
            battlefield_land_tapped,
            "arm {arm}: battlefield land tapped state must match the fixture"
        );

        runner.advance_to_combat();
        runner
            .declare_attackers(&[
                (hazy, AttackTarget::Player(P1)),
                (ally, AttackTarget::Player(P1)),
            ])
            .unwrap_or_else(|e| panic!("arm {arm}: both attackers must be legal: {e:?}"));
        runner.pass_both_players();

        // REACH-GUARD (all arms): actually reached DeclareBlockers with both
        // walls offered, and Hazy is recorded as attacking P1.
        let WaitingFor::DeclareBlockers {
            valid_blocker_ids, ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "arm {arm}: fixture must reach the declare-blockers prompt; got {:?}",
                runner.state().waiting_for.variant_name()
            );
        };
        assert!(
            valid_blocker_ids.contains(&wall) && valid_blocker_ids.contains(&wall2),
            "arm {arm}: both walls must be offered as blockers; got {valid_blocker_ids:?}"
        );
        let combat = runner
            .state()
            .combat
            .as_ref()
            .expect("combat state must exist at declare-blockers");
        assert!(
            combat
                .attackers
                .iter()
                .any(|a| a.object_id == hazy && a.defending_player == P1),
            "arm {arm}: Hazy must be recorded as attacking P1"
        );

        // REACH-GUARD (paired positive, on a cloned state): a legal block
        // exists against the unrestricted ally — proves the `Err` below is
        // specific to Hazy, not a dead step.
        let mut positive = GameRunner::from_state(runner.state().clone());
        assert!(
            positive.declare_blockers(&[(wall2, ally)]).is_ok(),
            "arm {arm}: positive control — wall2 must be able to block the unrestricted ally"
        );

        // PRIMARY, on a separately cloned state: does the restriction apply?
        let mut primary = GameRunner::from_state(runner.state().clone());
        let result = primary.declare_blockers(&[(wall, hazy)]);
        match arm {
            "A1" => assert!(
                result.is_err(),
                "arm A1: an untapped battlefield land must make Hazy unblockable"
            ),
            "A2" => assert!(
                result.is_ok(),
                "arm A2: no untapped land anywhere must leave Hazy blockable"
            ),
            "A3" => assert!(
                result.is_ok(),
                "arm A3 (THE LIVE BUG): an untapped Island in P1's GRAVEYARD must NOT \
                 satisfy \"defending player controls an untapped land\" — the census is \
                 battlefield-scoped (CR 109.2 + CR 108.4 + CR 110.1), not \
                 `state.objects.values()`-scoped"
            ),
            _ => unreachable!(),
        }
    }
}

/// Verbatim Tanglewalker Oracle text (MTGJSON AtomicCards).
const TANGLEWALKER: &str =
    "Each creature you control can't be blocked as long as defending player controls an artifact land.";

/// T3 — `tanglewalker_evasion_follows_the_defending_players_board` (PRIMARY,
/// remote carrier — multi-authority fixture).
///
/// Tanglewalker's `affected` filter is `Typed{[Creature], controller: You}` —
/// EVERY creature P0 controls, not just Tanglewalker itself — so once the fix
/// applies, the sole attacker (`bear`) becomes unblockable and no co-attacker
/// can rescue the declare-blockers prompt. The artifact-land arm's observable
/// is therefore the declare-blockers AUTO-PASS itself (`combat.rs:6773` →
/// `combat.rs:6337` → `game/engine.rs:9434-9441`), not a `declare_blockers`
/// `Err`.
///
/// Carrier (Tanglewalker) != recipient (`bear`, the affected attacker), and
/// the carrier never attacks at all — so the two candidate defender anchors
/// (`find(carrier)` vs `find(recipient)` in `combat.attackers`) genuinely
/// disagree. This is the multi-authority fixture the identity/provenance
/// contract requires.
#[test]
fn tanglewalker_evasion_follows_the_defending_players_board() {
    for defender_artifact_land in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        // P0: Tanglewalker stays home (never attacks) — bear is the sole,
        // affected attacker.
        let _tanglewalker = scenario
            .add_creature_from_oracle(P0, "Tanglewalker", 2, 2, TANGLEWALKER)
            .id();
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

        // P1: one wall, and in the true arm an artifact land.
        let wall = scenario.add_creature(P1, "Wall A", 0, 4).id();
        let seat = defender_artifact_land.then(|| {
            scenario
                .add_land_from_oracle(P1, "Seat of the Synod", "{T}: Add {U}.")
                .as_artifact()
                .id()
        });

        let mut runner = scenario.build();

        // BOARD REACH-GUARD (true arm): the seat is really [Land, Artifact].
        if let Some(seat_id) = seat {
            let core_types = &runner.state().objects[&seat_id].card_types.core_types;
            assert!(
                core_types.contains(&CoreType::Land) && core_types.contains(&CoreType::Artifact),
                "the artifact-land arm must actually give P1 a [Land, Artifact] permanent; \
                 got {core_types:?}"
            );
        }

        runner.advance_to_combat();
        runner
            .declare_attackers(&[(bear, AttackTarget::Player(P1))])
            .expect("bear must be a legal attacker");
        runner.pass_both_players();

        // COMBAT REACH-GUARDS (both arms): separates "auto-passed because
        // everything is unblockable" from "never reached combat at all".
        let combat = runner
            .state()
            .combat
            .as_ref()
            .expect("combat state must exist after declaring attackers");
        assert!(
            combat
                .attackers
                .iter()
                .any(|a| a.object_id == bear && a.defending_player == P1),
            "bear must be recorded as attacking P1"
        );
        let wall_obj = &runner.state().objects[&wall];
        assert!(
            wall_obj.zone == Zone::Battlefield && !wall_obj.tapped && wall_obj.controller == P1,
            "wall must be on the battlefield, untapped, and controlled by P1"
        );

        if defender_artifact_land {
            // PRIMARY: the step auto-submitted — no legal block exists
            // anywhere, because bear is unblockable.
            assert_ne!(
                runner.state().waiting_for.variant_name(),
                "DeclareBlockers",
                "artifact-land arm: bear must be unblockable, so the \
                 declare-blockers step must auto-pass rather than prompt; \
                 got {:?}",
                runner.state().waiting_for.variant_name()
            );
        } else {
            // PRIMARY: no restriction is active — the prompt is reached and
            // wall can legally block bear.
            let WaitingFor::DeclareBlockers {
                valid_block_targets,
                ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "no-artifact-land arm: must reach the declare-blockers prompt; got {:?}",
                    runner.state().waiting_for.variant_name()
                );
            };
            assert!(
                valid_block_targets
                    .get(&wall)
                    .is_some_and(|targets| targets.contains(&bear)),
                "no-artifact-land arm: wall must be able to block bear; got {valid_block_targets:?}"
            );
            assert!(
                runner.declare_blockers(&[(wall, bear)]).is_ok(),
                "no-artifact-land arm: wall blocking bear must be legal"
            );
        }
    }
}

/// Verbatim Arctic Foxes Oracle text (MTGJSON AtomicCards).
const ARCTIC_FOXES: &str = "This creature can't be blocked by creatures with power 2 or greater as long as defending player controls a snow land.";

/// T5 — `arctic_foxes_evasion_unchanged` (regression guard, added in Unit 3).
///
/// The only `CantBeBlockedBy` card in the pool. Units 1-3 rewrite the defender
/// resolution and census on exactly this path, so this test must stay green
/// BEFORE and AFTER those changes — it guards the 6 already-working `SelfRef`
/// block-side cards against a silent regression while "fixing" the other 39.
///
/// The power-1 blocker needs no co-attacker to reach the declare-blockers
/// prompt: Arctic Foxes only restricts power->=2 blockers, so a power-1
/// blocker is always a legal block and is itself the reach guarantee.
#[test]
fn arctic_foxes_evasion_unchanged() {
    for defender_snow_land in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        let foxes = scenario
            .add_creature_from_oracle(P0, "Arctic Foxes", 1, 3, ARCTIC_FOXES)
            .id();

        let strong_blocker = scenario.add_creature(P1, "Wall of Power", 2, 4).id();
        let weak_blocker = scenario.add_creature(P1, "Wall of Weakness", 1, 4).id();
        let land = scenario.add_basic_land(P1, ManaColor::Blue);

        let mut runner = scenario.build();

        if defender_snow_land {
            let obj = runner.state_mut().objects.get_mut(&land).unwrap();
            obj.card_types.supertypes.push(Supertype::Snow);
            obj.base_card_types.supertypes.push(Supertype::Snow);
        }
        // REACH-GUARD: the land's snow supertype matches the arm.
        assert_eq!(
            runner.state().objects[&land]
                .card_types
                .supertypes
                .contains(&Supertype::Snow),
            defender_snow_land,
            "the land's Snow supertype must match the fixture arm"
        );

        runner.advance_to_combat();
        runner
            .declare_attackers(&[(foxes, AttackTarget::Player(P1))])
            .expect("foxes must be a legal attacker");
        runner.pass_both_players();

        // REACH-GUARD (both arms): the weak blocker keeps the step from
        // auto-passing, so DeclareBlockers is always reached here.
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareBlockers { .. }
            ),
            "fixture must reach the declare-blockers prompt; got {:?}",
            runner.state().waiting_for.variant_name()
        );

        // EXTRA GUARD (both arms): a power-1 blocker is unaffected by the
        // evasion — isolates the CantBeBlockedBy filter from the condition.
        let mut weak_attempt = GameRunner::from_state(runner.state().clone());
        assert!(
            weak_attempt
                .declare_blockers(&[(weak_blocker, foxes)])
                .is_ok(),
            "a power-1 blocker must be able to block Arctic Foxes regardless \
             of the defending player's board (snow land = {defender_snow_land})"
        );

        // PRIMARY: the power->=2 blocker is gated on the snow-land condition.
        let mut strong_attempt = GameRunner::from_state(runner.state().clone());
        let result = strong_attempt.declare_blockers(&[(strong_blocker, foxes)]);
        if defender_snow_land {
            assert!(
                result.is_err(),
                "a power-2 blocker must be unable to block Arctic Foxes when \
                 the defending player controls a snow land"
            );
        } else {
            assert!(
                result.is_ok(),
                "a power-2 blocker must be able to block Arctic Foxes when \
                 the defending player controls no snow land"
            );
        }
    }
}

/// Verbatim Sea Monster Oracle text (MTGJSON AtomicCards).
const SEA_MONSTER: &str = "This creature can't attack unless defending player controls an Island.";

/// T1 — `sea_monster_attack_legality_follows_the_defending_players_board`
/// (PRIMARY, attack side, Unit 4 — the CR 508.1c declaration-time deferral).
///
/// Drives the real `GameAction::DeclareAttackers` via
/// `runner.declare_attackers(...)`. Does NOT call `layers::evaluate_condition*`
/// and does NOT assert AST shape as a primary claim.
///
/// CR 506.2 + CR 508.1c: in this two-player fixture the nonactive player is
/// the defending player for the whole combat phase, so the printed "unless
/// defending player controls an Island" gate must be answerable at
/// declare-attackers time — BEFORE CR 508.1k records the attacker.
#[test]
fn sea_monster_attack_legality_follows_the_defending_players_board() {
    for defender_island in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        let sea = scenario
            .add_creature_from_oracle(P0, "Sea Monster", 6, 6, SEA_MONSTER)
            .id();
        // Positive control: an unrestricted attacker, proving any `Err` on
        // `sea` is specific rather than a dead step.
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let island = defender_island.then(|| scenario.add_basic_land(P1, ManaColor::Blue));

        let mut runner = scenario.build();

        // BOARD REACH-GUARD (Island arm): without this an (Err, Err) outcome
        // is also compatible with "the fixture never gave P1 an Island".
        if let Some(land) = island {
            let obj = &runner.state().objects[&land];
            assert!(
                obj.card_types.core_types.contains(&CoreType::Land)
                    && obj.card_types.subtypes.iter().any(|s| s == "Island"),
                "the Island arm must actually give P1 a Land with the Island \
                 subtype; got {:?} / {:?}",
                obj.card_types.core_types,
                obj.card_types.subtypes
            );
        }

        runner.advance_to_combat();
        let WaitingFor::DeclareAttackers {
            valid_attacker_ids,
            valid_attack_targets_by_attacker,
            attacker_constraints,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "fixture must reach the declare-attackers prompt; got {:?}",
                runner.state().waiting_for.variant_name()
            );
        };

        // REACH-GUARD A (positive, both arms): the unrestricted bear is a
        // valid attacker.
        assert!(
            valid_attacker_ids.contains(&bear),
            "positive control: the unrestricted bear must be a valid attacker \
             (defender_island = {defender_island}); got {valid_attacker_ids:?}"
        );
        // REACH-GUARD B (Unit 4's observable, both arms): Sea Monster is
        // offered as a candidate (the creature-level deferral let it through)
        // and carries no confounding MustAttack requirement.
        assert!(
            valid_attacker_ids.contains(&sea),
            "Sea Monster must be offered as a candidate attacker (the \
             creature-level query must defer, not refuse); \
             (defender_island = {defender_island}); got {valid_attacker_ids:?}"
        );
        // The assertion is about a MustAttack CONFOUND, so it tests for that rather
        // than for badge absence. On the no-Island arm Sea Monster legitimately
        // carries a `CantAttack` badge: the deferred prohibition leaves it eligible
        // with an empty target set (asserted below), which is exactly the state
        // #9265 added a badge for. Asserting `is_none()` here would pin the display
        // gap this suite's own `sea_targets.is_empty()` documents.
        assert!(
            !matches!(
                attacker_constraints.get(&sea),
                Some(CombatRequirement::MustAttack { .. })
            ),
            "Sea Monster must carry no confounding MustAttack requirement; got {:?}",
            attacker_constraints.get(&sea)
        );

        // PRIMARY, per-pairing map: proves the per-pairing authority decided,
        // independent of the action verdict below.
        let map = valid_attack_targets_by_attacker
            .as_ref()
            .expect("engine-authoritative per-attacker target map must be populated");
        let sea_targets = map.get(&sea).cloned().unwrap_or_default();
        if defender_island {
            assert!(
                sea_targets.contains(&AttackTarget::Player(P1)),
                "Island arm: Sea Monster must be able to target P1; got {sea_targets:?}"
            );
        } else {
            assert!(
                sea_targets.is_empty(),
                "no-Island arm: Sea Monster must have no selectable targets; \
                 got {sea_targets:?}"
            );
        }

        // PRIMARY, real action. Ordering matters: a SUCCESSFUL
        // `declare_attackers` advances past the step, so the Island arm's
        // positive control must run on a CLONE — the real runner stays live
        // for the assertion that follows it.
        if defender_island {
            let mut positive = GameRunner::from_state(runner.state().clone());
            assert!(
                positive
                    .declare_attackers(&[(bear, AttackTarget::Player(P1))])
                    .is_ok(),
                "positive control: the unrestricted bear must be a legal \
                 attacker (defender_island = true)"
            );
            assert!(
                runner
                    .declare_attackers(&[(sea, AttackTarget::Player(P1))])
                    .is_ok(),
                "CR 506.2 + CR 508.1c: Sea Monster must be a legal attacker \
                 when the defending player controls an Island"
            );
        } else {
            assert!(
                runner
                    .declare_attackers(&[(sea, AttackTarget::Player(P1))])
                    .is_err(),
                "CR 506.2 + CR 508.1c: Sea Monster must be refused as an \
                 attacker when the defending player controls no Island"
            );
            assert!(
                runner
                    .declare_attackers(&[(bear, AttackTarget::Player(P1))])
                    .is_ok(),
                "positive control: the unrestricted bear must be a legal \
                 attacker (defender_island = false)"
            );
        }
    }
}

/// Verbatim Orgg Oracle text (MTGJSON AtomicCards).
const ORGG: &str = "Trample\nThis creature can't attack if defending player controls an untapped creature with power 3 or greater.\nThis creature can't block creatures with power 3 or greater.";

/// T2 — `orgg_attack_legality_follows_the_defending_players_board` (hostile
/// sibling: un-negated grammar).
///
/// The 5 "if" cards carry a BARE `DefendingPlayerControls` (no `Not`), whose
/// polarity is inverted from Sea Monster's "unless": BEFORE this fix the bare
/// condition evaluated unanchored to `false`, so the restriction failed OPEN —
/// Orgg could attack on every board, including one where the defender has an
/// untapped 3-power creature. It now binds via `declared_attack`, which is what
/// this test asserts. Holds a power-3 creature present in BOTH arms and varies
/// only `tapped`, isolating the gate rather than board size. Orgg's own
/// `BlockRestriction` static carries `condition: null` and does not
/// participate.
#[test]
fn orgg_attack_legality_follows_the_defending_players_board() {
    for defender_creature_untapped in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        let orgg = scenario
            .add_creature_from_oracle(P0, "Orgg", 6, 6, ORGG)
            .id();
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let threat = scenario.add_creature(P1, "Threat", 3, 3).id();

        let mut runner = scenario.build();

        if !defender_creature_untapped {
            runner.state_mut().objects.get_mut(&threat).unwrap().tapped = true;
        }
        // REACH-GUARD: the threat's tapped state matches the arm.
        assert_eq!(
            !runner.state().objects[&threat].tapped,
            defender_creature_untapped,
            "the threat's tapped state must match the fixture arm"
        );

        runner.advance_to_combat();
        let WaitingFor::DeclareAttackers {
            valid_attacker_ids, ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "fixture must reach the declare-attackers prompt; got {:?}",
                runner.state().waiting_for.variant_name()
            );
        };
        assert!(
            valid_attacker_ids.contains(&bear),
            "positive control: the unrestricted bear must be a valid attacker"
        );

        // PRIMARY, real action, PR-C ordering discipline.
        if defender_creature_untapped {
            assert!(
                runner
                    .declare_attackers(&[(orgg, AttackTarget::Player(P1))])
                    .is_err(),
                "CR 506.2 + CR 508.1c: Orgg must be refused as an attacker \
                 when the defending player controls an untapped power->=3 \
                 creature"
            );
            assert!(
                runner
                    .declare_attackers(&[(bear, AttackTarget::Player(P1))])
                    .is_ok(),
                "positive control: the unrestricted bear must be a legal \
                 attacker"
            );
        } else {
            let mut positive = GameRunner::from_state(runner.state().clone());
            assert!(
                positive
                    .declare_attackers(&[(bear, AttackTarget::Player(P1))])
                    .is_ok(),
                "positive control: the unrestricted bear must be a legal \
                 attacker"
            );
            assert!(
                runner
                    .declare_attackers(&[(orgg, AttackTarget::Player(P1))])
                    .is_ok(),
                "Orgg must be a legal attacker when the defending player's \
                 power->=3 creature is tapped"
            );
        }
    }
}

/// T1b — `sea_monster_attack_legality_follows_planeswalker_defending_player`
/// (Unit 4 sibling, `AttackTarget::Planeswalker` arm).
///
/// T1 only ever declares `AttackTarget::Player(P1)`, so it never reaches
/// `combat::defending_player_for_target`'s `AttackTarget::Planeswalker` arm —
/// CR 508.5: the defending player for an attack on a planeswalker is that
/// planeswalker's CONTROLLER, not the planeswalker itself. Same fixture as
/// T1 (Sea Monster / island-controlled-by-defender), except Sea Monster
/// attacks a planeswalker P1 controls: legality must still track P1's board
/// (the planeswalker's controller). The planeswalker's owner is set to P0
/// (a Control-Magic-class split) so this is a genuine discriminator: with
/// owner == controller (the un-split default), a fix that read `owner`
/// instead of `controller` at `combat.rs:1377` would pass this test anyway —
/// splitting them is what proves the anchor resolves through the controller
/// and not through some other property of the planeswalker object.
///
/// `AttackTarget::Battle` is covered separately by T1c below (same Sea
/// Monster / island fixture, protector != controller via a hand-built
/// Siege).
#[test]
fn sea_monster_attack_legality_follows_planeswalker_defending_player() {
    for defender_island in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        let sea = scenario
            .add_creature_from_oracle(P0, "Sea Monster", 6, 6, SEA_MONSTER)
            .id();
        // Positive control: an unrestricted attacker, proving any `Err` on
        // `sea` is specific rather than a dead step.
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        // A bare planeswalker OWNED by P0 but CONTROLLED by P1 (the
        // Control-Magic-class case), with no abilities of its own — this is
        // what makes the anchor claim below actually discriminating: if
        // owner and controller were the same player (as a freshly-cast
        // planeswalker normally is), a fix that anchored on `owner` instead
        // of `controller` would be indistinguishable from a correct one.
        let pw = scenario
            .add_planeswalker_from_oracle(P1, "Probe Walker", "Jace", 3, "")
            .id();
        let island = defender_island.then(|| scenario.add_basic_land(P1, ManaColor::Blue));

        let mut runner = scenario.build();
        // CR 508.5 names the CONTROLLER, not the owner — split them here so
        // the reach-guard below (and thus the whole test) fails if the
        // anchor is ever silently read from `owner` instead.
        runner.state_mut().objects.get_mut(&pw).unwrap().owner = P0;

        // BOARD REACH-GUARD: owner/controller are split — P0 owns, P1
        // controls — so this test can only pass if the anchor genuinely
        // resolves through `controller`.
        assert_eq!(
            runner.state().objects[&pw].owner,
            P0,
            "the planeswalker fixture must be owned by P0"
        );
        assert_eq!(
            runner.state().objects[&pw].controller,
            P1,
            "the planeswalker fixture must be controlled by P1"
        );
        // BOARD REACH-GUARD (Island arm): without this an (Err, Err) outcome
        // is also compatible with "the fixture never gave P1 an Island".
        if let Some(land) = island {
            let obj = &runner.state().objects[&land];
            assert!(
                obj.card_types.core_types.contains(&CoreType::Land)
                    && obj.card_types.subtypes.iter().any(|s| s == "Island"),
                "the Island arm must actually give P1 a Land with the Island \
                 subtype; got {:?} / {:?}",
                obj.card_types.core_types,
                obj.card_types.subtypes
            );
        }

        runner.advance_to_combat();
        let WaitingFor::DeclareAttackers {
            valid_attacker_ids,
            valid_attack_targets_by_attacker,
            attacker_constraints,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "fixture must reach the declare-attackers prompt; got {:?}",
                runner.state().waiting_for.variant_name()
            );
        };

        // REACH-GUARD A (positive, both arms): the unrestricted bear is a
        // valid attacker.
        assert!(
            valid_attacker_ids.contains(&bear),
            "positive control: the unrestricted bear must be a valid attacker \
             (defender_island = {defender_island}); got {valid_attacker_ids:?}"
        );
        // REACH-GUARD B (both arms): Sea Monster is offered as a candidate
        // (the creature-level deferral let it through) and carries no
        // confounding MustAttack requirement.
        assert!(
            valid_attacker_ids.contains(&sea),
            "Sea Monster must be offered as a candidate attacker against the \
             planeswalker (the creature-level query must defer, not refuse); \
             (defender_island = {defender_island}); got {valid_attacker_ids:?}"
        );
        // The assertion is about a MustAttack CONFOUND, so it tests for that rather
        // than for badge absence. On the no-Island arm Sea Monster legitimately
        // carries a `CantAttack` badge: the deferred prohibition leaves it eligible
        // with an empty target set (asserted below), which is exactly the state
        // #9265 added a badge for. Asserting `is_none()` here would pin the display
        // gap this suite's own `sea_targets.is_empty()` documents.
        assert!(
            !matches!(
                attacker_constraints.get(&sea),
                Some(CombatRequirement::MustAttack { .. })
            ),
            "Sea Monster must carry no confounding MustAttack requirement; got {:?}",
            attacker_constraints.get(&sea)
        );

        // PRIMARY, per-pairing map: proves the per-pairing authority decided,
        // independent of the action verdict below.
        let map = valid_attack_targets_by_attacker
            .as_ref()
            .expect("engine-authoritative per-attacker target map must be populated");
        let sea_targets = map.get(&sea).cloned().unwrap_or_default();
        if defender_island {
            assert!(
                sea_targets.contains(&AttackTarget::Planeswalker(pw)),
                "Island arm: Sea Monster must be able to target P1's \
                 planeswalker; got {sea_targets:?}"
            );
        } else {
            assert!(
                sea_targets.is_empty(),
                "no-Island arm: Sea Monster must have no selectable targets; \
                 got {sea_targets:?}"
            );
        }

        // PRIMARY, real action. Ordering matters: a SUCCESSFUL
        // `declare_attackers` advances past the step, so the Island arm's
        // positive control must run on a CLONE — the real runner stays live
        // for the assertion that follows it.
        if defender_island {
            let mut positive = GameRunner::from_state(runner.state().clone());
            assert!(
                positive
                    .declare_attackers(&[(bear, AttackTarget::Player(P1))])
                    .is_ok(),
                "positive control: the unrestricted bear must be a legal \
                 attacker (defender_island = true)"
            );
            assert!(
                runner
                    .declare_attackers(&[(sea, AttackTarget::Planeswalker(pw))])
                    .is_ok(),
                "CR 508.5: Sea Monster must be a legal attacker against P1's \
                 planeswalker when its controller (P1) controls an Island"
            );
        } else {
            assert!(
                runner
                    .declare_attackers(&[(sea, AttackTarget::Planeswalker(pw))])
                    .is_err(),
                "CR 508.5: Sea Monster must be refused as an attacker against \
                 P1's planeswalker when its controller (P1) controls no Island"
            );
            assert!(
                runner
                    .declare_attackers(&[(bear, AttackTarget::Player(P1))])
                    .is_ok(),
                "positive control: the unrestricted bear must be a legal \
                 attacker (defender_island = false)"
            );
        }
    }
}

/// Turn an existing battlefield creature into a Siege battle with the given
/// protector and printed defense — a local copy of
/// `rules::battle::make_into_siege` (`crates/engine/tests/integration/rules/battle.rs:21-42`).
/// That helper is private to the `rules` module tree and this file is a
/// separate top-level `mod` in `tests/integration/main.rs`, so it isn't
/// reachable from here; duplicating the same `state_mut()`-only construction
/// keeps this file's `GameScenario`/`GameRunner`-only style intact rather than
/// reaching for `create_object` + raw `chosen_attributes` engine internals.
fn make_into_siege(
    runner: &mut GameRunner,
    id: ObjectId,
    protector: PlayerId,
    printed_defense: u32,
) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.clear();
    obj.card_types.core_types.push(CoreType::Battle);
    obj.card_types.subtypes = vec!["Siege".to_string()];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
    obj.defense = Some(printed_defense);
    obj.base_defense = Some(printed_defense);
    obj.counters.insert(CounterType::Defense, printed_defense);
    obj.chosen_attributes
        .push(ChosenAttribute::Player(protector));
}

/// T1c — `sea_monster_attack_legality_follows_battle_protector_defending_player`
/// (Unit 4 sibling, `AttackTarget::Battle` arm; CR 310.9d).
///
/// Same fixture as T1 (Sea Monster / island-controlled-by-defender), except
/// Sea Monster attacks a Siege battle instead of a player. The battle is
/// CONTROLLED by P0 (the attacking player — CR 310.9b: "a Siege battle can be
/// attacked by its own controller") but PROTECTED by P1: CR 310.9d routes
/// "defending player" to the PROTECTOR, not the controller, whenever they
/// differ, and P0 never controls an Island anywhere in this fixture — so this
/// is the one arm where defending player != controller in a
/// production-reachable way, and it genuinely discriminates protector-anchor
/// from controller-anchor (unlike the planeswalker T1b fixture, which needs
/// an owner/controller split to discriminate).
///
/// Verified to discriminate, and the measured shape matters: temporarily
/// changing `combat::defending_player_for_target`'s `AttackTarget::Battle` arm
/// (`combat.rs:1378-1380`) from `.and_then(|b| b.protector())` to read the
/// controller instead fails ONLY the `defender_island = true` arm, at the
/// per-pairing map assertion, with `sea_targets == [Player(PlayerId(1))]`.
///
/// The list is NOT empty: restrictions are evaluated per (attacker, target)
/// PAIRING, so the `Player(P1)` pairing still resolves its defending player to
/// P1 and stays legal; only the `Battle(siege)` pairing is refused, because the
/// mutated code consults P0's board (the siege's controller) and P0 never has
/// an Island.
///
/// The `defender_island = false` arm does NOT discriminate protector from
/// controller — under both the true and the mutated anchor the consulted player
/// holds no Island, so it passes either way. It is here as a polarity/vacuity
/// guard proving the gate is not stuck open, not as a second discriminator.
#[test]
fn sea_monster_attack_legality_follows_battle_protector_defending_player() {
    for defender_island in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        let sea = scenario
            .add_creature_from_oracle(P0, "Sea Monster", 6, 6, SEA_MONSTER)
            .id();
        // Positive control: an unrestricted attacker, proving any `Err` on
        // `sea` is specific rather than a dead step.
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        // Placeholder creature, converted into a Siege battle below.
        // Controlled by P0 (CR 310.9b permits this), protected by P1.
        let siege = scenario.add_creature(P0, "Placeholder Siege", 0, 0).id();
        let island = defender_island.then(|| scenario.add_basic_land(P1, ManaColor::Blue));

        let mut runner = scenario.build();
        make_into_siege(&mut runner, siege, P1, 5);

        // BOARD REACH-GUARD: the battle really is a Siege, controlled by P0,
        // protected by P1 — not some other object shape that would make
        // either arm's outcome vacuous.
        let siege_obj = &runner.state().objects[&siege];
        assert!(
            siege_obj.card_types.core_types.contains(&CoreType::Battle)
                && siege_obj.card_types.subtypes.contains(&"Siege".to_string()),
            "the siege fixture must actually be a [Battle, Siege] permanent; \
             got {:?} / {:?}",
            siege_obj.card_types.core_types,
            siege_obj.card_types.subtypes
        );
        assert_eq!(
            siege_obj.controller, P0,
            "the siege fixture must be controlled by P0"
        );
        assert_eq!(
            siege_obj.protector(),
            Some(P1),
            "the siege fixture must be protected by P1"
        );
        // BOARD REACH-GUARD (Island arm): without this an (Err, Err) outcome
        // is also compatible with "the fixture never gave P1 an Island".
        if let Some(land) = island {
            let obj = &runner.state().objects[&land];
            assert!(
                obj.card_types.core_types.contains(&CoreType::Land)
                    && obj.card_types.subtypes.iter().any(|s| s == "Island"),
                "the Island arm must actually give P1 a Land with the Island \
                 subtype; got {:?} / {:?}",
                obj.card_types.core_types,
                obj.card_types.subtypes
            );
        }

        runner.advance_to_combat();
        let WaitingFor::DeclareAttackers {
            valid_attacker_ids,
            valid_attack_targets_by_attacker,
            attacker_constraints,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "fixture must reach the declare-attackers prompt; got {:?}",
                runner.state().waiting_for.variant_name()
            );
        };

        // REACH-GUARD A (positive, both arms): the unrestricted bear is a
        // valid attacker.
        assert!(
            valid_attacker_ids.contains(&bear),
            "positive control: the unrestricted bear must be a valid attacker \
             (defender_island = {defender_island}); got {valid_attacker_ids:?}"
        );
        // REACH-GUARD B (both arms): Sea Monster is offered as a candidate
        // (the creature-level deferral let it through) and carries no
        // confounding MustAttack requirement.
        assert!(
            valid_attacker_ids.contains(&sea),
            "Sea Monster must be offered as a candidate attacker against the \
             battle (the creature-level query must defer, not refuse); \
             (defender_island = {defender_island}); got {valid_attacker_ids:?}"
        );
        // The assertion is about a MustAttack CONFOUND, so it tests for that rather
        // than for badge absence. On the no-Island arm Sea Monster legitimately
        // carries a `CantAttack` badge: the deferred prohibition leaves it eligible
        // with an empty target set (asserted below), which is exactly the state
        // #9265 added a badge for. Asserting `is_none()` here would pin the display
        // gap this suite's own `sea_targets.is_empty()` documents.
        assert!(
            !matches!(
                attacker_constraints.get(&sea),
                Some(CombatRequirement::MustAttack { .. })
            ),
            "Sea Monster must carry no confounding MustAttack requirement; got {:?}",
            attacker_constraints.get(&sea)
        );

        // PRIMARY, per-pairing map: proves the per-pairing authority decided,
        // independent of the action verdict below.
        let map = valid_attack_targets_by_attacker
            .as_ref()
            .expect("engine-authoritative per-attacker target map must be populated");
        let sea_targets = map.get(&sea).cloned().unwrap_or_default();
        if defender_island {
            assert!(
                sea_targets.contains(&AttackTarget::Battle(siege)),
                "Island arm: Sea Monster must be able to target P1's siege \
                 (P1 is the protector); got {sea_targets:?}"
            );
        } else {
            assert!(
                sea_targets.is_empty(),
                "no-Island arm: Sea Monster must have no selectable targets; \
                 got {sea_targets:?}"
            );
        }

        // PRIMARY, real action. Ordering matters: a SUCCESSFUL
        // `declare_attackers` advances past the step, so the Island arm's
        // positive control must run on a CLONE — the real runner stays live
        // for the assertion that follows it.
        if defender_island {
            let mut positive = GameRunner::from_state(runner.state().clone());
            assert!(
                positive
                    .declare_attackers(&[(bear, AttackTarget::Player(P1))])
                    .is_ok(),
                "positive control: the unrestricted bear must be a legal \
                 attacker (defender_island = true)"
            );
            assert!(
                runner
                    .declare_attackers(&[(sea, AttackTarget::Battle(siege))])
                    .is_ok(),
                "CR 310.9d: Sea Monster must be a legal attacker against the \
                 siege when its protector (P1) controls an Island"
            );
        } else {
            assert!(
                runner
                    .declare_attackers(&[(sea, AttackTarget::Battle(siege))])
                    .is_err(),
                "CR 310.9d: Sea Monster must be refused as an attacker \
                 against the siege when its protector (P1) controls no \
                 Island"
            );
            assert!(
                runner
                    .declare_attackers(&[(bear, AttackTarget::Player(P1))])
                    .is_ok(),
                "positive control: the unrestricted bear must be a legal \
                 attacker (defender_island = false)"
            );
        }
    }
}
