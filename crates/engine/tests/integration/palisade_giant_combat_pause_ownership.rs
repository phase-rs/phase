//! Matt R12: a redirect answered from one combat-damage ordering choice must
//! not strand the paused batch member when a second choice parks.
//!
//! Two unblocked attackers (A 2/2, B 4/4 lifelink) hit P1, who controls two
//! Palisade Giants plus a Healing Salve shield. A's event pauses twice: park
//! #1 orders the two Giant redirects plus the Salve (names P1, the original
//! recipient — the Salve shield presents unscoped); answering with Giant A's
//! redirect rewrites the event to Giant A, and the CR 616.1f repeat over
//! Giant B's redirect + the Salve parks #2 naming Giant A. Reconstructing
//! ownership from the original assignment target (P1) strands park #2's
//! answer: no stash fires, the resume misaligns
//! (`decided.len() != paused_entry + 1`), and in release the survivor vector
//! zips against the wrong entries. The combat-pause stamp (captured at the
//! park, inherited across the re-park by damage source) settles the slot.
//!
//! B is the distinguishable surviving second source: its 4 is Salved to 3,
//! lands on Giant B, and gains P0 3 life via batch lifelink (entry-driven, so
//! a survivor/entry swap strands B's entry and gains nothing), while A's 2 is
//! fully prevented. The final tallies plus the mid-run `decided == [None]`
//! read pin the alignment, not just completion.
//!
//! Revert-failing: without the stamp, park #2's answer fires no stash and the
//! resume trips `a resumed batch must carry its paused slot's stash` (debug);
//! the `decided` read and the lifelink tally catch it in release too.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CombatPauseSlot, PersistedGameState, ReplacementCandidateSummary, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::zones::Zone;

/// Verbatim Palisade Giant (`client/public/card-data.json`).
const PALISADE_GIANT_TEXT: &str =
    "All damage that would be dealt to you and other permanents you control is dealt to this creature instead.";

/// Verbatim Healing Salve (`client/public/card-data.json`). Mode index 1 is
/// the prevention half.
const HEALING_SALVE_TEXT: &str = "Choose one \u{2014}\n\u{2022} Target player gains 3 life.\n\u{2022} Prevent the next 3 damage that would be dealt to any target this turn.";

fn free_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 0,
    }
}

/// Stock both libraries. Without this a player decks out, the game ends in
/// `WaitingFor::GameOver`, combat never runs, and every tally assertion
/// passes vacuously.
fn stock_libraries(scenario: &mut GameScenario) {
    scenario.with_library_top(
        P0,
        &["F0a", "F0b", "F0c", "F0d", "F0e", "F0f", "F0g", "F0h"],
    );
    scenario.with_library_top(
        P1,
        &["F1a", "F1b", "F1c", "F1d", "F1e", "F1f", "F1g", "F1h"],
    );
}

fn pass_priority_to(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..4 {
        if runner.state().priority_player == player {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority must succeed");
    }
    panic!("priority never reached {player:?}");
}

struct Fixture {
    runner: GameRunner,
    a: ObjectId,
    b: ObjectId,
    giant_a: ObjectId,
    giant_b: ObjectId,
}

/// Two attackers for P0 (B has lifelink), two Giants for P1, and a Salve
/// shield — cast up front so CR 615.4 is satisfied before combat. The Salve
/// is aimed at Giant A, but the shield presents unscoped (see park #1).
fn setup() -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let a = scenario.add_creature(P0, "Cackler", 2, 2).id();
    let b = scenario.add_creature(P0, "Ogre", 4, 4).lifelink().id();
    let giant_a = scenario
        .add_creature_from_oracle(P1, "Palisade Giant", 3, 4, PALISADE_GIANT_TEXT)
        .id();
    let giant_b = scenario
        .add_creature_from_oracle(P1, "Palisade Giant", 3, 4, PALISADE_GIANT_TEXT)
        .id();
    let salve = scenario
        .add_spell_to_hand_from_oracle(P1, "Healing Salve", true, HEALING_SALVE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    pass_priority_to(&mut runner, P1);
    runner
        .cast(salve)
        .modes(&[1])
        .target_objects(&[giant_a])
        .resolve();
    assert_eq!(
        runner.state().objects[&salve].zone,
        Zone::Graveyard,
        "reach guard: the Salve must resolve before combat, leaving its shield"
    );
    Fixture {
        runner,
        a,
        b,
        giant_a,
        giant_b,
    }
}

/// Declare both attackers into P1 with no blockers, then drive to the next
/// replacement-ordering prompt, returning its candidates.
fn drive_to_choice(
    runner: &mut GameRunner,
    a: ObjectId,
    b: ObjectId,
) -> Vec<ReplacementCandidateSummary> {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { player, .. } if player == P0 => {
                runner
                    .declare_attackers(&[
                        (a, AttackTarget::Player(P1)),
                        (b, AttackTarget::Player(P1)),
                    ])
                    .expect("declaring both attackers must succeed");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .declare_attackers(&[])
                    .expect("declaring no attackers must succeed");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .declare_blockers(&[])
                    .expect("declaring no blockers must succeed");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
            }
            WaitingFor::ReplacementChoice { candidates, .. } => return candidates,
            other => panic!("unexpected prompt while driving combat: {other:?}"),
        }
    }
    panic!("never reached a replacement-ordering prompt");
}

fn answer(runner: &mut GameRunner, index: usize) {
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("the ordering choice must be answerable");
}

/// The parked damage event's (source, target); panics unless it is Damage.
fn parked_damage(runner: &GameRunner) -> (ObjectId, TargetRef) {
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("a replacement record must be parked at an ordering prompt");
    match &pending.proposed {
        ProposedEvent::Damage {
            source_id, target, ..
        } => (*source_id, target.clone()),
        other => panic!("the parked event must be damage, got {other:?}"),
    }
}

fn index_of_source(candidates: &[ReplacementCandidateSummary], source: ObjectId) -> usize {
    candidates
        .iter()
        .position(|c| c.source_id == source)
        .unwrap_or_else(|| panic!("no candidate from {source:?} in {candidates:?}"))
}

fn index_of_salve(
    candidates: &[ReplacementCandidateSummary],
    giant_a: ObjectId,
    giant_b: ObjectId,
) -> usize {
    candidates
        .iter()
        .position(|c| c.source_id != giant_a && c.source_id != giant_b)
        .unwrap_or_else(|| panic!("no Salve candidate in {candidates:?}"))
}

/// Park #1 (A's event, both Giant redirects) then park #2 (redirected to
/// Giant A: Giant B's redirect + the Salve). Answers park #1 with Giant A's
/// redirect and returns park #2's candidates.
fn drive_to_redirected_second_park(fixture: &mut Fixture) -> Vec<ReplacementCandidateSummary> {
    let Fixture {
        runner,
        a,
        b,
        giant_a,
        ..
    } = fixture;
    let (a, b, giant_a) = (*a, *b, *giant_a);

    // Park #1: A's event, pre-redirect — names P1. Three candidates: the
    // Salve shield presents unscoped (offered for damage to P1 as well as to
    // Giant A), so it joins the ordering. The test does not depend on Salve
    // scoping either way — it needs the Salve applicable post-redirect, which
    // holds regardless.
    let candidates = drive_to_choice(runner, a, b);
    assert_eq!(
        candidates.len(),
        3,
        "park #1 must offer the two Giant redirects plus the Salve, got {candidates:?}"
    );
    assert_eq!(
        candidates
            .iter()
            .filter(|c| c.source_name.contains("Palisade Giant"))
            .count(),
        2,
        "park #1 must offer both Giant redirects, got {candidates:?}"
    );
    assert_eq!(
        parked_damage(runner),
        (a, TargetRef::Player(P1)),
        "park #1 must name A's unredirected event"
    );
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .expect("parked record")
            .combat_pause_slot,
        Some(CombatPauseSlot {
            paused_entry: 0,
            source: a,
        }),
        "the combat park must stamp A's prompt with slot 0"
    );
    let record = runner
        .state()
        .pending_combat_damage
        .as_ref()
        .expect("the combat batch must be parked at park #1");
    assert_eq!(record.paused_entry, 0, "A must be the paused entry");
    assert!(
        record.decided.is_empty(),
        "nothing is decided before the first answer"
    );

    // Answer with Giant A's redirect; the CR 616.1f repeat re-parks naming
    // Giant A.
    answer(runner, index_of_source(&candidates, giant_a));
    let candidates = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates,
        other => panic!("park #2 must follow the redirect, got {other:?}"),
    };
    assert_eq!(
        candidates.len(),
        2,
        "park #2 must offer Giant B's redirect plus the Salve, got {candidates:?}"
    );
    assert_eq!(
        parked_damage(runner),
        (a, TargetRef::Object(giant_a)),
        "park #2 must name A's event redirected to Giant A"
    );
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .expect("parked record")
            .combat_pause_slot,
        Some(CombatPauseSlot {
            paused_entry: 0,
            source: a,
        }),
        "park #2 must inherit A's stamp across the redirect"
    );
    candidates
}

/// From park #2's candidates: Salve A's event away, run B's event through its
/// two parks (redirect then Salve remainder), and finish combat. Returns
/// nothing; the caller asserts the final tallies.
fn answer_from_second_park(fixture: &mut Fixture, candidates: &[ReplacementCandidateSummary]) {
    let Fixture {
        runner,
        a,
        b,
        giant_a,
        giant_b,
    } = fixture;
    let (a, b, giant_a, giant_b) = (*a, *b, *giant_a, *giant_b);

    // Park #2's answer (Salve) fully prevents A's 2. The stamp must settle the
    // paused slot even though the parked event names Giant A, not P1.
    answer(runner, index_of_salve(candidates, giant_a, giant_b));
    let record = runner
        .state()
        .pending_combat_damage
        .as_ref()
        .expect("the batch must still be parked while B's event runs");
    assert_eq!(
        record.decided.len(),
        1,
        "park #2's answer must stash A's slot before B's event pauses"
    );
    assert!(
        record.decided[0].is_none(),
        "A's stashed slot must be the full prevention, not a survivor"
    );

    // Park #3: B's event, pre-redirect — a FRESH slot on the re-paused record.
    let candidates = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates,
        other => panic!("park #3 must follow for B's event, got {other:?}"),
    };
    assert_eq!(
        candidates.len(),
        3,
        "park #3 must offer the two Giant redirects plus the Salve remainder, got {candidates:?}"
    );
    assert_eq!(
        parked_damage(runner),
        (b, TargetRef::Player(P1)),
        "park #3 must name B's unredirected event"
    );
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .expect("parked record")
            .combat_pause_slot,
        Some(CombatPauseSlot {
            paused_entry: 1,
            source: b,
        }),
        "the re-pause must stamp B's prompt with the fresh slot 1"
    );

    // Park #4: B's event redirected to Giant A — stamp inherited again.
    answer(runner, index_of_source(&candidates, giant_a));
    let candidates = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates,
        other => panic!("park #4 must follow the redirect, got {other:?}"),
    };
    assert_eq!(
        parked_damage(runner),
        (b, TargetRef::Object(giant_a)),
        "park #4 must name B's event redirected to Giant A"
    );
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .expect("parked record")
            .combat_pause_slot,
        Some(CombatPauseSlot {
            paused_entry: 1,
            source: b,
        }),
        "park #4 must inherit B's stamp across the redirect"
    );

    // The Salve's remainder prevents 1 of B's 4; Giant B's redirect then
    // applies unopposed and 3 lands on Giant B.
    answer(runner, index_of_salve(&candidates, giant_a, giant_b));
    for _ in 0..60 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
            }
            other => panic!("unexpected prompt after the last answer: {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ),
        "combat must complete, stuck at {:?} / {:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_combat_damage.is_none(),
        "the combat record must be consumed by completion"
    );
    assert!(
        runner.state().pending_replacement.is_none(),
        "no replacement record may survive completion"
    );
    let _ = a;
}

fn assert_final_tallies(fixture: &Fixture, p0_life_before: i32, p1_life_before: i32) {
    let Fixture {
        runner,
        a,
        b,
        giant_a,
        giant_b,
    } = fixture;
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_life_before,
        "P1 must lose no life: everything redirected"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        p0_life_before + 3,
        "P0 must gain exactly B's surviving 3 via batch lifelink (a survivor/entry swap strands B's entry and gains nothing)"
    );
    assert_eq!(
        runner.state().objects[giant_a].damage_marked,
        0,
        "Giant A must be unmarked: A's 2 was prevented, B's 4 moved on"
    );
    assert_eq!(
        runner.state().objects[giant_b].damage_marked,
        3,
        "Giant B must carry B's surviving 3 (4 minus the Salve remainder)"
    );
    assert_eq!(
        runner.state().objects[a].damage_marked,
        0,
        "unblocked A takes nothing back"
    );
    assert_eq!(
        runner.state().objects[b].damage_marked,
        0,
        "unblocked B takes nothing back"
    );
}

#[test]
fn redirect_before_choice_settles_the_paused_slot_and_keeps_the_batch_aligned() {
    let mut fixture = setup();
    let p0_life_before = fixture.runner.state().players[P0.0 as usize].life;
    let p1_life_before = fixture.runner.state().players[P1.0 as usize].life;
    let candidates = drive_to_redirected_second_park(&mut fixture);
    answer_from_second_park(&mut fixture, &candidates);
    assert_final_tallies(&fixture, p0_life_before, p1_life_before);
}

/// The same run save/loaded THROUGH park #2 — the redirected prompt, the
/// parked combat record, and the inherited stamp must all survive the
/// authoritative round trip, and the restored run must complete identically.
#[test]
fn redirected_second_park_survives_save_load() {
    let mut fixture = setup();
    let p0_life_before = fixture.runner.state().players[P0.0 as usize].life;
    let p1_life_before = fixture.runner.state().players[P1.0 as usize].life;
    let candidates = drive_to_redirected_second_park(&mut fixture);

    let saved = serde_json::to_string(&PersistedGameState::capture(fixture.runner.state().clone()))
        .expect("the authoritative paused state serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the authoritative paused state deserializes");
    fixture.runner = GameRunner::from_state(
        restored
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract"),
    );

    // Everything park #2 needs must survive restore.
    let restored_candidates = match fixture.runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates,
        other => panic!("park #2 must survive restore, got {other:?}"),
    };
    assert_eq!(
        restored_candidates.len(),
        candidates.len(),
        "the restored prompt must offer the same candidates"
    );
    assert_eq!(
        parked_damage(&fixture.runner),
        (fixture.a, TargetRef::Object(fixture.giant_a)),
        "the restored park must still name the redirected event"
    );
    assert_eq!(
        fixture
            .runner
            .state()
            .pending_replacement
            .as_ref()
            .expect("parked record")
            .combat_pause_slot,
        Some(CombatPauseSlot {
            paused_entry: 0,
            source: fixture.a,
        }),
        "the inherited stamp must survive restore"
    );
    assert!(
        fixture.runner.state().pending_combat_damage.is_some(),
        "the parked combat record must survive restore"
    );

    answer_from_second_park(&mut fixture, &restored_candidates);
    assert_final_tallies(&fixture, p0_life_before, p1_life_before);
}
