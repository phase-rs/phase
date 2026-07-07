//! Regression for GitHub issue #492: Serra's Emissary's "You and creatures you
//! control have protection from the chosen card type."
//!
//! CR 702.16 + CR 205.2 + CR 609.6. The compound-subject keyword
//! grant lowers to two `StaticDefinition`s:
//!   - object-half: `Continuous` / `AddKeyword(Protection(ChosenCardType))`;
//!   - player-half: `PlayerProtection(ChosenCardType)` with controller-You.
//!
//! These tests drive the real `apply()` pipeline for the ETB card-type choice
//! and exercise the layer applier, the damage-prevention gate, and the
//! targeting gate. Reverting any of layers Step 5b (layer resolution arm),
//! Step 6 (`source_matches_protection_target`), or Step 7b
//! (`player_protection_from`) independently breaks a distinct assertion.

use engine::database::card_db::CardDatabase;
use engine::game::effects::deal_damage;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{
    ChoiceType, Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P1: PlayerId = PlayerId(1);

use crate::support::shared_card_db as load_db;

/// Place a bare object with a single core type on the battlefield — used as a
/// damage/targeting *source* whose card type drives the protection check.
fn add_source(state: &mut GameState, player: PlayerId, name: &str, core: CoreType) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = engine::game::zones::create_object(
        state,
        card_id,
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![core];
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Build a `DealDamage` ability whose `source_id` is the damage source.
fn damage_ability(source_id: ObjectId, controller: PlayerId, target: TargetRef) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![target],
        source_id,
        controller,
    )
}

/// Drive Serra's Emissary onto P0's battlefield and resolve its ETB
/// "choose a card type" through the real `apply()` pipeline, choosing Creature.
/// Returns `(state, emissary_id, vanilla_creature_id)`.
fn setup_emissary_choosing_creature(db: &CardDatabase) -> (GameState, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    let emissary = scenario.add_real_card(P0, "Serra's Emissary", Zone::Battlefield, db);
    let mut runner = scenario.build();

    // The ETB Choose effect (a replacement) yields `WaitingFor::NamedChoice`
    // keyed to the entering permanent. Drive the real choice resolution path
    // — `ChooseOption` stores `ChosenAttribute::CardType(Creature)` on the
    // emissary via the production handler.
    runner.state_mut().waiting_for = WaitingFor::NamedChoice {
        player: P0,
        choice_type: ChoiceType::card_type(),
        options: vec![
            "Artifact".to_string(),
            "Creature".to_string(),
            "Enchantment".to_string(),
            "Instant".to_string(),
            "Land".to_string(),
            "Planeswalker".to_string(),
            "Sorcery".to_string(),
        ],
        source_id: Some(emissary),
        persist_player: None,
    };
    runner
        .act(GameAction::ChooseOption {
            choice: "Creature".to_string(),
        })
        .expect("ChooseOption(Creature) must resolve");

    // The choice must be recorded on the emissary via the real pipeline.
    assert_eq!(
        runner.state().objects[&emissary].chosen_card_type(),
        Some(CoreType::Creature),
        "ETB choose-card-type must store CardType(Creature) on the emissary"
    );

    let vanilla = {
        let s = runner.state_mut();
        add_source(s, P0, "Vanilla Bear", CoreType::Creature)
    };
    // The vanilla also needs P/T to be a real creature recipient.
    {
        let obj = runner.state_mut().objects.get_mut(&vanilla).unwrap();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }

    let mut state = runner.state().clone();
    evaluate_layers(&mut state);
    (state, emissary, vanilla)
}

/// Object-half discriminator (Step 5b): the layer applier bakes a concrete
/// `Protection(CardType("creature"))` onto creatures the emissary's static
/// affects. Reverting Step 5b drops the keyword and breaks this assertion.
#[test]
fn object_half_grants_concrete_protection_from_creature() {
    let Some(db) = load_db() else {
        eprintln!("card-data.json missing — skipping");
        return;
    };
    let (state, _emissary, vanilla) = setup_emissary_choosing_creature(db);

    let creature = &state.objects[&vanilla];
    assert!(
        creature
            .keywords
            .contains(&Keyword::Protection(ProtectionTarget::CardType(
                "creature".to_string()
            ))),
        "vanilla creature must carry layer-baked Protection(CardType(\"creature\")), got {:?}",
        creature.keywords
    );
}

/// Player-half + object-half damage discriminator (Steps 6 & 7b): a
/// Creature-source's damage to the controller (player-half via
/// `player_protection_from`) and to the controlled creature (object-half via
/// `source_matches_protection_target`) is prevented.
#[test]
fn creature_source_damage_to_player_and_creature_is_prevented() {
    let Some(db) = load_db() else {
        eprintln!("card-data.json missing — skipping");
        return;
    };
    let (mut state, _emissary, vanilla) = setup_emissary_choosing_creature(db);
    let attacker = add_source(&mut state, P1, "Creature Attacker", CoreType::Creature);

    // Player-half: damage to the emissary's controller (P0).
    let mut events = Vec::new();
    let ability = damage_ability(attacker, P1, TargetRef::Player(P0));
    deal_damage::resolve(&mut state, &ability, &mut events).expect("resolve");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "creature-source damage to protected player must be prevented, got {events:?}"
    );

    // Object-half: damage to the controlled vanilla creature.
    let mut events = Vec::new();
    let ability = damage_ability(attacker, P1, TargetRef::Object(vanilla));
    deal_damage::resolve(&mut state, &ability, &mut events).expect("resolve");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "creature-source damage to protected creature must be prevented, got {events:?}"
    );
}

/// Negative discriminator: an Instant-source has no quality the chosen-card-type
/// protection (Creature) matches — its damage is dealt normally.
#[test]
fn instant_source_damage_is_not_prevented() {
    let Some(db) = load_db() else {
        eprintln!("card-data.json missing — skipping");
        return;
    };
    let (mut state, _emissary, vanilla) = setup_emissary_choosing_creature(db);
    let bolt = add_source(&mut state, P1, "Instant Bolt", CoreType::Instant);

    let mut events = Vec::new();
    let ability = damage_ability(bolt, P1, TargetRef::Player(P0));
    deal_damage::resolve(&mut state, &ability, &mut events).expect("resolve");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "instant-source damage to the player must NOT be prevented, got {events:?}"
    );

    let mut events = Vec::new();
    let ability = damage_ability(bolt, P1, TargetRef::Object(vanilla));
    deal_damage::resolve(&mut state, &ability, &mut events).expect("resolve");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "instant-source damage to the creature must NOT be prevented, got {events:?}"
    );
}

/// Targeting discriminator (Step 7b via `player_protection_from`): the
/// emissary's controller is absent from a creature-source spell's legal player
/// targets, but present for an instant-source spell.
#[test]
fn targeting_excludes_controller_only_for_creature_source() {
    let Some(db) = load_db() else {
        eprintln!("card-data.json missing — skipping");
        return;
    };
    let (mut state, _emissary, _vanilla) = setup_emissary_choosing_creature(db);
    let creature_src = add_source(&mut state, P1, "Creature Spell", CoreType::Creature);
    let instant_src = add_source(&mut state, P1, "Instant Spell", CoreType::Instant);

    let creature_targets = find_legal_targets(&state, &TargetFilter::Player, P1, creature_src);
    assert!(
        !creature_targets.contains(&TargetRef::Player(P0)),
        "protected controller must NOT be a legal target for a creature-source, got {creature_targets:?}"
    );

    let instant_targets = find_legal_targets(&state, &TargetFilter::Player, P1, instant_src);
    assert!(
        instant_targets.contains(&TargetRef::Player(P0)),
        "controller must be a legal target for an instant-source, got {instant_targets:?}"
    );
}
