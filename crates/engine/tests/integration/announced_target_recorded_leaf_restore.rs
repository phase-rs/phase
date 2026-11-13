//! CR 601.2c + CR 115.1 + CR 608.2h: the restore guard reads the CAPTURED
//! announcement facts for a declared filter leaf the zone-change record cannot
//! answer — and answers UNKNOWN where no capture states the fact at all, rather
//! than reporting the record's fail-closed `false` as a contradiction.
//!
//! Two real cards name a leaf whose live matcher reads the board:
//!
//! * Cradle to Grave — "Destroy target nonblack creature that entered this turn."
//!   The entry turn lives on the battlefield object
//!   (`FilterProp::EnteredThisTurn` is `obj.entered_battlefield_turn ==
//!   Some(state.turn_number)`), and an announcement `LKISnapshot` captures no
//!   entry turn. The record walk's fail-closed group answers a definite `false`,
//!   which `reject_contradictory_announced_target_facts` refuses as
//!   `SnapshotContradictsDeclaredFilter` on a perfectly honest save.
//! * Heartless Act — "Choose one —\n• Destroy target creature with no counters on
//!   it.\n• Remove up to three counters from target creature." The review reported
//!   this leaf as the same class: the counter set IS captured into the
//!   announcement snapshot (`LKISnapshot::counters`), but the record walk's
//!   `Counters` arm reads `state.lki_cache`, which has no row for a target that
//!   never departed. MEASURED at the reviewed head (3036dfd62): the honest save is
//!   already ADMITTED — the record walk's own epoch arm answers that leaf from the
//!   capture — so this half is not a live defect. Its witnesses therefore pin both
//!   directions instead of repairing one: the honest save stays admitted, and a
//!   save whose fact states a counter the slot forbids is refused.
//!
//! Both witnesses drive the production path end to end: a legal cast, parked at
//! manual mana payment (after CR 601.2c has declared the targets and before
//! CR 601.2h pays), saved through `PersistedGameState::capture`, restored through
//! `prepare_for_restore`, then paid and resolved. Each board carries TWO eligible
//! creatures and one the leaf must exclude, so the offer itself is the reach
//! guard: two legal choices keep the engine's single-assignment auto-selection
//! out of the way, and the excluded creature is what proves the leaf is live in
//! the announced filter.
//!
//! CR set (each verified against `docs/MagicCompRules.txt`): CR 115.1 (targets
//! are declared as the spell is put on the stack), CR 601.2c (announcing
//! targets), CR 601.2h (costs are paid after targets are chosen), CR 608.2h (last
//! known information), CR 400.7 (a moved object is a new object), CR 122.1
//! (a counter is a marker placed on an object; counters are not objects and
//! have no characteristics).
//!
//! Oracle text is verbatim from Scryfall.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    ActivationTargetFact, AnnouncedTargetFactConflict, CastPaymentMode, PersistedGameState,
    PersistedRestoreError, PersistedRestoreFinalization, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::CounterType;

/// Verbatim from Scryfall (`cards/named?exact=Cradle%20to%20Grave`, Planar Chaos).
const CRADLE_TO_GRAVE: &str = "Destroy target nonblack creature that entered this turn.";

/// Verbatim from Scryfall (`cards/named?exact=Heartless%20Act`, Ikoria: Lair of
/// Behemoths).
const HEARTLESS_ACT: &str = "Choose one —\n• Destroy target creature with no counters on it.\n• Remove up to three counters from target creature.";

/// Restore through the engine's own save pipeline, reporting a refusal instead of
/// panicking on it.
fn try_restore(json: &str) -> Result<GameRunner, PersistedRestoreError> {
    let state = serde_json::from_str::<PersistedGameState>(json)
        .expect("persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)?;
    Ok(GameRunner::from_state(
        state
            .finalize_after_rehydration(|_| Ok(()))
            .expect("restored state is publishable"),
    ))
}

fn persisted(runner: &GameRunner) -> serde_json::Value {
    serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
        .expect("state serializes")
}

/// A payable main-phase board: `oracle` in P0's hand at a generic {1}, one
/// Mountain untapped, and the named 2/2 creatures on P1's battlefield in order.
fn board(oracle: &str, name: &str, creatures: &[&str]) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ids = creatures
        .iter()
        .map(|label| scenario.add_creature(P1, label, 2, 2).id())
        .collect::<Vec<_>>();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, true, oracle)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);
    (scenario.build(), spell, ids)
}

/// CR 601.2c: the legal targets the open slot offers, as sorted object ids.
fn offered_objects(runner: &GameRunner) -> Vec<ObjectId> {
    let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
        panic!(
            "expected the target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    let mut ids: Vec<ObjectId> = selection
        .current_legal_targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    ids.sort();
    ids
}

/// CR 601.2h: cast with manual payment and (for a modal card) choose `mode`, so
/// the cast is parked waiting for mana with CR 601.2c's announcement latched.
fn cast_to_payment(runner: &mut GameRunner, spell: ObjectId, mode: Option<usize>) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("cast");
    if let Some(mode) = mode {
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ModeChoice { .. }),
            "reach guard: the modal card asks for its mode, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::SelectModes {
                indices: vec![mode],
            })
            .expect("the mode is selectable");
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach guard: the declared slot is open, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 601.2c + CR 115.1: Cradle to Grave's "that entered this turn" is a leaf an
/// announcement snapshot cannot state, so the guard must answer UNKNOWN instead
/// of refusing the save. The live filter really does read the entry turn — the
/// creature that did not enter this turn is kept out of the offer — and the save
/// is taken mid-cast, after the target was declared.
#[test]
fn cradle_to_grave_save_preserves_a_legal_same_turn_entrant() {
    let (mut runner, spell, creatures) = board(
        CRADLE_TO_GRAVE,
        "Cradle to Grave",
        &["Fresh Recruit", "Fresh Recruit B", "Old Guard"],
    );
    let (fresh, fresh_b, old) = (creatures[0], creatures[1], creatures[2]);
    // CR 400.7: the live filter asks `entered_battlefield_turn == turn_number`.
    // The scenario builder places creatures as pre-existing (entered on a prior
    // turn), so the two entrants under test are dated to THIS turn and the third
    // creature is not — the offer below is what proves the difference is live.
    let turn = runner.state().turn_number;
    for id in [fresh, fresh_b] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .expect("the entrant row")
            .entered_battlefield_turn = Some(turn);
    }
    assert_eq!(
        runner.state().objects[&old].entered_battlefield_turn,
        Some(turn.saturating_sub(1)),
        "reach guard: the control creature did not enter this turn"
    );

    cast_to_payment(&mut runner, spell, None);
    let mut expected = vec![fresh, fresh_b];
    expected.sort();
    assert_eq!(
        offered_objects(&runner),
        expected,
        "the announced slot admits exactly the creatures that entered this turn"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(fresh)),
        })
        .expect("the entrant is admitted");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: parked for manual payment, got {:?}",
        runner.state().waiting_for
    );

    let save = persisted(&runner).to_string();
    let mut restored = try_restore(&save).expect("the honest save restores");
    assert!(
        matches!(restored.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "the restored cast resumes at its own payment: {:?}",
        restored.state().waiting_for
    );
    let pending = restored
        .state()
        .pending_cast
        .as_deref()
        .expect("the restored cast is still parked");
    assert_eq!(
        engine::game::ability_utils::declared_targets_in_chain(&pending.ability),
        vec![TargetRef::Object(fresh)],
        "the entrant is still the announced target"
    );
    restored.act(GameAction::PassPriority).expect("pay");
    restored.advance_until_stack_empty();
    assert_eq!(
        restored.state().objects[&fresh].zone,
        Zone::Graveyard,
        "the resumed cast destroyed the announced entrant"
    );
    for survivor in [fresh_b, old] {
        assert_eq!(
            restored.state().objects[&survivor].zone,
            Zone::Battlefield,
            "only the announced entrant was a target"
        );
    }
}

/// CR 122.1 + CR 601.2c: Heartless Act's "creature with no counters on it" is a
/// leaf the announcement snapshot DOES state (`LKISnapshot::counters`), so the
/// guard must read the captured set instead of the ObjectId-keyed exit cache,
/// which holds no row for a target that never departed. The creature carrying a
/// counter is what proves the predicate is live.
#[test]
fn heartless_act_save_preserves_a_legal_counterless_creature() {
    let (mut runner, spell, creatures) = board(
        HEARTLESS_ACT,
        "Heartless Act",
        &["Bare Creature", "Bare Creature B", "Loaded Creature"],
    );
    let (bare, bare_b, loaded) = (creatures[0], creatures[1], creatures[2]);
    runner
        .state_mut()
        .objects
        .get_mut(&loaded)
        .expect("the control row")
        .counters
        .insert(CounterType::Plus1Plus1, 1);

    cast_to_payment(&mut runner, spell, Some(0));
    let mut expected = vec![bare, bare_b];
    expected.sort();
    assert_eq!(
        offered_objects(&runner),
        expected,
        "the announced slot admits exactly the creatures with no counters on them"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bare)),
        })
        .expect("the counterless creature is admitted");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: parked for manual payment, got {:?}",
        runner.state().waiting_for
    );

    let save = persisted(&runner).to_string();
    let mut restored = try_restore(&save).expect("the honest save restores");
    assert!(
        matches!(restored.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "the restored cast resumes at its own payment: {:?}",
        restored.state().waiting_for
    );
    restored.act(GameAction::PassPriority).expect("pay");
    restored.advance_until_stack_empty();
    assert_eq!(
        restored.state().objects[&bare].zone,
        Zone::Graveyard,
        "the resumed cast destroyed the announced creature"
    );
    for survivor in [bare_b, loaded] {
        assert_eq!(
            restored.state().objects[&survivor].zone,
            Zone::Battlefield,
            "only the announced creature was a target"
        );
    }
}

/// CR 122.1 + CR 601.2c: the discriminating twin for Heartless Act. At the
/// reviewed head the honest save above is already admitted — the counter leaf is
/// answered from the CAPTURED snapshot by
/// `zone_change_record_matches_property`'s epoch arm, so this half of the finding
/// is not a live defect. What the twin pins is that the axis is genuinely read
/// from the capture rather than silently skipped: a save whose fact states a
/// counter the slot's "no counters on it" requirement forbids is refused.
#[test]
fn heartless_act_save_whose_recorded_counters_contradict_the_slot_is_refused() {
    let (mut runner, spell, creatures) = board(
        HEARTLESS_ACT,
        "Heartless Act",
        &["Bare Creature", "Bare Creature B", "Loaded Creature"],
    );
    let bare = creatures[0];
    cast_to_payment(&mut runner, spell, Some(0));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bare)),
        })
        .expect("the counterless creature is admitted");
    try_restore(&persisted(&runner).to_string()).expect("the untouched save is admitted");

    let mut forged = runner.state().clone();
    let pending = forged
        .pending_cast
        .as_deref_mut()
        .expect("the cast is parked");
    let facts = pending
        .committed_target_facts
        .as_mut()
        .expect("the latch is written");
    let ActivationTargetFact::Object { id, lki, .. } = &mut facts[0] else {
        panic!("the announced target is an object fact");
    };
    assert_eq!(*id, bare, "reach guard: the latch is positional");
    assert!(
        lki.counters.is_empty(),
        "reach guard: the honest capture states no counters on the announced creature"
    );
    lki.counters.insert(CounterType::Plus1Plus1, 1);

    let json = serde_json::to_string(&PersistedGameState::capture(forged)).expect("serializes");
    let refused = match try_restore(&json) {
        Ok(_) => panic!("a recorded counter must contradict the slot's no-counters requirement"),
        Err(error) => error,
    };
    assert_eq!(
        refused,
        PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id: spell,
            reason: AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter {
                target: TargetRef::Object(bare),
            },
        }
    );
}
