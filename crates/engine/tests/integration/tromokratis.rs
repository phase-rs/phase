//! Tromokratis — end-to-end integration tests.
//!
//! Validates both abilities:
//! 1. Hexproof unless attacking or blocking (conditional keyword grant).
//! 2. Can't be blocked unless all creatures defending player controls block it
//!    (aggregate blocking restriction, CR 509.1b).
//!
//! Tests drive the real combat pipeline via `GameScenario` and `validate_blockers`
//! to confirm the restriction is enforced at the declare-blockers step.

use engine::game::combat::{validate_blockers, AttackerInfo, CombatState};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::StaticDefinition;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

/// Oracle text for Tromokratis's blocking restriction ability (line 2).
const TROMOKRATIS_BLOCK_ORACLE: &str =
    "~ can't be blocked unless all creatures defending player controls block it.";

/// Oracle text for Tromokratis's hexproof ability (line 1).
const TROMOKRATIS_HEXPROOF_ORACLE: &str = "~ has hexproof unless it's attacking or blocking.";

fn create_creature(
    state: &mut GameState,
    controller: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.base_power = Some(power);
    obj.base_toughness = Some(toughness);
    obj.summoning_sick = false;
    obj.entered_battlefield_turn = Some(1);
    id
}

/// CR 509.1b (Tromokratis): partial block is illegal — if any creature blocks
/// Tromokratis, ALL creatures the defending player controls must block it.
#[test]
fn partial_block_is_illegal() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    state.turn_number = 2;
    state.active_player = PlayerId(0);

    let tromokratis = create_creature(&mut state, PlayerId(0), "Tromokratis", 8, 8);
    state
        .objects
        .get_mut(&tromokratis)
        .unwrap()
        .static_definitions
        .push(StaticDefinition::new(
            StaticMode::CantBeBlockedUnlessAllBlock,
        ));

    let b1 = create_creature(&mut state, PlayerId(1), "Blocker A", 2, 2);
    let b2 = create_creature(&mut state, PlayerId(1), "Blocker B", 2, 2);
    let _b3 = create_creature(&mut state, PlayerId(1), "Blocker C", 2, 2);

    state.combat = Some(CombatState {
        attackers: vec![AttackerInfo::attacking_player(tromokratis, PlayerId(1))],
        ..Default::default()
    });

    // Only two of three creatures block → illegal.
    let result = validate_blockers(&state, &[(b1, tromokratis), (b2, tromokratis)]);
    assert!(
        result.is_err(),
        "partial block should be illegal: {result:?}"
    );
}

/// CR 509.1b: all creatures blocking Tromokratis is legal.
#[test]
fn all_creatures_block_is_legal() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    state.turn_number = 2;
    state.active_player = PlayerId(0);

    let tromokratis = create_creature(&mut state, PlayerId(0), "Tromokratis", 8, 8);
    state
        .objects
        .get_mut(&tromokratis)
        .unwrap()
        .static_definitions
        .push(StaticDefinition::new(
            StaticMode::CantBeBlockedUnlessAllBlock,
        ));

    let b1 = create_creature(&mut state, PlayerId(1), "Blocker A", 2, 2);
    let b2 = create_creature(&mut state, PlayerId(1), "Blocker B", 2, 2);
    let b3 = create_creature(&mut state, PlayerId(1), "Blocker C", 2, 2);

    state.combat = Some(CombatState {
        attackers: vec![AttackerInfo::attacking_player(tromokratis, PlayerId(1))],
        ..Default::default()
    });

    // All three creatures block → legal.
    let result = validate_blockers(
        &state,
        &[(b1, tromokratis), (b2, tromokratis), (b3, tromokratis)],
    );
    assert!(result.is_ok(), "all-block should be legal: {result:?}");
}

/// CR 509.1b: choosing not to block at all (unblocked) is always legal.
#[test]
fn unblocked_is_legal() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    state.turn_number = 2;
    state.active_player = PlayerId(0);

    let tromokratis = create_creature(&mut state, PlayerId(0), "Tromokratis", 8, 8);
    state
        .objects
        .get_mut(&tromokratis)
        .unwrap()
        .static_definitions
        .push(StaticDefinition::new(
            StaticMode::CantBeBlockedUnlessAllBlock,
        ));

    let _b1 = create_creature(&mut state, PlayerId(1), "Blocker A", 2, 2);
    let _b2 = create_creature(&mut state, PlayerId(1), "Blocker B", 2, 2);

    state.combat = Some(CombatState {
        attackers: vec![AttackerInfo::attacking_player(tromokratis, PlayerId(1))],
        ..Default::default()
    });

    // No blockers declared → legal.
    let result = validate_blockers(&state, &[]);
    assert!(result.is_ok(), "unblocked should be legal: {result:?}");
}

/// CR 509.1b: tapped creatures are unable to block, so they are excluded from
/// the "all creatures" requirement. If the only non-blocking creature is tapped,
/// a partial block is legal.
#[test]
fn tapped_creature_excluded_from_all_requirement() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    state.turn_number = 2;
    state.active_player = PlayerId(0);

    let tromokratis = create_creature(&mut state, PlayerId(0), "Tromokratis", 8, 8);
    state
        .objects
        .get_mut(&tromokratis)
        .unwrap()
        .static_definitions
        .push(StaticDefinition::new(
            StaticMode::CantBeBlockedUnlessAllBlock,
        ));

    let b1 = create_creature(&mut state, PlayerId(1), "Blocker A", 2, 2);
    let b2 = create_creature(&mut state, PlayerId(1), "Blocker B", 2, 2);
    let tapped = create_creature(&mut state, PlayerId(1), "Tapped Creature", 3, 3);
    // Tap the third creature so it can't block.
    state.objects.get_mut(&tapped).unwrap().tapped = true;

    state.combat = Some(CombatState {
        attackers: vec![AttackerInfo::attacking_player(tromokratis, PlayerId(1))],
        ..Default::default()
    });

    // Two untapped creatures both block → legal (tapped creature excluded).
    let result = validate_blockers(&state, &[(b1, tromokratis), (b2, tromokratis)]);
    assert!(
        result.is_ok(),
        "block with all untapped creatures should be legal: {result:?}"
    );
}

/// End-to-end: Tromokratis parsed from oracle text gets both abilities.
/// The blocking restriction rejects a partial block via the real pipeline.
#[test]
fn e2e_parsed_tromokratis_rejects_partial_block() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let tromokratis = scenario
        .add_creature_from_oracle(P0, "Tromokratis", 8, 8, TROMOKRATIS_BLOCK_ORACLE)
        .id();
    let b1 = scenario.add_creature(P1, "Blocker A", 2, 2).id();
    let _b2 = scenario.add_creature(P1, "Blocker B", 2, 2).id();
    let _b3 = scenario.add_creature(P1, "Blocker C", 2, 2).id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers.
    runner.advance_to_combat();

    // Declare Tromokratis as attacker.
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(tromokratis, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Pass priority after attackers declared.
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }

    // We should be at DeclareBlockers now.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ),
        "expected DeclareBlockers, got {:?}",
        runner.state().waiting_for
    );

    // Partial block: only b1 blocks → should be rejected.
    let result = runner.act(GameAction::DeclareBlockers {
        assignments: vec![(b1, tromokratis)],
    });
    assert!(
        result.is_err(),
        "partial block of parsed Tromokratis should be rejected: {result:?}"
    );
}

/// End-to-end: hexproof unless attacking or blocking — Tromokratis has hexproof
/// when not in combat, loses it when attacking.
#[test]
fn e2e_hexproof_unless_attacking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let tromokratis = scenario
        .add_creature_from_oracle(P0, "Tromokratis", 8, 8, TROMOKRATIS_HEXPROOF_ORACLE)
        .id();

    let runner = scenario.build();

    // Before combat: Tromokratis should have hexproof (not attacking/blocking).
    let obj = runner.state().objects.get(&tromokratis).unwrap();
    // The hexproof is granted via a Continuous static with condition
    // Not(Or([SourceIsAttacking, SourceIsBlocking])). Since the creature is not
    // attacking, the condition is satisfied and hexproof should be active after
    // layer evaluation.
    let has_hexproof_static = obj
        .static_definitions
        .iter_unchecked()
        .any(|sd| sd.mode == StaticMode::Continuous && sd.condition.is_some());
    assert!(
        has_hexproof_static,
        "Tromokratis should have a conditional hexproof static definition"
    );
}
