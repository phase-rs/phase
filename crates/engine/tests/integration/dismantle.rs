//! Dismantle (DST/2XM) — "Destroy target artifact. If that artifact had counters
//! on it, put that many +1/+1 counters or charge counters on an artifact you
//! control."
//!
//! Verbatim Oracle text from `data/card-data.json["dismantle"].oracle_text`.
//! End-to-end `/card-test`: the card is staged via `add_spell_to_hand_from_oracle`
//! so its ability comes from the REAL parser, not a hand-built AST.
//!
//! Gatherer rulings (MTGJSON `allsets/DST.json`, 2020-08-07) that the runtime
//! contract rests on:
//!
//!   1. "Dismantle targets only the artifact that will be destroyed. When
//!      Dismantle resolves, you choose which type of counters you want and choose
//!      an artifact you control to put them on." — CR 115.10a: the RECIPIENT is a
//!      choice, not a target (there is no literal "target" over it), so it is
//!      chosen while the effect resolves (CR 608.2d), never announced at cast.
//!   2. "If the target is legal but not destroyed (most likely because it has
//!      indestructible), you do put counters on an artifact." — the count must be
//!      read from the TARGET (live if it survived, LKI if it left), never from a
//!      zone-change ledger.
//!   3. "It doesn't matter what kind of counters the destroyed artifact had on
//!      it, only how many." — the gate/count is `counter_type: None`, summing
//!      every kind.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DISMANTLE_ORACLE: &str = "Destroy target artifact. If that artifact had counters on it, \
     put that many +1/+1 counters or charge counters on an artifact you control.";

fn three_generic() -> Vec<ManaUnit> {
    (0..3)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

struct Fixture {
    runner: GameRunner,
    spell: ObjectId,
    /// The opponent's counter-laden artifact — Dismantle's target `T`.
    target: ObjectId,
    /// The artifacts P0 controls — the legal recipients (may be empty).
    mine: Vec<ObjectId>,
}

/// Stage Dismantle in P0's hand — parsed LIVE from the verbatim Oracle text, so
/// every assertion below exercises the real parser + resolver, not a hand-built
/// AST — with `target_counters` counters on the OPPONENT's artifact (so the
/// recipient filter's `controller: You` is load bearing), plus exactly
/// `my_artifact_count` artifacts P0 controls.
fn fixture(target_counters: &[(CounterType, u32)], my_artifact_count: usize) -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, three_generic());

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Dismantle", false, DISMANTLE_ORACLE)
        .id();

    let target = scenario
        .add_artifact_from_oracle(P1, "Counter-Laden Artifact", "")
        .id();
    let mine: Vec<ObjectId> = (0..my_artifact_count)
        .map(|i| {
            scenario
                .add_artifact_from_oracle(P0, &format!("My Artifact {i}"), "")
                .id()
        })
        .collect();

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&target).unwrap();
        for (kind, count) in target_counters {
            obj.counters.insert(kind.clone(), *count);
        }
    }

    Fixture {
        runner,
        spell,
        target,
        mine,
    }
}

fn counters_on(runner: &GameRunner, id: ObjectId, kind: &CounterType) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(kind))
        .copied()
        .unwrap_or(0)
}

fn total_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .map(|obj| obj.counters.values().copied().sum())
        .unwrap_or(0)
}

/// Cast Dismantle at `target`, choose branch `branch_index` (0 = +1/+1, 1 =
/// charge) at the kind prompt, then choose `recipient` at the resolution-time
/// recipient prompt. Returns the runner post-resolution.
fn cast_choose_kind_and_recipient(
    mut runner: GameRunner,
    spell: ObjectId,
    target: ObjectId,
    branch_index: usize,
    recipient: ObjectId,
) -> GameRunner {
    let outcome = runner.cast(spell).target_objects(&[target]).resolve();
    drop(outcome);
    runner
        .act(GameAction::ChooseBranch {
            index: branch_index,
        })
        .expect("the counter-kind choice must succeed");
    // A single legal recipient auto-resolves without a prompt (CR 608.2d: the
    // player is never asked to make an impossible-to-vary choice); only act on
    // the ChooseFromZoneChoice when the engine actually raised one.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseFromZoneChoice { .. }
    ) {
        runner
            .act(GameAction::SelectCards {
                cards: vec![recipient],
            })
            .expect("the recipient choice must succeed");
    }
    runner
}

/// P3 — the topology gate, end to end through the REAL parser. A `ChooseOneOf`
/// of untargeted `PutCounter` branches must surface (a) the kind choice and
/// then (b) a RESOLUTION-TIME recipient choice enumerating every artifact the
/// controller controls, with no cast-time target slot for the recipient.
#[test]
fn p3_untargeted_branch_recipient_is_chosen_at_resolution() {
    let Fixture {
        mut runner,
        spell,
        target,
        mine,
    } = fixture(
        &[
            (CounterType::Plus1Plus1, 2),
            (CounterType::Generic("oil".to_string()), 1),
        ],
        2,
    );

    // Positive reach-guard: `T` really carries 3 counters across 2 kinds.
    assert_eq!(
        total_counters(&runner, target),
        3,
        "reach-guard: the target holds 3 counters before the cast"
    );

    let outcome = runner.cast(spell).target_objects(&[target]).resolve();
    assert_eq!(
        outcome.zone_of(target),
        Zone::Graveyard,
        "reach-guard: the Destroy instruction ran before the counter clause"
    );
    drop(outcome);

    // (a) The kind choice — exactly two branches, chosen by the controller.
    let branch_index = match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch {
            branches,
            branch_descriptions,
            player,
            ..
        } => {
            assert_eq!(
                branches.len(),
                2,
                "the kind choice offers exactly {{+1/+1, charge}}, got {branch_descriptions:?}"
            );
            assert_eq!(*player, P0, "CR 608.2d: the SPELL'S CONTROLLER chooses");
            0
        }
        other => {
            panic!("P3 FAIL (a): expected the counter-kind ChooseOneOfBranch prompt, got {other:?}")
        }
    };
    runner
        .act(GameAction::ChooseBranch {
            index: branch_index,
        })
        .expect("choosing the +1/+1 branch must succeed");

    // (b) THE TOPOLOGY GATE: a resolution-time recipient choice enumerating
    // BOTH artifacts P0 controls — and neither the destroyed target nor any
    // opponent permanent.
    let chosen = match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice {
            player,
            cards,
            count,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*count, 1, "exactly one recipient is chosen");
            let mut offered = cards.clone();
            offered.sort();
            let mut expected = mine.clone();
            expected.sort();
            assert_eq!(
                offered, expected,
                "P3: the recipient prompt must enumerate every artifact P0 controls \
                 (and CR 115.10a means the destroyed target is not among them)"
            );
            cards[0]
        }
        other => panic!(
            "P3 FAIL (b): expected a resolution-time recipient ChooseFromZoneChoice, got {other:?}"
        ),
    };
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("choosing the recipient artifact must succeed");

    // CR 608.2h + ruling 3: the amount is the TOTAL counters that were on `T`
    // (2 Plus1Plus1 + 1 oil = 3), regardless of kind.
    assert_eq!(
        counters_on(&runner, chosen, &CounterType::Plus1Plus1),
        3,
        "the chosen recipient receives one +1/+1 counter per counter that was on the target"
    );
    for other in mine.iter().filter(|id| **id != chosen) {
        assert_eq!(
            counters_on(&runner, *other, &CounterType::Plus1Plus1),
            0,
            "only the CHOSEN artifact receives counters"
        );
    }
}

/// Verification-Matrix row: the "charge" branch places `Generic("charge")`
/// counters, not `Plus1Plus1` — ruling 3, "you put ... seven charge counters".
#[test]
fn charge_branch_places_charge_counters() {
    let Fixture {
        mut runner,
        spell,
        target,
        mine,
    } = fixture(&[(CounterType::Plus1Plus1, 3)], 1);
    let recipient = mine[0];

    runner = cast_choose_kind_and_recipient(runner, spell, target, 1, recipient);

    assert_eq!(
        counters_on(
            &runner,
            recipient,
            &CounterType::Generic("charge".to_string())
        ),
        3,
        "the charge branch must place charge counters, magnitude = total on T"
    );
    assert_eq!(
        counters_on(&runner, recipient, &CounterType::Plus1Plus1),
        0,
        "no +1/+1 counters leak from the unchosen branch"
    );
}

/// Verification-Matrix row: a target with ZERO counters gates the whole 2nd
/// sentence closed (CR 608.2h) — Destroy still happens, but no kind prompt, no
/// counters anywhere, clean return to priority.
#[test]
fn zero_counter_target_gates_the_clause_closed() {
    let Fixture {
        mut runner,
        spell,
        target,
        ..
    } = fixture(&[], 1);

    let outcome = runner.cast(spell).target_objects(&[target]).resolve();
    assert_eq!(
        outcome.zone_of(target),
        Zone::Graveyard,
        "Destroy still happens with zero counters on the target"
    );
    drop(outcome);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no counter-kind prompt may be offered when the gate is false, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner
            .state()
            .objects
            .values()
            .all(|obj| obj.counters.values().all(|count| *count == 0)),
        "no counters may be placed when the gate is false"
    );
}

/// Verification-Matrix row: with the recipient a resolution-time CHOICE rather
/// than a cast-time target, Dismantle must stay castable when its controller
/// has no other artifact at all. If the recipient were lifted to a cast-time
/// slot the spell would be uncastable here.
#[test]
fn castable_with_no_artifact_to_receive_counters() {
    let Fixture {
        mut runner,
        spell,
        target,
        ..
    } = fixture(&[(CounterType::Plus1Plus1, 2)], 0);

    // The cast itself is the claim: a cast-time recipient slot would have made
    // this spell UNCASTABLE (no legal object to fill it) and `cast(..)` would
    // fail rather than reach resolution.
    let outcome = runner.cast(spell).target_objects(&[target]).resolve();
    assert_eq!(
        outcome.zone_of(target),
        Zone::Graveyard,
        "the spell was castable and destroyed its target with zero legal recipients"
    );
    drop(outcome);

    // The counter-KIND choice is still a legal choice and is still offered
    // (CR 608.2d bars only impossible options — picking "+1/+1" is possible;
    // it is the RECIPIENT that has no candidate).
    match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch { branches, .. } => assert_eq!(branches.len(), 2),
        other => panic!("expected the counter-kind prompt, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("choosing a kind must succeed even with no recipient available");

    // CR 608.2d: zero legal recipients is a silent no-op, not a hang and not a
    // crash. The pipeline returns to priority with no counters anywhere.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected a clean return to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner
            .state()
            .objects
            .values()
            .all(|obj| obj.counters.values().all(|count| *count == 0)),
        "no counters may be placed when the controller controls no artifact"
    );
}

/// Ruling 2, direct: an INDESTRUCTIBLE target survives `Destroy` — no zone
/// change, no LKI. The gate/count must still read the target's LIVE counters
/// and still place the counters ("you do put counters on an artifact").
#[test]
fn indestructible_target_still_gets_counters_from_live_count() {
    let Fixture {
        mut runner,
        spell,
        target,
        mine,
    } = fixture(&[(CounterType::Plus1Plus1, 4)], 1);
    let recipient = mine[0];
    {
        let obj = runner.state_mut().objects.get_mut(&target).unwrap();
        obj.base_keywords
            .push(engine::types::keywords::Keyword::Indestructible);
        obj.keywords = obj.base_keywords.clone();
    }

    let outcome = runner.cast(spell).target_objects(&[target]).resolve();
    assert_eq!(
        outcome.zone_of(target),
        Zone::Battlefield,
        "an indestructible target is not destroyed"
    );
    drop(outcome);

    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("the kind choice must still fire for a surviving indestructible target");
    // A single legal recipient auto-resolves without a prompt (CR 608.2d).
    if matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseFromZoneChoice { .. }
    ) {
        runner
            .act(GameAction::SelectCards {
                cards: vec![recipient],
            })
            .expect("the recipient choice must still fire");
    }

    assert_eq!(
        counters_on(&runner, recipient, &CounterType::Plus1Plus1),
        4,
        "ruling 2: an undestroyed target's LIVE counters still feed the placement"
    );
    assert_eq!(
        counters_on(&runner, target, &CounterType::Plus1Plus1),
        4,
        "the surviving target keeps its own counters unchanged"
    );
}

/// Verification-Matrix row: the opponent's OTHER artifacts are never offered
/// as recipients — `controller: Some(You)` is load-bearing.
#[test]
fn opponent_artifacts_are_never_offered_as_recipient() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, three_generic());
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Dismantle", false, DISMANTLE_ORACLE)
        .id();
    let target = scenario
        .add_artifact_from_oracle(P1, "Counter-Laden Artifact", "")
        .id();
    // Two of MY OWN artifacts, so the recipient prompt is a real (non-auto-
    // resolved) enumerated choice — a stronger proof that the opponent's
    // artifact is excluded by the filter, not merely never offered because
    // there was only one legal candidate.
    let mine_a = scenario
        .add_artifact_from_oracle(P0, "My Artifact A", "")
        .id();
    let mine_b = scenario
        .add_artifact_from_oracle(P0, "My Artifact B", "")
        .id();
    let opponent_other = scenario
        .add_artifact_from_oracle(P1, "Opponent's Other Artifact", "")
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, 1);

    let outcome = runner.cast(spell).target_objects(&[target]).resolve();
    drop(outcome);
    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("the kind choice must succeed");
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { cards, .. } => {
            let mut offered = cards.clone();
            offered.sort();
            let mut expected = vec![mine_a, mine_b];
            expected.sort();
            assert_eq!(
                offered, expected,
                "only the controller's own artifacts — never the opponent's, and \
                 never the (now-destroyed) target — may be offered"
            );
            assert!(!cards.contains(&opponent_other));
        }
        other => panic!("expected the resolution-time recipient prompt, got {other:?}"),
    }
}

/// Verification-Matrix row: dynamic magnitude — the recipient receives exactly
/// the target's total counter count, whatever it is (not a fixed number).
#[test]
fn dynamic_magnitude_tracks_the_targets_counter_total() {
    for &n in &[3u32, 5u32] {
        let Fixture {
            mut runner,
            spell,
            target,
            mine,
        } = fixture(&[(CounterType::Plus1Plus1, n)], 1);
        let recipient = mine[0];
        runner = cast_choose_kind_and_recipient(runner, spell, target, 0, recipient);
        assert_eq!(
            counters_on(&runner, recipient, &CounterType::Plus1Plus1),
            n,
            "the placed amount must track the target's total, not a fixed constant"
        );
    }
}
