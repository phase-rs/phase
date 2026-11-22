//! CR 701.20a + CR 608.2c + CR 120.3: Goblin Charbelcher — "{3}, {T}: Reveal
//! cards from the top of your library until you reveal a land card. This
//! artifact deals damage equal to the number of nonland cards revealed this way
//! to any target. If the revealed land card was a Mountain, this artifact deals
//! double that damage instead. Put the revealed cards on the bottom of your
//! library in any order."
//!
//! The revealed land card is NOT put into a hand: every revealed card, the land
//! included, goes to the library bottom.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::{
    filter_events_for_viewer, filter_state_for_unseated_viewer, filter_state_for_viewer,
    HIDDEN_CARD_NAME,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CHARBELCHER: &str = "{3}, {T}: Reveal cards from the top of your library until you reveal a land card. Goblin Charbelcher deals damage equal to the number of nonland cards revealed this way to any target. If the revealed land card was a Mountain, Goblin Charbelcher deals double that damage instead. Put the revealed cards on the bottom of your library in any order.";

/// A library staged top-first as `nonland_count` nonland cards, then a land with
/// `land_subtype`, then one untouched card. Returns the revealed cards in
/// encounter order plus the untouched card.
fn stage(
    nonland_count: usize,
    land_name: &str,
    land_subtype: &str,
) -> (GameRunner, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Goblin Charbelcher", CHARBELCHER);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );

    // `library[0]` is the top: stage bottom-up.
    let deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let land = scenario
        .add_spell_to_library_top(P0, land_name, false)
        .as_land()
        .with_subtypes(vec![land_subtype])
        .id();
    let mut revealed = vec![land];
    for i in 0..nonland_count {
        let spell = scenario
            .add_spell_to_library_top(P0, &format!("Spell {i}"), false)
            .id();
        revealed.insert(0, spell);
    }
    (scenario.build(), revealed, deep)
}

fn charbelcher(runner: &GameRunner) -> ObjectId {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Goblin Charbelcher")
        .expect("Goblin Charbelcher on the battlefield")
}

/// Answer the library-bottom ordering prompt with the given order, then let the
/// stack empty. A pile of several cards must raise the prompt; a single card has
/// nothing to arrange.
fn finish(runner: &mut GameRunner, order: &[ObjectId]) {
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, .. } => {
            assert_eq!(
                cards.len(),
                order.len(),
                "the whole remaining pile is ordered"
            );
            runner
                .act(GameAction::SelectCards {
                    cards: order.to_vec(),
                })
                .expect("bottom order is accepted");
        }
        other => assert!(
            order.len() <= 1,
            "a pile of several cards must be offered for arrangement, got {other:?}"
        ),
    }
    runner.advance_until_stack_empty();
}

/// Activate targeting P1 and answer the library-bottom ordering prompt.
fn activate_and_finish(runner: &mut GameRunner, revealed: &[ObjectId]) {
    let source = charbelcher(runner);
    runner.activate(source, 0).target_player(P1).resolve();
    finish(runner, revealed);
}

fn assert_all_on_bottom_and_none_in_hand(
    runner: &GameRunner,
    revealed: &[ObjectId],
    deep: ObjectId,
) {
    let library: Vec<ObjectId> = runner.state().players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    let mut expected = vec![deep];
    expected.extend_from_slice(revealed);
    assert_eq!(
        library, expected,
        "the revealed cards, land included, go to the bottom of the library"
    );
    for id in revealed {
        assert_eq!(runner.state().objects[id].zone, Zone::Library);
    }
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "no revealed card is put into a hand"
    );
}

#[test]
fn deals_damage_equal_to_nonland_cards_revealed() {
    let (mut runner, revealed, deep) = stage(3, "Forest", "Forest");
    let life_before = runner.state().players[P1.0 as usize].life;

    activate_and_finish(&mut runner, &revealed);

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before - 3,
        "three nonland cards were revealed before the Forest"
    );
    assert_all_on_bottom_and_none_in_hand(&runner, &revealed, deep);
}

#[test]
fn mountain_doubles_the_damage() {
    let (mut runner, revealed, deep) = stage(3, "Mountain", "Mountain");
    let life_before = runner.state().players[P1.0 as usize].life;

    activate_and_finish(&mut runner, &revealed);

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before - 6,
        "a revealed Mountain doubles the three nonland cards' damage"
    );
    assert_all_on_bottom_and_none_in_hand(&runner, &revealed, deep);
}

#[test]
fn land_on_top_reveals_only_the_land_and_deals_no_damage() {
    let (mut runner, revealed, deep) = stage(0, "Mountain", "Mountain");
    let life_before = runner.state().players[P1.0 as usize].life;

    activate_and_finish(&mut runner, &revealed);

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before,
        "zero nonland cards revealed: zero damage, doubled or not"
    );
    assert_all_on_bottom_and_none_in_hand(&runner, &revealed, deep);
}

const SWANS: &str = "If a source would deal damage to this creature, prevent that damage. The source's controller draws cards equal to the damage prevented this way.";

/// CR 608.2c + CR 615.5: the revealed pile is put on the bottom AFTER the damage
/// instruction and its replacement have been carried out. Swans of Bryn Argoll
/// prevents the damage and draws that many cards at once; with the pile still in
/// place the draw takes the three revealed nonland cards from the top. Placing
/// the pile first would bury them and draw the deep card instead.
#[test]
fn pile_is_placed_after_damage_and_its_replacement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Goblin Charbelcher", CHARBELCHER);
    let swans = scenario
        .add_creature_from_oracle(P1, "Swans of Bryn Argoll", 4, 3, SWANS)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let forest = scenario
        .add_spell_to_library_top(P0, "Forest", false)
        .as_land()
        .with_subtypes(vec!["Forest"])
        .id();
    let mut drawn = Vec::new();
    for i in 0..3 {
        let spell = scenario
            .add_spell_to_library_top(P0, &format!("Spell {i}"), false)
            .id();
        drawn.insert(0, spell);
    }
    let mut runner = scenario.build();

    let source = charbelcher(&runner);
    runner.activate(source, 0).target_object(swans).resolve();

    // Damage and the Swans replacement have already run: the three nonland cards
    // are in hand, and only the land remains to be placed. Nothing was ordered
    // onto the bottom before the damage.
    let mut hand: Vec<ObjectId> = runner.state().players[P0.0 as usize]
        .hand
        .iter()
        .copied()
        .collect();
    hand.sort();
    let mut expected = drawn.clone();
    expected.sort();
    assert_eq!(
        hand, expected,
        "the Swans draw takes the revealed nonland cards"
    );
    assert_eq!(
        runner.state().objects[&swans].damage_marked,
        0,
        "the damage was prevented by Swans's replacement"
    );

    finish(&mut runner, &[forest]);
    let library: Vec<ObjectId> = runner.state().players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(
        library,
        vec![deep, forest],
        "the revealed land is bottomed last"
    );
}

/// CR 401.2 + CR 701.20a: the revealed set is the engine's own "revealed this
/// way" population, in reveal order. Its members end in hidden library
/// positions, so no audience's serialized state may carry their ids — while the
/// engine's own copy keeps the full population for its quantity readers.
#[test]
fn revealed_population_is_not_exposed_in_any_audiences_payload() {
    let (mut runner, revealed, _deep) = stage(3, "Forest", "Forest");
    activate_and_finish(&mut runner, &revealed);

    let engine_set_members: Vec<ObjectId> = runner
        .state()
        .tracked_object_sets
        .values()
        .flatten()
        .copied()
        .collect();
    for id in &revealed {
        assert!(
            engine_set_members.contains(id),
            "reach guard: the engine keeps the full revealed population"
        );
    }

    for (label, projected) in [
        ("P0", filter_state_for_viewer(runner.state(), P0)),
        ("P1", filter_state_for_viewer(runner.state(), P1)),
        ("unseated", filter_state_for_unseated_viewer(runner.state())),
    ] {
        let payload = serde_json::to_value(&projected).expect("projected state serializes");
        let carried: Vec<u64> = payload["tracked_object_sets"]
            .as_object()
            .expect("tracked sets serialize as a map")
            .values()
            .flat_map(|members| members.as_array().expect("members").iter())
            .filter_map(serde_json::Value::as_u64)
            .collect();
        for id in &revealed {
            assert!(
                !carried.contains(&id.0),
                "{label}: tracked_object_sets carries the library card {id:?}"
            );
        }
    }
}

const ABUNDANCE: &str = "If you would draw a card, you may instead choose land or nonland and reveal cards from the top of your library until you reveal a card of the chosen kind. Put that card into your hand and put all other cards revealed this way on the bottom of your library in any order.";

/// CR 608.2c + CR 615.5: Swans of Bryn Argoll's prevention draw is replaced by
/// Abundance, whose own reveal-until (with its own choices) runs mid-resolution.
/// Charbelcher's pile placement must still move ITS revealed cards: the child
/// reveals a different population (the first nonland card only), puts it in hand,
/// and the parent's leftover revealed land is then bottomed.
#[test]
fn replacement_child_reveal_does_not_replace_the_parents_pile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Goblin Charbelcher", CHARBELCHER);
    scenario.add_enchantment_from_oracle(P0, "Abundance", ABUNDANCE);
    let swans = scenario
        .add_creature_from_oracle(P1, "Swans of Bryn Argoll", 4, 3, SWANS)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let deep = scenario
        .add_spell_to_library_top(P0, "Deep Spell", false)
        .id();
    let forest = scenario
        .add_spell_to_library_top(P0, "Forest", false)
        .as_land()
        .with_subtypes(vec!["Forest"])
        .id();
    let spell = scenario.add_spell_to_library_top(P0, "Spell A", false).id();
    let mut runner = scenario.build();
    let forest_incarnation_before = runner.state().objects[&forest].incarnation;

    let source = charbelcher(&runner);
    runner.activate(source, 0).target_object(swans).resolve();

    // CR 701.20a: the Forest is still revealed to every audience while the
    // intervening prompts of the same resolution are open. It is still relevant
    // to Charbelcher's pending "put the revealed cards on the bottom"
    // instruction, so hiding it now would also leave its old occurrence for
    // that reorder to retire.
    let forest_visible_to =
        |runner: &GameRunner, viewer: Option<engine::types::player::PlayerId>| {
            let projected = match viewer {
                Some(viewer) => filter_state_for_viewer(runner.state(), viewer),
                None => filter_state_for_unseated_viewer(runner.state()),
            };
            projected.objects[&forest].name != HIDDEN_CARD_NAME
        };
    let assert_forest_revealed_at = |runner: &GameRunner, prompt: &str| {
        for (label, viewer) in [("P0", Some(P0)), ("P1", Some(P1)), ("unseated", None)] {
            assert!(
                forest_visible_to(runner, viewer),
                "{label}: the Forest is still revealed at the {prompt} prompt"
            );
        }
    };

    // Reach guards: the interactive replacement path actually ran.
    let mut saw_replacement_choice = false;
    let mut saw_kind_choice = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                saw_replacement_choice = true;
                assert_forest_revealed_at(&runner, "replacement");
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("accept Abundance's optional replacement");
            }
            WaitingFor::NamedChoice { .. } => {
                saw_kind_choice = true;
                assert_forest_revealed_at(&runner, "named");
                runner
                    .act(GameAction::ChooseOption {
                        choice: "Nonland".to_string(),
                    })
                    .expect("choose Nonland");
            }
            WaitingFor::RevealUntilBottomOrder { cards, .. }
            | WaitingFor::EffectZoneChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("bottom order is accepted");
            }
            _ => break,
        }
        runner.advance_until_stack_empty();
    }
    assert!(
        saw_replacement_choice,
        "Abundance's optional replacement was offered"
    );
    assert!(saw_kind_choice, "the land-or-nonland choice was made");

    // CR 701.20d: the Forest was reordered to the bottom by Charbelcher's own
    // placement, so it stops being revealed and becomes a new object. Without
    // the reveal surviving the prompts above there is no membership to retire
    // and the old occurrence would survive the move.
    assert!(
        runner.state().objects[&forest].incarnation > forest_incarnation_before,
        "the reordered Forest is a new object, not its pre-reorder occurrence"
    );
    assert!(
        !runner.state().revealed_cards.contains(&forest),
        "the reordered Forest stops being revealed"
    );

    let state = runner.state();
    let hand: Vec<ObjectId> = state.players[P0.0 as usize].hand.iter().copied().collect();
    assert_eq!(
        hand,
        vec![spell],
        "Abundance's reveal puts the first nonland card into hand in place of the draw"
    );
    let library: Vec<ObjectId> = state.players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(
        library,
        vec![deep, forest],
        "Charbelcher's leftover revealed land is still put on the bottom"
    );
}

/// CR 608.2c + CR 401.4: "in any order" — with several revealed cards left to
/// place, the controller is offered the arrangement and the submitted order, not
/// the encounter order, decides the library bottom. The deeper card is untouched.
#[test]
fn the_pile_is_arranged_in_the_submitted_order() {
    let (mut runner, revealed, deep) = stage(3, "Forest", "Forest");
    let source = charbelcher(&runner);
    runner.activate(source, 0).target_player(P1).resolve();

    let WaitingFor::EffectZoneChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected the any-order prompt for a four-card pile, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        cards
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        revealed.iter().copied().collect(),
        "the whole revealed pile is offered for arrangement"
    );

    // A permutation that is neither the encounter order nor its reverse.
    let submitted = vec![revealed[2], revealed[0], revealed[3], revealed[1]];
    runner
        .act(GameAction::SelectCards {
            cards: submitted.clone(),
        })
        .expect("the arrangement is accepted");
    runner.advance_until_stack_empty();

    let library: Vec<ObjectId> = runner.state().players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    let mut expected = vec![deep];
    expected.extend(submitted);
    assert_eq!(library, expected, "the bottom follows the submitted order");
}

/// A prevention creature whose rider draws exactly one card, however much damage
/// was prevented — one replaced draw, so one child reveal.
const ONE_CARD_SHIELD: &str = "If a source would deal damage to this creature, prevent that damage. The source's controller draws a card.";

/// CR 608.2c + CR 615.5: each pile is arranged by its own instruction. The shield
/// prevents Charbelcher's two damage and its controller draws one card; Abundance
/// replaces that draw with a reveal of its own (two misses, so ITS ordering prompt
/// is raised and answered). Charbelcher's own placement then still offers the two
/// revealed cards left in the library and arranges them in the order submitted
/// for THAT pile, not the child's.
#[test]
fn nested_ordering_pauses_arrange_each_pile_separately() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Goblin Charbelcher", CHARBELCHER);
    scenario.add_enchantment_from_oracle(P0, "Abundance", ABUNDANCE);
    let shield = scenario
        .add_creature_from_oracle(P1, "Shield Creature", 4, 3, ONE_CARD_SHIELD)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    // Bottom to top: three deeper cards, then Forest, then two nonland cards.
    let d3 = scenario.add_spell_to_library_top(P0, "Deep 3", false).id();
    let d2 = scenario.add_spell_to_library_top(P0, "Deep 2", false).id();
    let d1 = scenario.add_spell_to_library_top(P0, "Deep 1", false).id();
    let forest = scenario
        .add_spell_to_library_top(P0, "Forest", false)
        .as_land()
        .with_subtypes(vec!["Forest"])
        .id();
    let b = scenario.add_spell_to_library_top(P0, "Spell B", false).id();
    let a = scenario.add_spell_to_library_top(P0, "Spell A", false).id();
    let mut runner = scenario.build();

    let source = charbelcher(&runner);
    runner.activate(source, 0).target_object(shield).resolve();

    let mut child_order_prompts = 0;
    let mut parent_order_prompts = 0;
    let mut replacement_offers = 0;
    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                replacement_offers += 1;
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("accept Abundance's optional replacement");
            }
            WaitingFor::NamedChoice { .. } => {
                runner
                    .act(GameAction::ChooseOption {
                        choice: "Land".to_string(),
                    })
                    .expect("choose Land");
            }
            WaitingFor::RevealUntilBottomOrder { cards, .. } => {
                child_order_prompts += 1;
                assert_eq!(
                    cards
                        .iter()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>(),
                    [a, b].into_iter().collect(),
                    "the child orders ITS two misses"
                );
                runner
                    .act(GameAction::SelectCards { cards: vec![b, a] })
                    .expect("the child's arrangement is accepted");
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                parent_order_prompts += 1;
                assert_eq!(
                    cards
                        .iter()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>(),
                    [a, b].into_iter().collect(),
                    "the parent's remaining pile is the two cards still in the library"
                );
                runner
                    .act(GameAction::SelectCards { cards: vec![a, b] })
                    .expect("the parent's arrangement is accepted");
            }
            _ => break,
        }
        // Let the stack settle only when no prompt is pending: the settle helper
        // would answer a pending ordering prompt with the default order.
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            runner.advance_until_stack_empty();
        }
    }
    assert_eq!(replacement_offers, 1, "Abundance's replacement was offered");
    assert_eq!(
        child_order_prompts, 1,
        "the child's ordering pause was reached"
    );
    assert_eq!(parent_order_prompts, 1, "the parent's ordering was offered");

    let state = runner.state();
    let hand: Vec<ObjectId> = state.players[P0.0 as usize].hand.iter().copied().collect();
    assert_eq!(
        hand,
        vec![forest],
        "Abundance's reveal delivered the Forest"
    );
    let library: Vec<ObjectId> = state.players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(
        library,
        vec![d1, d2, d3, a, b],
        "the parent's arrangement [A, B], not the child's [B, A], decides the bottom"
    );
}

/// CR 701.20d + CR 400.7: a card that a later reveal in the same action
/// reorders is a new object, and the knowledge published at that action's
/// boundary binds to the occurrence each reveal disclosed — never to the new
/// object holding the same storage id.
///
/// P0's library is Grizzly Bears, Lightning Bolt, Forest, then two deeper cards.
/// Goblin Charbelcher reveals the top three and deals 2 damage to P1's Swans of
/// Bryn Argoll, which prevents it, so P0 draws two cards. The first draw is
/// ordinary (Grizzly Bears). Abundance replaces the second: choosing land, it
/// reveals Lightning Bolt — still revealed by Charbelcher, so its reveal is not
/// re-recorded — and the Forest, puts the Forest into P0's hand and the Bolt on
/// the bottom below the deeper cards. That reorder retires the Bolt's revealed
/// occurrence. Every viewer keeps the reveals' names and the Forest's identity
/// in hand; nobody learns which object now sits on the bottom.
#[test]
fn reordered_reveal_is_not_republished_at_its_new_occurrence() {
    let db = crate::support::shared_card_db().expect("the fixture contains the printed cards");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let charbelcher = scenario.add_real_card(P0, "Goblin Charbelcher", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Abundance", Zone::Battlefield, db);
    let swans = scenario.add_real_card(P1, "Swans of Bryn Argoll", Zone::Battlefield, db);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    // `add_real_card` appends to the library, so the first card added is on top.
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Library, db);
    let counterspell = scenario.add_real_card(P0, "Counterspell", Zone::Library, db);
    for _ in 0..3 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![bears, bolt, forest, island, counterspell],
        "reach guard: the staged library order"
    );

    runner
        .activate(charbelcher, 0)
        .target_object(swans)
        .resolve();
    let bolt_disclosed = runner.state().objects[&bolt].incarnation;
    assert!(
        runner.state().revealed_cards.contains(&bolt),
        "reach guard: Charbelcher's reveal of the Bolt is still open at Swans' draws"
    );

    let mut events: Vec<GameEvent> = Vec::new();
    let mut replacement_offers = 0;
    let mut kind_choices = 0;
    for _ in 0..16 {
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                let descriptions: Vec<&str> =
                    candidates.iter().map(|c| c.description.as_str()).collect();
                assert_eq!(descriptions, vec!["Accept", "Decline"]);
                replacement_offers += 1;
                // Decline the first draw, replace the second.
                let index = if replacement_offers == 1 { 1 } else { 0 };
                runner.act(GameAction::ChooseReplacement { index })
            }
            WaitingFor::NamedChoice { .. } => {
                kind_choices += 1;
                runner.act(GameAction::ChooseOption {
                    choice: "Land".to_string(),
                })
            }
            WaitingFor::RevealUntilBottomOrder { cards, .. }
            | WaitingFor::EffectZoneChoice { cards, .. } => {
                runner.act(GameAction::SelectCards { cards })
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority)
            }
            _ => break,
        };
        events.extend(result.expect("the prompt accepts the answer").events);
    }
    assert_eq!(
        replacement_offers, 2,
        "Abundance was offered for both draws"
    );
    assert_eq!(kind_choices, 1, "only the replaced draw chose a kind");
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));

    let state = runner.state();
    let hand: Vec<ObjectId> = state.players[P0.0 as usize].hand.iter().copied().collect();
    assert_eq!(
        hand,
        vec![bears, forest],
        "the ordinary draw, then Abundance's land"
    );
    let library: Vec<ObjectId> = state.players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(
        library,
        vec![island, counterspell, bolt],
        "Abundance bottomed the Bolt below the deeper cards"
    );
    assert!(
        state.objects[&bolt].incarnation > bolt_disclosed,
        "CR 701.20d: the reordered Bolt is a new object"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { card_ids, .. } if card_ids == &vec![bolt, forest]
        )),
        "reach guard: Abundance's reveal disclosed the Bolt and the Forest"
    );

    // The table was never shown the Bolt's new occurrence.
    assert!(!state.revealed_cards.contains(&bolt));
    assert!(!state.public_revealed_cards.contains(&bolt));
    assert!(
        !state.viewer_knows_card_identity(P1, bolt),
        "the opponent learned no fact about the bottomed Bolt"
    );
    for (label, projected) in [
        ("P1", filter_state_for_viewer(state, P1)),
        ("unseated", filter_state_for_unseated_viewer(state)),
    ] {
        assert_eq!(
            projected.objects[&bolt].name, HIDDEN_CARD_NAME,
            "{label}: the bottomed Bolt is concealed"
        );
        // CR 701.20a: the table saw which revealed card was put into the hand.
        assert_eq!(
            projected.objects[&forest].name, "Forest",
            "{label}: the Forest put into hand stays identified"
        );
    }

    // The reveal history keeps its names; only the hidden object's id is gone.
    for (label, viewer) in [("P1", P1), ("unseated", engine::types::player::PlayerId(9))] {
        let filtered = filter_events_for_viewer(&events, state, viewer);
        let abundance_reveal = filtered
            .iter()
            .find_map(|event| match event {
                GameEvent::CardsRevealed {
                    card_ids,
                    card_names,
                    ..
                } if card_names.iter().any(|i: &String| i == "Forest") => {
                    Some((card_ids.clone(), card_names.clone()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{label}: Abundance's reveal reaches the viewer"));
        assert_eq!(
            abundance_reveal,
            (
                vec![forest],
                vec!["Lightning Bolt".to_string(), "Forest".to_string()]
            ),
            "{label}: the revealed names survive; the bottomed Bolt's id does not"
        );
    }
}
