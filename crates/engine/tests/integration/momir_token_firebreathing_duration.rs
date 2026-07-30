//! BUG B repro (HIGH FIDELITY): a Momir token COPY of a REAL transform back
//! face (`Insectile Aberration`) carrying "{1}: This creature gets +1/+0 until
//! end of turn" must NOT accumulate power across turns.
//!
//! The user insists the pump "stacks every turn" — a CROSS-TURN base-P/T creep
//! claim. The first investigation used a synthetic `Cost{3}` creature face and
//! a single `execute_cleanup` call, which cannot detect cross-turn creep. This
//! file closes three fidelity gaps:
//!
//!   1. Drives the REAL multi-phase turn boundary through `GameRunner`
//!      (PassPriority + the actual phase machinery, incl. the `Phase::Cleanup`
//!      arm of `advance_phase` at turns.rs:2016-2018), NOT a direct
//!      `execute_cleanup` call.
//!   2. Runs TWO consecutive turns for the SAME controller (turn N -> N+2),
//!      checking the token's BASE power is still its printed 3, never 4.
//!   3. Uses the REAL `Insectile Aberration` transform back face from the
//!      MTGJSON test fixture (a `layout: transform`, NoCost, 3/2 creature),
//!      built into a Momir token via `CreateTokenCopyFromPool`, so the
//!      transform/copy/`is_token` base-P/T storage path is exercised. The
//!      firebreathing ability is attached to that real face (the printed back
//!      face has no firebreathing) while keeping it a real NoCost transform
//!      face.
//!
//! CR 611.2a: a continuous effect from a resolved ability with "until end of
//! turn" lasts until the cleanup step.
//! CR 514.2: "until end of turn" effects end during the cleanup step.
//! CR 707.2: a copy uses the copiable values (incl. abilities) of the copied
//! object. CR 202.3b: a copy of a transform back face has mana value 0.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use engine::database::card_db::CardDatabase;
use engine::game::effects::create_token_copy_from_pool;
use engine::game::scenario::GameRunner;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, Comparator, Duration, Effect,
    PtValue, QuantityExpr, ResolvedAbility, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

fn fixture_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        CardDatabase::from_mtgjson(&data.join("mtgjson/test_fixture.json"))
            .expect("CardDatabase::from_mtgjson should succeed")
    })
}

/// The firebreathing ability: `{1}: This creature gets +1/+0 until end of turn.`
fn firebreathing_ability() -> AbilityDefinition {
    let mut ab = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Pump {
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(0),
            target: TargetFilter::SelfRef,
        },
    );
    ab.cost = Some(AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![],
            generic: 1,
        },
    });
    ab.duration = Some(Duration::UntilEndOfTurn);
    ab
}

/// The REAL `Insectile Aberration` transform back face from the fixture, with
/// the firebreathing ability attached. Verifies the face is a `NoCost` creature
/// (the transform-back signature) so the copy/transform path is genuinely
/// exercised.
fn insectile_aberration_firebreather() -> CardFace {
    let mut face = fixture_db()
        .get_face_by_name("Insectile Aberration")
        .expect("fixture must contain the transform back face 'Insectile Aberration'")
        .clone();
    assert!(
        matches!(face.mana_cost, ManaCost::NoCost),
        "Insectile Aberration is a transform BACK face with no castable mana cost"
    );
    assert!(
        face.card_type.core_types.contains(&CoreType::Creature),
        "Insectile Aberration must be a creature face"
    );
    // Printed base P/T from the fixture is 3/2.
    assert_eq!(
        face.power,
        Some(PtValue::Fixed(3)),
        "printed base power is 3"
    );
    face.abilities = vec![firebreathing_ability()];
    face
}

/// Momir state at precombat main with P0 active/priority and `mana` colorless
/// mana available to pay generic costs.
fn momir_state_with_mana(mana: u32) -> GameState {
    let mut state = GameState::new(FormatConfig::momir(), 2, 42);
    state.phase = Phase::PreCombatMain;
    state.turn_number = 2;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    // Seed both libraries so neither player loses to an empty-library draw
    // (CR 104.3a / CR 704.5c) while we cross two real turn boundaries.
    seed_library(&mut state, P0, 20);
    seed_library(&mut state, PlayerId(1), 20);
    fund(&mut state, mana);
    state
}

/// Put `n` disposable cards in `player`'s library so turn-based draws (CR 504.1)
/// across multiple turns never empty it.
fn seed_library(state: &mut GameState, player: PlayerId, n: u32) {
    for i in 0..n as u64 {
        engine::game::zones::create_object(
            state,
            engine::types::identifiers::CardId(10_000 + i + player.0 as u64 * 1000),
            player,
            "Plains".to_string(),
            engine::types::zones::Zone::Library,
        );
    }
}

fn fund(state: &mut GameState, mana: u32) {
    for _ in 0..mana {
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

/// Create a Momir token copy of the real transform back face via the REAL
/// `CreateTokenCopyFromPool` resolver. Returns the token id.
fn token_copy_of_transform_back(state: &mut GameState) -> ObjectId {
    let face = insectile_aberration_firebreather();
    let mut by_mv: BTreeMap<i32, Vec<String>> = BTreeMap::new();
    // CR 202.3b: a transform back-face copy has mana value 0 — it keys at 0.
    by_mv.insert(0, vec![face.name.clone()]);
    let mut faces = HashMap::new();
    faces.insert(face.name.to_lowercase(), face);
    state.momir_pool = by_mv;
    state.momir_pool_faces = Arc::new(faces);

    let effect = Effect::CreateTokenCopyFromPool {
        owner: TargetFilter::Controller,
        type_filter: TargetFilter::Any,
        mv: Comparator::EQ,
        mv_bound: QuantityExpr::Fixed { value: 0 },
        selection: CardSelectionMode::Random,
        count: QuantityExpr::Fixed { value: 1 },
        tapped: false,
        enters_attacking: false,
    };
    let ability = ResolvedAbility::new(effect, vec![], ObjectId(500), P0);
    let mut events = Vec::new();
    create_token_copy_from_pool::resolve(state, &ability, &mut events)
        .expect("token copy from pool must resolve");

    state
        .battlefield
        .iter()
        .copied()
        .find(|id| state.objects.get(id).is_some_and(|o| o.is_token))
        .expect("a firebreather token must exist on the battlefield")
}

fn firebreathing_index(state: &GameState, obj: ObjectId) -> usize {
    state.objects[&obj]
        .abilities
        .iter()
        .position(|a| matches!(a.kind, AbilityKind::Activated))
        .expect("the object must carry an activated firebreathing ability")
}

/// Activate the `{1}` firebreathing ability through the REAL activation -> stack
/// -> resolve pipeline. Funds {1} immediately before so mana is always present.
fn activate_firebreathing(runner: &mut GameRunner, obj: ObjectId, ability_index: usize) {
    fund(runner.state_mut(), 1);
    runner
        .act(GameAction::ActivateAbility {
            source_id: obj,
            ability_index,
        })
        .expect("activating firebreathing must be accepted");

    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing mana payment must be accepted");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected WaitingFor during activation: {other:?}"),
        }
    }
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority to resolve firebreathing must be accepted");
    }
}

/// Drive the engine through the REAL phase machinery until `state.turn_number`
/// reaches `target_turn`, crossing End -> Cleanup -> next turn(s) exactly as the
/// live SP-vs-AI driver does. Declares no attackers/blockers, drains trigger
/// ordering, answers any cleanup discard with no cards. Bounded to guard stalls.
fn advance_to_turn(runner: &mut GameRunner, target_turn: u32) {
    for _ in 0..400 {
        if runner.state().turn_number >= target_turn {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declaring no attackers must be accepted");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declaring no blockers must be accepted");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DiscardChoice { .. } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .expect("no-op cleanup discard must be accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected WaitingFor while advancing turns: {other:?}"),
        }
    }
    panic!(
        "failed to reach turn {target_turn} (stuck at turn {} phase {:?})",
        runner.state().turn_number,
        runner.state().phase
    );
}

fn power_of(runner: &GameRunner, obj: ObjectId) -> Option<i32> {
    runner.state().objects[&obj].power
}

/// BUG B — CROSS-TURN CREEP TEST (transform token, two real turns).
///
/// On P0's turn N: activate `{1}` once -> 4/2. Advance through the REAL turn
/// boundary (End -> Cleanup -> P1's turn -> back to P0's turn N+2). Assert the
/// token's power is back to its printed BASE 3, NOT 4. If it is 4 (or higher),
/// the pump persisted/accumulated across the turn boundary for a transform
/// token copy — the user's reported bug (P2). If it is 3, no cross-turn creep.
#[test]
fn transform_token_firebreathing_no_cross_turn_creep() {
    let mut state = momir_state_with_mana(0);
    let token = token_copy_of_transform_back(&mut state);
    let start_turn = state.turn_number;
    let idx = firebreathing_index(&state, token);

    let mut runner = GameRunner::from_state(state);
    assert_eq!(
        power_of(&runner, token),
        Some(3),
        "transform token printed base power is 3"
    );

    // Turn N: one firebreathing activation.
    activate_firebreathing(&mut runner, token, idx);
    assert_eq!(
        power_of(&runner, token),
        Some(4),
        "after one {{1}} the token is 4/2 during turn N"
    );

    // Drive the REAL multi-phase boundary back to P0's NEXT turn (N+2 turn
    // numbers later: P0 -> P1 -> P0).
    advance_to_turn(&mut runner, start_turn + 2);
    assert_eq!(
        runner.state().active_player,
        P0,
        "we must be back on P0's turn"
    );
    assert!(
        runner.state().objects.contains_key(&token),
        "the token must still exist on P0's next turn"
    );

    assert_eq!(
        power_of(&runner, token),
        Some(3),
        "CR 514.2: the transform token's firebreathing pump from turn N MUST be \
         gone on P0's next turn — base power back to 3. If this is 4+, the pump \
         persists/creeps across the turn boundary (BUG B = P2, real bug)."
    );
}

/// BUG B — MULTI-ACTIVATION ACROSS TWO TURNS. Activate on turn N (->4/2),
/// cross to P0's next turn (->should be 3/2), activate again (->should be 4/2,
/// NOT 5/2). Catches "stacks every turn" cumulative base creep directly.
#[test]
fn transform_token_firebreathing_reactivate_next_turn_does_not_stack() {
    let mut state = momir_state_with_mana(0);
    let token = token_copy_of_transform_back(&mut state);
    let start_turn = state.turn_number;
    let idx = firebreathing_index(&state, token);

    let mut runner = GameRunner::from_state(state);

    activate_firebreathing(&mut runner, token, idx);
    assert_eq!(
        power_of(&runner, token),
        Some(4),
        "turn N: 4/2 after one {{1}}"
    );

    advance_to_turn(&mut runner, start_turn + 2);
    assert_eq!(
        power_of(&runner, token),
        Some(3),
        "turn N+2: pump from turn N must have worn off (3/2)"
    );

    // Reactivate on the new turn.
    activate_firebreathing(&mut runner, token, idx);
    assert_eq!(
        power_of(&runner, token),
        Some(4),
        "turn N+2: ONE {{1}} this turn yields 4/2 — NOT 5/2. If 5+, prior-turn \
         pumps accumulated into the base (the 'stacks every turn' bug)."
    );
}

/// CONTROL — within-turn stacking still reverts at the real turn boundary.
/// Three activations in turn N (-> 6/2), cross to P0's next turn -> 3/2.
#[test]
fn transform_token_firebreathing_within_turn_stack_clears_at_boundary() {
    let mut state = momir_state_with_mana(0);
    let token = token_copy_of_transform_back(&mut state);
    let start_turn = state.turn_number;
    let idx = firebreathing_index(&state, token);

    let mut runner = GameRunner::from_state(state);
    for _ in 0..3 {
        activate_firebreathing(&mut runner, token, idx);
    }
    assert_eq!(
        power_of(&runner, token),
        Some(6),
        "three {{1}} activations in one turn stack to 6/2"
    );

    advance_to_turn(&mut runner, start_turn + 2);
    assert_eq!(
        power_of(&runner, token),
        Some(3),
        "CR 514.2: all three within-turn pumps clear at the real cleanup step"
    );
}
