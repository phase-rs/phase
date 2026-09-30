//! Integration tests for Battle permanents (CR 310).
//!
//! Covers:
//! - Defense-counter ETB (CR 310.4b)
//! - Zero-defense SBA (CR 704.5v + CR 310.7)
//! - Protector choice/getter (CR 310.9a + CR 310.12a)
//! - Attack target routing — defending player = protector (CR 508.5 + CR 310.9d)
//! - Protector cannot attack own battle (CR 310.9b)
//! - CR 704.5v + CR 510.3a — a Siege defeated by combat damage survives until
//!   its victory trigger resolves; CR 614.12 + CR 712.11a + CR 712.13 — the
//!   victory-cast permanent enters with its back face's replacement effects,
//!   not the Siege's

#![allow(unused_imports)]
use super::*;

use crate::support::shared_card_db;

use engine::game::sba;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::stack;
use engine::types::ability::{
    ChoiceType, ChosenAttribute, Effect, ReplacementMode, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{ReplacementChoiceKind, StackEntry, StackEntryKind};
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::proposed_event::EtbTapState;
use engine::types::replacements::ReplacementEvent;

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

/// CR 310.12b + CR 712.14a: Accepting a Siege victory cast during trigger
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
            is_swap_snapshot: false,
            trigger_printed_origins: Vec::new(),
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
            parse_warnings: vec![],
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
            additional_cost: None,
            cast_cost_modifier: None,
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

/// Put a triggered ability whose source is `source` onto the stack, so the
/// zero-defense SBA sees the battle as "the source of an ability that has
/// triggered but not yet left the stack" (CR 704.5v).
fn push_own_trigger(runner: &mut GameRunner, source: ObjectId, controller: PlayerId) {
    let entry = {
        let state = runner.state_mut();
        let id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        StackEntry {
            id,
            source_id: source,
            controller,
            kind: StackEntryKind::TriggeredAbility {
                source_id: source,
                ability: Box::new(ResolvedAbility::new(
                    Effect::unimplemented("battle trigger", "this battle's own trigger"),
                    vec![],
                    source,
                    controller,
                )),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        }
    };
    stack::push_to_stack(runner.state_mut(), entry, &mut Vec::new());
}

/// CR 704.5v + CR 310.7: a Siege at defense 0 is NOT put into its owner's
/// graveyard while it is the source of an ability that has triggered but not
/// yet left the stack — CR 310.12b's victory trigger has to find the Siege on
/// the battlefield when it resolves.
#[test]
fn zero_defense_siege_survives_its_own_trigger_on_stack() {
    let (mut runner, battle) = prime_siege(P0, P1, "Deferred Siege", 0);
    push_own_trigger(&mut runner, battle, P0);

    let mut events = Vec::new();
    sba::check_state_based_actions(runner.state_mut(), &mut events);

    assert_eq!(
        runner.state().objects[&battle].zone,
        Zone::Battlefield,
        "CR 704.5v: a 0-defense Siege must survive while its own triggered ability is on the stack"
    );
}

/// CR 704.5w + CR 310.8: a non-Siege battle at defense 0 is put into its owner's
/// graveyard even while it is the source of an ability that has triggered but
/// not yet left the stack. CR 704.5w states the rule with no deferral clause at
/// all — the clause in CR 704.5v is Siege-only.
#[test]
fn zero_defense_non_siege_battle_dies_with_its_own_trigger_on_stack() {
    // CR 310.9a: a battle with no battle type may only have its controller as
    // its protector, so P0 is both here. The non-Siege battles that ship in the
    // card data (Occupation of Kulrath / Occupation of Llanowar, "Battle —
    // Control Point") reach CR 704.5w by the same door: not being a Siege.
    let (mut runner, battle) = prime_siege(P0, P0, "Occupied Battle", 0);
    {
        let obj = runner.state_mut().objects.get_mut(&battle).unwrap();
        obj.card_types.subtypes.clear();
        obj.base_card_types.subtypes.clear();
    }
    push_own_trigger(&mut runner, battle, P0);

    let mut events = Vec::new();
    sba::check_state_based_actions(runner.state_mut(), &mut events);

    assert_eq!(
        runner.state().objects[&battle].zone,
        Zone::Graveyard,
        "CR 704.5w: a 0-defense non-Siege battle has no trigger-on-stack deferral"
    );
}

/// CR 310.9 + CR 310.9a: The `protector()` getter returns the chosen opponent.
#[test]
fn protector_getter_returns_chosen_player() {
    let (runner, battle) = prime_siege(P0, P1, "Protected Siege", 3);
    assert_eq!(runner.state().objects[&battle].protector(), Some(P1));
}

/// CR 310.8: Non-battle permanents always return None from `protector()`.
#[test]
fn non_battle_has_no_protector() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_vanilla(P0, 2, 2);
    let runner = scenario.build();
    assert_eq!(runner.state().objects[&creature].protector(), None);
}

/// CR 508.1b + CR 508.5 + CR 310.9d: When a creature attacks a battle, the
/// defending player for combat purposes is the battle's protector, not the
/// battle's controller. Controller (P0) can attack their own Siege when the
/// protector (P1) is different — CR 310.9b.
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
// CR 310.11 + CR 704.5x: SBA protector reassignment.
// Multi-candidate (3+ player) branch must pause with
// `WaitingFor::BattleProtectorChoice`; singleton (2-player) must auto-apply.
// ---------------------------------------------------------------------------

/// CR 704.5x: 2-player Siege whose protector equals its controller (illegal).
/// Only one legal opponent remains, so the SBA auto-applies and never pauses.
#[test]
fn battle_protector_auto_applies_with_single_candidate_2p() {
    let (mut runner, battle) = prime_siege(P0, P0, "Self-Protected Siege", 3);
    // Baseline: protector == controller (illegal per CR 310.12a).
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

/// CR 310.11 + CR 704.5x: In a 3-player game the controller has two
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

/// CR 310.11: Submitting a protector that isn't in the candidate list is rejected.
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

    // P0 is the controller — not a legal Siege protector (CR 310.12a).
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

/// CR 310.11 / CR 704.5x: When no legal candidate exists, the battle is put
/// into its owner's graveyard. This preserves the existing 0-candidate fallback.
#[test]
fn battle_with_no_legal_protector_goes_to_graveyard() {
    // 2-player Siege whose only opponent (P1) has been eliminated — no legal
    // protector exists, so CR 310.11 sends the battle to the graveyard.
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

/// R4l — CR 310.12a (*"must choose its protector from among their opponents"*) +
/// CR 704.5x (*"no player **in the game** designated as its protector"*): the protector
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
/// the sole surviving legal opponent, which CR 310.11 + CR 310.12a make the only
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
        "the auto-applied seat is the ONLY surviving legal opponent (CR 310.12a)"
    );
    assert!(runner.state().battlefield.contains(&battle));
}

/// R4l arm 3 — the `→ 0` crossing: with every opponent phased out there is no appropriate
/// player, and CR 310.11 / CR 704.5x put the battle into its owner's graveyard.
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
         is what keeps this arm about CR 310.11 instead of about the game ending"
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

/// CR 310.11 + CR 704.5x: AI routing — when the 3-player SBA pauses with a
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

/// CR 310.9b: A battle's protector cannot attack it — the declaration is illegal.
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

// ---------------------------------------------------------------------------
// CR 704.5v + CR 614.12: a Siege defeated in combat is not destroyed before
// its own victory trigger resolves, and the transformed entry consults the
// back face's own characteristics/replacements, not the front Siege's.
// Real cards from the committed integration fixture (`shared_card_db`).
// ---------------------------------------------------------------------------

/// Drive P0's real Siege `name` through combat damage from a P0 attacker with
/// `attacker_power` power (first strike if requested), stopping at the first
/// decision point after the last defense counter is removed. `graveyard_creature`
/// seeds a synthetic "Graveyard Bear" 2/2 into P1's graveyard (Lazotep Convert's
/// copy target). `p1_real_permanents` are added to P1's battlefield
/// as real cards (Kismet, for the Kismet-vs-copy CR 616.1c precedence test). Returns the
/// runner, the battle's `ObjectId`, and every event after the attack
/// declaration through the damage action.
fn defeat_siege_in_combat(
    name: &str,
    attacker_power: i32,
    first_strike: bool,
    graveyard_creature: bool,
    p1_real_permanents: &[&str],
) -> (GameRunner, ObjectId, Vec<GameEvent>) {
    let db = shared_card_db().expect("integration fixture must be present");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if graveyard_creature {
        scenario.add_creature_to_graveyard(P1, "Graveyard Bear", 2, 2);
    }
    for real_name in p1_real_permanents {
        scenario.add_real_card(P1, real_name, Zone::Battlefield, db);
    }
    let battle = scenario.add_real_card(P0, name, Zone::Battlefield, db);
    let attacker = {
        let mut builder =
            scenario.add_creature(P0, "Siege Breaker", attacker_power, attacker_power);
        if first_strike {
            builder.with_keyword(Keyword::FirstStrike);
        }
        builder.id()
    };

    let mut runner = scenario.build();

    if first_strike {
        assert!(
            runner.state().objects[&attacker].has_keyword(&Keyword::FirstStrike),
            "CR 510.4: the attacker must have first strike for the first-strike \
             damage step to be exercised"
        );
    }

    // Attacker must be combat-ready (not summoning sick).
    {
        let turn = runner.state().turn_number.saturating_sub(1);
        runner
            .state_mut()
            .objects
            .get_mut(&attacker)
            .unwrap()
            .entered_battlefield_turn = Some(turn);
    }

    // Reach guards: `name` really is a Siege with defense counters and a back
    // face to transform into.
    {
        let obj = &runner.state().objects[&battle];
        assert!(
            obj.card_types.core_types.contains(&CoreType::Battle),
            "{name} must be a Battle"
        );
        assert!(
            obj.card_types.subtypes.iter().any(|s| s == "Siege"),
            "{name} must be a Siege"
        );
        assert!(
            obj.counters
                .get(&CounterType::Defense)
                .copied()
                .unwrap_or(0)
                > 0,
            "{name} must have defense counters"
        );
        assert!(obj.back_face.is_some(), "{name} must have a back face");
    }

    // `add_real_card` abandons the as-enters protector prompt (CR 310.12a);
    // no protector is chosen yet.
    assert_eq!(runner.state().objects[&battle].protector(), None);

    // CR 704.5x + CR 310.11: the first priority pass runs the no-protector
    // SBA sweep, which auto-assigns the only legal opponent (P1) in this
    // 2-player game.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    assert_eq!(
        runner.state().objects[&battle].protector(),
        Some(P1),
        "CR 704.5x: the SBA must assign P1 as protector"
    );

    // Advance to Declare Attackers (bounded; CR 500.1: the combat phase
    // directly follows the precombat main phase).
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            break;
        }
        runner.pass_both_players();
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "must reach Declare Attackers"
    );

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Battle(battle))],
            bands: vec![],
        })
        .expect("attacking a battle controlled by you but protected by an opponent is legal");

    let mut events = Vec::new();
    let mut damage_dealt = false;
    for _ in 0..12 {
        if damage_dealt {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => {
                let result = runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("decline to block");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } => {
                let result = runner.act(GameAction::PassPriority).expect("pass priority");
                damage_dealt = result.events.iter().any(|e| {
                    matches!(
                        e,
                        GameEvent::CounterRemoved {
                            object_id,
                            counter_type: CounterType::Defense,
                            ..
                        } if *object_id == battle
                    )
                });
                events.extend(result.events);
            }
            other => panic!("unexpected wait while driving combat: {other:?}"),
        }
    }
    assert!(
        damage_dealt,
        "combat damage must have removed the Siege's last defense counter"
    );

    (runner, battle, events)
}

/// Drive from the post-damage decision point through the Siege's CR 310.12b
/// victory trigger: pass priority (bounded) until the `OptionalEffectChoice`
/// for the battle's own trigger appears, assert the battle is in exile
/// (CR 310.12b "exile it"), then accept or decline the free transformed
/// cast. On accept, also assert CR 107.3b's free-cast X is never prompted
/// (`WaitingFor::ChooseXValue` must not appear), then keep passing priority
/// until the wait leaves `Priority` (a replacement prompt may park the entry
/// before delivery, when the back face has its own as-enters replacement) or
/// the battle leaves the stack. Returns every event from the accept/decline
/// action onward.
fn resolve_victory(runner: &mut GameRunner, battle: ObjectId, accept: bool) -> Vec<GameEvent> {
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { source_id, .. } if source_id == battle
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority toward the victory trigger");
    }
    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice { source_id, .. } if source_id == battle => {}
        other => panic!("expected the Siege's own OptionalEffectChoice, got {other:?}"),
    }
    assert_eq!(
        runner.state().objects[&battle].zone,
        Zone::Exile,
        "CR 310.12b: the Siege must be exiled before the optional cast is offered"
    );

    let decide_result = runner
        .act(GameAction::DecideOptionalEffect { accept })
        .expect("decide the victory cast");
    let mut events = decide_result.events;

    if accept {
        assert!(
            !matches!(decide_result.waiting_for, WaitingFor::ChooseXValue { .. }),
            "CR 107.3b: a free cast must never prompt for X"
        );
        for _ in 0..8 {
            let still_on_stack = runner.state().objects[&battle].zone == Zone::Stack;
            let waiting_is_priority =
                matches!(runner.state().waiting_for, WaitingFor::Priority { .. });
            if !still_on_stack || !waiting_is_priority {
                break;
            }
            let result = runner
                .act(GameAction::PassPriority)
                .expect("pass priority to resolve the victory cast");
            events.extend(result.events);
        }
    }

    events
}

/// A Siege defeated by combat damage survives (CR 704.5v) long
/// enough for its own victory trigger to resolve, and the transformed entry
/// consults the back face's own characteristics (CR 614.12 + CR 712.8c +
/// CR 712.11a + CR 712.13) — not the front Siege's CR 310.12a protector
/// replacement.
#[test]
fn siege_defeated_in_combat_enters_transformed_invasion_of_ikoria() {
    let (mut runner, battle, damage_events) =
        defeat_siege_in_combat("Invasion of Ikoria", 7, false, false, &[]);

    {
        let obj = &runner.state().objects[&battle];
        assert_eq!(
            obj.back_face.as_ref().map(|b| b.name.as_str()),
            Some("Zilortha, Apex of Ikoria")
        );
        assert!(
            matches!(&obj.mana_cost, ManaCost::Cost { shards, .. } if shards.contains(&ManaCostShard::X)),
            "Invasion of Ikoria's front face must have an X in its cost"
        );
    }

    // Discriminator: the battle survived combat damage (CR 704.5v).
    assert!(
        damage_events.iter().any(|e| matches!(
            e,
            GameEvent::CounterRemoved { object_id, counter_type: CounterType::Defense, .. }
                if *object_id == battle
        )),
        "combat damage must have removed the last defense counter"
    );
    assert_eq!(
        runner.state().objects[&battle].zone,
        Zone::Battlefield,
        "CR 704.5v: the Siege must survive combat damage while its own \
         victory trigger has triggered but not yet left the stack"
    );
    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == battle
        )),
        "the victory trigger must be on the stack"
    );

    let events = resolve_victory(&mut runner, battle, true);

    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::SpellCast { object_id, cast_mana_value: Some(2), .. } if *object_id == battle
        )),
        "CR 107.3b + CR 712.8c: the free cast's mana value comes from the \
         front face's mana cost with X = 0"
    );

    {
        let obj = &runner.state().objects[&battle];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(obj.transformed, "must enter showing the back face");
        assert_eq!(obj.name, "Zilortha, Apex of Ikoria");
        assert!(obj.card_types.core_types.contains(&CoreType::Creature));
        assert!(!obj.card_types.core_types.contains(&CoreType::Battle));
        assert_eq!(obj.power, Some(8));
        assert_eq!(obj.toughness, Some(8));
        assert_eq!(obj.controller, P0);
    }

    // Discriminators (CR 614.12 + CR 310.12a): Zilortha is not a Siege, so
    // the front face's protector replacement must not apply, and the game
    // must return to a normal priority state with no spurious prompt.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "CR 614.12 + CR 310.12a: no protector prompt should be left open"
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().objects[&battle].protector(),
        None,
        "CR 614.12: the front face's CR 310.12a protector replacement must \
         not apply to the back face's entry"
    );
    assert!(!runner.state().objects[&battle]
        .chosen_attributes
        .iter()
        .any(|a| matches!(a, ChosenAttribute::Player(_))));
    assert!(
        !events.iter().any(|e| matches!(
            e,
            GameEvent::ReplacementApplied { source_id, event_type }
                if *source_id == battle && event_type == "Moved"
        )),
        "the front face's protector replacement must not have applied"
    );
}

/// The same CR 704.5v deferral in the first-strike combat-damage
/// sub-step (CR 510.4) — the class, not just the regular-damage step.
#[test]
fn siege_defeated_by_first_strike_damage_survives_invasion_of_amonkhet() {
    let (mut runner, battle, damage_events) =
        defeat_siege_in_combat("Invasion of Amonkhet", 5, true, false, &[]);

    assert!(damage_events.iter().any(|e| matches!(
        e,
        GameEvent::CounterRemoved { object_id, counter_type: CounterType::Defense, .. }
            if *object_id == battle
    )));
    assert_eq!(
        runner.state().objects[&battle].zone,
        Zone::Battlefield,
        "CR 704.5v + CR 510.4: the Siege must survive first-strike combat \
         damage while its own victory trigger is on the stack"
    );

    resolve_victory(&mut runner, battle, true);

    let obj = &runner.state().objects[&battle];
    assert_eq!(obj.name, "Lazotep Convert");
    assert!(obj.transformed);
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Declining the free transformed cast leaves the Siege exiled
/// (CR 310.12b).
#[test]
fn siege_defeated_in_combat_declined_cast_stays_exiled() {
    let (mut runner, battle, _damage_events) =
        defeat_siege_in_combat("Invasion of Ikoria", 7, false, false, &[]);

    resolve_victory(&mut runner, battle, false);

    let obj = &runner.state().objects[&battle];
    assert_eq!(obj.zone, Zone::Exile);
    assert!(!obj.transformed);
}

/// Positive control: an untransformed Siege entry from
/// exile still gets its CR 310.12a protector choice —
/// `transformed_entry_entrant` must return `None` for `enter_transformed: false`.
#[test]
fn siege_entering_untransformed_still_chooses_protector() {
    let db = shared_card_db().expect("integration fixture must be present");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let battle = scenario.add_real_card(P0, "Invasion of Ikoria", Zone::Exile, db);
    let mut runner = scenario.build();

    let resolved = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Battlefield,
            target: TargetFilter::SelfRef,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![TargetRef::Object(battle)],
        battle,
        P0,
    );
    let mut events = Vec::new();
    engine::game::effects::resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("untransformed battlefield entry resolves");

    let options = match runner.state().waiting_for.clone() {
        WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            ..
        } => {
            assert_eq!(player, P0);
            assert!(matches!(choice_type, ChoiceType::Opponent { .. }));
            options
        }
        other => panic!("expected NamedChoice for the protector, got {other:?}"),
    };

    {
        let obj = &runner.state().objects[&battle];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(!obj.transformed);
        assert_eq!(obj.counters.get(&CounterType::Defense).copied(), Some(6));
    }

    runner
        .act(GameAction::ChooseOption {
            choice: options[0].clone(),
        })
        .expect("answer the protector choice");
    assert_eq!(runner.state().objects[&battle].protector(), Some(P1));
}

/// A back face's own optional as-enters replacement applies on transformed
/// entry (CR 614.12 + CR 614.1c + CR 707.9), offered exactly once — never the
/// suppressed front-face protector replacement at the same index.
#[test]
fn siege_victory_cast_offers_back_face_copy_replacement_invasion_of_amonkhet() {
    let (mut runner, battle, _events) =
        defeat_siege_in_combat("Invasion of Amonkhet", 5, false, true, &[]);

    let copy_description = runner.state().objects[&battle]
        .back_face
        .as_ref()
        .and_then(|back| back.replacement_definitions.get(0))
        .and_then(|def| def.description.clone())
        .expect("Lazotep Convert must carry its copy replacement");

    resolve_victory(&mut runner, battle, true);

    match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice {
            kind, candidates, ..
        } => {
            assert_eq!(
                kind,
                ReplacementChoiceKind::OptionalBranch,
                "a single optional replacement must render as accept/decline, not an ordering list"
            );
            assert_eq!(
                candidates.first().map(|c| c.description.clone()),
                Some(copy_description.clone()),
                "CR 614.12: the offered definition must be the back face's own"
            );
        }
        other => panic!("expected the Lazotep Convert copy prompt, got {other:?}"),
    }

    // Decline branch: the permanent enters as Lazotep Convert with no copy.
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline the copy");
    {
        let obj = &runner.state().objects[&battle];
        assert_eq!(obj.name, "Lazotep Convert");
        assert!(obj.transformed);
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(!obj
            .chosen_attributes
            .iter()
            .any(|a| matches!(a, ChosenAttribute::Player(_))));
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    // Accept sibling: a fresh scenario where the copy is accepted and
    // resolved against a real graveyard creature (CR 614.12 + CR 707.9).
    let (mut runner2, battle2, _events2) =
        defeat_siege_in_combat("Invasion of Amonkhet", 5, false, true, &[]);
    resolve_victory(&mut runner2, battle2, true);
    runner2
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept the copy");

    let bear = {
        let state = runner2.state();
        let WaitingFor::CopyTargetChoice { valid_targets, .. } = &state.waiting_for else {
            panic!("expected CopyTargetChoice, got {:?}", state.waiting_for);
        };
        *valid_targets
            .iter()
            .find(|id| state.objects[*id].name == "Graveyard Bear")
            .expect("Graveyard Bear must be a legal copy target")
    };

    // A projection still resident at this answer would route
    // `handle_copy_target_choice` into its liminal branch, which fails with
    // "Missing liminal entry resume" — `Ok` proves the projection was
    // released before the copy choice (CR 614.12 lifetime).
    runner2
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("the projection must be released before the copy choice resumes");

    let obj = &runner2.state().objects[&battle2];
    assert_eq!(obj.name, "Graveyard Bear");
    assert_eq!(obj.power, Some(4));
    assert_eq!(obj.toughness, Some(4));
    assert!(!runner2.state().liminal_entries.contains_key(&battle2));
}

/// (CR 614.12 + CR 614.1d + CR 616.1f): another source's entry replacement
/// (Kismet) matches the entrant as it will exist on the battlefield (the back
/// face), and the replacement-choice prompt's candidates come from the
/// projection's own definitions — never the stored front face's definition at
/// the same index. CR 616.1c: the optional copy replacement is decided before
/// Kismet's CR 616.1e effect, which applies once the copy is declined.
#[test]
fn siege_victory_cast_decides_back_face_copy_before_kismet() {
    let (mut runner, battle, _events) =
        defeat_siege_in_combat("Invasion of Amonkhet", 5, false, true, &["Kismet"]);

    let copy_description = runner.state().objects[&battle]
        .back_face
        .as_ref()
        .and_then(|back| back.replacement_definitions.get(0))
        .and_then(|def| def.description.clone())
        .expect("Lazotep Convert must carry its copy replacement");

    // Reach guard: Kismet is really on the battlefield under P1 with its
    // mandatory ChangeZone replacement.
    {
        let state = runner.state();
        let kismet_id = *state
            .battlefield
            .iter()
            .find(|id| state.objects[*id].name == "Kismet")
            .expect("Kismet must be on the battlefield");
        let kismet = &state.objects[&kismet_id];
        assert_eq!(kismet.controller, P1);
        let def = kismet
            .replacement_definitions
            .get(0)
            .expect("Kismet must carry a replacement definition");
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
    }

    resolve_victory(&mut runner, battle, true);

    let WaitingFor::ReplacementChoice {
        player,
        candidates,
        candidate_count,
        kind,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the copy's accept/decline, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, P0,
        "CR 616.1: the affected object's controller chooses"
    );
    assert_eq!(
        kind,
        ReplacementChoiceKind::OptionalBranch,
        "CR 616.1c: only the optional copy replacement is offered"
    );
    assert_eq!(candidate_count, 2, "accept or decline the copy");
    assert_eq!(
        candidates.first().map(|c| c.description.clone()),
        Some(copy_description.clone())
    );
    assert!(
        !candidates.iter().any(|c| c.description == "Enters tapped"),
        "CR 616.1c: Kismet's CR 616.1e effect is not offered before the copy is decided"
    );
    assert!(
        !candidates
            .iter()
            .any(|c| c.description.starts_with("CR 310.12a")),
        "CR 614.12: the front face's suppressed protector replacement is not a candidate"
    );

    // An index past the two offered options is rejected and leaves the choice open.
    assert!(
        runner
            .act(GameAction::ChooseReplacement { index: 2 })
            .is_err(),
        "no acceptable index names the withheld Kismet effect"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice {
            kind: ReplacementChoiceKind::OptionalBranch,
            ..
        }
    ));

    // Decline the copy — CR 616.1f: Kismet is still applicable and applies.
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline the copy");

    let obj = &runner.state().objects[&battle];
    assert_eq!(obj.name, "Lazotep Convert");
    assert!(obj.transformed);
    assert!(
        obj.tapped,
        "CR 614.12 + CR 614.1d: Kismet must still apply to the entrant's back face"
    );
    assert_eq!(obj.controller, P0);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
