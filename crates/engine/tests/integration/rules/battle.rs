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
            printed_loyalty: None,
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
            mana_spend_permission: None,
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

/// R4l — CR 310.11a (*"must choose its protector from among their opponents"*) +
/// CR 704.5w (*"no player **in the game** designated as its protector"*): the protector
/// pick is a CHOICE (CR 115.10a), so a phased-out seat is not among the choosable
/// opponents (the CR 702.26b MIRROR), and a departed one is not either (CR 800.4 +
/// CR 102.1).
///
/// THE SHARED 5-SEAT BOARD: P0 controls the Siege, P1 is phased out, P2 eliminated, P3/P4
/// valid. Nothing in this file exercises phasing at all — every existing row asserts the
/// behaviour 5c changes — so the shapes below are copied and the setups are not.
///
/// ARM 1 of three (the other two are `..._crosses_to_a_silent_auto_apply` and
/// `..._crosses_to_the_graveyard`). Arm 1 is the published-prompt arm: two survivors keep
/// `legal_choices.len() >= 2`, which is the reach-guard — below that the SBA takes a branch
/// that publishes nothing and every `candidates` assertion would be unreachable.
///
/// REVERT-PROBE: restore `players::opponents` at the `legal_choices` derivation ⇒ P1
/// reappears ⇒ the total equality FAILS.
#[test]
fn battle_protector_choice_excludes_a_phased_out_opponent_and_still_offers_the_rest() {
    let (mut runner, battle) = phased_protector_board(&[P1]);

    let mut events = Vec::new();
    sba::check_state_based_actions(runner.state_mut(), &mut events);

    match runner.state().waiting_for.clone() {
        WaitingFor::BattleProtectorChoice {
            player,
            battle_id,
            candidates,
        } => {
            assert_eq!(player, P0);
            assert_eq!(battle_id, battle);
            assert_eq!(
                candidates,
                vec![PlayerId(3), PlayerId(4)],
                "phased-out P1 and eliminated P2 are out; both valid opponents are in"
            );
        }
        other => panic!("Expected BattleProtectorChoice, got {other:?}"),
    }
}

/// R4l arm 2 — THE `2 → 1` CROSSING, which is the hazard this site is actually about.
///
/// Narrowing the choosable set moves a board across `legal_choices.len()`'s branch
/// boundary, and at `1` the engine writes the protector ITSELF and publishes nothing: no
/// `WaitingFor`, no events. That is invisible to every `candidates` assertion the R4-family
/// shape prescribes, so it needs its own arm. The auto-applied seat is not wrong — it is
/// the sole surviving legal opponent, which CR 310.10 / CR 310.11a make the only
/// appropriate player. What this arm guards is the SILENT DISAPPEARANCE of the prompt.
///
/// BOTH halves are required: (a) alone would pass on a board where the SBA never ran at
/// all, and (b) alone would pass if the prompt had ALSO been published.
///
/// The crossing is reached by PHASING, not by board size — `battle_protector_auto_applies_
/// with_single_candidate_2p` reaches `1` because its board has one opponent, which cannot
/// witness a narrowing. It is also not reached by elimination: that is the A5 confound,
/// which additionally ends the game.
///
/// REVERT-PROBE: restore `players::opponents` at site 14 ⇒ both phased-out seats return ⇒
/// `legal_choices` is `[P1, P3, P4]` ⇒ `len() >= 2` ⇒ the prompt returns ⇒ (a) FAILS.
#[test]
fn battle_protector_narrowing_to_one_auto_applies_silently() {
    let (mut runner, battle) = phased_protector_board(&[P1, PlayerId(4)]);

    let mut events = Vec::new();
    sba::check_state_based_actions(runner.state_mut(), &mut events);

    // (a) the prompt is NOT published…
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::BattleProtectorChoice { .. }
        ),
        "one surviving legal opponent ⇒ the singleton branch, which publishes nothing"
    );
    // (b) …and the SBA did run: it wrote the sole surviving legal opponent as protector.
    assert_eq!(
        runner.state().objects[&battle].protector(),
        Some(PlayerId(3)),
        "the auto-applied seat is the ONLY surviving legal opponent (CR 310.11a)"
    );
    assert!(runner.state().battlefield.contains(&battle));
}

/// R4l arm 3 — the `→ 0` crossing: with every opponent phased out there is no appropriate
/// player, and CR 310.10 / CR 704.5w put the battle into its owner's graveyard.
///
/// Reached by PHASING rather than by elimination on purpose: eliminating every opponent
/// also ends the game (`waiting_for = GameOver`), which would confound the assertions with
/// a game-over transition. Phasing keeps the table live, so what this arm reads is the
/// battle rule and nothing else — asserted below.
#[test]
fn battle_protector_narrowing_to_zero_sends_the_battle_to_the_graveyard() {
    let (mut runner, battle) = phased_protector_board(&[P1, PlayerId(3), PlayerId(4)]);

    let mut events = Vec::new();
    sba::check_state_based_actions(runner.state_mut(), &mut events);

    assert_eq!(runner.state().objects[&battle].zone, Zone::Graveyard);
    assert!(!runner.state().battlefield.contains(&battle));
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::BattleProtectorChoice { .. }
    ));
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "the table must still be LIVE — reaching 0 by phasing rather than by elimination \
         is what keeps this arm about CR 310.10 instead of about the game ending"
    );
}

/// The shared choice-legality board for R4l's three arms: five seats, P0 controls a Siege
/// seeded with the illegal `protector == controller` (CR 704.5x) so the SBA fires, P2
/// eliminated, and each seat in `phase_out` transitioned through the PRODUCTION phasing
/// API. Every arm differs ONLY in that list, which is what makes them one crossing series
/// rather than three unrelated boards.
fn phased_protector_board(phase_out: &[PlayerId]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(5, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let battle = scenario.add_creature(P0, "Contested Siege", 0, 0).id();
    let mut runner = scenario.build();
    make_into_siege(&mut runner, battle, P0, 3);

    let mut events = Vec::new();
    for seat in phase_out {
        // Setup anti-vacuity: the production API reports what it transitioned, so a
        // silent no-op fails loudly here rather than quietly weakening the arm.
        let transitioned =
            engine::game::phasing::phase_out_player(runner.state_mut(), *seat, &mut events);
        assert_eq!(
            transitioned,
            vec![*seat],
            "phase_out_player must actually transition {seat:?}"
        );
    }
    engine::game::elimination::eliminate_player(runner.state_mut(), PlayerId(2), &mut events);
    assert!(
        runner.state().players[2].is_eliminated,
        "P2 must read as eliminated"
    );
    (runner, battle)
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
