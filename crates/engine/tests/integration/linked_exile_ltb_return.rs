//! CR 607.2a + CR 607.3: the two-ability "exile … / when this leaves the
//! battlefield, return the exiled card(s)" class returns its cards when the
//! printed leaves-the-battlefield trigger resolves, and an LTB that resolves
//! before the exile happened returns nothing.
//!
//! Oracle text (Scryfall):
//!   Fiend Hunter: "When this creature enters, you may exile another target
//!    creature. When this creature leaves the battlefield, return the exiled
//!    card to the battlefield under its owner's control."
//!   Oblivion Ring: "When this enchantment enters, exile another target nonland
//!    permanent. When this enchantment leaves the battlefield, return the
//!    exiled card to the battlefield under its owner's control."
//!   Journey to Nowhere: "When this enchantment enters, exile target creature.
//!    When this enchantment leaves the battlefield, return the exiled card to
//!    the battlefield under its owner's control."
//!   Wormfang Newt: "When this creature enters, exile a land you control. When
//!    this creature leaves the battlefield, return the exiled card to the
//!    battlefield under its owner's control."
//!   Worldgorger Dragon: "Flying, trample / When this creature enters, exile all
//!    other permanents you control. When this creature leaves the battlefield,
//!    return the exiled cards to the battlefield under their owners' control."
//!   Admonition Angel: "Flying / Landfall — Whenever a land you control enters,
//!    you may exile target nonland permanent other than this creature. When
//!    this creature leaves the battlefield, return all cards exiled with it to
//!    the battlefield under their owners' control."
//!   Swords to Plowshares: "Exile target creature. Its controller gains life
//!    equal to its power."
//!   Mind Raker: "Devoid (This card has no color.) When this creature enters,
//!    you may put a card an opponent owns from exile into that player's
//!    graveyard. If you do, each opponent discards a card."
//!   Reanimate: "Put target creature card from a graveyard onto the battlefield
//!    under your control. You lose life equal to that card's mana value."
//!   Unsummon: "Return target creature to its owner's hand."
//! Ruling (Fiend Hunter, 2018-12-07): if Fiend Hunter leaves the battlefield
//! before its first ability resolves, its second ability "will trigger and do
//! nothing. Then its first ability will resolve and exile the target creature
//! indefinitely."

use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::merge::{merge_object_onto, MergeSide};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{Effect, EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, LoopDetectionMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn pool(n: usize, mana: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(mana, ObjectId(0), false, vec![]))
        .collect()
}

fn is_triggered_from(state: &GameState, index: usize, source: ObjectId) -> bool {
    state.stack.get(index).is_some_and(|entry| {
        entry.source_id == source && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
    })
}

fn top_is_triggered_from(state: &GameState, source: ObjectId) -> bool {
    state
        .stack
        .len()
        .checked_sub(1)
        .is_some_and(|top| is_triggered_from(state, top, source))
}

fn at_priority(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { .. })
}

/// Submits the one answer the current prompt needs and returns its events.
fn answer(
    runner: &mut GameRunner,
    accept_optional: bool,
    target: Option<ObjectId>,
) -> Vec<GameEvent> {
    let action = match &runner.state().waiting_for {
        WaitingFor::Priority { .. } => GameAction::PassPriority,
        WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
            order: (0..triggers.len()).collect(),
        },
        WaitingFor::OptionalEffectChoice { .. } => GameAction::DecideOptionalEffect {
            accept: accept_optional,
        },
        WaitingFor::TriggerTargetSelection { .. } => GameAction::ChooseTarget {
            target: Some(TargetRef::Object(
                target.expect("a trigger target prompt needs a declared target"),
            )),
        },
        WaitingFor::EffectZoneChoice { .. } => GameAction::SelectCards {
            cards: vec![target.expect("a resolution card choice needs a declared target")],
        },
        other => panic!("unhandled prompt: {other:?}"),
    };
    runner.act(action).expect("action accepted").events
}

/// Answers prompts until `stop` holds at a priority window or the stack is
/// empty at one; returns every event emitted on the way.
fn drive_until(
    runner: &mut GameRunner,
    accept_optional: bool,
    target: Option<ObjectId>,
    mut on_priority: impl FnMut(&GameState),
    stop: impl Fn(&GameState) -> bool,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..60 {
        let state = runner.state();
        if at_priority(state) {
            on_priority(state);
            if state.stack.is_empty() || stop(state) {
                return events;
            }
        }
        events.extend(answer(runner, accept_optional, target));
    }
    panic!("the stack did not settle within the step bound");
}

fn drive_to_empty_stack(
    runner: &mut GameRunner,
    accept_optional: bool,
    target: Option<ObjectId>,
    on_priority: impl FnMut(&GameState),
) -> Vec<GameEvent> {
    drive_until(runner, accept_optional, target, on_priority, |_| false)
}

fn ltb_resolved(events: &[GameEvent], source: ObjectId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved { kind: EffectKind::ChangeZoneAll, source_id, .. }
                if *source_id == source
        )
    })
}

/// CR 607.2a + the Fiend Hunter ruling: the Hunter leaves while its
/// accepted ETB is on the stack, so its LTB resolves first and returns nothing;
/// the ETB then exiles the Bears for good.
#[test]
fn fiend_hunter_leaving_before_its_etb_resolves_exiles_indefinitely() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hunter = scenario.add_real_card(P0, "Fiend Hunter", Zone::Hand, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let mut mana = pool(2, ManaType::White);
    mana.extend(pool(1, ManaType::Colorless));
    mana.extend(pool(1, ManaType::Red));
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();

    drop(runner.cast(hunter).commit());
    drive_until(
        &mut runner,
        true,
        Some(bears),
        |_| {},
        |state| top_is_triggered_from(state, hunter),
    );
    assert!(
        top_is_triggered_from(runner.state(), hunter),
        "Hunter's ETB must be on the stack"
    );

    drop(runner.cast(bolt).target_object(hunter).commit());
    let mut ltb_above_etb = false;
    drive_to_empty_stack(&mut runner, true, Some(bears), |state| {
        let len = state.stack.len();
        ltb_above_etb |= len >= 2
            && is_triggered_from(state, len - 1, hunter)
            && is_triggered_from(state, len - 2, hunter);
    });

    assert!(
        ltb_above_etb,
        "the Hunter's LTB must stand on the stack above its ETB"
    );
    assert_eq!(runner.state().objects[&hunter].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Exile,
        "the Bears stay exiled: the LTB resolved before the exile (CR 607.2a)"
    );
}

/// Declining Fiend Hunter's optional exile leaves nothing linked,
/// so the LTB resolves with an empty pile.
#[test]
fn fiend_hunter_declined_exile_ltb_returns_nothing() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hunter = scenario.add_real_card(P0, "Fiend Hunter", Zone::Hand, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let mut mana = pool(2, ManaType::White);
    mana.extend(pool(1, ManaType::Colorless));
    mana.extend(pool(1, ManaType::Red));
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();

    drop(runner.cast(hunter).commit());
    drive_to_empty_stack(&mut runner, false, Some(bears), |_| {});
    assert_eq!(runner.state().objects[&hunter].zone, Zone::Battlefield);

    drop(runner.cast(bolt).target_object(hunter).commit());
    let events = drive_to_empty_stack(&mut runner, false, Some(bears), |_| {});

    assert!(
        ltb_resolved(&events, hunter),
        "the Hunter's LTB must resolve"
    );
    assert_eq!(runner.state().objects[&hunter].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
}

/// CR 607.2a + CR 110.2a: the exiled card stays in exile while the
/// LTB is on the stack, then returns under its owner's control (owner P1 is not
/// the LTB's controller P0).
#[test]
fn removal_of_exiler_returns_card_when_ltb_resolves_under_owner() {
    for name in ["Oblivion Ring", "Journey to Nowhere"] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let exiler = scenario.add_real_card(P0, name, Zone::Hand, db);
        let disenchant = scenario.add_real_card(P0, "Disenchant", Zone::Hand, db);
        let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
        let mut mana = pool(3, ManaType::White);
        mana.extend(pool(3, ManaType::Colorless));
        scenario.with_mana_pool(P0, mana);
        let mut runner = scenario.build();

        drop(runner.cast(exiler).commit());
        drive_to_empty_stack(&mut runner, true, Some(bears), |_| {});
        assert_eq!(
            runner.state().objects[&bears].zone,
            Zone::Exile,
            "{name} must exile the Bears"
        );

        drop(runner.cast(disenchant).target_object(exiler).commit());
        let mut zone_while_ltb_pending = None;
        drive_to_empty_stack(&mut runner, true, None, |state| {
            if top_is_triggered_from(state, exiler)
                && state.objects[&exiler].zone != Zone::Battlefield
            {
                zone_while_ltb_pending.get_or_insert(state.objects[&bears].zone);
            }
        });

        assert_eq!(
            zone_while_ltb_pending,
            Some(Zone::Exile),
            "{name}: the Bears must stay exiled while the LTB is on the stack"
        );
        let state = runner.state();
        assert_eq!(
            state.objects[&bears].zone,
            Zone::Battlefield,
            "{name}: the Bears return"
        );
        assert_eq!(
            state.objects[&bears].controller, P1,
            "{name}: under their owner's control"
        );
        assert!(state.exile_links.is_empty(), "{name}: the link is consumed");
    }
}

/// The resolution-choice member: Wormfang Newt's "exile a land you
/// control" is chosen at resolution, links to the Newt, and returns on its LTB.
#[test]
fn wormfang_newt_chosen_land_returns_when_ltb_resolves() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let newt = scenario.add_real_card(P0, "Wormfang Newt", Zone::Hand, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let plains = scenario.add_real_card(P0, "Plains", Zone::Battlefield, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let mut mana = pool(1, ManaType::Blue);
    mana.extend(pool(1, ManaType::Colorless));
    mana.extend(pool(1, ManaType::Red));
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();

    drop(runner.cast(newt).commit());
    drive_to_empty_stack(&mut runner, true, Some(island), |_| {});
    assert_eq!(runner.state().objects[&island].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&plains].zone, Zone::Battlefield);

    drop(runner.cast(bolt).target_object(newt).commit());
    let mut zone_while_ltb_pending = None;
    drive_to_empty_stack(&mut runner, true, None, |state| {
        if top_is_triggered_from(state, newt) && state.objects[&newt].zone != Zone::Battlefield {
            zone_while_ltb_pending.get_or_insert(state.objects[&island].zone);
        }
    });

    assert_eq!(zone_while_ltb_pending, Some(Zone::Exile));
    assert_eq!(runner.state().objects[&island].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&island].controller, P0);
}

/// With each loop event triggering exactly one ability (Dragon ETB →
/// Animate Dead's sacrifice → Dragon LTB → Animate Dead's return), the
/// Worldgorger Dragon + Animate Dead loop never stacks more than one object.
#[test]
fn worldgorger_animate_dead_loop_keeps_stack_bounded_with_detection_off() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragon = scenario.add_real_card(P0, "Worldgorger Dragon", Zone::Graveyard, db);
    let animate = scenario.add_real_card(P0, "Animate Dead", Zone::Hand, db);
    scenario.with_mana_pool(P0, pool(2, ManaType::Black));
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Off;

    drop(runner.cast(animate).target_object(dragon).commit());
    let is_dragon_etb = |state: &GameState| {
        state.stack.iter().any(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id, ability, ..
            } => {
                *source_id == dragon
                    && matches!(
                        ability.effect,
                        Effect::ChangeZoneAll {
                            destination: Zone::Exile,
                            ..
                        }
                    )
            }
            _ => false,
        })
    };
    let (mut stack_max, mut etb_cycles, mut etb_was_on_stack) = (0, 0, false);
    for _ in 0..120 {
        answer(&mut runner, true, Some(dragon));
        let state = runner.state();
        stack_max = stack_max.max(state.stack.len());
        let etb_on_stack = is_dragon_etb(state);
        etb_cycles += usize::from(etb_on_stack && !etb_was_on_stack);
        etb_was_on_stack = etb_on_stack;
    }

    assert!(
        etb_cycles >= 3,
        "the loop must run several Dragon cycles, ran {etb_cycles}"
    );
    assert!(
        stack_max <= 1,
        "the stack must not grow per cycle, reached {stack_max}"
    );
}

/// Mutates `rider` onto `host` on the battlefield (CR 730.2); mutate casting is
/// not under test.
fn merge(runner: &mut GameRunner, rider: ObjectId, host: ObjectId) {
    let state = runner.state_mut();
    let mut events = Vec::new();
    merge_object_onto(state, rider, host, MergeSide::Top, &mut events);
    state.battlefield.retain(|&id| id != rider);
}

fn assert_returned_unmerged(state: &GameState, ids: &[ObjectId], label: &str) {
    for id in ids {
        let obj = &state.objects[id];
        assert_eq!(obj.zone, Zone::Battlefield, "{label}: {id:?} returns");
        assert_eq!(obj.controller, P1, "{label}: {id:?} under its owner");
        assert!(
            obj.merged_components.is_empty() && obj.split_from_merge_survivor.is_none(),
            "{label}: {id:?} returns as a separate, unmerged object (CR 730.3)"
        );
    }
}

/// CR 607.2a + CR 730.3c: Oblivion Ring's LTB finds every object its
/// exiled merged permanent became, and only those: Journey to Nowhere's merged
/// pile, co-located in exile, stays there.
#[test]
fn class_ltb_return_brings_back_every_merged_component_of_its_own_pile() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host_a = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let rider_a = scenario.add_real_card(P1, "Gemrazer", Zone::Battlefield, db);
    let host_b = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let rider_b = scenario.add_real_card(P1, "Gemrazer", Zone::Battlefield, db);
    let ring = scenario.add_real_card(P0, "Oblivion Ring", Zone::Hand, db);
    let journey = scenario.add_real_card(P0, "Journey to Nowhere", Zone::Hand, db);
    let disenchant = scenario.add_real_card(P0, "Disenchant", Zone::Hand, db);
    let mut mana = pool(3, ManaType::White);
    mana.extend(pool(4, ManaType::Colorless));
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();
    merge(&mut runner, rider_a, host_a);
    merge(&mut runner, rider_b, host_b);

    drop(runner.cast(ring).commit());
    drive_to_empty_stack(&mut runner, true, Some(host_a), |_| {});
    drop(runner.cast(journey).commit());
    drive_to_empty_stack(&mut runner, true, Some(host_b), |_| {});
    for id in [host_a, rider_a, host_b, rider_b] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
    }

    drop(runner.cast(disenchant).target_object(ring).commit());
    drive_to_empty_stack(&mut runner, true, None, |_| {});

    let state = runner.state();
    assert_returned_unmerged(state, &[host_a, rider_a], "Oblivion Ring's pile");
    assert_eq!(state.objects[&host_b].zone, Zone::Exile);
    assert_eq!(
        state.objects[&rider_b].zone,
        Zone::Exile,
        "a pile linked to another source is not found"
    );
    assert!(
        state
            .exile_links
            .iter()
            .any(|link| link.exiled_id == host_b && link.source_id == journey),
        "Journey's link survives the Ring's return"
    );
}

/// CR 607.2a + CR 730.3c: Admonition Angel's "return all cards exiled
/// with it" brings back every component of a merged permanent it exiled.
#[test]
fn admonition_angel_return_brings_back_every_merged_component() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let rider = scenario.add_real_card(P1, "Gemrazer", Zone::Battlefield, db);
    let angel = scenario.add_real_card(P0, "Admonition Angel", Zone::Battlefield, db);
    let plains = scenario.add_real_card(P0, "Plains", Zone::Hand, db);
    let unsummon = scenario.add_real_card(P0, "Unsummon", Zone::Hand, db);
    scenario.with_mana_pool(P0, pool(1, ManaType::Blue));
    let mut runner = scenario.build();
    merge(&mut runner, rider, host);

    let card_id = runner.state().objects[&plains].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: plains,
            card_id,
        })
        .expect("land play accepted");
    drive_to_empty_stack(&mut runner, true, Some(host), |_| {});
    assert_eq!(runner.state().objects[&host].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&rider].zone, Zone::Exile);

    drop(runner.cast(unsummon).target_object(angel).commit());
    drive_to_empty_stack(&mut runner, true, None, |_| {});

    let state = runner.state();
    assert_eq!(state.objects[&angel].zone, Zone::Hand);
    assert_returned_unmerged(state, &[host, rider], "Admonition Angel's pile");
}

/// CR 400.7 + CR 607.2a + CR 730.3c: a component split from a merged
/// permanent is found only through the survivor's incarnation that split it.
/// Once that survivor changes zones again (graveyard, battlefield, then a
/// second, linked exile), the linked return brings back the survivor alone.
#[test]
fn linked_return_leaves_component_of_a_survivor_that_changed_zones_again() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let rider = scenario.add_real_card(P1, "Gemrazer", Zone::Battlefield, db);
    let angel = scenario.add_real_card(P0, "Admonition Angel", Zone::Battlefield, db);
    let swords = scenario.add_real_card(P0, "Swords to Plowshares", Zone::Hand, db);
    let raker = scenario.add_real_card(P0, "Mind Raker", Zone::Hand, db);
    let reanimate = scenario.add_real_card(P0, "Reanimate", Zone::Hand, db);
    let plains = scenario.add_real_card(P0, "Plains", Zone::Hand, db);
    let unsummon = scenario.add_real_card(P0, "Unsummon", Zone::Hand, db);
    let mut mana = pool(1, ManaType::White);
    mana.extend(pool(2, ManaType::Black));
    mana.extend(pool(1, ManaType::Blue));
    mana.extend(pool(3, ManaType::Colorless));
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();
    merge(&mut runner, rider, host);

    drop(runner.cast(swords).target_object(host).commit());
    drive_to_empty_stack(&mut runner, true, Some(host), |_| {});
    let state = runner.state();
    assert_eq!(state.objects[&rider].zone, Zone::Exile);
    assert_eq!(
        state.objects[&rider].split_from_merge_survivor,
        Some(ObjectIncarnationRef::from_object(&state.objects[&host])),
        "the rider is pinned to the survivor's exile incarnation"
    );

    drop(runner.cast(raker).commit());
    drive_to_empty_stack(&mut runner, true, Some(host), |_| {});
    assert_eq!(runner.state().objects[&host].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&rider].zone, Zone::Exile);

    drop(runner.cast(reanimate).target_object(host).commit());
    drive_to_empty_stack(&mut runner, true, Some(host), |_| {});
    assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);

    let card_id = runner.state().objects[&plains].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: plains,
            card_id,
        })
        .expect("land play accepted");
    drive_to_empty_stack(&mut runner, true, Some(host), |_| {});
    assert_eq!(runner.state().objects[&host].zone, Zone::Exile);

    drop(runner.cast(unsummon).target_object(angel).commit());
    drive_to_empty_stack(&mut runner, true, None, |_| {});

    let state = runner.state();
    assert_eq!(state.objects[&angel].zone, Zone::Hand);
    assert_eq!(state.objects[&host].zone, Zone::Battlefield);
    assert_eq!(state.objects[&host].controller, P1);
    assert_eq!(
        state.objects[&rider].zone,
        Zone::Exile,
        "the rider split from an earlier incarnation of the survivor (CR 400.7)"
    );
}
