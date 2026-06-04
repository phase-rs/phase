//! Integration tests for Battle permanents (CR 310).
//!
//! Covers:
//! - Defense-counter ETB (CR 310.4b)
//! - Zero-defense SBA (CR 704.5v + CR 310.7)
//! - Protector choice/getter (CR 310.11a + CR 310.8e)
//! - Attack target routing — defending player = protector (CR 508.5 + CR 310.8d)
//! - Protector cannot attack own battle (CR 310.8b)

#![allow(unused_imports)]
use super::*;

use engine::game::sba;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ChosenAttribute;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;

/// Convert an existing battlefield creature into a Siege with the given defense.
fn make_into_siege(
    runner: &mut GameRunner,
    id: ObjectId,
    protector: PlayerId,
    printed_defense: u32,
) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.clear();
    obj.card_types.core_types.push(CoreType::Battle);
    obj.card_types.subtypes = vec!["Siege".to_string()];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
    obj.defense = Some(printed_defense);
    obj.base_defense = Some(printed_defense);
    obj.counters.insert(CounterType::Defense, printed_defense);
    obj.chosen_attributes
        .push(ChosenAttribute::Player(protector));
}

fn prime_siege(
    controller: PlayerId,
    protector: PlayerId,
    name: &str,
    printed_defense: u32,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario.add_creature(controller, name, 0, 0).id();
    let mut runner = scenario.build();
    make_into_siege(&mut runner, id, protector, printed_defense);
    (runner, id)
}

/// CR 310.4b + CR 310.4c: A battle on the battlefield has defense equal to its
/// defense counters, with the `defense` field mirroring the counter count.
#[test]
fn battle_has_defense_equal_to_counters() {
    let (runner, battle) = prime_siege(P0, P1, "Test Siege", 4);
    let obj = &runner.state().objects[&battle];
    assert_eq!(obj.defense, Some(4));
    assert_eq!(obj.counters.get(&CounterType::Defense).copied(), Some(4));
}

/// CR 310.11b + CR 712.14a: Accepting a Siege victory cast during trigger
/// resolution must preserve `cast_transformed`, so the permanent resolves onto
/// the battlefield back face up.
#[test]
fn siege_victory_cast_during_resolution_enters_transformed() {
    use engine::game::game_object::BackFaceData;
    use engine::types::ability::{
        CardPlayMode, CastFromZoneDriver, Effect, ResolvedAbility, TargetFilter, TargetRef,
    };
    use engine::types::card_type::CardType;
    use engine::types::mana::ManaCost;

    let (mut runner, battle) = prime_siege(P0, P1, "Invasion of Test", 3);
    {
        let obj = runner.state_mut().objects.get_mut(&battle).unwrap();
        obj.back_face = Some(BackFaceData {
            name: "Test Back Face".to_string(),
            power: Some(4),
            toughness: Some(4),
            loyalty: None,
            defense: None,
            card_types: CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Spirit".to_string()],
            },
            mana_cost: ManaCost::default(),
            keywords: Vec::new(),
            abilities: Vec::new(),
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: Vec::new(),
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: Vec::new(),
            casting_options: Vec::new(),
            layout_kind: None,
        });
    }

    let cast_victory_back_face = ResolvedAbility::new(
        Effect::CastFromZone {
            target: TargetFilter::SelfRef,
            without_paying_mana_cost: true,
            mode: CardPlayMode::Cast,
            cast_transformed: true,
            alt_ability_cost: None,
            constraint: None,
            duration: None,
            driver: CastFromZoneDriver::DuringResolution,
        },
        vec![TargetRef::Object(battle)],
        battle,
        P0,
    );
    let mut events = Vec::new();
    engine::game::effects::resolve_ability_chain(
        runner.state_mut(),
        &cast_victory_back_face,
        &mut events,
        0,
    )
    .expect("Siege victory CastFromZone should cast during resolution");

    assert_eq!(
        runner.state().objects[&battle].zone,
        Zone::Stack,
        "victory cast should put the Siege on the stack during resolution"
    );

    runner.resolve_top();

    let obj = &runner.state().objects[&battle];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        obj.transformed,
        "victory cast must preserve cast_transformed through the during-resolution permission"
    );
    assert_eq!(obj.name, "Test Back Face");
    assert!(obj.card_types.core_types.contains(&CoreType::Creature));
}

/// CR 704.5v + CR 310.7: A battle with 0 defense is put into its owner's
/// graveyard by state-based actions.
#[test]
fn zero_defense_battle_goes_to_graveyard_via_sba() {
    let (mut runner, battle) = prime_siege(P0, P1, "Dying Siege", 0);

    let mut events = Vec::new();
    sba::check_state_based_actions(runner.state_mut(), &mut events);

    assert_eq!(
        runner.state().objects[&battle].zone,
        Zone::Graveyard,
        "0-defense battle should be sent to graveyard by SBA"
    );
}

/// CR 310.8e + CR 310.11a: The `protector()` getter returns the chosen opponent.
#[test]
fn protector_getter_returns_chosen_player() {
    let (runner, battle) = prime_siege(P0, P1, "Protected Siege", 3);
    assert_eq!(runner.state().objects[&battle].protector(), Some(P1));
}

/// CR 310.8e: Non-battle permanents always return None from `protector()`.
#[test]
fn non_battle_has_no_protector() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_vanilla(P0, 2, 2);
    let runner = scenario.build();
    assert_eq!(runner.state().objects[&creature].protector(), None);
}

/// CR 508.1b + CR 508.5 + CR 310.8d: When a creature attacks a battle, the
/// defending player for combat purposes is the battle's protector, not the
/// battle's controller. Controller (P0) can attack their own Siege when the
/// protector (P1) is different — CR 310.8b.
#[test]
fn battle_attack_defending_player_is_protector() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let siege_id = scenario.add_creature(P0, "Attackable Siege", 0, 0).id();

    let attacker = scenario.add_creature(P0, "Attacker", 3, 3).id();
    let mut runner = scenario.build();

    // Make attacker combat-ready (not summoning sick).
    {
        let turn = runner.state().turn_number.saturating_sub(1);
        runner
            .state_mut()
            .objects
            .get_mut(&attacker)
            .unwrap()
            .entered_battlefield_turn = Some(turn);
    }
    // Turn the placeholder into a Siege with P0 controller, P1 protector.
    make_into_siege(&mut runner, siege_id, P1, 5);

    runner.pass_both_players(); // → DeclareAttackers

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Battle(siege_id))],
            bands: vec![],
        })
        .expect("attacking a battle controlled by you but protected by an opponent is legal");

    let combat = runner.state().combat.as_ref().expect("combat state");
    let info = combat
        .attackers
        .iter()
        .find(|a| a.object_id == attacker)
        .expect("attacker recorded");
    assert_eq!(
        info.defending_player, P1,
        "defending player for battle = protector (not controller)"
    );
    assert!(matches!(info.attack_target, AttackTarget::Battle(id) if id == siege_id));
}

// ---------------------------------------------------------------------------
// CR 310.10 + CR 704.5w + CR 704.5x: SBA protector reassignment.
// Multi-candidate (3+ player) branch must pause with
// `WaitingFor::BattleProtectorChoice`; singleton (2-player) must auto-apply.
// ---------------------------------------------------------------------------

/// CR 704.5x: 2-player Siege whose protector equals its controller (illegal).
/// Only one legal opponent remains, so the SBA auto-applies and never pauses.
#[test]
fn battle_protector_auto_applies_with_single_candidate_2p() {
    let (mut runner, battle) = prime_siege(P0, P0, "Self-Protected Siege", 3);
    // Baseline: protector == controller (illegal per CR 310.8b / 310.11a).
    assert_eq!(runner.state().objects[&battle].protector(), Some(P0));

    let mut events = Vec::new();
    sba::check_state_based_actions(runner.state_mut(), &mut events);

    // SBA auto-picked the only legal opponent (P1). No choice was surfaced.
    assert_eq!(runner.state().objects[&battle].protector(), Some(P1));
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::BattleProtectorChoice { .. }
        ),
        "2-player Siege with a singleton candidate list must not surface a choice"
    );
    assert!(runner.state().battlefield.contains(&battle));
}

/// CR 310.10 + CR 704.5w + CR 704.5x: In a 3-player game the controller has two
/// legal opponents, so the SBA must pause with `BattleProtectorChoice`. Submitting
/// `ChooseBattleProtector` assigns the chosen player via `ChosenAttribute::Player`
/// and resumes the game.
#[test]
fn battle_protector_pauses_for_choice_with_multiple_candidates_3p() {
    const P2: PlayerId = PlayerId(2);

    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let battle = scenario.add_creature(P0, "Contested Siege", 0, 0).id();
    let mut runner = scenario.build();
    // Seed with controller == protector (illegal per CR 704.5x), so the SBA
    // fires with both opponents (P1, P2) as legal candidates.
    make_into_siege(&mut runner, battle, P0, 3);

    let mut events = Vec::new();
    sba::check_state_based_actions(runner.state_mut(), &mut events);

    // SBA paused with an interactive choice for the battle's controller.
    match runner.state().waiting_for.clone() {
        WaitingFor::BattleProtectorChoice {
            player,
            battle_id,
            candidates,
        } => {
            assert_eq!(player, P0);
            assert_eq!(battle_id, battle);
            assert!(candidates.contains(&P1));
            assert!(candidates.contains(&P2));
            assert_eq!(candidates.len(), 2);
        }
        other => panic!("Expected BattleProtectorChoice, got {:?}", other),
    }
    // Protector field is unchanged while the choice is pending.
    assert_eq!(runner.state().objects[&battle].protector(), Some(P0));

    // Controller submits their pick (P2) — assignment is applied and the game
    // resumes at Priority.
    runner
        .act(GameAction::ChooseBattleProtector { protector: P2 })
        .expect("ChooseBattleProtector should resolve");

    assert_eq!(runner.state().objects[&battle].protector(), Some(P2));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 310.10: Submitting a protector that isn't in the candidate list is rejected.
#[test]
fn battle_protector_choice_rejects_invalid_candidate() {
    const P2: PlayerId = PlayerId(2);

    let mut scenario = GameScenario::new_n_player(3, 11);
    scenario.at_phase(Phase::PreCombatMain);
    let battle = scenario.add_creature(P0, "Invalid Choice Siege", 0, 0).id();
    let mut runner = scenario.build();
    make_into_siege(&mut runner, battle, P0, 3);

    let mut events = Vec::new();
    sba::check_state_based_actions(runner.state_mut(), &mut events);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::BattleProtectorChoice { .. }
    ));

    // P0 is the controller — not a legal Siege protector (CR 310.11a).
    let err = runner
        .act(GameAction::ChooseBattleProtector { protector: P0 })
        .expect_err("choosing a non-candidate player must be rejected");
    // Choice is still pending; battle is still on the battlefield.
    let _ = err;
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::BattleProtectorChoice { .. }
    ));
    // Valid choice still resolves.
    runner
        .act(GameAction::ChooseBattleProtector { protector: P2 })
        .expect("valid candidate should resolve");
    assert_eq!(runner.state().objects[&battle].protector(), Some(P2));
}

/// CR 310.10 / CR 704.5w: When no legal candidate exists, the battle is put
/// into its owner's graveyard. This preserves the existing 0-candidate fallback.
#[test]
fn battle_with_no_legal_protector_goes_to_graveyard() {
    // 2-player Siege whose only opponent (P1) has been eliminated — no legal
    // protector exists, so CR 310.10 sends the battle to the graveyard.
    let (mut runner, battle) = prime_siege(P0, P0, "Abandoned Siege", 3);
    runner.state_mut().eliminated_players.push(P1);

    let mut events = Vec::new();
    sba::check_state_based_actions(runner.state_mut(), &mut events);

    assert_eq!(runner.state().objects[&battle].zone, Zone::Graveyard);
    assert!(!runner.state().battlefield.contains(&battle));
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::BattleProtectorChoice { .. }
    ));
}

/// CR 310.10 + CR 704.5w: AI routing — when the 3-player SBA pauses with a
/// protector choice, `legal_actions` emits one `ChooseBattleProtector` candidate
/// per legal opponent, so the AI has a deterministic decision surface.
#[test]
fn battle_protector_choice_emits_ai_candidates_per_opponent() {
    const P2: PlayerId = PlayerId(2);

    let mut scenario = GameScenario::new_n_player(3, 19);
    scenario.at_phase(Phase::PreCombatMain);
    let battle = scenario.add_creature(P0, "AI Siege", 0, 0).id();
    let mut runner = scenario.build();
    make_into_siege(&mut runner, battle, P0, 3);

    let mut events = Vec::new();
    sba::check_state_based_actions(runner.state_mut(), &mut events);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::BattleProtectorChoice { .. }
    ));

    let actions = engine::ai_support::legal_actions(runner.state());
    let picks: Vec<PlayerId> = actions
        .into_iter()
        .filter_map(|a| match a {
            GameAction::ChooseBattleProtector { protector } => Some(protector),
            _ => None,
        })
        .collect();
    assert!(picks.contains(&P1));
    assert!(picks.contains(&P2));
    assert_eq!(picks.len(), 2);
}

/// CR 310.8b: A battle's protector cannot attack it — the declaration is illegal.
#[test]
fn battle_protector_cannot_attack_own_battle() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let siege_id = scenario.add_creature(P1, "My Siege", 0, 0).id();
    let attacker = scenario.add_creature(P0, "Attacker", 3, 3).id();
    let mut runner = scenario.build();

    {
        let turn = runner.state().turn_number.saturating_sub(1);
        runner
            .state_mut()
            .objects
            .get_mut(&attacker)
            .unwrap()
            .entered_battlefield_turn = Some(turn);
    }
    // P1 controls, P0 (active) is the protector → P0 cannot attack.
    make_into_siege(&mut runner, siege_id, P0, 3);

    runner.pass_both_players();

    let result = runner.act(GameAction::DeclareAttackers {
        attacks: vec![(attacker, AttackTarget::Battle(siege_id))],
        bands: vec![],
    });
    assert!(
        result.is_err(),
        "protector cannot attack the battle it protects"
    );
}
