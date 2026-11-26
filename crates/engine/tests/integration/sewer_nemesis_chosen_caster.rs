use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ChoiceType;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::PlayerId;
use engine::types::Zone;

const ORACLE: &str = "As this creature enters, choose a player.
Sewer Nemesis's power and toughness are each equal to the number of cards in the chosen player's graveyard.
Whenever the chosen player casts a spell, that player mills a card.";
const WHITESUNS_PASSAGE: &str = "You gain 5 life.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";

fn cast_and_choose(runner: &mut GameRunner, nemesis: ObjectId, chosen: PlayerId) {
    runner.cast(nemesis).commit();
    runner.advance_until_stack_empty();
    // CR 614.1c: The controller makes the replacement-effect choice as Nemesis enters.
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::NamedChoice { player: P0, choice_type: ChoiceType::Player { .. }, options, .. }
            if options.contains(&chosen.0.to_string())
    ));
    runner
        .act(GameAction::ChooseOption {
            choice: chosen.0.to_string(),
        })
        .expect("enter choice must succeed");
}

#[test]
fn sewer_nemesis_triggers_and_mills_only_for_the_chosen_caster() {
    let chosen = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let nemesis = scenario
        .add_creature_to_hand_from_oracle(P0, "Sewer Nemesis", 0, 0, ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let unchosen_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Whitesun's Passage", true, WHITESUNS_PASSAGE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let other_opponent_spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Whitesun's Passage", true, WHITESUNS_PASSAGE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let chosen_spell = scenario
        .add_spell_to_hand_from_oracle(chosen, "Whitesun's Passage", true, WHITESUNS_PASSAGE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.add_creature_to_graveyard(chosen, "Chosen Graveyard Filler", 1, 1);
    scenario.with_graveyard(P1, &["Forest", "Mountain"]);
    let unchosen_top = scenario.add_card_to_library_top(P0, "Plains");
    let other_opponent_top = scenario.add_card_to_library_top(P1, "Forest");
    let chosen_next = scenario.add_card_to_library_top(chosen, "Island");
    let chosen_top = scenario.add_card_to_library_top(chosen, "Swamp");
    let mut runner = scenario.build();

    cast_and_choose(&mut runner, nemesis, chosen);
    assert_eq!(runner.state().objects[&nemesis].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&nemesis].chosen_player(),
        Some(chosen)
    );
    // CR 604.3a and 607.2d: The CDA uses the linked player's graveyard.
    let object = &runner.state().objects[&nemesis];
    assert_eq!((object.power, object.toughness), (Some(1), Some(1)));

    // CR 603.2 and 607.2d: A cast matches only if the caster is the chosen player.
    runner.cast(unchosen_spell).commit();
    assert_eq!(runner.state().stack.len(), 1);
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&chosen_top].zone, Zone::Library);

    runner.act(GameAction::PassPriority).expect("pass to P1");
    runner.cast(other_opponent_spell).commit();
    assert_eq!(runner.state().stack.len(), 1);
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&chosen_top].zone, Zone::Library);
    assert_eq!(
        runner.state().objects[&other_opponent_top].zone,
        Zone::Library
    );

    runner.act(GameAction::PassPriority).expect("pass to P1");
    runner.act(GameAction::PassPriority).expect("pass to P2");
    runner.cast(chosen_spell).commit();
    assert_eq!(runner.state().stack.len(), 2);
    assert_eq!(runner.state().stack.last().unwrap().source_id, nemesis);
    runner.resolve_top();

    assert_eq!(runner.state().stack.len(), 1);
    // CR 701.17a: The chosen caster mills exactly their top card.
    assert_eq!(runner.state().objects[&chosen_top].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&chosen_next].zone, Zone::Library);
    assert_eq!(runner.state().objects[&unchosen_top].zone, Zone::Library);
    assert_eq!(
        runner.state().objects[&other_opponent_top].zone,
        Zone::Library
    );
    let object = &runner.state().objects[&nemesis];
    assert_eq!((object.power, object.toughness), (Some(2), Some(2)));
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn sewer_nemeses_keep_separate_choices_including_their_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opponent_nemesis = scenario
        .add_creature_to_hand_from_oracle(P0, "Sewer Nemesis", 0, 0, ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let controller_nemesis = scenario
        .add_creature_to_hand_from_oracle(P0, "Sewer Nemesis", 0, 0, ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let controller_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Whitesun's Passage", true, WHITESUNS_PASSAGE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let opponent_spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Whitesun's Passage", true, WHITESUNS_PASSAGE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_graveyard(P0, &["Mountain"]);
    scenario.with_graveyard(P1, &["Island"]);
    let controller_top = scenario.add_card_to_library_top(P0, "Plains");
    let opponent_top = scenario.add_card_to_library_top(P1, "Swamp");
    let mut runner = scenario.build();

    cast_and_choose(&mut runner, opponent_nemesis, P1);
    cast_and_choose(&mut runner, controller_nemesis, P0);

    // CR 607.2d: Each permanent's trigger uses its own linked choice.
    runner.cast(controller_spell).commit();
    assert_eq!(runner.state().stack.len(), 2);
    assert_eq!(runner.state().stack[1].source_id, controller_nemesis);
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&controller_top].zone,
        Zone::Graveyard
    );
    assert_eq!(runner.state().objects[&opponent_top].zone, Zone::Library);
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());

    runner.act(GameAction::PassPriority).expect("pass to P1");
    runner.cast(opponent_spell).commit();
    assert_eq!(runner.state().stack.len(), 2);
    assert_eq!(runner.state().stack[1].source_id, opponent_nemesis);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&opponent_top].zone, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn sewer_nemesis_trigger_still_mills_after_the_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nemesis = scenario
        .add_creature_to_hand_from_oracle(P0, "Sewer Nemesis", 0, 0, ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Whitesun's Passage", true, WHITESUNS_PASSAGE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_graveyard(P1, &["Island"]);
    let chosen_top = scenario.add_card_to_library_top(P1, "Swamp");
    let mut runner = scenario.build();
    cast_and_choose(&mut runner, nemesis, P1);

    runner.act(GameAction::PassPriority).expect("pass to P1");
    runner.cast(spell).commit();
    assert_eq!(runner.state().stack.len(), 2);
    assert_eq!(runner.state().stack[1].source_id, nemesis);
    // P0 responds with the spell and the trigger both on the stack. CR 117.3c
    // gives the caster priority first; pass it to P0 if P1 still holds it.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ) {
        runner.act(GameAction::PassPriority).expect("pass to P0");
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));

    runner.cast(unsummon).target_object(nemesis).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&nemesis].zone, Zone::Hand);

    // CR 113.7a: Once triggered, the ability exists independently of its
    // source, and "that player" is the caster that matched the trigger.
    assert_eq!(runner.state().stack.len(), 2);
    runner.resolve_top();
    assert_eq!(runner.state().objects[&chosen_top].zone, Zone::Graveyard);
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
}
