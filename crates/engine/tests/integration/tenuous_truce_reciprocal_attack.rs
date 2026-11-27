use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to_player;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ChosenAttribute;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::Zone;

const P2: PlayerId = PlayerId(2);
const TRUCE: &str = "Enchant opponent\nAt the beginning of enchanted opponent's end step, you and that player each draw a card.\nWhen you attack enchanted opponent or a planeswalker they control or when they attack you or a planeswalker you control, sacrifice this Aura.";
const JACE: &str = "+2: Each player draws a card.\n−1: Target player draws a card.\n−10: Target player mills twenty cards.";

#[test]
fn reciprocal_attack_requires_the_two_parties_and_fires_once_per_declaration() {
    for (controller, enchanted, defender, attack_player, attack_walker, expected) in [
        (P0, P1, P1, true, false, 1),
        (P0, P1, P1, false, true, 1),
        (P0, P1, P1, true, true, 1),
        (P1, P0, P1, true, false, 1),
        (P1, P0, P1, false, true, 1),
        (P1, P0, P1, true, true, 1),
        (P0, P1, P2, true, true, 0),
        (P1, P0, P2, true, true, 0),
        (P1, P2, P1, true, true, 0),
        (P1, P2, P2, true, true, 0),
    ] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let aura = add_truce(&mut scenario, controller);
        let walker = scenario
            .add_planeswalker_from_oracle(defender, "Jace Beleren", "Jace", 3, JACE)
            .id();
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let wolf = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
        let mut runner = scenario.build();
        let previous_host = [P0, P1, P2]
            .into_iter()
            .find(|player| *player != controller && *player != enchanted)
            .expect("third player");
        attach_truce(&mut runner, aura, previous_host);
        attach_truce(&mut runner, aura, enchanted);

        runner.advance_to_combat();
        let mut attacks = Vec::new();
        if attack_player {
            attacks.push((bear, AttackTarget::Player(defender)));
        }
        if attack_walker {
            attacks.push((wolf, AttackTarget::Planeswalker(walker)));
        }
        runner.declare_attackers(&attacks).expect("legal attackers");
        assert_eq!(
            runner.state().stack.iter().filter(|entry| entry.source_id == aura).count(),
            expected,
            "controller {controller:?}, enchanted {enchanted:?}, defender {defender:?}, player attack {attack_player}, planeswalker attack {attack_walker}"
        );
        assert_eq!(runner.state().objects[&aura].zone, Zone::Battlefield);
        if expected > 0 {
            runner.resolve_top();
            assert!(runner.state().stack.is_empty());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert_eq!(runner.state().objects[&aura].zone, Zone::Graveyard);
        }
    }
}

fn add_truce(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(controller, "Tenuous Truce", TRUCE)
        .with_subtypes(vec!["Aura"])
        .id()
}

fn attach_truce(runner: &mut GameRunner, aura: ObjectId, enchanted: PlayerId) {
    attach_to_player(runner.state_mut(), aura, enchanted);
    assert_eq!(
        runner.state().objects[&aura]
            .attached_to
            .and_then(|host| host.as_player()),
        Some(enchanted)
    );
}

#[test]
fn a_battle_protected_by_the_enchanted_player_does_not_break_the_truce() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let aura = add_truce(&mut scenario, P0);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let siege = scenario.add_creature(P0, "Invasion of Zendikar", 0, 0).id();
    let mut runner = scenario.build();
    let battle = runner.state_mut().objects.get_mut(&siege).unwrap();
    battle.card_types.core_types = vec![CoreType::Battle];
    battle.card_types.subtypes = vec!["Siege".into()];
    battle.base_card_types = battle.card_types.clone();
    battle.defense = Some(3);
    battle.base_defense = Some(3);
    battle.counters.insert(CounterType::Defense, 3);
    battle.chosen_attributes.push(ChosenAttribute::Player(P1));
    attach_truce(&mut runner, aura, P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bear, AttackTarget::Battle(siege))])
        .unwrap();
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&aura].zone, Zone::Battlefield);
}

#[test]
fn a_myriad_copy_enters_attacking_the_enchanted_player_without_breaking_the_truce() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let aura = add_truce(&mut scenario, P0);
    let viper = scenario
        .add_creature(P0, "Broodbirth Viper", 3, 3)
        .with_subtypes(vec!["Snake"])
        .from_oracle_text_with_keywords(
            &["Myriad"],
            "Myriad (Whenever this creature attacks, for each opponent other than defending player, you may create a token copy that's tapped and attacking that player or a planeswalker they control. Exile the tokens at end of combat.)
Whenever this creature deals combat damage to a player, you may draw a card.",
        )
        .id();
    let mut runner = scenario.build();
    attach_truce(&mut runner, aura, P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(viper, AttackTarget::Player(P2))])
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, viper);

    runner.resolve_top();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat is in progress");
    assert!(combat.attackers.iter().any(|attack| {
        attack.attack_target == AttackTarget::Player(P1)
            && runner.state().objects[&attack.object_id].is_token
    }));
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&aura].zone, Zone::Battlefield);
}

#[test]
fn truce_end_step_still_draws_for_its_controller_and_enchanted_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let aura = add_truce(&mut scenario, P1);
    let enchanted_draw = scenario.add_card_to_library_top(P0, "Forest");
    let controller_draw = scenario.add_card_to_library_top(P1, "Plains");
    let bystander_top = scenario.add_card_to_library_top(P2, "Island");
    let mut runner = scenario.build();
    attach_truce(&mut runner, aura, P0);

    runner.advance_to_end_step();
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, aura);
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&enchanted_draw].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&controller_draw].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&bystander_top].zone, Zone::Library);
    assert_eq!(runner.state().objects[&aura].zone, Zone::Battlefield);
}
