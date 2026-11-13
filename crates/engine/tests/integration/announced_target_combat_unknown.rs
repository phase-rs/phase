//! CR 601.2c + CR 508.1k: an announced target's recorded fact carries the
//! target's public characteristics, the zone it was announced in and the token
//! bit it then held. It carries NO combat role: `snapshot_public_characteristics`
//! captures an `LKISnapshot`, and an `LKISnapshot` has no attacking field.
//!
//! Kill Shot ("Destroy target attacking creature.") announces a target whose
//! slot filter asks `FilterProp::Attacking`. At manual mana payment the cast
//! holds the announced-target latch, the target is still the attacking
//! creature, and the live filter answers `Attacking` from `state.combat`. The
//! restore guard has only the recorded fact, so a projection that answers
//! `Attacking` from a zeroed `ZoneChangeCombatStatus` reports a fact the save
//! never stated as FALSE and refuses an untouched save.
//!
//! These tests pin both directions of the rule the maintainer asked for: a
//! missing recorded fact is not a failed predicate (the untouched save is
//! admitted), while a recorded predicate that is genuinely contradicted (a
//! recorded noncreature for a "creature" requirement) is still refused.

use engine::ai_support::legal_actions_full;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{FilterProp, TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    ActivationTargetFact, AnnouncedTargetFactConflict, CastPaymentMode, PersistedGameState,
    PersistedRestoreError, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall): Kill Shot, an instant.
const KILL_SHOT: &str = "Destroy target attacking creature.";

struct ParkedKillShot {
    runner: GameRunner,
    spell: ObjectId,
    attacker: ObjectId,
    plains: ObjectId,
}

/// P1 is the active player attacking P0, so the board sits in the
/// declare-attackers step (CR 508.1) with P1's creature attacking P0. P0 casts
/// Kill Shot at that attacker with `CastPaymentMode::Manual`, which stops at
/// `WaitingFor::ManaPayment` after the announcement (CR 601.2h) and no later.
fn park_kill_shot_at_manual_payment() -> ParkedKillShot {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P1, "Raider", 2, 2).id();
    // White source for Kill Shot's {W}.
    let plains = scenario.add_basic_land(P0, ManaColor::White);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kill Shot", true, KILL_SHOT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("P1 declares its attacker at P0");
    // CR 508.2: the active player receives priority after attackers are
    // declared; P1 passes so the defending player can respond (CR 117.3d).
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "reach-guard: P0 holds priority with the attacker declared"
    );

    let card_id = runner.state().objects[&spell].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![attacker],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the manual Kill Shot cast is announced at the attacker");
    assert!(
        matches!(cast.waiting_for, WaitingFor::ManaPayment { player: P0, .. }),
        "reach-guard: the cast parks at manual payment, got {:?}",
        cast.waiting_for
    );
    ParkedKillShot {
        runner,
        spell,
        attacker,
        plains,
    }
}

/// The one object fact the parked cast's announcement latch carries.
fn parked_latch_fact(runner: &GameRunner) -> &ActivationTargetFact {
    let pending = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach-guard: the manual payment park keeps its pending cast");
    let facts = pending
        .committed_target_facts
        .as_deref()
        .expect("reach-guard: the announcement latch is populated at the payment park");
    assert_eq!(
        facts.len(),
        1,
        "reach-guard: Kill Shot declares exactly one target"
    );
    &facts[0]
}

/// Save the parked cast through the authoritative capture and read it back.
fn save_and_restore(
    runner: &GameRunner,
) -> Result<engine::types::game_state::GameState, PersistedRestoreError> {
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the parked Kill Shot cast serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the parked Kill Shot cast deserializes");
    restored.into_game_state()
}

/// REPRODUCTION + WITNESS: an UNTOUCHED save of a Kill Shot cast announced at an
/// attacking creature restores and resumes.
#[test]
fn untouched_kill_shot_save_restores_and_resumes() {
    let ParkedKillShot {
        runner,
        spell,
        attacker,
        plains,
    } = park_kill_shot_at_manual_payment();

    // THE DECLARING FILTER: the slot Kill Shot announced into reads combat role,
    // so this witness exercises the predicate the announcement never records.
    let declared = runner.state().objects[&spell]
        .abilities
        .iter()
        .find_map(|ability| ability.effect.target_filter())
        .expect("reach-guard: Kill Shot declares a target slot");
    assert!(
        matches!(
            declared,
            TargetFilter::Typed(TypedFilter { properties, .. })
                if properties.contains(&FilterProp::Attacking { defender: None })
        ),
        "reach-guard: the declaring slot filter asks for an attacking creature, got {declared:?}"
    );

    // The reach guard: the announcement latch IS populated, and it states the
    // two announcement-time axes it records — the zone and the token bit.
    match parked_latch_fact(&runner) {
        ActivationTargetFact::Object {
            id, zone, token, ..
        } => {
            assert_eq!(*id, attacker, "the latch names the announced attacker");
            assert_eq!(
                *zone,
                engine::types::game_state::AnnouncedTargetZone::Known(Zone::Battlefield),
                "CR 601.2c: the target was announced on the battlefield"
            );
            assert_eq!(
                *token,
                engine::types::game_state::CapturedTokenBit::Known(false),
                "CR 111.1: the recorded token bit says the target was no token"
            );
        }
        other => panic!("reach-guard: Kill Shot's declared target is an object, got {other:?}"),
    }
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == attacker)),
        "reach-guard: the live board still records the attacker"
    );

    let restored = save_and_restore(&runner)
        .expect("CR 508.1k: an untouched Kill Shot save is admitted, not refused");
    let mut runner = GameRunner::from_state(restored);

    // Resume: pay {W} from the Plains, finalize, and the spell destroys the
    // creature it announced.
    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&plains)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the restored payment prompt offers the Plains' own {T}: Add {W}");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("the restored cast takes the mana it announced it would");
    runner
        .act(GameAction::PassPriority)
        .expect("the restored manual payment finalizes");
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "reach-guard: the resumed cast reached the stack"
    );

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "the spell resolved");
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Graveyard,
        "CR 701.8a: the resumed Kill Shot destroys the announced attacker"
    );
}

/// THE DISCRIMINATING TWIN: the same save with the recorded snapshot restated as
/// a NONCREATURE is refused. This predicate IS recorded — `lki.card_types` is
/// part of the announcement — so the guard must still judge it, and an
/// unrecorded combat role must not blanket-accept a contradiction.
#[test]
fn restore_refuses_a_recorded_snapshot_that_contradicts_the_declared_filter() {
    let ParkedKillShot {
        runner,
        spell,
        attacker,
        ..
    } = park_kill_shot_at_manual_payment();

    let mut state = runner.state().clone();
    let pending = state
        .pending_cast
        .as_mut()
        .expect("reach-guard: the manual payment park keeps its pending cast");
    let facts = pending
        .committed_target_facts
        .as_deref_mut()
        .expect("reach-guard: the announcement latch is populated");
    match &mut facts[0] {
        ActivationTargetFact::Object { id, lki, .. } => {
            assert_eq!(
                *id, attacker,
                "reach-guard: the fact is the announced attacker"
            );
            assert!(
                lki.card_types
                    .contains(&engine::types::card_type::CoreType::Creature),
                "reach-guard: the recorded snapshot says creature before the restatement"
            );
            lki.card_types
                .retain(|card_type| *card_type != engine::types::card_type::CoreType::Creature);
        }
        other => panic!("reach-guard: the fact is an object fact, got {other:?}"),
    }

    let error = PersistedGameState::capture(state)
        .into_game_state()
        .expect_err("a recorded noncreature cannot satisfy Kill Shot's creature requirement");
    match error {
        PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason,
        } => {
            assert_eq!(
                spell_object_id, spell,
                "the refusal names the cast it is about"
            );
            match reason {
                AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter { target } => {
                    assert_eq!(
                        target,
                        engine::types::ability::TargetRef::Object(attacker),
                        "and the target whose recorded announcement contradicts its own slot filter"
                    );
                }
                other => panic!(
                    "expected the declared-filter refusal that reads the recorded snapshot, got {other:?}"
                ),
            }
        }
        other => panic!("expected a recorded-announcement refusal, got {other:?}"),
    }
}
