//! Civilized Scholar — "{T}: Draw a card, then discard a card. If a creature
//! card is discarded this way, untap this creature, then transform it."
//!
//! Runtime coverage through the real `ActivateAbility` pipeline: the looting
//! discard is interactive, and the creature-card rider must untap AND transform
//! (CR 701.9a discard, CR 701.27a transform, CR 608.2c "this way" condition).
//! Every test sets a `back_face` so "did not transform" is never vacuous.

use engine::game::casting::activated_ability_definitions;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "{T}: Draw a card, then discard a card. If a creature card is discarded this way, untap this creature, then transform it.";

fn homicidal_brute_back_face() -> BackFaceData {
    BackFaceData {
        is_swap_snapshot: false,
        trigger_printed_origins: Vec::new(),
        name: "Homicidal Brute".to_string(),
        power: Some(5),
        toughness: Some(1),
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
        parse_warnings: vec![],
    }
}

fn setup() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let scholar = scenario
        .add_creature(P0, "Civilized Scholar", 0, 1)
        .from_oracle_text(ORACLE)
        .id();
    let mut runner = scenario.build();
    let turn = runner.state().turn_number;
    let obj = runner.state_mut().objects.get_mut(&scholar).unwrap();
    obj.summoning_sick = false;
    obj.entered_battlefield_turn = Some(turn.saturating_sub(1));
    obj.back_face = Some(homicidal_brute_back_face());
    (runner, scholar)
}

/// Put a card of `core_type` on top of P0's library so the draw pulls it.
fn seed_library_top(runner: &mut GameRunner, name: &str, core_type: CoreType) -> ObjectId {
    let state = runner.state_mut();
    let id = create_object(
        state,
        CardId(state.next_object_id),
        P0,
        name.to_string(),
        Zone::Library,
    );
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(core_type);
    let player = state.players.iter_mut().find(|p| p.id == P0).unwrap();
    player.library.retain(|&oid| oid != id);
    player.library.insert(0, id);
    id
}

fn activate_loot(runner: &mut GameRunner, scholar: ObjectId) {
    let idx = activated_ability_definitions(runner.state(), scholar)
        .into_iter()
        .find(|(_, ability)| matches!(ability.effect.as_ref(), Effect::Draw { .. }))
        .expect("Civilized Scholar must carry a Draw activated ability")
        .0;
    runner
        .act(GameAction::ActivateAbility {
            source_id: scholar,
            ability_index: idx,
            payment_mode: Default::default(),
        })
        .expect("Civilized Scholar's {T} ability must be activatable");
}

/// Put a card of `core_type` directly into P0's hand.
fn add_hand_card(runner: &mut GameRunner, name: &str, core_type: CoreType) -> ObjectId {
    let state = runner.state_mut();
    let id = create_object(
        state,
        CardId(state.next_object_id),
        P0,
        name.to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(core_type);
    id
}

/// Activate the loot ability with a two-card hand (a creature and a land) plus
/// a drawn Island, so the discard is a real choice (CR 701.9b), then discard
/// `chosen` through `SelectCards`.
fn loot_choosing_discard(runner: &mut GameRunner, scholar: ObjectId, chosen_core: CoreType) {
    let creature = add_hand_card(runner, "Grizzly Bears", CoreType::Creature);
    let land = add_hand_card(runner, "Forest", CoreType::Land);
    seed_library_top(runner, "Island", CoreType::Land);
    activate_loot(runner, scholar);
    for _ in 0..10 {
        if matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("resolve the loot ability");
    }
    let chosen = if chosen_core == CoreType::Creature {
        creature
    } else {
        land
    };
    match &runner.state().waiting_for {
        WaitingFor::DiscardChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "the Scholar's controller chooses the discard");
            assert!(cards.contains(&creature) && cards.contains(&land));
        }
        other => panic!("expected a DiscardChoice prompt, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("discard the chosen card");
    assert!(
        runner.state().players[0].graveyard.contains(&chosen),
        "the chosen card must have reached the graveyard"
    );
    assert!(runner.state().stack.is_empty(), "the loot ability resolved");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "priority returns after the discard"
    );
}

#[test]
fn interactive_discard_of_a_creature_card_untaps_and_transforms() {
    let (mut runner, scholar) = setup();
    loot_choosing_discard(&mut runner, scholar, CoreType::Creature);

    let obj = &runner.state().objects[&scholar];
    assert!(!obj.tapped, "creature discard untaps the Scholar");
    assert!(obj.transformed, "creature discard transforms the Scholar");
    assert_eq!(obj.name, "Homicidal Brute");
}

#[test]
fn interactive_discard_of_a_noncreature_card_neither_untaps_nor_transforms() {
    let (mut runner, scholar) = setup();
    loot_choosing_discard(&mut runner, scholar, CoreType::Land);

    let obj = &runner.state().objects[&scholar];
    assert!(obj.tapped, "noncreature discard leaves the Scholar tapped");
    assert!(!obj.transformed);
    assert_eq!(obj.name, "Civilized Scholar");
}

/// Activate the loot ability and discard the drawn card (the only card in hand).
fn loot_discarding_drawn_card(runner: &mut GameRunner, scholar: ObjectId, drawn: ObjectId) {
    activate_loot(runner, scholar);

    // The lone drawn card is discarded without a prompt (CR 701.9b: a hand
    // of one card leaves no choice), so just drain the stack; still answer a
    // discard prompt if the engine presents one.
    for _ in 0..30 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve the loot ability");
            }
            WaitingFor::DiscardChoice { cards, .. } => {
                let card = cards[0];
                runner
                    .act(GameAction::SelectCards { cards: vec![card] })
                    .expect("discard the drawn card");
            }
            other => panic!("unexpected window during loot: {other:?}"),
        }
    }
    assert!(
        runner.state().players[0].graveyard.contains(&drawn),
        "the drawn card must have reached the graveyard"
    );
}

#[test]
fn discarding_a_creature_card_untaps_and_transforms() {
    let (mut runner, scholar) = setup();
    let bears = seed_library_top(&mut runner, "Grizzly Bears", CoreType::Creature);

    loot_discarding_drawn_card(&mut runner, scholar, bears);

    let obj = &runner.state().objects[&scholar];
    assert!(!obj.tapped, "creature discard untaps the Scholar");
    assert!(obj.transformed, "creature discard transforms the Scholar");
    assert_eq!(obj.name, "Homicidal Brute");
}

#[test]
fn discarding_a_noncreature_card_neither_untaps_nor_transforms() {
    let (mut runner, scholar) = setup();
    let island = seed_library_top(&mut runner, "Island", CoreType::Land);

    loot_discarding_drawn_card(&mut runner, scholar, island);

    let obj = &runner.state().objects[&scholar];
    assert!(obj.tapped, "noncreature discard leaves the Scholar tapped");
    assert!(!obj.transformed);
    assert_eq!(obj.name, "Civilized Scholar");
}

/// The transform's "it" names the Scholar (named by `~` in the preceding untap),
/// not the chain's carried-down parent target.
#[test]
fn transform_pronoun_binds_to_the_scholar() {
    use engine::types::ability::{AbilityDefinition, TargetFilter};
    let parsed = engine::parser::parse_oracle_text(
        ORACLE,
        "Civilized Scholar",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let mut cur: Option<&AbilityDefinition> = parsed.abilities.first();
    let mut found = false;
    while let Some(def) = cur {
        if let Effect::Transform { target, .. } = def.effect.as_ref() {
            assert_eq!(*target, TargetFilter::SelfRef);
            found = true;
        }
        cur = def.sub_ability.as_deref();
    }
    assert!(found, "ability chain must contain a Transform");
}
