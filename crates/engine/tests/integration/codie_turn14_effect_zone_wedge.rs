//! The Codie, Vociferous Codex turn-14 `EffectZoneChoice` wedge — a board that
//! reached `legal_actions_full == 0` and could never be advanced again.
//!
//! Codie's mana ability ends "**Put each other card exiled this way on the
//! bottom of your library in a random order**", parsed as `PutAtLibraryPosition
//! { position: Bottom, target: And [ Typed{Card,[Another]}, ExiledBySource ] }`.
//! Its members are therefore in `Zone::Exile`, but the producer hardcoded
//! `zone: Zone::Library` on the prompt, and the delivery guard admitted
//! `PutAtLibraryPosition` members only from `Hand | Library`. Every candidate
//! was refused, so the prompt offered zero legal actions and the game wedged
//! permanently.
//!
//! The fix has **two independent halves**, and this module is written to keep
//! both alive:
//!
//! * **Part 1 (`engine_resolution_choices.rs`) rescues already-wedged saves.**
//!   `into_game_state` restores a persisted prompt verbatim, so a save written
//!   by the old producer still arrives claiming `zone: Library` over
//!   Exile-resident members. No producer fix can reach it. Rows A1 and A2 pin
//!   this half.
//! * **Part 2 (`effects/put_on_top.rs`) prevents future wedges** by deriving the
//!   prompt's zone from the filter instead of asserting `Library`. Rows here do
//!   not discriminate on it — see the module-level note on A2.
//!
//! Row A3 pins the *bound*: the relocation exemption must not swallow
//! `Battlefield` or `Stack`. Row A4 pins that it did not become universal
//! across effect kinds.
//!
//! # Fixture provenance
//!
//! `../fixtures/codie_turn14.json.gz` is derived, not captured:
//!
//! | artifact | bytes | sha256 |
//! |---|---|---|
//! | source zip | 2 933 529 | `a1135fce4a95c1df2ebb731cc4cbc1bc5f2e11d8f5cedbabbcd3197949118656` |
//! | member `game-state-turn-14-2026-08-28T21-23-02-414Z.json` | 13 046 083 | `ae952d954f569abc32f6c5953d84e74c6fdc815575ffd9dde129b9e55d9daebf` |
//! | derived `codie_turn14.json.gz` (this fixture) | 476 408 | `2e3d7586ee9c9def2afd808f0de9e629b7c93ace67197ef2039437680fdad141` |
//!
//! Regeneration recipe — `-n` is **load-bearing**, since without it gzip stamps
//! an mtime into the header and the digest above never reproduces:
//!
//! ```text
//! unzip -p <zip> game-state-turn-14-2026-08-28T21-23-02-414Z.json \
//!   | jq -c '{gameState}' | gzip -9 -n > crates/engine/tests/fixtures/codie_turn14.json.gz
//! ```
//!
//! The raw 13 MB member is deliberately NOT tracked; only the 476 KB
//! `.json.gz` is.

use engine::ai_support::{legal_actions_full, stuck_decision_diagnostic};
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones;
use engine::types::ability::{EffectKind, LibraryPosition};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, PersistedGameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::zones::{EtbTapState, Zone};

fn gunzip(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

/// Load the dump's `["gameState"]` through the REAL production restore
/// chokepoint `PersistedGameState::into_game_state` — never a bare `GameState`
/// decode. The chokepoint is the whole point of row A1: it is what restores the
/// stale `zone: Library` prompt verbatim, which is why Part 2 alone cannot
/// rescue this board.
fn load_codie_turn14() -> GameState {
    let json = gunzip(include_bytes!("../fixtures/codie_turn14.json.gz"));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("gameState deserializes through the production decoder")
        .into_game_state()
        // `bc218c51ce` (#8039) made the chokepoint fallible: it now routes through
        // `prepare_for_restore`, which can reject a capture (`PersistedRestoreError`)
        // and which repairs terminal resolution state before publication. This
        // capture carries a live `resolving_stack_entry` plus a paused
        // `EffectZoneChoice` — exactly that repair's subject — so a rejection here
        // would itself be a user-facing regression, not a test problem. Matches the
        // `dina_noff_turn5_loader.rs` precedent updated in that same commit.
        .expect("persisted test snapshot satisfies the checked restore contract")
}

/// The two frozen members of the captured prompt, both sitting in `Zone::Exile`:
/// Lorehold, the Historian (88) and Thran Portal (4).
const CODIE_FROZEN_MEMBERS: [ObjectId; 2] = [ObjectId(88), ObjectId(4)];

/// A1 — **the wedge clears on the real captured board.**
///
/// Revert-failing assertion: `assert!(!actions.is_empty(), ...)`. Restoring the
/// Part 1 guard to `Some(Zone::Hand | Zone::Library)` refuses both Exile-resident
/// members, `legal_actions_full` returns 0, and this row reds. The follow-on
/// `stuck_decision_diagnostic(&state).is_none()` reds on the same revert.
///
/// **This row deliberately pins currently-INCORRECT behaviour.** The persisted
/// ability carries `count: Fixed(1)` verbatim and `into_game_state` does not
/// re-parse abilities, so exactly ONE of the two members is delivered and the
/// other is stranded in Exile — the library grows by 1, not 2. Codie's Oracle
/// text says "each other card", so the correct count is 2. That is a separate
/// parser defect (universal-quantifier cardinality) and is NOT fixed here; what
/// is fixed here is the STALL. Do not "fix" this assertion to `baseline + 2`
/// without fixing the parser first.
#[test]
fn a1_wedged_codie_save_regains_legal_actions_and_advances() {
    let mut state = load_codie_turn14();

    // Reach-guard: this is the real captured wedge, not a default or empty
    // state. Every clause below must hold or the row measures nothing.
    let (cards, baseline_library) = match &state.waiting_for {
        WaitingFor::EffectZoneChoice {
            effect_kind,
            library_position,
            zone,
            cards,
            player,
            ..
        } => {
            assert_eq!(
                *effect_kind,
                EffectKind::PutAtLibraryPosition,
                "the captured prompt must be the PutAtLibraryPosition bottom step",
            );
            assert_eq!(
                *library_position,
                Some(LibraryPosition::Bottom),
                "Codie bottoms the leftovers",
            );
            assert_eq!(
                *zone,
                Zone::Library,
                "the persisted prompt must still CLAIM Library — that stale claim, \
                 restored verbatim by into_game_state, is exactly what Part 1 rescues",
            );
            assert_eq!(cards.len(), 2, "two other cards were exiled this way");
            assert_eq!(
                cards.as_slice(),
                CODIE_FROZEN_MEMBERS.as_slice(),
                "the frozen member ids identify this exact board",
            );
            let library = state.players[player.0 as usize].library.len();
            (cards.clone(), library)
        }
        other => panic!("fixture must load parked on EffectZoneChoice, got {other:?}"),
    };

    // Reach-guard: BOTH frozen members are in Exile — the contradiction with the
    // prompt's advertised `Library` is the defect under test.
    for card_id in &cards {
        assert_eq!(
            state.objects[card_id].zone,
            Zone::Exile,
            "member {card_id:?} must be Exile-resident, or this board is not the wedge",
        );
    }
    assert!(
        baseline_library > 0,
        "library baseline must be non-zero, or the +1 delta below proves nothing",
    );

    // The wedge itself: before Part 1 this was 0.
    let actions = legal_actions_full(&state).0;
    assert!(
        !actions.is_empty(),
        "the wedged board must offer at least one legal action; 0 is the permanent wedge",
    );

    // Drive the prompt with a STEP budget — bounded termination in steps, with
    // no sleeps and no wall-clock reads.
    const STEP_BUDGET: usize = 64;
    let mut steps = 0;
    while matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }) && steps < STEP_BUDGET {
        let action = legal_actions_full(&state)
            .0
            .first()
            .expect("a non-wedged prompt always offers an action")
            .clone();
        let actor = *engine::game::turn_control::authorized_submitters(&state)
            .first()
            .expect("a parked prompt has an authorized submitter");
        engine::game::engine::apply_interaction(&mut state, actor, actor, action)
            .expect("the first legal action must be accepted by the reducer");
        steps += 1;
    }

    assert!(
        !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
        "the EffectZoneChoice must be left within {STEP_BUDGET} steps, took {steps}",
    );
    assert!(
        stuck_decision_diagnostic(&state).is_none(),
        "no player may be left stuck after the prompt clears",
    );
    assert_eq!(
        state.players[P0.0 as usize].library.len(),
        baseline_library + 1,
        "ONE of the two members is bottomed — the persisted ability's `count: Fixed(1)` \
         is a separate parser defect (universal-quantifier cardinality) that is NOT fixed \
         here, so this assertion deliberately pins currently-incorrect behaviour: Codie's \
         \"each other card\" should bottom BOTH. The stall is what this fix repairs.",
    );
}

/// Build a synthetic prompt in the shape a persisted save arrives in: members
/// resident in `Zone::Exile` while the prompt advertises `zone: Zone::Library`.
///
/// That mismatch is DELIBERATE. It reproduces the wedged-save shape and makes
/// the rows below discriminate on **Part 1 alone** — they never call the Part 2
/// producer, so reverting Part 2 cannot turn them green or red.
fn park_exile_resident_library_prompt(
    exiled: usize,
    library_position: LibraryPosition,
    library_seed: usize,
) -> (engine::game::scenario::GameRunner, Vec<ObjectId>, usize) {
    let mut scenario = GameScenario::new();
    for i in 0..library_seed {
        scenario.add_card_to_library_top(P0, &format!("Library Filler {i}"));
    }
    let members: Vec<ObjectId> = (0..exiled)
        .map(|i| {
            scenario
                .add_creature_to_exile(P0, &format!("Exiled This Way {i}"), 1, 1)
                .id()
        })
        .collect();

    let mut runner = scenario.build();
    let baseline = runner.state().players[P0.0 as usize].library.len();
    park_prompt(
        &mut runner,
        members.clone(),
        EffectKind::PutAtLibraryPosition,
        Zone::Library,
        Some(library_position),
    );
    (runner, members, baseline)
}

fn park_prompt(
    runner: &mut engine::game::scenario::GameRunner,
    cards: Vec<ObjectId>,
    effect_kind: EffectKind,
    zone: Zone,
    library_position: Option<LibraryPosition>,
) {
    runner.state_mut().waiting_for = WaitingFor::EffectZoneChoice {
        player: P0,
        cards,
        count: 1,
        min_count: 0,
        up_to: false,
        source_id: ObjectId(9_001),
        effect_kind,
        zone,
        destination: None,
        enter_tapped: EtbTapState::Unspecified,
        enter_transformed: false,
        enters_under_player: None,
        enters_attacking: false,
        owner_library: false,
        track_exiled_by_source: false,
        face_down_profile: None,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        count_param: 0,
        library_position,
        mass_library_order: None,
        is_cost_payment: false,
        enters_modified_if: None,
        duration: None,
    };
}

/// A2 — **`LibraryPosition::Top` relocates from Exile, to the right index.**
///
/// A1 covers `Bottom` on the real board; this row covers the other placement
/// arm on a constructed board, and asserts the resulting POSITION rather than
/// merely `is_ok()` — an accepted selection that placed the card at the wrong
/// end would still be a defect.
///
/// Revert-failing assertion: `result.is_ok()`. With the Part 1 guard restored to
/// `Some(Zone::Hand | Zone::Library)`, the Exile-resident member is refused and
/// `SelectCards` returns `Err("Selected card is no longer in Library")`.
#[test]
fn a2_exile_resident_member_relocates_to_library_top() {
    let (mut runner, members, baseline) =
        park_exile_resident_library_prompt(3, LibraryPosition::Top, 4);

    // Reach-guard: the prompt is parked in the wedged-save shape — members in
    // Exile, prompt claiming Library — so this row tests the guard, not setup.
    let chosen = members[1];
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, zone, .. } => {
            assert_eq!(*zone, Zone::Library, "prompt must claim Library");
            assert!(cards.contains(&chosen), "chosen member must be eligible");
        }
        other => panic!("expected a parked EffectZoneChoice, got {other:?}"),
    }
    assert_eq!(
        runner.state().objects[&chosen].zone,
        Zone::Exile,
        "the chosen member must be Exile-resident, or this row measures nothing",
    );
    assert!(baseline > 0, "library baseline must be non-zero");

    let result = runner.act(GameAction::SelectCards {
        cards: vec![chosen],
    });
    assert!(
        result.is_ok(),
        "an Exile-origin library relocation must be admitted, got {result:?}",
    );

    let library = &runner.state().players[P0.0 as usize].library;
    assert_eq!(
        library.len(),
        baseline + 1,
        "the chosen card must have joined the library",
    );
    assert_eq!(
        library.front().copied(),
        Some(chosen),
        "LibraryPosition::Top must place the card at index 0, not merely somewhere",
    );
}

/// A3 — **a Battlefield member is REFUSED.** This is the bound on the
/// exemption, and the most important new row here.
///
/// Admitting it would be rules-wrong, not merely surprising. Per CR 110.1 a
/// permanent is a card or token *on the battlefield* and stops being a permanent
/// as it moves to another zone, so its departure is a leaves-the-battlefield
/// event under CR 603.6c: it fires LTB triggers, severs the attachment graph,
/// and purges the trigger index. Silently threading it through a prompt whose
/// delivery treats it as a plain relocation would skip all of that.
///
/// The `Zone::Stack` sibling (CR 112.1 — a spell is a card *on the stack*, so
/// removing it would destroy a spell mid-resolution) IS constructible and is
/// covered below: `zones::move_to_zone` sets `obj.zone = Zone::Stack` and, per
/// its own `Zone::Stack => {}` arm, creates no `StackEntry`. That is sufficient,
/// because the guard reads exactly `state.objects[id].zone` and nothing else.
///
/// Revert-failing assertion: both `is_err()` assertions. They red if the
/// predicate is widened to admit `Battlefield`/`Stack` (i.e. if
/// `Zone::is_library_relocation_origin` stops being a bound and becomes a
/// blanket exemption).
#[test]
fn a3_battlefield_and_stack_members_are_refused() {
    for (blocked_zone, label) in [
        (
            Zone::Battlefield,
            "CR 110.1 + CR 603.6c leaves-the-battlefield",
        ),
        (Zone::Stack, "CR 112.1 spell-on-the-stack"),
    ] {
        let (mut runner, members, _) =
            park_exile_resident_library_prompt(3, LibraryPosition::Top, 4);
        let blocked = members[0];

        let mut events = vec![];
        zones::move_to_zone(runner.state_mut(), blocked, blocked_zone, &mut events);

        // Reach-guard: the blocked member IS in the eligible set, so the
        // expected `Err` cannot come from the membership check firing first, and
        // it IS in the zone under test.
        match &runner.state().waiting_for {
            WaitingFor::EffectZoneChoice { cards, .. } => assert!(
                cards.contains(&blocked),
                "{label}: member must be eligible, or the Err proves nothing",
            ),
            other => panic!("expected a parked EffectZoneChoice, got {other:?}"),
        }
        assert_eq!(
            runner.state().objects[&blocked].zone,
            blocked_zone,
            "{label}: member must actually be in {blocked_zone:?}",
        );

        let result = runner.act(GameAction::SelectCards {
            cards: vec![blocked],
        });
        assert!(
            result.is_err(),
            "{label}: a {blocked_zone:?} member must be REFUSED, not silently relocated; \
             got {result:?}",
        );
    }
}

/// A4 — **the exemption did not become universal across effect kinds.**
///
/// The relocation exemption is scoped to `EffectKind::PutAtLibraryPosition`. A
/// `Sacrifice` prompt whose member has left the advertised zone must still be
/// refused, even though the member's current zone (`Graveyard`) IS a member of
/// the relocation-origin set — so this row reds specifically if the
/// `matches!(effect_kind, EffectKind::PutAtLibraryPosition)` conjunct is dropped
/// and the zone predicate is applied to every effect kind.
///
/// The discriminator here is over-widening, not the revert: restoring the old
/// `Hand | Library` guard leaves this row green, by design.
#[test]
fn a4_sacrifice_prompt_still_rechecks_its_frozen_zone() {
    let mut scenario = GameScenario::new();
    let victim = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    let mut events = vec![];
    zones::move_to_zone(runner.state_mut(), victim, Zone::Graveyard, &mut events);
    park_prompt(
        &mut runner,
        vec![victim],
        EffectKind::Sacrifice,
        Zone::Battlefield,
        None,
    );

    // Reach-guard: the member IS eligible (so the Err below is the zone recheck,
    // whose message differs from the membership check's "not in eligible set"),
    // and its current zone is one the relocation predicate WOULD admit.
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, zone, .. } => {
            assert!(cards.contains(&victim), "member must be eligible");
            assert_eq!(*zone, Zone::Battlefield, "prompt froze Battlefield");
        }
        other => panic!("expected a parked EffectZoneChoice, got {other:?}"),
    }
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "member must have LEFT the frozen zone into a relocation-origin zone",
    );

    let result = runner.act(GameAction::SelectCards {
        cards: vec![victim],
    });
    let message = match result {
        Err(engine::game::engine::EngineError::InvalidAction(message)) => message,
        other => panic!("a departed Sacrifice member must be refused, got {other:?}"),
    };
    assert!(
        message.contains("Selected card is no longer in"),
        "must fail the ZONE RECHECK, not the membership check; got {message:?}",
    );
}
