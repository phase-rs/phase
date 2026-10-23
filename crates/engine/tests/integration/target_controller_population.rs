//! Controller-relative damage populations use the sole target's live controller.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{GameRestriction, PreventionAmount, ShieldKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
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
fn shieldmates_blessing_is_bypassed_without_spending_the_shield() {
    for prevent_disabled in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario.add_creature(P1, "Shielded target", 0, 9).id();
        let peer = scenario.add_creature(P1, "Peer", 0, 9).id();
        let blessing = scenario
            .add_spell_to_hand_from_oracle(P0, "Shieldmate's Blessing", true, BLESSING)
            .with_mana_cost(cost(vec![ManaCostShard::White], 0))
            .id();
        let shock = scenario
            .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
            .with_mana_cost(cost(vec![ManaCostShard::Red], 0))
            .id();
        let fff = spell(&mut scenario);
        let mut runner = scenario.build();
        add_mana(&mut runner, P0, ManaType::White, 1);
        runner
            .cast(blessing)
            .target_object(target)
            .resolve()
            .assert_zone(&[blessing], Zone::Graveyard);
        finished(&runner);
        assert!(
            runner
                .state()
                .pending_damage_replacements
                .iter()
                .any(|shield| {
                    shield.shield_kind
                        == ShieldKind::Prevention {
                            amount: PreventionAmount::Next(3),
                        }
                        && !shield.is_consumed
                }),
            "printed three-point shield installed"
        );
        if prevent_disabled {
            cast(&mut runner, fff, target, 3);
            // CR 615.12: unpreventable damage does not consume the existing shield.
            assert_eq!(runner.state().objects[&target].damage_marked, 3);
            assert_eq!(runner.state().objects[&peer].damage_marked, 1);
            assert!(
                runner
                    .state()
                    .pending_damage_replacements
                    .iter()
                    .any(|shield| {
                        shield.shield_kind
                            == ShieldKind::Prevention {
                                amount: PreventionAmount::Next(3),
                            }
                            && !shield.is_consumed
                    }),
                "unpreventable damage leaves all three prevention points available"
            );
        }
        add_mana(&mut runner, P0, ManaType::Red, 1);
        runner
            .cast(shock)
            .target_object(target)
            .resolve()
            .assert_zone(&[shock], Zone::Graveyard);
        finished(&runner);
        assert_eq!(
            runner.state().objects[&target].damage_marked,
            if prevent_disabled { 5 } else { 0 }
        );
        // CR 615.7 + CR 615.12: Shock consumes two points only in the shield-only control.
        let remaining = if prevent_disabled { 3 } else { 1 };
        assert!(runner
            .state()
            .pending_damage_replacements
            .iter()
            .any(|shield| {
                shield.shield_kind
                    == ShieldKind::Prevention {
                        amount: PreventionAmount::Next(remaining),
                    }
                    && !shield.is_consumed
            }));
        if prevent_disabled {
            runner.advance_to_upkeep();
            assert!(!disabled(&runner), "this-turn restriction expires");
            finished(&runner);
        }
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
