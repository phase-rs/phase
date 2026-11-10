//! CR 400.3 + CR 701.24c: "shuffle them into <library>" moves the named objects
//! into the library before the shuffle — never a bare shuffle that leaves them
//! where they were.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, TargetFilter,
};
use engine::types::events::{GameEvent, PlayerActionKind};
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

/// CR 701.24c + CR 400.3: "shuffle that card into its owner's library" (the
/// Malfegor Avatar / Mephidross Slime shape) captures the moved card's owner
/// before delivery. The printed Malfegor trigger's "if it was unearthed"
/// condition is unsupported, so this exercises the same supported trigger and
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
