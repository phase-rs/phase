//! Controller-relative damage populations use the sole target's live controller.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCondition, AbilityCost, Effect, GameRestriction, TapCreaturesRequirement,
    TapCreaturesSelectionMode, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, CostResume, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Complete Oracle text verified in data/mtgjson/AtomicCards.json.
const FFF: &str = "Damage can't be prevented this turn. Fear, Fire, Foes! deals X damage to target creature and 1 damage to each other creature with the same controller.";
const RAY: &str = "Untap target creature an opponent controls and gain control of it until end of turn. That creature gains haste until end of turn. When you lose control of the creature, tap it.";
const MURDER: &str = "Destroy target creature.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";
const DEFENSE: &str = "Target creature you control gets +2/+2 and gains hexproof until end of turn. (It can't be the target of spells or abilities your opponents control.)";
const BLESSING: &str = "Prevent the next 3 damage that would be dealt to any target this turn.";
const SHOCK: &str = "Shock deals 2 damage to any target.";
const ALPHA_BRAWL: &str = "Target creature an opponent controls deals damage equal to its power to each other creature that player controls, then each of those creatures deals damage equal to its power to that creature.";

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

fn add_mana(runner: &mut GameRunner, player: PlayerId, color: ManaType, count: usize) {
    for _ in 0..count {
        runner
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

fn spell(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Fear, Fire, Foes!", false, FFF)
        .with_mana_cost(cost(vec![ManaCostShard::X, ManaCostShard::Red], 0))
        .id()
}

fn finished(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty(),
        "all instructions completed"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn disabled(runner: &GameRunner) -> bool {
    runner.state().restrictions.iter().any(|restriction| {
        matches!(
            restriction,
            GameRestriction::DamagePreventionDisabled { .. }
        )
    })
}

fn priority(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..runner.state().players.len() {
        match runner.state().waiting_for {
            WaitingFor::Priority { player: current } if current == player => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            ref other => panic!("expected priority, got {other:?}"),
        }
    }
    panic!("priority did not reach {player:?}");
}

fn cast(runner: &mut GameRunner, fff: ObjectId, target: ObjectId, x: u32) {
    add_mana(runner, P0, ManaType::Red, x as usize + 1);
    // CR 107.3a: X is announced through the real casting/payment pipeline.
    let outcome = runner.cast(fff).x(x).target_object(target).resolve();
    outcome.assert_zone(&[fff], Zone::Graveyard);
    finished(runner);
    assert!(disabled(runner), "the first printed instruction resolved");
}

fn population(target_controller: PlayerId, x: u32, players: u8) {
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(target_controller, "Head", 0, 9).id();
    let peers: Vec<_> = (0..players)
        .map(|index| {
            let controller = PlayerId(index);
            [
                scenario.add_creature(controller, "Peer A", 0, 9).id(),
                scenario.add_creature(controller, "Peer B", 0, 9).id(),
            ]
        })
        .collect();
    let fff = spell(&mut scenario);
    let mut runner = scenario.build();
    cast(&mut runner, fff, target, x);
    // CR 120.3e + CR 120.8: only X is marked on the target, including X=0.
    assert_eq!(
        runner.state().objects[&target].damage_marked,
        x,
        "target takes X once"
    );
    for (index, group) in peers.iter().enumerate() {
        let expected = u32::from(PlayerId(index as u8) == target_controller);
        for peer in group {
            assert_eq!(
                runner.state().objects[peer].damage_marked,
                expected,
                "peer {peer:?}, controller {index}"
            );
        }
    }
    assert_eq!(
        runner.state().objects[&peers[target_controller.0 as usize][0]].damage_marked,
        1,
        "positive reach: the matching population took damage"
    );
}

#[test]
fn opponent_population() {
    population(P1, 3, 2);
}

#[test]
fn own_target_population() {
    population(P0, 3, 2);
}

#[test]
fn zero_x_still_damages_other_creatures() {
    population(P1, 0, 2);
}

#[test]
fn multiplayer_population() {
    population(P1, 3, 3);
}

#[test]
fn live_controller_change_in_response() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Head owned by P1", 0, 9).id();
    let old_peer = scenario.add_creature(P1, "Old controller peer", 0, 9).id();
    let new_peer = scenario.add_creature(P0, "New controller peer", 0, 9).id();
    let ray = scenario
        .add_spell_to_hand_from_oracle(P0, "Ray of Command", true, RAY)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 3))
        .id();
    let fff = spell(&mut scenario);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Red, 4);
    add_mana(&mut runner, P0, ManaType::Blue, 4);
    let _ = runner.cast(fff).x(3).target_object(target).commit();
    let _ = runner.cast(ray).target_object(target).commit();
    runner.resolve_top();
    assert_eq!(runner.state().stack.len(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&ray].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&target].controller,
        P0,
        "Ray resolved first"
    );
    assert_eq!(runner.state().objects[&target].owner, P1);
    runner.resolve_top();
    finished(&runner);
    assert!(disabled(&runner));
    // CR 608.2h + CR 110.2: read current controller, not owner or announcement state.
    assert_eq!(runner.state().objects[&target].damage_marked, 3);
    assert_eq!(runner.state().objects[&new_peer].damage_marked, 1);
    assert_eq!(runner.state().objects[&old_peer].damage_marked, 0);
}

#[test]
fn removed_sole_target_prevents_entire_spell_resolution() {
    for (name, text, response_cost, color, destination) in [
        (
            "Murder",
            MURDER,
            cost(vec![ManaCostShard::Black, ManaCostShard::Black], 1),
            ManaType::Black,
            Zone::Graveyard,
        ),
        (
            "Unsummon",
            UNSUMMON,
            cost(vec![ManaCostShard::Blue], 0),
            ManaType::Blue,
            Zone::Hand,
        ),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario.add_creature(P1, "Head", 0, 9).id();
        let peer = scenario.add_creature(P1, "Peer", 0, 9).id();
        let response = scenario
            .add_spell_to_hand_from_oracle(P1, name, true, text)
            .with_mana_cost(response_cost)
            .id();
        let fff = spell(&mut scenario);
        let mut runner = scenario.build();
        add_mana(&mut runner, P0, ManaType::Red, 4);
        add_mana(&mut runner, P1, color, 3);
        let _ = runner.cast(fff).x(3).target_object(target).commit();
        priority(&mut runner, P1);
        let _ = runner.cast(response).target_object(target).commit();
        runner.resolve_top();
        assert_eq!(runner.state().stack.len(), 1);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(runner.state().objects[&response].zone, Zone::Graveyard);
        assert_eq!(
            runner.state().objects[&target].zone,
            destination,
            "response reached the target"
        );
        runner.resolve_top();
        finished(&runner);
        assert_eq!(runner.state().objects[&fff].zone, Zone::Graveyard);
        // CR 608.2b: no legal targets means neither the restriction nor splash resolves.
        assert!(!disabled(&runner));
        assert_eq!(runner.state().objects[&peer].damage_marked, 0);
    }
    population(P1, 3, 2);
}

#[test]
fn hexproof_response_makes_the_only_target_illegal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Head", 0, 9).id();
    let peer = scenario.add_creature(P1, "Peer", 0, 9).id();
    let defense = scenario
        .add_spell_to_hand_from_oracle(P1, "Blossoming Defense", true, DEFENSE)
        .with_mana_cost(cost(vec![ManaCostShard::Green], 0))
        .id();
    let fff = spell(&mut scenario);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Red, 4);
    add_mana(&mut runner, P1, ManaType::Green, 1);
    let _ = runner.cast(fff).x(3).target_object(target).commit();
    priority(&mut runner, P1);
    let _ = runner.cast(defense).target_object(target).commit();
    runner.resolve_top();
    assert_eq!(runner.state().stack.len(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&defense].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&target].power,
        Some(2),
        "Defense applied its printed boost"
    );
    runner.resolve_top();
    finished(&runner);
    assert_eq!(runner.state().objects[&fff].zone, Zone::Graveyard);
    // CR 608.2b: the sole target is illegal, so the whole spell fails to resolve.
    assert!(!disabled(&runner));
    assert_eq!(runner.state().objects[&target].damage_marked, 0);
    assert_eq!(runner.state().objects[&peer].damage_marked, 0);
    population(P1, 3, 2);
}

#[test]
fn lethal_head_keeps_its_identity_until_the_splash_finishes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Head", 0, 2).id();
    let peer = scenario.add_creature(P1, "Peer", 0, 9).id();
    let unrelated = scenario.add_creature(P0, "Unrelated", 0, 9).id();
    let fff = spell(&mut scenario);
    let mut runner = scenario.build();
    cast(&mut runner, fff, target, 3);
    // CR 704.4 + CR 120.5: lethal damage does not remove the head mid-resolution.
    assert_eq!(runner.state().objects[&target].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&peer].damage_marked, 1);
    assert_eq!(runner.state().objects[&unrelated].damage_marked, 0);
}

#[test]
fn empty_population_still_resolves_the_head() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Only creature", 0, 9).id();
    let fff = spell(&mut scenario);
    let mut runner = scenario.build();
    cast(&mut runner, fff, target, 3);
    assert_eq!(runner.state().objects[&target].damage_marked, 3);
}

#[test]
fn shieldmates_blessing_prevents_three_damage_and_fff_bypasses_it_for_the_turn() {
    fn remaining_prevention_capacity(runner: &GameRunner, target: ObjectId) -> u32 {
        use engine::types::ability::{PreventionAmount, ShieldKind};

        // CR 615.7: only unconsumed finite shields contribute remaining capacity.
        runner.state().objects[&target]
            .replacement_definitions
            .iter_unchecked()
            .filter(|replacement| !replacement.is_consumed)
            .filter_map(|replacement| {
                if let ShieldKind::Prevention {
                    amount: PreventionAmount::Next(amount),
                } = replacement.shield_kind
                {
                    Some(amount)
                } else {
                    None
                }
            })
            .sum()
    }

    for prevent_disabled in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario.add_creature(P1, "Shielded target", 0, 9).id();
        let peer = scenario.add_creature(P1, "Unshielded peer", 0, 9).id();
        let [blessing, fresh_blessing] = [(); 2].map(|()| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Shieldmate's Blessing", true, BLESSING)
                .with_mana_cost(cost(vec![ManaCostShard::White], 0))
                .id()
        });
        let [peer_shock, first_shock, second_shock, expired_shield_shock, fresh_shield_shock] =
            [(); 5].map(|()| {
                scenario
                    .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
                    .with_mana_cost(cost(vec![ManaCostShard::Red], 0))
                    .id()
            });
        let fff = spell(&mut scenario);
        let mut runner = scenario.build();
        add_mana(&mut runner, P0, ManaType::White, 1);
        runner
            .cast(blessing)
            .target_object(target)
            .resolve()
            .assert_zone(&[blessing], Zone::Graveyard);
        finished(&runner);
        // CR 615.7: Blessing installs its printed three-point prevention shield.
        assert_eq!(remaining_prevention_capacity(&runner, target), 3);

        add_mana(&mut runner, P0, ManaType::Red, 1);
        runner
            .cast(peer_shock)
            .target_object(peer)
            .resolve()
            .assert_zone(&[peer_shock], Zone::Graveyard);
        finished(&runner);
        // CR 615.7: the shield protects only the selected recipient, not its peers.
        assert_eq!(runner.state().objects[&peer].damage_marked, 2);
        assert_eq!(runner.state().objects[&target].damage_marked, 0);
        assert_eq!(remaining_prevention_capacity(&runner, target), 3);

        if prevent_disabled {
            cast(&mut runner, fff, target, 3);
            // CR 615.12: unpreventable damage neither is prevented nor reduces the shield.
            assert_eq!(runner.state().objects[&target].damage_marked, 3);
            assert_eq!(runner.state().objects[&peer].damage_marked, 3);
            assert_eq!(remaining_prevention_capacity(&runner, target), 3);
        }
        for (shock, expected_damage, expected_capacity) in [
            (
                first_shock,
                if prevent_disabled { 5 } else { 0 },
                if prevent_disabled { 3 } else { 1 },
            ),
            (
                second_shock,
                if prevent_disabled { 7 } else { 1 },
                if prevent_disabled { 3 } else { 0 },
            ),
        ] {
            add_mana(&mut runner, P0, ManaType::Red, 1);
            runner
                .cast(shock)
                .target_object(target)
                .resolve()
                .assert_zone(&[shock], Zone::Graveyard);
            finished(&runner);
            // CR 615.7 + CR 615.12: the control prevents exactly three of four
            // damage across two Shocks; FFF makes both follow-up Shocks unpreventable.
            // Only prevented damage depletes the shield; unpreventable damage leaves it intact.
            assert_eq!(
                remaining_prevention_capacity(&runner, target),
                expected_capacity
            );
            assert_eq!(
                runner.state().objects[&target].damage_marked,
                expected_damage
            );
            assert_eq!(
                runner.state().objects[&peer].damage_marked,
                if prevent_disabled { 3 } else { 2 }
            );
        }

        runner.advance_to_upkeep();
        assert_eq!(runner.state().phase, Phase::Upkeep);
        finished(&runner);
        // CR 514.2: cleanup removes marked damage and ends both this-turn effects.
        assert_eq!(runner.state().objects[&target].damage_marked, 0);
        assert_eq!(runner.state().objects[&peer].damage_marked, 0);
        assert!(!disabled(&runner), "this-turn restriction expires");
        assert_eq!(remaining_prevention_capacity(&runner, target), 0);
        add_mana(&mut runner, P0, ManaType::Red, 1);
        priority(&mut runner, P0);
        runner
            .cast(expired_shield_shock)
            .target_object(target)
            .resolve()
            .assert_zone(&[expired_shield_shock], Zone::Graveyard);
        finished(&runner);
        assert_eq!(
            runner.state().objects[&target].damage_marked,
            2,
            "the previous turn's shield no longer prevents damage"
        );

        add_mana(&mut runner, P0, ManaType::White, 1);
        priority(&mut runner, P0);
        runner
            .cast(fresh_blessing)
            .target_object(target)
            .resolve()
            .assert_zone(&[fresh_blessing], Zone::Graveyard);
        finished(&runner);
        // CR 615.7: a newly resolved Blessing installs a fresh three-point shield.
        assert_eq!(remaining_prevention_capacity(&runner, target), 3);
        add_mana(&mut runner, P0, ManaType::Red, 1);
        priority(&mut runner, P0);
        runner
            .cast(fresh_shield_shock)
            .target_object(target)
            .resolve()
            .assert_zone(&[fresh_shield_shock], Zone::Graveyard);
        finished(&runner);
        // CR 514.2 + CR 615.7: fresh prevention works after FFF's prohibition ends.
        assert_eq!(
            runner.state().objects[&target].damage_marked,
            2,
            "a fresh shield prevents the new Shock after cleanup"
        );
        assert_eq!(remaining_prevention_capacity(&runner, target), 1);
    }
}

#[test]
fn alpha_brawl_preserves_source_relative_other_without_self_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P1, "Brawler", 3, 9).id();
    let peer = scenario.add_creature(P1, "Peer", 2, 9).id();
    let unrelated = scenario.add_creature(P0, "Unrelated", 1, 9).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Alpha Brawl", false, ALPHA_BRAWL)
        .with_mana_cost(cost(vec![ManaCostShard::Red, ManaCostShard::Red], 6))
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Red, 8);
    let outcome = runner.cast(spell).target_object(source).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    finished(&runner);
    // CR 120.2b: the chosen creature, not the sorcery, deals the first damage.
    assert!(outcome.events().iter().any(|event| matches!(event,
        GameEvent::DamageDealt { source_id, target: TargetRef::Object(id), amount: 3, .. }
        if *source_id == source && *id == peer)));
    assert!(!outcome.events().iter().any(|event| matches!(event,
        GameEvent::DamageDealt { source_id, target: TargetRef::Object(id), .. }
        if *source_id == source && *id == source)));
    assert_eq!(runner.state().objects[&peer].damage_marked, 3);
    assert_eq!(runner.state().objects[&source].damage_marked, 2);
    assert_eq!(runner.state().objects[&unrelated].damage_marked, 0);
}

// Synthetic Oracle probes mirror the parser's independent-body fixtures.
const ABILITY_DAMAGE: &str = "~ deals 2 damage to target creature and 1 damage to each other creature with the same controller.";

fn ability_population(scenario: &mut GameScenario) -> [ObjectId; 3] {
    [
        scenario.add_creature(P1, "Selected creature", 0, 9).id(),
        scenario.add_creature(P0, "P0 population", 0, 9).id(),
        scenario.add_creature(P1, "P1 population", 0, 9).id(),
    ]
}

fn assert_ability_population(
    runner: &GameRunner,
    target: ObjectId,
    hit: ObjectId,
    spared: ObjectId,
) {
    finished(runner);
    // CR 120.3e: the targeted instruction marks two; "other" excludes it from splash.
    assert_eq!(runner.state().objects[&target].damage_marked, 2);
    assert_eq!(
        runner.state().objects[&hit].damage_marked,
        1,
        "positive reach: the target controller's other creature took splash"
    );
    assert_eq!(
        runner.state().objects[&spared].damage_marked,
        0,
        "the other controller's creature is outside the population"
    );
}

fn choose_trigger_population_target(runner: &mut GameRunner, source: ObjectId, target: ObjectId) {
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("dispatch the queued trigger");
    }
    // CR 603.3d: announce the creature target when this trigger goes on the stack.
    let WaitingFor::TriggerTargetSelection {
        player,
        source_id,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected trigger target announcement, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, Some(source));
    assert_eq!(target_slots.len(), 1, "the splash has no additional target");
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(target)),
        "the selected creature is offered by the trigger's target prompt"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the trigger being announced is on the stack"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("the selected creature is a legal trigger target");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the targeted trigger is on the stack"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn activate_population(runner: &mut GameRunner, source: ObjectId, target: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate the parsed tap ability");
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected activation target announcement, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(target_slots.len(), 1);
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(target)),
        "the selected creature is offered by the activation's target prompt"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("choose the activation's creature target");
    assert!(
        runner.state().objects[&source].tapped,
        "the tap cost was paid"
    );
    assert_eq!(runner.state().stack.len(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    runner.advance_until_stack_empty();
}

#[test]
fn activated_ability_runtime_population() {
    // oracle.rs: parse_activated_ability_ir_with_self_ref_fallback starts the body.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let [target, own_peer, opposing_peer] = ability_population(&mut scenario);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Activated Population Probe",
            0,
            9,
            &format!("{{T}}: {ABILITY_DAMAGE}"),
        )
        .id();
    let mut runner = scenario.build();
    activate_population(&mut runner, source, target);
    assert_ability_population(&runner, target, opposing_peer, own_peer);
    assert_eq!(runner.state().objects[&source].damage_marked, 0);
}

#[test]
fn granted_activated_ability_runtime_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let [target, own_peer, opposing_peer] = ability_population(&mut scenario);
    let source = scenario
        .add_creature(P0, "Granted Ability Recipient", 0, 9)
        .id();
    // oracle_static/grammar.rs:1761 starts a root at the quoted cost/effect separator.
    scenario.add_enchantment_from_oracle(
        P0,
        "Population Grant",
        &format!("Creatures you control have \"{{T}}: {ABILITY_DAMAGE}\""),
    );
    let mut runner = scenario.build();
    // Scenario seeding does not evaluate continuous grants. A real priority
    // action runs the layer engine before the recipient can activate its grant.
    runner
        .act(GameAction::PassPriority)
        .expect("apply the continuous grant");
    priority(&mut runner, P0);
    activate_population(&mut runner, source, target);
    assert_ability_population(&runner, target, opposing_peer, own_peer);
    assert_eq!(runner.state().objects[&source].damage_marked, 0);
}

fn upkeep_population(override_sentence: &str) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let [target, own_peer, opposing_peer] = ability_population(&mut scenario);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Upkeep Population Probe",
            0,
            9,
            &format!("At the beginning of your upkeep, {ABILITY_DAMAGE}{override_sentence}"),
        )
        .id();
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    choose_trigger_population_target(&mut runner, source, target);
    runner.advance_until_stack_empty();
    assert_ability_population(&runner, target, opposing_peer, own_peer);
    assert_eq!(runner.state().objects[&source].damage_marked, 0);
}

#[test]
fn upkeep_trigger_runtime_population() {
    // oracle_trigger.rs:2054: ordinary DoesNotResolve trigger-body branch.
    upkeep_population("");
}

#[test]
fn still_resolving_trigger_runtime_population() {
    // oracle_trigger.rs:2064: StillResolves parses the stripped body on its own context.
    upkeep_population(" This ability still resolves if its target becomes illegal.");
}

fn in_chain_reflexive_clause_population(change_controller: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let [target, own_peer, opposing_peer] = ability_population(&mut scenario);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "In-chain Reflexive Population Probe",
            0,
            9,
            &format!(
                "At the beginning of your upkeep, you may pay {{R}}. When you do, {ABILITY_DAMAGE}"
            ),
        )
        .id();
    let ray = scenario
        .add_spell_to_hand_from_oracle(P0, "Ray of Command", true, RAY)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 3))
        .id();
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    add_mana(&mut runner, P0, ManaType::Red, 1);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the parent upkeep ability exists"
    );
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if source_id == source
        ),
        "the parent offers payment before the reflexive trigger has targets: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("pay the parent ability's red mana cost");
    // CR 603.12 + CR 603.3d: payment finishes the parent; only then announce
    // the separate reflexive trigger's target, not a target of the parent.
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("finish the parent and enqueue its reflexive trigger");
    }
    assert_eq!(
        runner.state().objects[&target].damage_marked,
        0,
        "the reflexive trigger has not resolved while announcing its target"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .units()
            .count(),
        0,
        "the parent actually paid"
    );
    choose_trigger_population_target(&mut runner, source, target);
    if change_controller {
        add_mana(&mut runner, P0, ManaType::Blue, 4);
        priority(&mut runner, P0);
        let _ = runner.cast(ray).target_object(target).commit();
        runner.resolve_top();
        assert_eq!(runner.state().objects[&ray].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&target].controller, P0);
        assert_eq!(runner.state().objects[&target].owner, P1);
        assert_eq!(
            runner.state().stack.len(),
            1,
            "the reflexive trigger waits below Ray"
        );
    }
    runner.advance_until_stack_empty();
    // CR 608.2h: determine the population using current information on resolution.
    let (hit, spared) = if change_controller {
        (own_peer, opposing_peer)
    } else {
        (opposing_peer, own_peer)
    };
    assert_ability_population(&runner, target, hit, spared);
    assert_eq!(
        runner.state().objects[&source].damage_marked,
        u32::from(change_controller)
    );
}

#[test]
fn in_chain_reflexive_clause_runtime_population() {
    // oracle_trigger.rs:2054: mana payment stays in the plain effect-chain body;
    // split_reflexive_optional_payment rejects a standalone mana cost.
    in_chain_reflexive_clause_population(false);
}

#[test]
fn in_chain_reflexive_clause_reads_current_controller_on_resolution() {
    in_chain_reflexive_clause_population(true);
}

fn split_reflexive_population(change_controller: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let [target, own_peer, opposing_peer] = ability_population(&mut scenario);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Split Reflexive Population Probe",
            0,
            9,
            &format!(
                "At the beginning of your upkeep, you may tap two untapped creatures you control. When you do, {ABILITY_DAMAGE}"
            ),
        )
        .id();
    let ray = scenario
        .add_spell_to_hand_from_oracle(P0, "Ray of Command", true, RAY)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 3))
        .id();
    let mut runner = scenario.build();

    // SHAPE reach guard: this is the split PayCost parent and reflexive child,
    // not an optional clause lowered through the plain effect-chain branch.
    let triggers = &runner.state().objects[&source].trigger_definitions;
    assert_eq!(triggers.len(), 1);
    let parent = triggers[0]
        .definition
        .execute
        .as_deref()
        .expect("upkeep payment parent");
    assert!(matches!(
        parent.effect.as_ref(),
        Effect::PayCost {
            cost: AbilityCost::TapCreatures {
                requirement: TapCreaturesRequirement::Count { count: 2 },
                ..
            },
            ..
        }
    ));
    assert!(matches!(
        parent
            .sub_ability
            .as_deref()
            .expect("split reflexive child")
            .condition,
        Some(AbilityCondition::WhenYouDo)
    ));

    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(runner.state().stack.len(), 1, "only the parent is stacked");
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if source_id == source
        ),
        "the parent offers its optional cost before announcing any target: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the parent's tap payment");
    let WaitingFor::PayCost {
        player,
        kind,
        choices,
        count,
        min_count,
        resume,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected resolution tap payment, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(
        *kind,
        PayCostKind::TapCreatures {
            mode: TapCreaturesSelectionMode::Fixed
        }
    );
    assert!(matches!(resume, CostResume::Resolution));
    assert_eq!((*count, *min_count), (2, 2));
    assert!(choices.contains(&source) && choices.contains(&own_peer));
    assert!(!runner.state().objects[&source].tapped);
    assert!(!runner.state().objects[&own_peer].tapped);
    runner
        .act(GameAction::SelectCards {
            cards: vec![source, own_peer],
        })
        .expect("pay by tapping the two selected creatures");
    assert!(runner.state().objects[&source].tapped);
    assert!(runner.state().objects[&own_peer].tapped);
    assert_eq!(
        runner.state().objects[&target].damage_marked,
        0,
        "payment does not resolve the reflexive damage"
    );
    // CR 603.12 + CR 603.3d: only after the parent resolves does its separate
    // reflexive trigger go on the stack and announce its creature target.
    choose_trigger_population_target(&mut runner, source, target);
    if change_controller {
        add_mana(&mut runner, P0, ManaType::Blue, 4);
        priority(&mut runner, P0);
        let _ = runner.cast(ray).target_object(target).commit();
        runner.resolve_top();
        assert_eq!(runner.state().objects[&ray].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&target].controller, P0);
        assert_eq!(runner.state().objects[&target].owner, P1);
        assert_eq!(
            runner.state().stack.len(),
            1,
            "the reflexive trigger waits below Ray"
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
    runner.advance_until_stack_empty();
    // CR 608.2h: read the target's current controller when applying the damage.
    let (hit, spared) = if change_controller {
        (own_peer, opposing_peer)
    } else {
        (opposing_peer, own_peer)
    };
    assert_ability_population(&runner, target, hit, spared);
    assert_eq!(
        runner.state().objects[&source].damage_marked,
        u32::from(change_controller)
    );
}

#[test]
fn split_reflexive_trigger_runtime_population() {
    // oracle_trigger.rs:1940: split_reflexive_optional_payment's independent body.
    split_reflexive_population(false);
}

#[test]
fn split_reflexive_trigger_reads_current_controller_on_resolution() {
    split_reflexive_population(true);
}

#[test]
fn exert_reflexive_trigger_runtime_population() {
    // oracle.rs:6894: the exert payoff is an independent "When you do" body.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let [target, own_peer, opposing_peer] = ability_population(&mut scenario);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Exert Population Probe",
            0,
            9,
            &format!("You may exert ~ as it attacks. When you do, {ABILITY_DAMAGE}"),
        )
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(source, AttackTarget::Player(P1))])
        .expect("attack with the exert creature");
    // CR 701.43d: pay the optional attack cost through its real combat prompt.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ExertChoice { player: P0, attacker, .. } if attacker == source
    ));
    runner
        .act(GameAction::ChooseExert { exert: true })
        .expect("exert as this creature attacks");
    assert!(runner.state().exerted_this_turn.contains(&source));
    choose_trigger_population_target(&mut runner, source, target);
    runner.advance_until_stack_empty();
    assert_ability_population(&runner, target, opposing_peer, own_peer);
    assert_eq!(runner.state().objects[&source].damage_marked, 0);
}
