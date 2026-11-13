//! CR 601.2c + CR 115.1 + CR 115.3 + CR 701.12a: a paired-subject spell declares
//! TWO slots with two DIFFERENT filters, and a restore must judge each recorded
//! fact against its OWN slot's requirement.
//!
//! Switcheroo — "Exchange control of two target creatures." — lowers to
//! `Effect::ExchangeControl`, which has no generic `target` field: both operands
//! are separate instances of the word "target", so the live declaration reads
//! `paired_subject_slot_filters` and `Effect::target_filter()` answers `None`.
//! The restore guard's filter recovery fell through to that generic extraction,
//! recovered NO filter for either slot, and therefore skipped BOTH declared
//! creature checks: a save that kept a fact's id, zone and announcement pin while
//! restating its captured types as a Land passed restore for either slot.
//!
//! Witnesses, all on the production path (legal cast, manual park during the mana
//! payment, save, restore):
//!
//! * the honest save is admitted and — after the payment — the exchange really
//!   happens, so the save is not merely tolerated;
//! * two independent negatives, each contradicting the CAPTURED type of one
//!   declared slot (slot 0 and, separately, slot 1) while leaving that fact's id,
//!   zone and incarnation untouched, are each refused with
//!   `SnapshotContradictsDeclaredFilter` naming that slot's target.
//!
//! CR set (each verified against `docs/MagicCompRules.txt`): CR 115.1 (targets
//! are declared as the spell is put on the stack), CR 115.3 (an object may be
//! chosen once for each instance of the word "target"), CR 601.2c (announcing
//! targets), CR 601.2h (costs are paid after targets are chosen), CR 701.12a
//! (exchange), CR 400.7 (a moved object is a new object).
//!
//! Oracle text is verbatim from Scryfall.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    ActivationTargetFact, AnnouncedTargetFactConflict, AnnouncedTargetZone, CastPaymentMode,
    PersistedGameState, PersistedRestoreError, PersistedRestoreFinalization, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim from Scryfall (`cards/named?exact=Switcheroo`, Magic 2013).
const SWITCHEROO: &str = "Exchange control of two target creatures.";

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

/// P0's 2/2 and P1's 3/3, with Switcheroo payable at a generic {1} and one
/// Mountain, cast with manual payment and BOTH declared slots answered in
/// declaration order.
fn parked_switcheroo() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "P0 Creature", 2, 2).id();
    let theirs = scenario.add_creature(P1, "P1 Creature", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Switcheroo", false, SWITCHEROO)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("cast");
    for (slot, target) in [(0usize, mine), (1usize, theirs)] {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { .. }
            ),
            "reach guard: slot {slot} must be open, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap_or_else(|error| panic!("slot {slot}: {target:?} must be admitted: {error:?}"));
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: parked for manual payment, got {:?}",
        runner.state().waiting_for
    );
    (runner, spell, mine, theirs)
}

/// Rewrite the CAPTURED types of the fact at `slot` to a Land — leaving that
/// fact's id, zone and announcement pin exactly as the honest capture wrote them.
/// Returns the state ready to save.
fn forge_captured_type(runner: &GameRunner, slot: usize, target: ObjectId) -> PersistedGameState {
    let mut forged = runner.state().clone();
    let pending = forged
        .pending_cast
        .as_deref_mut()
        .expect("the cast is parked");
    let facts = pending
        .committed_target_facts
        .as_mut()
        .expect("the latch is written");
    let ActivationTargetFact::Object { id, lki, zone, .. } = &mut facts[slot] else {
        panic!("slot {slot} holds an object fact");
    };
    assert_eq!(*id, target, "reach guard: the latch is positional");
    assert_eq!(
        *zone,
        AnnouncedTargetZone::Known(Zone::Battlefield),
        "reach guard: the capture states the announcement zone"
    );
    assert!(
        lki.card_types.contains(&CoreType::Creature),
        "reach guard: the capture states the creature type the slot's filter requires"
    );
    lki.card_types = vec![CoreType::Land];
    lki.subtypes = vec!["Forest".to_string()];
    PersistedGameState::capture(forged)
}

/// CR 701.12a + CR 601.2c + CR 115.3: the honest, unmodified save is admitted
/// and the resumed cast really exchanges control of BOTH announced creatures —
/// both declared slots survived with their own filters.
#[test]
fn switcheroo_save_restores_and_exchanges_both_announced_creatures() {
    let (runner, _spell, mine, theirs) = parked_switcheroo();
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
        vec![TargetRef::Object(mine), TargetRef::Object(theirs)],
        "both declared slots survived, in declaration order"
    );
    restored.act(GameAction::PassPriority).expect("pay");
    restored.advance_until_stack_empty();
    let state = restored.state();
    assert_eq!(
        state.objects[&mine].controller, P1,
        "CR 701.12a: the first announced creature changed controller"
    );
    assert_eq!(
        state.objects[&theirs].controller, P0,
        "CR 701.12a: the second announced creature changed controller"
    );
}

/// CR 601.2c + CR 115.1: slot 0's recorded fact contradicts the filter that
/// declared it — a Land where the declaration named a creature — so the save is
/// refused. Its id, zone and announcement pin are untouched, which is what makes
/// the refusal attributable to the type axis alone.
#[test]
fn switcheroo_save_whose_slot_zero_type_contradicts_the_declaration_is_refused() {
    let (runner, spell, mine, _theirs) = parked_switcheroo();
    try_restore(&persisted(&runner).to_string()).expect("the untouched save is admitted");

    let forged = forge_captured_type(&runner, 0, mine);
    let json = serde_json::to_string(&forged).expect("serializes");
    let refused = match try_restore(&json) {
        Ok(_) => panic!("a captured type that contradicts slot 0 must be refused"),
        Err(error) => error,
    };
    assert_eq!(
        refused,
        PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id: spell,
            reason: AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter {
                target: TargetRef::Object(mine),
            },
        }
    );
}

/// The same contradiction in the OTHER declared slot, independently: slot 1's
/// fact is the one that contradicts its own filter, so the refusal must name that
/// target rather than the first one.
#[test]
fn switcheroo_save_whose_slot_one_type_contradicts_the_declaration_is_refused() {
    let (runner, spell, _mine, theirs) = parked_switcheroo();
    try_restore(&persisted(&runner).to_string()).expect("the untouched save is admitted");

    let forged = forge_captured_type(&runner, 1, theirs);
    let json = serde_json::to_string(&forged).expect("serializes");
    let refused = match try_restore(&json) {
        Ok(_) => panic!("a captured type that contradicts slot 1 must be refused"),
        Err(error) => error,
    };
    assert_eq!(
        refused,
        PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id: spell,
            reason: AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter {
                target: TargetRef::Object(theirs),
            },
        }
    );
}
