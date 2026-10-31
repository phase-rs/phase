//! Matt R15-1: the combat batch parks the live one-shot ledger in its record,
//! so a mid-batch save/load into a re-prompt neither loses the record nor
//! duplicates it.
//!
//! Fixture: a 4/4 trampler, Impulsive Maneuvers (flip won — doubling hosted
//! on the attacker), and Awe Strike targeting the attacker, assigned 1/1/2.
//! Portion 1 pauses on [doubling, awe] and consumes both one-shots; portion 2
//! re-prompts both from the ledger; portion 3 (the 2 trample to P1) is THE
//! pause — two consumed one-shots parked, two ledger candidates re-published.
//! Save/load at that pause must restore the same two candidates and the same
//! two-entry record, and the restored run must complete identically to the
//! no-save twin.
//!
//! Discriminating: without the park, the save/load run's live ledger is gone
//! (`#[serde(skip)]`, unparked), so the resumed portion-3 prompt offers zero
//! candidates and 2 damage goes through — the no-save twin still passes.
//!
//! Oracle texts are verbatim (Scryfall / MTGJSON AtomicCards).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{DamageModification, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    GameRng, PersistedGameState, ReplacementCandidateSummary, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::zones::Zone;

use crate::impulsive_maneuvers_batch_shield::{
    assign_trample, damage_amounts, declare_attacks, reach_combat_damage,
};

// Verbatim Oracle text from data/mtgjson/AtomicCards.json (shared with
// `impulsive_maneuvers_batch_shield`).
const IMPULSIVE_MANEUVERS: &str = "Whenever a creature attacks, flip a coin. If you win the flip, the next time that creature would deal combat damage this turn, it deals double that damage instead. If you lose the flip, the next time that creature would deal combat damage this turn, prevent that damage.";
// Verbatim Awe Strike (Scryfall; shared with `awe_strike_prevention`).
const AWE_STRIKE: &str = "The next time target creature would deal damage this turn, prevent that damage. You gain life equal to the damage prevented this way.";

fn free_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 0,
    }
}

struct Fixture {
    runner: GameRunner,
    rhino: ObjectId,
    a: ObjectId,
    b: ObjectId,
}

/// A prompt observed mid-batch: who chose and what was offered.
#[derive(Debug, PartialEq, Eq)]
struct PromptShape {
    player: PlayerId,
    candidate_count: usize,
    sources: Vec<ObjectId>,
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    p0_life: i32,
    p1_life: i32,
    prompts: Vec<PromptShape>,
    prevented: Vec<(TargetRef, u32)>,
}

fn setup() -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rhino = scenario
        .add_creature(P0, "Stampeding Rhino", 4, 4)
        .trample()
        .id();
    let a = scenario.add_creature(P1, "First blocker", 1, 1).id();
    let b = scenario.add_creature(P1, "Second blocker", 1, 1).id();
    scenario.add_enchantment_from_oracle(P0, "Impulsive Maneuvers", IMPULSIVE_MANEUVERS);
    let awe = scenario
        .add_spell_to_hand_from_oracle(P0, "Awe Strike", true, AWE_STRIKE)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();
    runner.cast(awe).target_object(rhino).resolve();
    Fixture {
        runner,
        rhino,
        a,
        b,
    }
}

/// Resolve the single Maneuvers attack trigger with a pinned win.
fn resolve_attack_flip_win(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { player, triggers } => {
                assert_eq!(player, P0, "P0 orders the Maneuvers attack trigger");
                assert!(!triggers.is_empty(), "the attack trigger must exist");
                events.extend(
                    runner
                        .act(GameAction::OrderTriggers {
                            order: (0..triggers.len()).collect(),
                        })
                        .expect("order the attack trigger")
                        .events,
                );
            }
            WaitingFor::Priority { .. } => {
                assert!(
                    !runner.state().stack.is_empty(),
                    "the attack trigger must still be on the stack"
                );
                // Pin immediately before resolution, as in
                // `impulsive_maneuvers_batch_shield`.
                runner.state_mut().rng = GameRng::seed_from_u64(0);
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("resolve the attack trigger")
                        .events,
                );
            }
            other => panic!("unexpected attack-trigger prompt: {other:?}"),
        }
        let flips: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CoinFlipped { player_id, won } => Some((*player_id, *won)),
                _ => None,
            })
            .collect();
        if !flips.is_empty() {
            assert_eq!(
                flips,
                vec![(P0, true)],
                "observe the pinned win and its actual actor"
            );
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            return events;
        }
    }
    panic!("the attack trigger never flipped its coin");
}

fn assert_doubling_installed(runner: &GameRunner, rhino: ObjectId) {
    // The won flip installs a hosted consume-on-apply Double (the loss branch
    // installs prevention instead); the shield kind stays `None`, as in
    // `impulsive_maneuvers_batch_shield`'s own guard.
    assert!(
        runner.state().objects[&rhino]
            .replacement_definitions
            .as_slice()
            .iter()
            .any(|shield| {
                shield.damage_modification == Some(DamageModification::Double)
                    && shield.consume_on_apply
                    && !shield.is_consumed
            }),
        "the won flip must install a live doubling one-shot on the attacker"
    );
}

/// Read the live two-candidate prompt: the hosted doubling plus the floating
/// Awe shield. Returns the doubling's index (answer double-first: the
/// doubling always has something to double, so both one-shots consume).
/// The parked event must name this portion's own recipient, so a same-event
/// repeat can never masquerade as the next portion's pause.
fn assert_double_awe_prompt(
    runner: &GameRunner,
    rhino: ObjectId,
    portion: &str,
    expected_target: TargetRef,
) -> (usize, Vec<ReplacementCandidateSummary>) {
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player,
            candidate_count,
            candidates,
            ..
        } => {
            assert_eq!(
                *player, P1,
                "{portion}: the affected player chooses the replacement order"
            );
            assert_eq!(
                *candidate_count, 2,
                "{portion}: doubling + Awe must publish two candidates, got {candidates:?}"
            );
            match runner
                .state()
                .pending_replacement
                .as_ref()
                .expect("a parked event must back the prompt")
                .proposed
                .clone()
            {
                ProposedEvent::Damage { target, .. } => assert_eq!(
                    target, expected_target,
                    "{portion}: the pause must name its own portion"
                ),
                other => panic!("{portion}: the prompt must park damage, got {other:?}"),
            }
            assert_eq!(
                candidates.len(),
                2,
                "{portion}: the prompt must carry both candidates, got {candidates:?}"
            );
            let double_index = candidates
                .iter()
                .position(|c| c.source_id == rhino)
                .unwrap_or_else(|| {
                    panic!("{portion}: the hosted doubling must be offered, got {candidates:?}")
                });
            assert!(
                candidates.iter().any(|c| c.source_id != rhino),
                "{portion}: the floating Awe shield must be offered, got {candidates:?}"
            );
            (double_index, candidates.clone())
        }
        other => panic!("{portion}: expected a two-candidate prompt, got {other:?}"),
    }
}

/// Answer double-first, then drain CR 616.1f single-candidate repeats (the
/// lone remainder may auto-apply or re-prompt; either way it must not be the
/// doubling twice). Stops at the next multi-candidate prompt — the following
/// portion's own pause — leaving it for the caller.
fn answer_double_then_drain(
    runner: &mut GameRunner,
    double_index: usize,
    rhino: ObjectId,
    portion: &str,
    log: &mut Vec<GameEvent>,
) {
    log.extend(
        runner
            .act(GameAction::ChooseReplacement {
                index: double_index,
            })
            .expect("the doubling answer must be answerable")
            .events,
    );
    for _ in 0..8 {
        let WaitingFor::ReplacementChoice {
            candidate_count,
            candidates,
            ..
        } = runner.state().waiting_for.clone()
        else {
            return;
        };
        if candidate_count != 1 {
            return;
        }
        assert_ne!(
            candidates[0].source_id, rhino,
            "{portion}: the doubling must never apply twice to the same event"
        );
        log.extend(
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect("the lone remainder must be answerable")
                .events,
        );
    }
    panic!("{portion}: lone-remainder drain did not settle");
}

fn prompt_shape(runner: &GameRunner) -> PromptShape {
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player,
            candidate_count,
            candidates,
            ..
        } => PromptShape {
            player: *player,
            candidate_count: *candidate_count,
            sources: candidates.iter().map(|c| c.source_id).collect(),
        },
        other => panic!("expected a replacement prompt, got {other:?}"),
    }
}

fn prevented_pairs(events: &[GameEvent]) -> Vec<(TargetRef, u32)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamagePrevented { target, amount, .. } => Some((target.clone(), *amount)),
            _ => None,
        })
        .collect()
}

fn record_len(runner: &GameRunner) -> usize {
    runner
        .state()
        .pending_combat_damage
        .as_ref()
        .expect("a parked combat record must exist at the pause")
        .one_shot_applications
        .len()
}

/// Drive the 1/1/2 through the portion-3 pause; when `save_load`, round-trip
/// the paused state through the authoritative save format before resuming.
fn drive(save_load: bool) -> Outcome {
    let mut fixture = setup();
    let runner = &mut fixture.runner;
    let mut log = declare_attacks(runner, &[fixture.rhino]);
    log.extend(resolve_attack_flip_win(runner));
    assert_doubling_installed(runner, fixture.rhino);
    log.extend(reach_combat_damage(
        runner,
        &[(fixture.a, fixture.rhino), (fixture.b, fixture.rhino)],
    ));
    log.extend(assign_trample(
        runner,
        fixture.rhino,
        &[(fixture.a, 1), (fixture.b, 1)],
        2,
    ));

    let mut prompts = Vec::new();
    // Portion 1 (Rhino -> A): both one-shots live — prompt, consume both.
    let (double, _) = assert_double_awe_prompt(
        runner,
        fixture.rhino,
        "portion 1",
        TargetRef::Object(fixture.a),
    );
    prompts.push(prompt_shape(runner));
    answer_double_then_drain(runner, double, fixture.rhino, "portion 1", &mut log);
    // Portion 2 (Rhino -> B): both consumed — re-prompted from the ledger.
    let (double, _) = assert_double_awe_prompt(
        runner,
        fixture.rhino,
        "portion 2",
        TargetRef::Object(fixture.b),
    );
    prompts.push(prompt_shape(runner));
    answer_double_then_drain(runner, double, fixture.rhino, "portion 2", &mut log);
    // Portion 3 (2 trample to P1): THE pause — two consumed, two re-published.
    let (double, parked_candidates) =
        assert_double_awe_prompt(runner, fixture.rhino, "portion 3", TargetRef::Player(P1));
    prompts.push(prompt_shape(runner));
    assert_eq!(
        record_len(runner),
        2,
        "the park must carry the two consumed one-shots"
    );

    if save_load {
        let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
            .expect("the authoritative paused state serializes");
        let restored: PersistedGameState =
            serde_json::from_str(&saved).expect("the authoritative paused state deserializes");
        fixture.runner = GameRunner::from_state(
            restored
                .into_game_state()
                .expect("persisted test snapshot satisfies the checked restore contract"),
        );
        let runner = &mut fixture.runner;
        let restored_candidates = match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { candidates, .. } => candidates,
            other => panic!("the pause must survive restore, got {other:?}"),
        };
        assert_eq!(
            restored_candidates.len(),
            parked_candidates.len(),
            "the restored prompt must offer the same two candidates"
        );
        assert_eq!(
            restored_candidates
                .iter()
                .map(|c| (c.source_id, c.description.clone()))
                .collect::<Vec<_>>(),
            parked_candidates
                .iter()
                .map(|c| (c.source_id, c.description.clone()))
                .collect::<Vec<_>>(),
            "the restored prompt must re-publish the same two candidates"
        );
        assert_eq!(
            record_len(runner),
            2,
            "the restored record must still hold the two consumed one-shots"
        );
    }

    let runner = &mut fixture.runner;
    answer_double_then_drain(runner, double, fixture.rhino, "portion 3", &mut log);

    // Completion boundary: the batch finished, nothing left parked.
    assert_eq!(runner.state().phase, Phase::CombatDamage);
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .expect("combat must still be present")
            .regular_damage_done
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "damage must finish, not suspend at an unanswered prompt"
    );
    assert!(runner.state().pending_combat_damage.is_none());
    assert_eq!(runner.life(P1), 20, "every portion was prevented");
    assert_eq!(
        runner.life(P0),
        28,
        "Awe's rider gains the batch total once (2+2+4)"
    );
    for blocker in [fixture.a, fixture.b] {
        assert_eq!(runner.state().objects[&blocker].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&blocker].damage_marked, 0);
    }
    // Per (shield, recipient) aggregates: A 2, B 2, P1 4.
    assert!(
        damage_amounts(&log, fixture.rhino, TargetRef::Player(P1)).is_empty(),
        "no trample may be dealt through"
    );
    Outcome {
        p0_life: runner.life(P0),
        p1_life: runner.life(P1),
        prompts,
        prevented: prevented_pairs(&log),
    }
}

/// Matt R15 item 1's verbatim example: Stampeding Rhino with Impulsive
/// Maneuvers' doubling, split 1/1/2, with the SECOND blocker's protection
/// choice pausing the batch. Portion 1 auto-applies the doubling (consuming
/// it); the portion-2 [doubling, protection] pause parks the one-entry
/// ledger; portion 3's 2 trample must still double to 4 after save/load.
/// Pre-fix the resumed batch starts an empty ledger and the player takes 2.
fn setup_matt_example() -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rhino = scenario
        .add_creature(P0, "Stampeding Rhino", 4, 4)
        .trample()
        .with_color(vec![ManaColor::Green])
        .id();
    let a = scenario.add_creature(P1, "First blocker", 1, 1).id();
    let b = scenario
        .add_creature(P1, "Second blocker", 1, 1)
        .from_oracle_text_with_keywords(&["Protection"], "Protection from green")
        .id();
    scenario.add_enchantment_from_oracle(P0, "Impulsive Maneuvers", IMPULSIVE_MANEUVERS);
    let runner = scenario.build();
    Fixture {
        runner,
        rhino,
        a,
        b,
    }
}

fn assert_double_protection_prompt(
    runner: &GameRunner,
    rhino: ObjectId,
    blocker: ObjectId,
) -> (usize, Vec<ReplacementCandidateSummary>) {
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player,
            candidate_count,
            candidates,
            ..
        } => {
            assert_eq!(
                *player, P1,
                "the affected player chooses the replacement order"
            );
            assert_eq!(
                *candidate_count, 2,
                "doubling + protection must publish two candidates, got {candidates:?}"
            );
            // Portion 1 applied silently: the parked event must already name
            // the second blocker.
            match runner
                .state()
                .pending_replacement
                .as_ref()
                .expect("a parked event must back the prompt")
                .proposed
                .clone()
            {
                ProposedEvent::Damage { target, .. } => assert_eq!(
                    target,
                    TargetRef::Object(blocker),
                    "the pause must be portion 2's, after portion 1 applied"
                ),
                other => panic!("the prompt must park damage, got {other:?}"),
            }
            let double_index = candidates
                .iter()
                .position(|c| c.source_id == rhino)
                .unwrap_or_else(|| {
                    panic!("the hosted doubling must be offered, got {candidates:?}")
                });
            assert!(
                candidates.iter().any(|c| c
                    .description
                    .starts_with("Prevent damage with protection from ")),
                "the blocker's protection must be offered, got {candidates:?}"
            );
            (double_index, candidates.clone())
        }
        other => panic!("expected the portion-2 two-candidate prompt, got {other:?}"),
    }
}

fn drive_matt_example(save_load: bool) -> Outcome {
    let mut fixture = setup_matt_example();
    let runner = &mut fixture.runner;
    let mut log = declare_attacks(runner, &[fixture.rhino]);
    log.extend(resolve_attack_flip_win(runner));
    assert_doubling_installed(runner, fixture.rhino);
    log.extend(reach_combat_damage(
        runner,
        &[(fixture.a, fixture.rhino), (fixture.b, fixture.rhino)],
    ));
    log.extend(assign_trample(
        runner,
        fixture.rhino,
        &[(fixture.a, 1), (fixture.b, 1)],
        2,
    ));

    let mut prompts = Vec::new();
    let (double, parked_candidates) =
        assert_double_protection_prompt(runner, fixture.rhino, fixture.b);
    prompts.push(prompt_shape(runner));
    assert_eq!(
        record_len(runner),
        1,
        "the park must carry the one consumed doubling"
    );

    if save_load {
        let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
            .expect("the authoritative paused state serializes");
        let restored: PersistedGameState =
            serde_json::from_str(&saved).expect("the authoritative paused state deserializes");
        fixture.runner = GameRunner::from_state(
            restored
                .into_game_state()
                .expect("persisted test snapshot satisfies the checked restore contract"),
        );
        let runner = &mut fixture.runner;
        let restored_candidates = match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { candidates, .. } => candidates,
            other => panic!("the pause must survive restore, got {other:?}"),
        };
        assert_eq!(
            restored_candidates
                .iter()
                .map(|c| (c.source_id, c.description.clone()))
                .collect::<Vec<_>>(),
            parked_candidates
                .iter()
                .map(|c| (c.source_id, c.description.clone()))
                .collect::<Vec<_>>(),
            "the restored prompt must re-publish the same two candidates"
        );
        assert_eq!(
            record_len(runner),
            1,
            "the restored record must still hold the consumed doubling"
        );
    }

    let runner = &mut fixture.runner;
    answer_double_then_drain(runner, double, fixture.rhino, "portion 2", &mut log);

    assert_eq!(runner.state().phase, Phase::CombatDamage);
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .expect("combat must still be present")
            .regular_damage_done
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "damage must finish, not suspend at an unanswered prompt"
    );
    assert!(runner.state().pending_combat_damage.is_none());
    // THE assertion: portion 3's 2 trample doubled to 4 — pre-fix the
    // resumed batch loses the doubling and the player takes 2.
    assert_eq!(
        damage_amounts(&log, fixture.rhino, TargetRef::Player(P1)),
        vec![4],
        "the doubling must span the save/load pause to portion 3"
    );
    assert_eq!(runner.life(P1), 16);
    assert_eq!(
        damage_amounts(&log, fixture.rhino, TargetRef::Object(fixture.a)),
        vec![2],
        "portion 1's 1 doubled before the pause"
    );
    assert_eq!(
        runner.state().objects[&fixture.b].damage_marked,
        0,
        "portion 2 was prevented after doubling"
    );
    assert_eq!(runner.state().objects[&fixture.b].zone, Zone::Battlefield);
    Outcome {
        p0_life: runner.life(P0),
        p1_life: runner.life(P1),
        prompts,
        prevented: prevented_pairs(&log),
    }
}

#[test]
fn doubling_spans_save_load_pause_to_later_portion() {
    let direct = drive_matt_example(false);
    assert_eq!(
        direct.prompts.len(),
        1,
        "only portion 2 pauses — portion 1 auto-applies, portion 3 re-uses the ledger"
    );
    let saved = drive_matt_example(true);
    assert_eq!(
        saved, direct,
        "the save/load run must complete identically to the no-save twin"
    );
}

#[test]
fn one_shot_ledger_survives_mid_batch_save_load() {
    let direct = drive(false);
    assert_eq!(
        direct.prompts.len(),
        3,
        "all three portions must pause, no more, no fewer"
    );
    assert!(
        direct
            .prompts
            .iter()
            .all(|p| p.player == P1 && p.candidate_count == 2 && p.sources.len() == 2),
        "every pause offers doubling + Awe to P1: {:?}",
        direct.prompts
    );
    let saved = drive(true);
    assert_eq!(
        saved, direct,
        "the save/load run must complete identically to the no-save twin"
    );
}
