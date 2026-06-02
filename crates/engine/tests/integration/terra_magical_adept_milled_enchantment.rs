//! Regression: GitHub issue #1298 — Terra, Magical Adept ("When Terra enters,
//! mill five cards. Put up to one enchantment card milled this way into your
//! hand.").
//!
//! Bug: the put-from-milled clause offered battlefield enchantments instead of
//! scoping the choice to the cards milled by the preceding `Mill` (CR 701.17c).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter, TypeFilter, TypedFilter};
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

fn mark_enchantment(runner: &mut GameRunner, id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Enchantment);
}

fn seed_terra_library(scenario: &mut GameScenario) -> ObjectId {
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("Padding {i}"));
    }
    for i in (0..4).rev() {
        scenario.add_card_to_library_top(P0, &format!("Instant {i}"));
    }
    scenario.add_card_to_library_top(P0, "Milled Aura")
}

/// Issue #1298: parsed ETB must chain `Mill` → `ChangeZone` with
/// `TrackedSetFiltered`, not a bare type filter.
#[test]
fn terra_etb_parses_milled_this_way_as_tracked_set_filtered() {
    let parsed = parse_oracle_text(
        "When Terra enters, mill five cards. Put up to one enchantment card milled this way into your hand.\n\
         Trance — {4}{R}{G}, {T}: Exile Terra, then return it to the battlefield transformed under its owner's control. Activate only as a sorcery.",
        "Terra, Magical Adept",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Human".to_string(), "Wizard".to_string()],
    );

    let etb = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone && t.destination == Some(Zone::Battlefield))
        .expect("Terra must have an ETB trigger");

    let execute = etb.execute.as_ref().expect("ETB trigger must have execute");
    assert!(
        matches!(&*execute.effect, Effect::Mill { .. }),
        "ETB root must be Mill, got {:?}",
        execute.effect
    );

    let put = execute
        .sub_ability
        .as_ref()
        .expect("Mill must chain to put-from-milled clause");
    let Effect::ChangeZone { target, .. } = &*put.effect else {
        panic!("expected ChangeZone put clause, got {:?}", put.effect);
    };
    let TargetFilter::TrackedSetFiltered { id, filter } = target else {
        panic!("put-from-milled must target TrackedSetFiltered, got {target:?}");
    };
    assert_eq!(id.0, 0, "sentinel TrackedSetId(0) — resolved at runtime");
    assert!(
        matches!(
            filter.as_ref(),
            TargetFilter::Typed(TypedFilter { type_filters, .. })
                if type_filters.contains(&TypeFilter::Enchantment)
        ),
        "inner filter must scope to enchantment cards, got {filter:?}"
    );
}

/// Issue #1298: resolving Terra's ETB must offer only milled enchantments.
#[test]
fn terra_etb_offers_only_milled_enchantments_not_battlefield() {
    use engine::game::ability_utils::build_resolved_from_def;
    use engine::game::effects::resolve_ability_chain;

    let parsed = parse_oracle_text(
        "When Terra enters, mill five cards. Put up to one enchantment card milled this way into your hand.\n\
         Trance — {4}{R}{G}, {T}: Exile Terra, then return it to the battlefield transformed under its owner's control. Activate only as a sorcery.",
        "Terra, Magical Adept",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Human".to_string(), "Wizard".to_string()],
    );

    let etb = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone && t.destination == Some(Zone::Battlefield))
        .expect("ETB trigger");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let terra_id = scenario
        .add_creature_to_hand(P0, "Terra, Magical Adept", 2, 2)
        .id();

    let milled_enchantment = seed_terra_library(&mut scenario);

    let mut runner = scenario.build();
    mark_enchantment(&mut runner, milled_enchantment);

    // Trap: battlefield enchantment matches the inner type filter but is not milled.
    let battlefield_enchantment = create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(99),
        P0,
        "Battlefield Aura".to_string(),
        Zone::Battlefield,
    );
    mark_enchantment(&mut runner, battlefield_enchantment);

    let execute = etb.execute.as_ref().expect("ETB execute");
    let ability = build_resolved_from_def(execute, terra_id, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Terra ETB chain should resolve");

    let WaitingFor::EffectZoneChoice {
        cards, destination, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected EffectZoneChoice for put-from-milled clause, got {:?}",
            runner.state().waiting_for
        );
    };

    assert!(
        cards.contains(&milled_enchantment),
        "the milled enchantment must be offered; offered = {cards:?}"
    );
    assert!(
        !cards.contains(&battlefield_enchantment),
        "a battlefield enchantment must NEVER be offered — selection is scoped \
         to the milled tracked set (issue #1298); offered = {cards:?}"
    );
    assert_eq!(
        *destination,
        Some(Zone::Hand),
        "the chosen milled card moves to hand"
    );
}

/// Full cast → ETB trigger → mill → put-from-milled path (issue #1298).
#[test]
fn terra_cast_etb_offers_only_milled_enchantments() {
    use engine::types::actions::GameAction;
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

    let parsed = parse_oracle_text(
        "When Terra enters, mill five cards. Put up to one enchantment card milled this way into your hand.\n\
         Trance — {4}{R}{G}, {T}: Exile Terra, then return it to the battlefield transformed under its owner's control. Activate only as a sorcery.",
        "Terra, Magical Adept",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Human".to_string(), "Wizard".to_string()],
    );

    let etb = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone && t.destination == Some(Zone::Battlefield))
        .expect("ETB trigger")
        .clone();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let terra_id = scenario
        .add_creature_to_hand(P0, "Terra, Magical Adept", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Red],
            generic: 0,
        })
        .with_trigger_definition(etb)
        .id();

    let milled_enchantment = seed_terra_library(&mut scenario);

    let mut runner = scenario.build();
    mark_enchantment(&mut runner, milled_enchantment);

    let battlefield_enchantment = create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(99),
        P0,
        "Battlefield Aura".to_string(),
        Zone::Battlefield,
    );
    mark_enchantment(&mut runner, battlefield_enchantment);

    let dummy = ObjectId(0);
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .add(ManaUnit::new(ManaType::Green, dummy, false, vec![]));
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .add(ManaUnit::new(ManaType::Red, dummy, false, vec![]));

    let card_id = runner.state().objects[&terra_id].card_id;
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: terra_id,
            card_id,
            targets: vec![],
        })
        .expect("cast Terra");

    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 96, "stuck waiting; last = {:?}", result.waiting_for);
        match &result.waiting_for {
            WaitingFor::EffectZoneChoice {
                cards, destination, ..
            } => {
                assert!(cards.contains(&milled_enchantment), "offered = {cards:?}");
                assert!(
                    !cards.contains(&battlefield_enchantment),
                    "battlefield trap offered (issue #1298); offered = {cards:?}"
                );
                assert_eq!(*destination, Some(Zone::Hand));
                return;
            }
            WaitingFor::ManaPayment { .. } => {
                result = runner
                    .act(GameAction::PassPriority)
                    .expect("finalize mana payment");
            }
            WaitingFor::Priority { .. } => {
                result = runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for during Terra cast ETB: {other:?}"),
        }
    }
}
