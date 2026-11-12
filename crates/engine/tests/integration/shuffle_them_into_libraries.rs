//! CR 400.3 + CR 701.24c: "shuffle them into <library>" moves the named objects
//! into the library before the shuffle — never a bare shuffle that leaves them
//! where they were.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

/// Printed Oracle text of Turn the Earth (MTGJSON AtomicCards).
const TURN_THE_EARTH: &str = "Choose up to three target cards in graveyards. The owners of those cards shuffle them into their libraries. You gain 2 life.\nFlashback {1}{G} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";

fn shuffles_for(events: &[GameEvent], player: PlayerId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } if *player_id == player
            )
        })
        .count()
}

/// Two targets owned by different players: each goes to ITS OWNER's library,
/// each owner's library is shuffled, and the caster still gains 2 life.
#[test]
fn turn_the_earth_puts_each_target_into_its_owners_library_then_each_owner_shuffles() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Turn the Earth", false, TURN_THE_EARTH)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mine = scenario
        .add_creature_to_graveyard(P0, "P0 Graveyard Bear", 2, 2)
        .id();
    let theirs = scenario
        .add_creature_to_graveyard(P1, "P1 Graveyard Bear", 2, 2)
        .id();
    scenario.add_card_to_library_top(P0, "P0 Library Card");
    scenario.add_card_to_library_top(P1, "P1 Library Card");

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[mine, theirs]).resolve();
    let state = outcome.state();
    let events = outcome.events();

    assert_eq!(state.objects[&mine].zone, Zone::Library);
    assert!(state.players[P0.0 as usize].library.contains(&mine));
    assert_eq!(state.objects[&theirs].zone, Zone::Library);
    assert!(state.players[P1.0 as usize].library.contains(&theirs));
    // CR 701.24a + CR 701.24c: "The owners of those cards shuffle them into
    // their libraries" is one shuffle instruction for each affected owner, so
    // each owner's library is randomized exactly once even though two owners are
    // involved. A single terminal owner shuffle cannot cover both owners; the
    // per-owner delivery shuffles are the authority here.
    assert_eq!(shuffles_for(events, P0), 1, "{events:?}");
    assert_eq!(shuffles_for(events, P1), 1, "{events:?}");
    outcome.assert_life_delta(P0, 2);
}

/// CR 701.24c: two selected cards from the same graveyard are one owner's
/// cards, so that owner shuffles their library exactly once, not once per card.
#[test]
fn turn_the_earth_same_owner_shuffles_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Turn the Earth", false, TURN_THE_EARTH)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let first = scenario
        .add_creature_to_graveyard(P0, "P0 First Bear", 2, 2)
        .id();
    let second = scenario
        .add_creature_to_graveyard(P0, "P0 Second Bear", 2, 2)
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[first, second])
        .resolve();
    let events = outcome.events();

    assert_eq!(shuffles_for(events, P0), 1, "{events:?}");
    assert_eq!(shuffles_for(events, P1), 0, "{events:?}");
    assert!(outcome.state().players[P0.0 as usize]
        .library
        .contains(&first));
    assert!(outcome.state().players[P0.0 as usize]
        .library
        .contains(&second));
}

/// CR 701.24c: the owner is captured before delivery, so the library is
/// shuffled even when every eligible move is replaced and no card reaches it.
#[test]
fn turn_the_earth_replaced_deliveries_still_shuffle_the_owner() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Turn the Earth", false, TURN_THE_EARTH)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let redirect = ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Library)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                library_shuffle: Default::default(),
                origin: None,
                destination: Zone::Exile,
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
        ));
    scenario
        .add_creature(P1, "Library Redirector", 1, 1)
        .with_replacement_definition(redirect)
        .id();
    let first = scenario
        .add_creature_to_graveyard(P0, "P0 Replaced Bear", 2, 2)
        .id();
    let second = scenario
        .add_creature_to_graveyard(P0, "P0 Replaced Bear Two", 2, 2)
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[first, second])
        .resolve();
    let events = outcome.events();

    assert_eq!(
        outcome.state().objects[&first].zone,
        Zone::Exile,
        "the replacement redirected the first delivery"
    );
    assert_eq!(
        outcome.state().objects[&second].zone,
        Zone::Exile,
        "the replacement redirected the second delivery"
    );
    assert_eq!(
        shuffles_for(events, P0),
        1,
        "the owner is still shuffled exactly once: {events:?}"
    );
    assert_eq!(shuffles_for(events, P1), 0, "{events:?}");
}

/// CR 701.24c + CR 400.3: "shuffle that card into its owner's library" captures
/// the moved card's owner before delivery. Malfegor Avatar and Mephidross Slime
/// are Alchemy shapes; the nearest printed owner-move cards are Serene
/// Remembrance (compound, covered above) and Cathartic Parting (specific,
/// covered below). This synthetic trigger exercises the supported trigger and
/// effect shape: one exiled card, one owner, exactly one shuffle.
#[test]
fn exile_trigger_shuffle_that_card_shuffles_its_owner_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let trigger_source = scenario
        .add_creature_from_oracle(
            P0,
            "Exile Shuffler",
            1,
            1,
            "Whenever a creature you control is exiled, shuffle that card into its owner's library.",
        )
        .id();
    let victim = scenario.add_creature(P0, "Exiled Bear", 2, 2).id();
    let swords = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Swords to Plowshares",
            true,
            "Exile target creature. Its controller gains life equal to its power.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::White],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, swords, false, vec![])],
    );
    let mut runner = scenario.build();

    let outcome = runner.cast(swords).target_object(victim).resolve();
    let events = outcome.events();

    assert_eq!(
        outcome.state().objects[&victim].zone,
        Zone::Library,
        "the exiled card is shuffled into its owner's library"
    );
    assert_eq!(
        shuffles_for(events, P0),
        1,
        "one card, one owner, exactly one shuffle: {events:?}"
    );
    assert_eq!(shuffles_for(events, P1), 0, "{events:?}");
    assert!(
        outcome.state().objects.contains_key(&trigger_source),
        "reach guard: the trigger source stayed on the battlefield"
    );
}

/// CR 701.24c + CR 400.3: Serene Remembrance shuffles itself and cards from one
/// graveyard into their owners' libraries. The source's owner and the selected
/// cards' owner can differ, so the compound move settles once for each owner,
/// not through a first-owner terminal.
#[test]
fn serene_remembrance_settles_once_for_each_compound_owner() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Serene Remembrance",
            false,
            "Shuffle Serene Remembrance and up to three target cards from a single graveyard into their owners' libraries.",
        )
        .id();
    let first = scenario
        .add_creature_to_graveyard(P1, "P1 First Relic", 1, 1)
        .id();
    let second = scenario
        .add_creature_to_graveyard(P1, "P1 Second Relic", 1, 1)
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[first, second])
        .resolve();
    let events = outcome.events();

    assert_eq!(outcome.state().objects[&spell].zone, Zone::Library);
    assert_eq!(outcome.state().objects[&first].zone, Zone::Library);
    assert_eq!(outcome.state().objects[&second].zone, Zone::Library);
    assert_eq!(
        shuffles_for(events, P0),
        1,
        "the source's owner shuffles once: {events:?}"
    );
    assert_eq!(
        shuffles_for(events, P1),
        1,
        "the selected cards' owner shuffles once: {events:?}"
    );
}

/// CR 701.24c + CR 400.3: Cathartic Parting's target is a pure type-union
/// ("artifact or enchantment an opponent controls") — one object, one exact
/// owner — so it settles through the specific owner terminal with one shuffle.
#[test]
fn cathartic_parting_pure_type_union_shuffles_owner_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Cathartic Parting",
            false,
            "The owner of target artifact or enchantment an opponent controls shuffles it into their library. You may shuffle up to four target cards from your graveyard into your library.",
        )
        .id();
    let relic = scenario
        .add_artifact_from_oracle(P1, "Opponent Relic", "")
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(relic).resolve();
    let events = outcome.events();

    assert_eq!(outcome.state().objects[&relic].zone, Zone::Library);
    assert_eq!(
        shuffles_for(events, P1),
        1,
        "the chosen target's owner shuffles once: {events:?}"
    );
    assert_eq!(shuffles_for(events, P0), 0, "{events:?}");
}

/// CR 701.24c: when the compound source and both selected cards share an owner,
/// that owner's library is shuffled exactly once for the whole instruction.
#[test]
fn serene_remembrance_same_owner_shuffles_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Serene Remembrance",
            false,
            "Shuffle Serene Remembrance and up to three target cards from a single graveyard into their owners' libraries.",
        )
        .id();
    let first = scenario
        .add_creature_to_graveyard(P0, "P0 First Relic", 1, 1)
        .id();
    let second = scenario
        .add_creature_to_graveyard(P0, "P0 Second Relic", 1, 1)
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[first, second])
        .resolve();
    let events = outcome.events();

    assert_eq!(
        shuffles_for(events, P0),
        1,
        "the shared owner shuffles exactly once: {events:?}"
    );
    assert_eq!(shuffles_for(events, P1), 0, "{events:?}");
    assert_eq!(outcome.state().objects[&spell].zone, Zone::Library);
    assert_eq!(outcome.state().objects[&first].zone, Zone::Library);
    assert_eq!(outcome.state().objects[&second].zone, Zone::Library);
}

/// CR 400.7 + CR 701.24c: a plain "when this creature dies, shuffle it into its
/// owner's library" (Alabaster Dragon class) moves the card from the graveyard,
/// not from an expected battlefield. The one library shuffle belongs to its
/// owner.
#[test]
fn alabaster_dragon_dies_trigger_shuffles_from_the_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragon = scenario
        .add_creature_from_oracle(
            P0,
            "Alabaster Dragon",
            4,
            4,
            "Flying\nWhen this creature dies, shuffle it into its owner's library.",
        )
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(murder).target_object(dragon).resolve();
    let events = outcome.events();

    assert_eq!(
        outcome.state().objects[&dragon].zone,
        Zone::Library,
        "the dies-triggered card is shuffled into its owner's library"
    );
    assert_eq!(
        shuffles_for(events, P0),
        1,
        "the owner shuffles exactly once: {events:?}"
    );
    assert_eq!(shuffles_for(events, P1), 0, "{events:?}");
}

/// Put a source's activated ability on the stack and stop before resolution,
/// selecting its object target. Mirrors the shared response-test driver.
fn activate_onto_stack(runner: &mut GameRunner, source: ObjectId, index: usize, target: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation is accepted");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("the target is accepted");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while activating: {other:?}"),
        }
    }
    assert!(
        !runner.state().stack.is_empty(),
        "the ability is on the stack, not resolved"
    );
}

/// CR 400.7j: Assemble from Parts grants "Exile this card from your graveyard:
/// Shuffle it into its owner's library." The cost moves the source to exile, and
/// the ability's self-reference binds that public-zone successor, so the card is
/// shuffled from exile, not skipped as a stale battlefield object.
#[test]
fn exiled_cost_self_shuffle_finds_the_public_successor() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Assemble Shape",
            1,
            1,
            "{1}{B}{B}, Exile this card from your graveyard: Shuffle it into its owner's library.",
        )
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, source, false, vec![]),
            ManaUnit::new(ManaType::Black, source, false, vec![]),
            ManaUnit::new(ManaType::Black, source, false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    runner.cast(murder).target_object(source).resolve();
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);

    let outcome = runner.activate(source, 0).resolve();
    let events = outcome.events();

    assert_eq!(
        outcome.state().objects[&source].zone,
        Zone::Library,
        "the exiled cost successor is shuffled into its owner's library"
    );
    assert_eq!(
        shuffles_for(events, P0),
        1,
        "the owner shuffles exactly once: {events:?}"
    );
}

/// CR 701.24c: Floodpits Drowner can be destroyed in response while its other
/// owner's target remains legal. The stale source does not move, but its known
/// owner still shuffles; the legal target's owner also shuffles.
#[test]
fn drowner_destroyed_in_response_still_shuffles_both_owners() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    const DROWNER: &str = "Flash\nVigilance\nWhen this creature enters, tap target creature an opponent controls and put a stun counter on it.\n{1}{U}, {T}: Shuffle this creature and target creature with a stun counter on it into their owners' libraries.";
    let drowner = scenario
        .add_creature_from_oracle(P0, "Floodpits Drowner", 2, 1, DROWNER)
        .id();
    let target = scenario.add_creature(P1, "Stunned Target", 1, 1).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, "Destroy target creature.")
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, drowner, false, vec![]),
            ManaUnit::new(ManaType::Blue, drowner, false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .unwrap()
        .counters
        .insert(CounterType::Stun, 1);

    activate_onto_stack(&mut runner, drowner, 0, target);
    // Give P1 priority so it can destroy the source before the ability resolves.
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to P1");
    }
    let outcome = runner.cast(murder).target_object(drowner).resolve();
    let events = outcome.events();

    assert_eq!(
        outcome.state().objects[&drowner].zone,
        Zone::Graveyard,
        "the source was destroyed before resolution"
    );
    assert_eq!(
        outcome.state().objects[&target].zone,
        Zone::Library,
        "the legal target still moves"
    );
    assert_eq!(
        shuffles_for(events, P0),
        1,
        "the stale source's owner still shuffles: {events:?}"
    );
    assert_eq!(
        shuffles_for(events, P1),
        1,
        "the target's owner shuffles: {events:?}"
    );
}
