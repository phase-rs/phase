//! `CombatRelation::BlockedBySubject` — the persistent pairwise "creatures it
//! blocked this combat / this turn" primitive (issue #9179, PR 2).
//!
//! `CombatRelation::BlockingOrBlockedBy` reads live `combat.blocker_to_attacker`,
//! which `prune_object_from_combat` empties per CR 506.4 the moment either
//! creature leaves combat — exactly when a dies-trigger needs the answer. These
//! tests drive the real engine (`GameScenario`, real `declare_blockers_for_player`
//! / `place_blocking` / phase machinery), never hand-built `GameState` literals,
//! so a reverted edit actually reaches them.

use engine::game::combat::{AttackTarget, BlockHistoryPair};
use engine::game::filter::{
    matches_target_filter, matches_target_filter_on_zone_change_record, FilterContext,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    CombatHistoryScope, CombatRelation, CombatRelationSubject, FilterProp, TargetFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const MURDER: &str = "Destroy target creature.";
const EPHEMERATE: &str =
    "Exile target creature you control, then return it to the battlefield under its owner's control.";
const FULL_THROTTLE: &str = "After this main phase, there are two additional combat phases.
At the beginning of each combat this turn, untap all creatures that attacked this turn.";

/// Drive from `PreCombatMain` through a single declare-blockers step: pass to
/// declare-attackers, declare `attacks`/`bands`, pass the CR 508.2 post-attack
/// priority window, then declare `blocks`. Leaves the runner paused right after
/// blockers are declared, with `state.combat` still live — the model is
/// `banding_combat.rs::drive_to_first_damage_prompt`, stopped one step earlier.
fn drive_declare_blockers(
    runner: &mut GameRunner,
    attacks: Vec<(ObjectId, AttackTarget)>,
    bands: Vec<Vec<ObjectId>>,
    blocks: Vec<(ObjectId, ObjectId)>,
) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers { attacks, bands })
        .expect("DeclareAttackers should succeed");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    runner
        .act(GameAction::DeclareBlockers {
            assignments: blocks,
        })
        .expect("DeclareBlockers should succeed");
}

/// Drive past the end of the current turn regardless of what's waiting,
/// declaring no attackers/blockers along the way. Mirrors
/// `delayed_parent_target_incarnation.rs::advance_past_end_of_turn`.
fn advance_past_end_of_turn(runner: &mut GameRunner) {
    let start_turn = runner.state().turn_number;
    let mut guard = 0;
    while runner.state().turn_number == start_turn {
        guard += 1;
        assert!(
            guard < 256,
            "turn never ended; phase = {:?}, waiting_for = {:?}",
            runner.state().phase,
            runner.state().waiting_for,
        );
        let action = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            _ => GameAction::PassPriority,
        };
        if let Err(e) = runner.act(action) {
            panic!(
                "advancing past end of turn failed: {e:?} (phase = {:?}, waiting_for = {:?})",
                runner.state().phase,
                runner.state().waiting_for,
            );
        }
    }
}

/// Drive until `state.combat` becomes `None` (CR 511.3's end of the end of
/// combat step), declaring no attackers/blockers along the way.
fn advance_until_combat_ends(runner: &mut GameRunner) {
    let mut guard = 0;
    while runner.state().combat.is_some() {
        guard += 1;
        assert!(
            guard < 256,
            "combat never ended; phase = {:?}, waiting_for = {:?}",
            runner.state().phase,
            runner.state().waiting_for,
        );
        let action = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            _ => GameAction::PassPriority,
        };
        if let Err(e) = runner.act(action) {
            panic!(
                "advancing until combat ends failed: {e:?} (phase = {:?}, waiting_for = {:?})",
                runner.state().phase,
                runner.state().waiting_for,
            );
        }
    }
}

/// Hand `player` priority directly, mirroring
/// `cr733_resolved_combat_membership.rs::advance_to_declare_blockers_and_give_priority`.
/// After a real `DeclareBlockers` action CR 509.2 gives the ACTIVE player
/// priority, so a defending-player instant cast (e.g. destroying the blocker)
/// needs an explicit hand-off — casting is gated on `priority_player`, not
/// merely on holding the card.
fn give_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

/// `TargetFilter::Typed(creature)` with a single `CombatRelation` property,
/// subject always `Source` — the template shared by every test below, matching
/// `filter.rs`'s own `combat_relation_matches_creatures_blocking_or_blocked_by_parent_target`
/// unit test's construction shape.
fn combat_relation_filter(relation: CombatRelation) -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::creature().properties(vec![FilterProp::CombatRelation {
            relation,
            subject: CombatRelationSubject::Source,
        }]),
    )
}

/// T1: after a real declare-blockers step, the block-history ledgers hold the
/// exact `(blocker, attacker)` incarnation pair, `BlockedBySubject` matches
/// the blocked attacker with the blocker as source, and — the whole reason
/// this primitive is pairwise rather than the unary `creatures_blocked_this_turn`
/// — a second attacker the blocker never blocked matches on NEITHER window.
#[test]
fn declare_blockers_records_each_blocker_to_attacker_pair() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blocked = scenario.add_creature(P0, "Blocked Attacker", 2, 2).id();
    let unblocked = scenario.add_creature(P0, "Unblocked Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![
            (blocked, AttackTarget::Player(P1)),
            (unblocked, AttackTarget::Player(P1)),
        ],
        vec![],
        vec![(blocker, blocked)],
    );

    let state = runner.state();
    let combat = state.combat.as_ref().expect("combat is live");

    // Reach guard: the block landed and `unblocked` is genuinely undeclared.
    assert!(
        combat
            .blocker_to_attacker
            .get(&blocker)
            .is_some_and(|a| a.contains(&blocked) && !a.contains(&unblocked)),
        "reach guard: the live reverse lookup must name only the blocked attacker"
    );

    let blocker_ref = ObjectIncarnationRef::from_object(&state.objects[&blocker]);
    let blocked_ref = ObjectIncarnationRef::from_object(&state.objects[&blocked]);
    let unblocked_ref = ObjectIncarnationRef::from_object(&state.objects[&unblocked]);
    let blocked_pair = BlockHistoryPair {
        blocker: blocker_ref,
        attacker: blocked_ref,
    };
    let unblocked_pair = BlockHistoryPair {
        blocker: blocker_ref,
        attacker: unblocked_ref,
    };

    // Direct ledger reads (E3, E4, E5).
    assert!(
        combat
            .creature_blocked_attackers_this_combat
            .contains(&blocked_pair)
            && !combat
                .creature_blocked_attackers_this_combat
                .contains(&unblocked_pair),
        "the combat-scoped ledger must hold exactly the declared pair"
    );
    assert!(
        state
            .creature_blocked_attackers_this_turn
            .contains(&blocked_pair)
            && !state
                .creature_blocked_attackers_this_turn
                .contains(&unblocked_pair),
        "the turn-scoped ledger must hold exactly the declared pair"
    );

    // Evaluator reads (E10).
    let ctx = FilterContext::from_source_with_controller(blocker, P1);
    for scope in [CombatHistoryScope::ThisCombat, CombatHistoryScope::ThisTurn] {
        let filter = combat_relation_filter(CombatRelation::BlockedBySubject { scope });
        assert!(
            matches_target_filter(state, blocked, &filter, &ctx),
            "{scope:?}: the blocked attacker must match"
        );
        assert!(
            !matches_target_filter(state, unblocked, &filter, &ctx),
            "{scope:?}: an attacker the blocker never blocked must not match"
        );
    }
}

/// T2: CR 702.22h propagates a block across a band, so a single declared block
/// against one band member records BOTH members — not just the chosen one.
#[test]
fn banding_block_records_the_whole_band_not_just_the_chosen_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let banded = scenario
        .add_creature(P0, "Banded Attacker", 2, 2)
        .with_keyword(Keyword::Banding)
        .id();
    let plain = scenario.add_creature(P0, "Plain Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 4, 4).id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![
            (banded, AttackTarget::Player(P1)),
            (plain, AttackTarget::Player(P1)),
        ],
        vec![vec![banded, plain]],
        vec![(blocker, banded)],
    );

    let state = runner.state();
    let combat = state.combat.as_ref().expect("combat is live");

    // Reach guard: banding really did propagate the block to the plain member.
    assert!(
        combat
            .blocker_to_attacker
            .get(&blocker)
            .is_some_and(|a| a.contains(&banded) && a.contains(&plain)),
        "reach guard: CR 702.22h must propagate the block across the band"
    );

    let blocker_ref = ObjectIncarnationRef::from_object(&state.objects[&blocker]);
    let banded_ref = ObjectIncarnationRef::from_object(&state.objects[&banded]);
    let plain_ref = ObjectIncarnationRef::from_object(&state.objects[&plain]);
    assert!(
        combat
            .creature_blocked_attackers_this_combat
            .contains(&BlockHistoryPair {
                blocker: blocker_ref,
                attacker: banded_ref,
            })
            && combat
                .creature_blocked_attackers_this_combat
                .contains(&BlockHistoryPair {
                    blocker: blocker_ref,
                    attacker: plain_ref,
                }),
        "CR 702.22h + CR 702.22k: the ledger must hold the whole band, not just the chosen attacker"
    );
}

/// T3: a dying blocker's OWN dies trigger still finds what it blocked,
/// because the resolving trigger's captured identity (`ability.trigger_source`)
/// names the exact incarnation that blocked — not the live graveyard
/// incarnation the blocker becomes once SBAs move it (CR 400.7). The live
/// `BlockingOrBlockedBy` relation still fails closed here (CR 506.4): the live
/// map was pruned the moment the blocker left combat.
#[test]
fn a_dying_blocker_still_finds_what_it_blocked_through_its_trigger_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blocker = scenario
        .add_creature_from_oracle(
            P1,
            "Blocker",
            2,
            2,
            "When this creature dies, you gain 1 life.",
        )
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![],
        vec![(blocker, attacker)],
    );
    let blocker_ref_at_block = ObjectIncarnationRef::from_object(&runner.state().objects[&blocker]);

    give_priority(&mut runner, P1);
    let mut commit = runner.cast(murder).target_object(blocker).commit();
    commit
        .act(GameAction::PassPriority)
        .expect("P1 passes priority back");
    commit
        .act(GameAction::PassPriority)
        .expect("P0 passes; Murder resolves and the blocker dies");

    let state = commit.state();
    assert_eq!(
        state.objects[&blocker].zone,
        Zone::Graveyard,
        "reach guard: the blocker must actually have died"
    );
    let entry = state
        .stack
        .iter()
        .find(|entry| {
            matches!(&entry.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == blocker)
        })
        .expect("reach guard: the dies trigger must be on the stack, unresolved");
    let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind else {
        unreachable!("matched above")
    };
    let trigger_source = ability
        .trigger_source
        .as_ref()
        .expect("reach guard: a dies trigger must carry its captured source identity");
    assert_eq!(
        trigger_source.identity.reference, blocker_ref_at_block,
        "reach guard: the trigger's captured identity is the incarnation that blocked"
    );
    assert_ne!(
        trigger_source.identity.reference,
        ObjectIncarnationRef::from_object(&state.objects[&blocker]),
        "reach guard: the trigger's captured identity must differ from the live graveyard incarnation"
    );

    let ctx = FilterContext::from_ability(ability);
    for scope in [CombatHistoryScope::ThisCombat, CombatHistoryScope::ThisTurn] {
        let history_filter = combat_relation_filter(CombatRelation::BlockedBySubject { scope });
        assert!(
            matches_target_filter(state, attacker, &history_filter, &ctx),
            "{scope:?}: CR 509.1g + CR 400.7: the dying blocker's own trigger must still find what it blocked"
        );
    }
    let live_filter = combat_relation_filter(CombatRelation::BlockingOrBlockedBy);
    assert!(
        !matches_target_filter(state, attacker, &live_filter, &ctx),
        "CR 506.4: the live relation must be pruned once the blocker leaves combat"
    );
}

/// T4: the look-back evaluator (`zone_change_record_matches_property`) answers
/// `BlockedBySubject` for a departed candidate, while `BlockingOrBlockedBy`
/// correctly fails closed there (E11).
#[test]
fn look_back_evaluator_answers_the_history_relation_for_a_departed_candidate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![],
        vec![(blocker, attacker)],
    );

    give_priority(&mut runner, P1);
    runner.cast(murder).target_object(attacker).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    let record = state
        .zone_changes_this_turn
        .iter()
        .rev()
        .find(|r| r.object_id == attacker && r.to_zone == Zone::Graveyard)
        .expect("reach guard: the attacker's death must be recorded as a zone change");

    let ctx = FilterContext::from_source_with_controller(blocker, P1);
    let history_filter = combat_relation_filter(CombatRelation::BlockedBySubject {
        scope: CombatHistoryScope::ThisCombat,
    });
    let live_filter = combat_relation_filter(CombatRelation::BlockingOrBlockedBy);

    assert!(
        matches_target_filter_on_zone_change_record(state, record, &history_filter, &ctx),
        "CR 509.1g + CR 608.2i: the look-back leg must answer from the block-history ledger"
    );
    assert!(
        !matches_target_filter_on_zone_change_record(state, record, &live_filter, &ctx),
        "CR 506.4: the live relation has nothing for a departed record to match"
    );
}

/// T4b: the look-back leg also answers when the SUBJECT itself is departed —
/// every zone-change record carries its own trigger-bound identity
/// (`GameObject::snapshot_for_zone_change`), not only records for objects with
/// their own triggered ability, so a plain dead blocker's death record still
/// names the exact incarnation that blocked.
#[test]
fn look_back_answers_with_a_departed_subject_named_by_its_trigger_identity() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let murder_blocker = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let murder_attacker = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![],
        vec![(blocker, attacker)],
    );

    give_priority(&mut runner, P1);
    runner.cast(murder_blocker).target_object(blocker).resolve();
    runner.advance_until_stack_empty();

    // CR 117.3b: after Murder's resolution the active player (P0) receives
    // priority; hand it back to P1 to cast the second Murder.
    give_priority(&mut runner, P1);
    runner
        .cast(murder_attacker)
        .target_object(attacker)
        .resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    let blocker_record = state
        .zone_changes_this_turn
        .iter()
        .rev()
        .find(|r| r.object_id == blocker && r.to_zone == Zone::Graveyard)
        .expect("reach guard: the blocker's death must be recorded as a zone change");
    let attacker_record = state
        .zone_changes_this_turn
        .iter()
        .rev()
        .find(|r| r.object_id == attacker && r.to_zone == Zone::Graveyard)
        .expect("reach guard: the attacker's death must be recorded as a zone change");

    let blocker_trigger_source = blocker_record
        .trigger_source_context()
        .expect("reach guard: the blocker's own death record must carry its trigger identity");
    assert_ne!(
        blocker_trigger_source.identity.reference,
        ObjectIncarnationRef::from_object(&state.objects[&blocker]),
        "reach guard: the departed subject's captured identity must differ from its live graveyard incarnation"
    );

    let ctx = FilterContext::from_trigger_source(blocker_trigger_source);
    let history_filter = combat_relation_filter(CombatRelation::BlockedBySubject {
        scope: CombatHistoryScope::ThisTurn,
    });
    assert!(
        matches_target_filter_on_zone_change_record(state, attacker_record, &history_filter, &ctx),
        "CR 509.1g + CR 608.2i: the look-back leg must answer even though the subject itself is departed"
    );
}

/// T5: `ThisCombat` and `ThisTurn` disagree across two combat phases in one
/// turn (CR 500.8). A block declared in combat 1 is absent from `ThisCombat`
/// once combat 2's fresh `CombatState` is installed, but still present in
/// `ThisTurn`. Built on the in-tree two-extra-combats harness
/// (`issue_828_full_throttle.rs::full_throttle_turn_advances_through_two_extra_combats`).
#[test]
fn this_combat_and_this_turn_disagree_across_two_combat_phases() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Asymmetric P/T: a dead attacker would leave no legal attacker for
    // combat 2's DeclareAttackers window to offer, and the engine skips
    // straight past an attacker-less declare-attackers step without ever
    // raising it.
    let attacker = scenario.add_creature(P0, "Attacker", 3, 3).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 4).id();
    let throttle = scenario
        .add_spell_to_hand_from_oracle(P0, "Full Throttle", false, FULL_THROTTLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    runner.cast(throttle).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().extra_phases.len(),
        2,
        "reach guard: Full Throttle must schedule two extra combats"
    );

    let mut declare_attackers_rounds = 0;
    let mut checked_combat_two = false;
    for _ in 0..600 {
        if checked_combat_two {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                declare_attackers_rounds += 1;
                if declare_attackers_rounds == 2 {
                    // Combat 2's `CombatState` was just installed fresh at this
                    // `Phase::BeginCombat` entry (§4.1: unconditional replacement).
                    let state = runner.state();
                    let combat = state.combat.as_ref().expect("combat 2 is live");
                    // Reach guard: neither creature died in combat 1, so their
                    // incarnations are unchanged and directly comparable here.
                    let blocker_ref = ObjectIncarnationRef::from_object(&state.objects[&blocker]);
                    let attacker_ref = ObjectIncarnationRef::from_object(&state.objects[&attacker]);
                    assert!(
                        !combat
                            .creature_blocked_attackers_this_combat
                            .iter()
                            .any(|pair| pair.blocker.object_id == blocker),
                        "combat 1's block must not survive into combat 2's fresh CombatState"
                    );
                    assert!(
                        state
                            .creature_blocked_attackers_this_turn
                            .contains(&BlockHistoryPair {
                                blocker: blocker_ref,
                                attacker: attacker_ref,
                            }),
                        "reach guard: the turn-scoped ledger must still hold combat 1's block"
                    );

                    let ctx = FilterContext::from_source_with_controller(blocker, P1);
                    let this_combat = combat_relation_filter(CombatRelation::BlockedBySubject {
                        scope: CombatHistoryScope::ThisCombat,
                    });
                    let this_turn = combat_relation_filter(CombatRelation::BlockedBySubject {
                        scope: CombatHistoryScope::ThisTurn,
                    });
                    assert!(
                        !matches_target_filter(state, attacker, &this_combat, &ctx),
                        "ThisCombat must not see combat 1's block during combat 2"
                    );
                    assert!(
                        matches_target_filter(state, attacker, &this_turn, &ctx),
                        "ThisTurn must still see combat 1's block during combat 2 (CR 500.8)"
                    );
                    checked_combat_two = true;
                }
                let attacks = if declare_attackers_rounds == 1 {
                    vec![(attacker, AttackTarget::Player(P1))]
                } else {
                    vec![]
                };
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks,
                        bands: vec![],
                    })
                    .expect("declare attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                let assignments = if declare_attackers_rounds == 1 {
                    vec![(blocker, attacker)]
                } else {
                    vec![]
                };
                runner
                    .act(GameAction::DeclareBlockers { assignments })
                    .expect("declare blockers");
            }
            WaitingFor::Priority { .. } => {
                runner.pass_both_players();
            }
            _ => {
                runner.act(GameAction::PassPriority).ok();
            }
        }
    }

    assert!(
        checked_combat_two,
        "must have reached combat 2's DeclareAttackers window to assert the disagreement; \
         stalled at phase = {:?}, waiting_for = {:?}, declare_attackers_rounds = {declare_attackers_rounds}, \
         extra_phases = {:?}",
        runner.state().phase,
        runner.state().waiting_for,
        runner.state().extra_phases,
    );
}

/// T6: the turn-scoped block-history ledger clears at the turn boundary (E9),
/// exactly like its unary sibling `creatures_blocked_this_turn`.
#[test]
fn turn_boundary_clears_the_per_turn_block_history() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![],
        vec![(blocker, attacker)],
    );

    // Reach guard: without this, deleting E9's clear would leave this test
    // green against a ledger that was empty either way.
    assert!(
        !runner
            .state()
            .creature_blocked_attackers_this_turn
            .is_empty(),
        "reach guard: the turn-scoped ledger must be populated before the turn boundary"
    );

    advance_past_end_of_turn(&mut runner);

    assert!(
        !runner
            .state()
            .creature_blocked_attackers_this_turn
            .iter()
            .any(|pair| pair.blocker.object_id == blocker),
        "the turn-scoped ledger must clear at the next turn's boundary"
    );
}

/// T7: the combat-scoped ledger is unreachable once `state.combat` is `None`
/// (CR 511.3, the end of the end of combat step); the turn-scoped ledger
/// still answers for the same recorded block. Toughness high enough that
/// neither creature dies in combat, or a CR 400.7 incarnation bump would hide
/// the answer either way.
#[test]
fn combat_scoped_history_is_gone_after_the_end_of_combat_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 4).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 4).id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![],
        vec![(blocker, attacker)],
    );

    assert!(
        !runner
            .state()
            .combat
            .as_ref()
            .expect("combat is live")
            .creature_blocked_attackers_this_combat
            .is_empty(),
        "reach guard: the combat-scoped ledger must be populated before combat ends"
    );

    advance_until_combat_ends(&mut runner);

    let state = runner.state();
    assert!(
        state.combat.is_none(),
        "CR 511.3: combat must actually have ended for this test to mean anything"
    );
    assert!(
        state.objects[&attacker].zone == Zone::Battlefield
            && state.objects[&blocker].zone == Zone::Battlefield,
        "reach guard: neither creature dies, so a CR 400.7 incarnation bump cannot hide the answer"
    );

    let ctx = FilterContext::from_source_with_controller(blocker, P1);
    let this_combat = combat_relation_filter(CombatRelation::BlockedBySubject {
        scope: CombatHistoryScope::ThisCombat,
    });
    let this_turn = combat_relation_filter(CombatRelation::BlockedBySubject {
        scope: CombatHistoryScope::ThisTurn,
    });
    assert!(
        !matches_target_filter(state, attacker, &this_combat, &ctx),
        "CR 511.3: the combat-scoped ledger is gone once combat has ended"
    );
    assert!(
        matches_target_filter(state, attacker, &this_turn, &ctx),
        "the turn-scoped ledger must still hold the block after combat ends"
    );
}

/// T9 — the CR 400.7 test. A blocked attacker that left and returned (blink)
/// is a new object at the same `ObjectId`; the ledger's captured incarnation
/// must not match its successor.
#[test]
fn an_attacker_that_left_and_returned_is_a_new_object_and_does_not_match() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let ephemerate = scenario
        .add_spell_to_hand_from_oracle(P0, "Ephemerate", true, EPHEMERATE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![],
        vec![(blocker, attacker)],
    );

    let ctx = FilterContext::from_source_with_controller(blocker, P1);
    let filter = combat_relation_filter(CombatRelation::BlockedBySubject {
        scope: CombatHistoryScope::ThisCombat,
    });

    // Positive control: before the blink, the relation matches.
    assert!(
        matches_target_filter(runner.state(), attacker, &filter, &ctx),
        "reach guard: the relation must match its own recorded incarnation"
    );

    let incarnation_before = runner.state().objects[&attacker].incarnation;
    runner.cast(ephemerate).target_object(attacker).resolve();
    runner.advance_until_stack_empty();
    let incarnation_after = runner.state().objects[&attacker].incarnation;
    assert_ne!(
        incarnation_after, incarnation_before,
        "reach guard: the blink must bump the attacker's incarnation (CR 400.7)"
    );

    assert!(
        !matches_target_filter(runner.state(), attacker, &filter, &ctx),
        "CR 400.7: a re-entered object is a new object the ledger never blocked"
    );
}

/// T13 — the CR 400.7 test on the SUBJECT side, the maintainer's reported bug:
/// a BLOCKER that left and returned (blink) is a new object at the same
/// `ObjectId` and must not inherit its predecessor's recorded blocks. A
/// trigger-bound subject is named by its captured identity (`pre`), so it
/// still finds the block after the blink; a live-object lookup (no trigger
/// source, or the returned object's own latch) finds nothing, because the
/// returned blocker never blocked anything.
#[test]
fn a_blocker_that_left_and_returned_does_not_inherit_its_predecessors_blocks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let ephemerate = scenario
        .add_spell_to_hand_from_oracle(P1, "Ephemerate", true, EPHEMERATE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![],
        vec![(blocker, attacker)],
    );

    // `pre`: the blocker's identity as it was WHILE it blocked, captured
    // before the blink.
    let pre = engine::game::triggers::trigger_source_context_for_latch(
        runner.state(),
        &runner.state().objects[&blocker],
    );

    let incarnation_before = runner.state().objects[&blocker].incarnation;
    give_priority(&mut runner, P1);
    runner.cast(ephemerate).target_object(blocker).resolve();
    runner.advance_until_stack_empty();

    // Reach guard: the blocker really did leave and return as a new object.
    let state = runner.state();
    assert_eq!(
        state.objects[&blocker].zone,
        Zone::Battlefield,
        "reach guard: the blinked blocker must be back on the battlefield"
    );
    assert_ne!(
        state.objects[&blocker].incarnation, incarnation_before,
        "reach guard: the blink must bump the blocker's incarnation (CR 400.7)"
    );

    // `post`: the returned blocker's own latch, at its NEW incarnation.
    let post =
        engine::game::triggers::trigger_source_context_for_latch(state, &state.objects[&blocker]);

    let filter_combat = combat_relation_filter(CombatRelation::BlockedBySubject {
        scope: CombatHistoryScope::ThisCombat,
    });
    let filter_turn = combat_relation_filter(CombatRelation::BlockedBySubject {
        scope: CombatHistoryScope::ThisTurn,
    });

    // (a) The predecessor's captured identity still finds its own record.
    let pre_ctx = FilterContext::from_trigger_source(&pre);
    assert!(
        matches_target_filter(state, attacker, &filter_combat, &pre_ctx)
            && matches_target_filter(state, attacker, &filter_turn, &pre_ctx),
        "the predecessor's trigger-bound identity must still find its own recorded block"
    );

    // (b) A live-object lookup (no trigger source) reads the RETURNED
    // blocker's new incarnation, which never blocked anything.
    let live_ctx = FilterContext::from_source_with_controller(blocker, P1);
    assert!(
        !matches_target_filter(state, attacker, &filter_combat, &live_ctx)
            && !matches_target_filter(state, attacker, &filter_turn, &live_ctx),
        "CR 400.7: the returned blocker is a new object and must not inherit its predecessor's blocks"
    );

    // (c) The returned object's own latch names the same new incarnation and
    // finds nothing either.
    let post_ctx = FilterContext::from_trigger_source(&post);
    assert!(
        !matches_target_filter(state, attacker, &filter_combat, &post_ctx)
            && !matches_target_filter(state, attacker, &filter_turn, &post_ctx),
        "CR 400.7: the returned object's own identity must not find its predecessor's block either"
    );

    // CR 117.3b: after Ephemerate's resolution the active player (P0)
    // receives priority; hand it back to P1 to cast the second spell.
    give_priority(&mut runner, P1);
    runner.cast(murder).target_object(attacker).resolve();
    runner.advance_until_stack_empty();

    // (d) Look-back: the attacker's death record is answered by the
    // predecessor's trigger-bound identity, not by the live fallback.
    let state = runner.state();
    let record = state
        .zone_changes_this_turn
        .iter()
        .rev()
        .find(|r| r.object_id == attacker && r.to_zone == Zone::Graveyard)
        .expect("reach guard: the attacker's death must be recorded as a zone change");
    assert!(
        matches_target_filter_on_zone_change_record(state, record, &filter_combat, &pre_ctx),
        "the look-back leg must still find the block through the predecessor's identity"
    );
    assert!(
        !matches_target_filter_on_zone_change_record(state, record, &filter_combat, &live_ctx),
        "the look-back leg must not find the block through the returned blocker's live identity"
    );
}

/// T14 — an ACTIVATED ability's `Source` must resolve through the incarnation
/// stamped at push time (CR 400.7 + CR 113.7a), not fall
/// straight to the live object the way T13's untriggered lookup does. The
/// blocker's no-cost activated ability reaches the stack while it is still the
/// incarnation that blocked; it is then blinked in response, so by the time the
/// filter is evaluated the live object is a new, unrelated incarnation (CR
/// 400.7) and only the stamped `source_incarnation` still names what blocked.
#[test]
fn an_activated_ability_finds_its_stamped_incarnation_not_the_live_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blocker = scenario
        .add_creature_from_oracle(P1, "Blocker", 2, 2, "{0}: You gain 1 life.")
        .id();
    let ephemerate = scenario
        .add_spell_to_hand_from_oracle(P1, "Ephemerate", true, EPHEMERATE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![],
        vec![(blocker, attacker)],
    );
    let blocker_ref_at_block = ObjectIncarnationRef::from_object(&runner.state().objects[&blocker]);

    give_priority(&mut runner, P1);
    runner
        .act(GameAction::ActivateAbility {
            source_id: blocker,
            ability_index: 0,
        })
        .expect("activating the blocker's {0} ability must succeed");

    {
        let state = runner.state();
        let entry = state
            .stack
            .iter()
            .find(|entry| {
                matches!(&entry.kind, StackEntryKind::ActivatedAbility { source_id, .. } if *source_id == blocker)
            })
            .expect("reach guard: the activated ability must be on the stack, unresolved");
        let StackEntryKind::ActivatedAbility { ability, .. } = &entry.kind else {
            unreachable!("matched above")
        };
        assert!(
            ability.trigger_source.is_none(),
            "reach guard: an activated ability carries no trigger-captured identity"
        );
        assert_eq!(
            ability.source_incarnation,
            Some(blocker_ref_at_block.incarnation),
            "reach guard: the ability must carry the incarnation that just blocked"
        );
    }

    // In response, blink the blocker — it returns as a new object (CR 400.7)
    // while its OWN activated ability is still unresolved beneath Ephemerate
    // on the stack.
    let mut commit = runner.cast(ephemerate).target_object(blocker).commit();
    commit
        .act(GameAction::PassPriority)
        .expect("P1 passes priority back");
    commit
        .act(GameAction::PassPriority)
        .expect("P0 passes; Ephemerate resolves, blinking the blocker");

    let state = commit.state();
    assert_eq!(
        state.objects[&blocker].zone,
        Zone::Battlefield,
        "reach guard: the blinked blocker must be back on the battlefield"
    );
    assert_ne!(
        state.objects[&blocker].incarnation, blocker_ref_at_block.incarnation,
        "reach guard: the blink must bump the blocker's incarnation (CR 400.7)"
    );
    let entry = state
        .stack
        .iter()
        .find(|entry| {
            matches!(&entry.kind, StackEntryKind::ActivatedAbility { source_id, .. } if *source_id == blocker)
        })
        .expect("reach guard: the activated ability must still be on the stack, unresolved");
    let StackEntryKind::ActivatedAbility { ability, .. } = &entry.kind else {
        unreachable!("matched above")
    };

    let ctx = FilterContext::from_ability(ability);
    for scope in [CombatHistoryScope::ThisCombat, CombatHistoryScope::ThisTurn] {
        let history_filter = combat_relation_filter(CombatRelation::BlockedBySubject { scope });
        assert!(
            matches_target_filter(state, attacker, &history_filter, &ctx),
            "{scope:?}: CR 608.2h: the activated ability's stamped incarnation must still find what it blocked"
        );
    }

    // Negative control: a context for the RETURNED object, carrying no stored
    // incarnation, must not inherit its predecessor's blocks.
    let live_ctx = FilterContext::from_source_with_controller(blocker, P1);
    for scope in [CombatHistoryScope::ThisCombat, CombatHistoryScope::ThisTurn] {
        let history_filter = combat_relation_filter(CombatRelation::BlockedBySubject { scope });
        assert!(
            !matches_target_filter(state, attacker, &history_filter, &live_ctx),
            "{scope:?}: CR 400.7: the returned blocker's live incarnation must not inherit its predecessor's blocks"
        );
    }
}

/// T12a: `CombatState::creature_blocked_attackers_this_combat` must participate
/// in `impl PartialEq for CombatState` (E3), or an omission from the loop-cover
/// gate (`analysis::resource::eq_except_growable`) would be indistinguishable
/// from the fields that omission is deliberate for.
#[test]
fn combat_scoped_block_history_participates_in_state_equality() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![],
        vec![(blocker, attacker)],
    );

    let state = runner.state().clone();
    assert!(
        !state
            .combat
            .as_ref()
            .expect("combat is live")
            .creature_blocked_attackers_this_combat
            .is_empty(),
        "reach guard: the combat-scoped ledger must be populated"
    );

    let mut clone = state.clone();
    clone
        .combat
        .as_mut()
        .expect("combat is live")
        .creature_blocked_attackers_this_combat
        .clear();

    assert!(
        state != clone,
        "E3's PartialEq registration must make a cleared combat-scoped ledger visible"
    );
}

/// T12b: `GameState::creature_blocked_attackers_this_turn` must participate in
/// `impl PartialEq for GameState` (E4), the sibling of T12a on the turn-scoped
/// field.
#[test]
fn turn_scoped_block_history_participates_in_state_equality() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![],
        vec![(blocker, attacker)],
    );

    let state = runner.state().clone();
    assert!(
        !state.creature_blocked_attackers_this_turn.is_empty(),
        "reach guard: the turn-scoped ledger must be populated"
    );

    let mut clone = state.clone();
    clone.creature_blocked_attackers_this_turn.clear();

    assert!(
        state != clone,
        "E4's PartialEq registration must make a cleared turn-scoped ledger visible"
    );
}
