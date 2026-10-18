//! CR 608.2c: "You may put [a card] ... If you don't, [fallback]." resolves the
//! "you may put" as an up-to-one `EffectZoneChoice`. The "If you don't" rider
//! must read whether a card was actually put: taking a card skips the fallback,
//! declining (or having no eligible card) runs it.
//!
//! Class members exercised: Fallaji Archaeologist, Ravenous Gigamole and Ainok
//! Wayfarer (mill, then put from among milled; else +1/+1 counter). Hakbal of
//! the Surging Soul (yes/no, then put a land from hand; else draw) guards the
//! sibling shape that resolves through the optional-decision authority.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const FALLAJI_ORACLE: &str = "When this creature enters, mill three cards. You may put a noncreature, nonland card from among the cards milled this way into your hand. If you don't, put a +1/+1 counter on this creature. (To mill a card, put the top card of your library into your graveyard.)";

const GIGAMOLE_ORACLE: &str = "When this creature enters, mill three cards. You may put a creature card from among the cards milled this way into your hand. If you don't, put a +1/+1 counter on this creature. (To mill a card, put the top card of your library into your graveyard.)";

const AINOK_ORACLE: &str = "When this creature enters, mill three cards. You may put a land card from among them into your hand. If you don't, put a +1/+1 counter on this creature. (To mill three cards, put the top three cards of your library into your graveyard.)";

const HAKBAL_ATTACK_ORACLE: &str =
    "Whenever Hakbal attacks, you may put a land card from your hand onto the battlefield. If you don't, draw a card.";

fn set_core_type(runner: &mut GameRunner, id: ObjectId, core: CoreType) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![core];
    obj.base_card_types = obj.card_types.clone();
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// Resolve the ETB trigger of a "mill three, you may put ..., if you don't
/// +1/+1 counter" creature against a library whose top three cards are
/// `top_three` (top first) with the given core types.
fn resolve_mill_etb(
    oracle: &str,
    name: &str,
    subtypes: &[&str],
    top_three: [(&str, CoreType); 3],
) -> (GameRunner, ObjectId, [ObjectId; 3]) {
    let parsed = parse_oracle_text(
        oracle,
        name,
        &[],
        &["Creature".to_string()],
        &subtypes.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    );
    let etb = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone)
        .expect("ETB trigger")
        .clone();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, name, 2, 2).id();
    scenario.add_card_to_library_top(P0, "Padding");
    let mut ids = Vec::new();
    for (card, _) in top_three.iter().rev() {
        ids.push(scenario.add_card_to_library_top(P0, card));
    }
    ids.reverse();
    let mut runner = scenario.build();
    for (id, (_, core)) in ids.iter().zip(top_three.iter()) {
        set_core_type(&mut runner, *id, *core);
    }

    let ability = build_resolved_from_def(etb.execute.as_ref().expect("execute"), source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("ETB chain resolves");
    (runner, source, [ids[0], ids[1], ids[2]])
}

fn fallaji_consider_swamp_angler() -> (GameRunner, ObjectId, [ObjectId; 3]) {
    resolve_mill_etb(
        FALLAJI_ORACLE,
        "Fallaji Archaeologist",
        &["Human", "Scout"],
        [
            ("Consider", CoreType::Instant),
            ("Swamp", CoreType::Land),
            ("Gurmag Angler", CoreType::Creature),
        ],
    )
}

fn assert_offered_only(runner: &GameRunner, offered: &[ObjectId]) {
    let WaitingFor::EffectZoneChoice {
        cards,
        up_to,
        destination,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected up-to EffectZoneChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(*up_to, "\"you may put\" must be an up-to choice");
    assert_eq!(*destination, Some(Zone::Hand));
    assert_eq!(cards, offered);
}

/// T1: taking the eligible card skips the "If you don't" counter.
#[test]
fn fallaji_taking_noncreature_nonland_card_adds_no_counter() {
    let (mut runner, source, [consider, swamp, angler]) = fallaji_consider_swamp_angler();
    assert_offered_only(&runner, &[consider]);

    runner
        .act(GameAction::SelectCards {
            cards: vec![consider],
        })
        .expect("select Consider");

    assert_eq!(zone_of(&runner, consider), Zone::Hand);
    assert_eq!(zone_of(&runner, swamp), Zone::Graveyard);
    assert_eq!(zone_of(&runner, angler), Zone::Graveyard);
    assert_eq!(
        plus_one_counters(&runner, source),
        0,
        "a card was put into hand, so \"If you don't\" must not add a counter"
    );
}

/// T2: declining the choice adds exactly one counter.
#[test]
fn fallaji_declining_adds_counter() {
    let (mut runner, source, [consider, _, _]) = fallaji_consider_swamp_angler();
    assert_offered_only(&runner, &[consider]);
    let hand_before = runner.state().players[0].hand.clone();

    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("decline");

    assert_eq!(zone_of(&runner, consider), Zone::Graveyard);
    assert_eq!(runner.state().players[0].hand, hand_before);
    assert_eq!(plus_one_counters(&runner, source), 1);
}

/// T3: no eligible card among the milled cards means no prompt and a counter.
#[test]
fn fallaji_no_eligible_milled_card_adds_counter() {
    let (runner, source, milled) = resolve_mill_etb(
        FALLAJI_ORACLE,
        "Fallaji Archaeologist",
        &["Human", "Scout"],
        [
            ("Island", CoreType::Land),
            ("Swamp", CoreType::Land),
            ("Gurmag Angler", CoreType::Creature),
        ],
    );

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "no eligible card must not prompt"
    );
    for id in milled {
        assert_eq!(zone_of(&runner, id), Zone::Graveyard, "three cards milled");
    }
    assert_eq!(plus_one_counters(&runner, source), 1);
}

/// H3: second class member (creature filter) — take and decline.
#[test]
fn ravenous_gigamole_take_skips_counter_decline_adds_counter() {
    let setup = || {
        resolve_mill_etb(
            GIGAMOLE_ORACLE,
            "Ravenous Gigamole",
            &["Mole", "Horror"],
            [
                ("Consider", CoreType::Instant),
                ("Swamp", CoreType::Land),
                ("Gurmag Angler", CoreType::Creature),
            ],
        )
    };

    let (mut runner, source, [_, _, angler]) = setup();
    assert_offered_only(&runner, &[angler]);
    runner
        .act(GameAction::SelectCards {
            cards: vec![angler],
        })
        .expect("select Angler");
    assert_eq!(zone_of(&runner, angler), Zone::Hand);
    assert_eq!(plus_one_counters(&runner, source), 0);

    let (mut runner, source, [_, _, angler]) = setup();
    assert_offered_only(&runner, &[angler]);
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("decline");
    assert_eq!(zone_of(&runner, angler), Zone::Graveyard);
    assert_eq!(plus_one_counters(&runner, source), 1);
}

/// "from among them" phrasing (Ainok Wayfarer, Ostrich-Horse) — take and
/// decline.
#[test]
fn ainok_wayfarer_from_among_them_take_skips_counter_decline_adds_counter() {
    let setup = || {
        resolve_mill_etb(
            AINOK_ORACLE,
            "Ainok Wayfarer",
            &["Dog", "Scout"],
            [
                ("Consider", CoreType::Instant),
                ("Swamp", CoreType::Land),
                ("Gurmag Angler", CoreType::Creature),
            ],
        )
    };

    let (mut runner, source, [_, swamp, _]) = setup();
    assert_offered_only(&runner, &[swamp]);
    runner
        .act(GameAction::SelectCards { cards: vec![swamp] })
        .expect("select Swamp");
    assert_eq!(zone_of(&runner, swamp), Zone::Hand);
    assert_eq!(plus_one_counters(&runner, source), 0);

    let (mut runner, source, [_, swamp, _]) = setup();
    assert_offered_only(&runner, &[swamp]);
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("decline");
    assert_eq!(zone_of(&runner, swamp), Zone::Graveyard);
    assert_eq!(plus_one_counters(&runner, source), 1);
}

/// H2: Hakbal's "you may put a land card from your hand onto the battlefield.
/// If you don't, draw a card." is a yes/no optional followed by an exact
/// one-card choice (not an up-to choice), so it resolves through the optional
/// decision authority rather than the up-to stamp. Guard that this sibling
/// shape stays correct: accept + put a land skips the draw; decline draws.
#[test]
fn hakbal_attack_take_land_skips_draw_decline_draws() {
    let parsed = parse_oracle_text(
        HAKBAL_ATTACK_ORACLE,
        "Hakbal of the Surging Soul",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Merfolk".to_string(), "Scout".to_string()],
    );
    let attack = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Attacks)
        .expect("attacks trigger")
        .clone();

    let setup = || {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let hakbal = scenario
            .add_creature(P0, "Hakbal of the Surging Soul", 3, 3)
            .id();
        let forest = scenario.add_land_to_hand(P0, "Forest").id();
        let island = scenario.add_land_to_hand(P0, "Island").id();
        let drawn = scenario.add_card_to_library_top(P0, "Library Top");
        let mut runner = scenario.build();
        let ability =
            build_resolved_from_def(attack.execute.as_ref().expect("execute"), hakbal, P0);
        let mut events = Vec::new();
        resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
            .expect("attack chain resolves");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));
        (runner, forest, island, drawn)
    };

    // Accept, put a land: no draw.
    let (mut runner, forest, island, drawn) = setup();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept");
    let WaitingFor::EffectZoneChoice {
        cards,
        up_to,
        destination,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected EffectZoneChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(!*up_to, "Hakbal's accepted put is an exact one-card choice");
    assert_eq!(*destination, Some(Zone::Battlefield));
    assert_eq!(cards.len(), 2);
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("put land");
    assert_eq!(zone_of(&runner, forest), Zone::Battlefield);
    assert_eq!(zone_of(&runner, island), Zone::Hand);
    assert_eq!(
        zone_of(&runner, drawn),
        Zone::Library,
        "a land was put, so \"If you don't\" must not draw"
    );

    // Decline the "you may": draw.
    let (mut runner, forest, _, drawn) = setup();
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline");
    assert_eq!(zone_of(&runner, forest), Zone::Hand);
    assert_eq!(zone_of(&runner, drawn), Zone::Hand);
}
