use engine::ai_support::legal_actions_full;
use engine::database::synthesis::synthesize_plot;
use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::AttachTarget;
use engine::game::mana_abilities::activate_mana_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::parser::oracle_cost::parse_oracle_cost;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, BounceSelection, CardPlayMode, CardSelectionMode,
    CastFromZoneDriver, CastingPermission, CategoryChooserScope, ChoiceType, Chooser,
    ContinuousModification, DelayedTriggerCondition, DelayedTriggerLifetime, DigRestOrder,
    DigSource, DiscardSelfScope, Effect, EffectKind, FilterProp, ForEachCategoryAction,
    IterationCategory, ManaContribution, ManaProduction, ManaSpendRestriction, ModalChoice,
    OpponentMayScope, QuantityExpr, QuantityRef, ReplacementDefinition, ReplacementMode,
    ResolvedAbility, SacrificeCost, SpellCastingOption, TargetFilter, TargetRef,
    TargetSelectionMode, TriggerConstraint, TriggerDefinition, TypeFilter, TypedFilter,
    UnlessPayModifier, WheneverEventExpiry,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{
    BatchCompletion, CastPaymentMode, CollectEvidenceResume, ExileLinkKind, GameState,
    ManaAbilityCostParentLifecycle, ManaAbilityCostResolutionMode, ManaAbilityResume, ManaChoice,
    PayCostKind, PendingCast, PendingCostMoveResume, PendingReplacement, StackEntryKind,
    WaitingFor, ZoneDeliveryExileTracking,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::proposed_event::{ProposedEvent, ReplacementId};
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};
use std::sync::Arc;

fn redirect_moved_to(destination: Zone, redirected_to: Zone) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(destination)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: redirected_to,
                origin: None,
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
        ))
}

/// CR 616.1: source-linked exile tracking is part of the parked zone-move
/// request and must survive an optional replacement choice.
#[test]
fn exile_tracking_parked_resume_preserves_source_link() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Exile Source", 1, 1).id();
    scenario
        .add_creature(P0, "Optional Exile Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(
            redirect_moved_to(Zone::Exile, Zone::Graveyard)
                .mode(ReplacementMode::Optional { decline: None }),
        );
    let exiled = scenario
        .add_creature_to_graveyard(P0, "Tracked Card", 1, 1)
        .id();
    let mut runner = scenario.build();

    let mut events = Vec::new();
    let paused = move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(exiled, Zone::Exile, source).track_exiled_by_source(),
        &mut events,
    );
    assert!(paused);
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .map(|pending| pending.exile_tracking),
        Some(ZoneDeliveryExileTracking::TrackBySource)
    );

    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline optional redirect");

    assert_eq!(runner.state().objects[&exiled].zone, Zone::Exile);
    assert!(runner.state().exile_links.iter().any(|link| {
        link.exiled_id == exiled
            && link.source_id == source
            && matches!(link.kind, ExileLinkKind::TrackedBySource)
    }));
}

/// W-R1 (red first): a Dig rest pile sent to the library bottom is an
/// effect-owned batch. Competing Library-destination `Moved` replacements must
/// pause before the kept tracked set is published, then re-pause safely while
/// the rest pile drains.
#[test]
fn dig_rest_pile_library_redirect_pauses_before_tracked_set_publish() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Dig Rest-Pile Redirect Source", 1, 1)
        .id();
    let kept = scenario
        .add_spell_to_library_top(P0, "Dig Kept Card", true)
        .id();
    let rest_a = scenario
        .add_spell_to_library_top(P0, "Dig Rest Card A", true)
        .id();
    let rest_b = scenario
        .add_spell_to_library_top(P0, "Dig Rest Card B", true)
        .id();
    let redirect_sources = [
        scenario
            .add_creature(P0, "Dig Library To Graveyard", 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Library, Zone::Graveyard))
            .id(),
        scenario
            .add_creature(P0, "Dig Library To Exile", 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Library, Zone::Exile))
            .id(),
    ];

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![kept, rest_a, rest_b];
    let ability = ResolvedAbility::new(
        Effect::Dig {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 3 },
            destination: None,
            keep_count: Some(1),
            keep_count_expr: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: Some(Zone::Library),
            rest_order: DigRestOrder::Preserve,
            reveal: true,
            enter_tapped: false,
            enters_attacking: false,
            source: DigSource::Library,
        },
        vec![],
        source,
        P0,
    );
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("Dig reaches its selection");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DigChoice { .. }
    ));

    let paused = runner
        .act(GameAction::SelectCards { cards: vec![kept] })
        .expect("Dig submits the kept card and reaches the first bottom placement");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let parked_order = runner
        .state()
        .active_batch_delivery()
        .expect("the second rest card is parked behind the first replacement choice")
        .remaining
        .clone();
    assert_eq!(parked_order.len(), 1);
    assert!(
        runner.state().chain_tracked_set_id.is_none(),
        "the kept set cannot publish while a rest placement remains undecided"
    );
    for card_id in [kept, rest_a, rest_b] {
        assert!(
            runner.state().revealed_cards.contains(&card_id),
            "reveal bookkeeping must remain intact while the rest-pile batch is parked"
        );
    }

    let first_redirect = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("first rest-card redirect resolves");
    assert!(matches!(
        first_redirect.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        runner.state().chain_tracked_set_id.is_none(),
        "a re-paused rest batch still cannot publish its tracked set"
    );
    for redirect_source in redirect_sources {
        let redirect_source = runner
            .state_mut()
            .objects
            .get_mut(&redirect_source)
            .expect("synthetic redirect source remains on the battlefield");
        redirect_source.replacement_definitions.clear();
        Arc::make_mut(&mut redirect_source.base_replacement_definitions).clear();
    }
    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("unredirected rest-pile suffix drains");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
    let tracked = runner
        .state()
        .tracked_object_sets
        .get(
            &runner
                .state()
                .chain_tracked_set_id
                .expect("Dig publishes a tracked set once its rest pile settles"),
        )
        .expect("the freshly-published Dig tracked set exists");
    assert_eq!(tracked, &vec![kept]);
    let redirected_id = [rest_a, rest_b]
        .into_iter()
        .find(|id| !parked_order.contains(id))
        .expect("first attempted rest card is outside the parked suffix");
    assert_ne!(runner.state().objects[&redirected_id].zone, Zone::Library);
    assert_eq!(runner.state().objects[&parked_order[0]].zone, Zone::Library);
}

/// CR 608.2c + CR 616.1: declining an up-to Dig selection publishes its empty
/// kept set after a deferred rest-pile delivery; it must not publish the cards
/// that merely completed the rest route.
#[test]
fn dig_zero_kept_deferred_rest_pile_publishes_an_empty_tracked_set() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Zero-Kept Dig Source", 1, 1).id();
    let rest_a = scenario
        .add_spell_to_library_top(P0, "Zero-Kept Rest A", true)
        .id();
    let rest_b = scenario
        .add_spell_to_library_top(P0, "Zero-Kept Rest B", true)
        .id();
    let redirect_sources = [
        scenario
            .add_creature(P0, "Zero-Kept Library To Graveyard", 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Library, Zone::Graveyard))
            .id(),
        scenario
            .add_creature(P0, "Zero-Kept Library To Exile", 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Library, Zone::Exile))
            .id(),
    ];

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![rest_a, rest_b];
    let ability = ResolvedAbility::new(
        Effect::Dig {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 2 },
            destination: None,
            keep_count: Some(1),
            keep_count_expr: None,
            up_to: true,
            filter: TargetFilter::Any,
            rest_destination: Some(Zone::Library),
            rest_order: DigRestOrder::Preserve,
            reveal: true,
            enter_tapped: false,
            enters_attacking: false,
            source: DigSource::Library,
        },
        vec![],
        source,
        P0,
    );
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("Dig reaches its up-to selection");

    let paused = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("an empty up-to selection reaches the rest-pile replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let reparking = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the first rest redirect reaches the remaining rest card");
    assert!(matches!(
        reparking.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    for redirect_source in redirect_sources {
        let redirect_source = runner
            .state_mut()
            .objects
            .get_mut(&redirect_source)
            .expect("synthetic redirect source remains on the battlefield");
        redirect_source.replacement_definitions.clear();
        Arc::make_mut(&mut redirect_source.base_replacement_definitions).clear();
    }
    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the remaining rest-pile delivery completes");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
    let redirected_rest = [rest_a, rest_b]
        .into_iter()
        .find(|id| {
            matches!(
                runner.state().objects[id].zone,
                Zone::Graveyard | Zone::Exile
            )
        })
        .expect("one rest-pile card must take the configured redirect");
    let returned_rest = [rest_a, rest_b]
        .into_iter()
        .find(|id| runner.state().objects[id].zone == Zone::Library)
        .expect("the other rest-pile card must complete its library placement");
    assert_ne!(redirected_rest, returned_rest);
    assert!(
        runner.state().players[P0.0 as usize]
            .library
            .contains(&returned_rest),
        "the unredirected rest-pile card must be present in P0's library"
    );

    let tracked = runner
        .state()
        .tracked_object_sets
        .get(
            &runner
                .state()
                .chain_tracked_set_id
                .expect("the deferred Dig completion publishes a tracked set"),
        )
        .expect("the freshly-published Dig tracked set exists");
    assert!(
        tracked.is_empty(),
        "the rest-pile delivery must not replace an empty kept selection"
    );
}

/// W-R3 (red first): deterministic Dig's nonbattlefield kept batch must defer
/// its tracked-set publication and downstream tracked-set consumer until every
/// selected card has either reached the requested destination or been
/// redirected elsewhere.
#[test]
fn dig_mass_put_all_nonbattlefield_redirect_publishes_only_delivered_set() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Mass Dig Redirect Source", 1, 1)
        .id();
    let selected_a = scenario
        .add_spell_to_library_top(P0, "Mass Dig Selected A", true)
        .id();
    let selected_b = scenario
        .add_spell_to_library_top(P0, "Mass Dig Selected B", true)
        .id();
    let drawn = scenario
        .add_spell_to_library_top(P0, "Mass Dig Tracked-Set Draw", true)
        .id();
    let redirect_sources = [
        scenario
            .add_creature(P0, "Mass Dig Hand To Exile", 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Hand, Zone::Exile))
            .id(),
        scenario
            .add_creature(P0, "Mass Dig Hand To Graveyard", 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Hand, Zone::Graveyard))
            .id(),
    ];

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![selected_a, selected_b, drawn];
    let mut ability = ResolvedAbility::new(
        Effect::Dig {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 2 },
            destination: Some(Zone::Hand),
            keep_count: Some(u32::MAX),
            keep_count_expr: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: Some(Zone::Library),
            rest_order: DigRestOrder::Preserve,
            reveal: true,
            enter_tapped: false,
            enters_attacking: false,
            source: DigSource::Library,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize,
            },
            target: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("mass Dig reaches its first kept-card delivery");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let parked_order = runner
        .state()
        .active_batch_delivery()
        .expect("the second selected card is batch-owned behind the first redirect")
        .remaining
        .clone();
    assert_eq!(parked_order.len(), 1);
    assert!(
        runner
            .state()
            .tracked_object_sets
            .values()
            .all(|set| !set.contains(&selected_a) && !set.contains(&selected_b)),
        "a nested replacement may allocate an unrelated empty tracked set, but the mass Dig's selected cards cannot publish before their batch settles"
    );
    assert_eq!(
        runner.state().objects[&drawn].zone,
        Zone::Library,
        "the chained tracked-set consumer cannot run while the selected batch is parked"
    );
    assert!(
        !initial_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::Dig,
                source_id,
                ..
            } if *source_id == source
        )),
        "the parent Dig result must wait for the selected batch"
    );

    let first_redirect = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("first selected-card redirect resolves");
    assert!(matches!(
        first_redirect.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(runner
        .state()
        .tracked_object_sets
        .values()
        .all(|set| !set.contains(&selected_a) && !set.contains(&selected_b)));
    for redirect_source in redirect_sources {
        let redirect_source = runner
            .state_mut()
            .objects
            .get_mut(&redirect_source)
            .expect("synthetic redirect source remains on the battlefield");
        redirect_source.replacement_definitions.clear();
        Arc::make_mut(&mut redirect_source.base_replacement_definitions).clear();
    }
    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the remaining selected card reaches hand and the mass Dig completes");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));

    let redirected_id = [selected_a, selected_b]
        .into_iter()
        .find(|id| !parked_order.contains(id))
        .expect("first selected card is outside the parked suffix");
    let delivered_id = parked_order[0];
    assert_ne!(runner.state().objects[&redirected_id].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&delivered_id].zone, Zone::Hand);
    let tracked = runner
        .state()
        .tracked_object_sets
        .get(
            &runner
                .state()
                .chain_tracked_set_id
                .expect("mass Dig publishes only after the kept batch settles"),
        )
        .expect("the mass Dig tracked set exists");
    assert_eq!(tracked, &vec![delivered_id]);
    assert_eq!(
        runner.state().objects[&drawn].zone,
        Zone::Hand,
        "the chained tracked-set draw sees exactly the delivered selected-card count"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(first_redirect.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::Dig,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the parent Dig completion fires exactly once after the kept batch settles"
    );
}

/// W-REG: The migration keeps the no-replacement fast paths synchronous for
/// both interactive and deterministic Dig; the search split fast path remains
/// covered by `cultivate_split_destination`.
#[test]
fn uninterrupted_dig_rest_and_mass_put_all_complete_synchronously() {
    let mut dig_scenario = GameScenario::new();
    dig_scenario.at_phase(Phase::PreCombatMain);
    let dig_source = dig_scenario
        .add_creature(P0, "Synchronous Dig Source", 1, 1)
        .id();
    let kept = dig_scenario
        .add_spell_to_library_top(P0, "Synchronous Dig Kept", true)
        .id();
    let rest_a = dig_scenario
        .add_spell_to_library_top(P0, "Synchronous Dig Rest A", true)
        .id();
    let rest_b = dig_scenario
        .add_spell_to_library_top(P0, "Synchronous Dig Rest B", true)
        .id();
    let mut dig_runner = dig_scenario.build();
    dig_runner.state_mut().players[P0.0 as usize].library = im::vector![kept, rest_a, rest_b];
    let dig = ResolvedAbility::new(
        Effect::Dig {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 3 },
            destination: None,
            keep_count: Some(1),
            keep_count_expr: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: Some(Zone::Library),
            rest_order: DigRestOrder::Preserve,
            reveal: false,
            enter_tapped: false,
            enters_attacking: false,
            source: DigSource::Library,
        },
        vec![],
        dig_source,
        P0,
    );
    let mut dig_events = Vec::new();
    resolve_ability_chain(dig_runner.state_mut(), &dig, &mut dig_events, 0)
        .expect("uninterrupted Dig reaches its selection");
    let dig_completed = dig_runner
        .act(GameAction::SelectCards { cards: vec![kept] })
        .expect("uninterrupted Dig rest batch completes inline");
    assert!(matches!(
        dig_completed.waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(dig_runner.state().active_batch_delivery().is_none());
    assert_eq!(dig_runner.state().objects[&rest_a].zone, Zone::Library);
    assert_eq!(dig_runner.state().objects[&rest_b].zone, Zone::Library);
    let dig_tracked = dig_runner
        .state()
        .tracked_object_sets
        .get(
            &dig_runner
                .state()
                .chain_tracked_set_id
                .expect("synchronous Dig publishes its kept set"),
        )
        .expect("synchronous Dig tracked set exists");
    assert_eq!(dig_tracked, &vec![kept]);

    let mut mass_scenario = GameScenario::new();
    mass_scenario.at_phase(Phase::PreCombatMain);
    let mass_source = mass_scenario
        .add_creature(P0, "Synchronous Mass Dig Source", 1, 1)
        .id();
    let selected_a = mass_scenario
        .add_spell_to_library_top(P0, "Synchronous Mass Dig A", true)
        .id();
    let selected_b = mass_scenario
        .add_spell_to_library_top(P0, "Synchronous Mass Dig B", true)
        .id();
    let mut mass_runner = mass_scenario.build();
    mass_runner.state_mut().players[P0.0 as usize].library = im::vector![selected_a, selected_b];
    let mass_dig = ResolvedAbility::new(
        Effect::Dig {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 2 },
            destination: Some(Zone::Hand),
            keep_count: Some(u32::MAX),
            keep_count_expr: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: Some(Zone::Library),
            rest_order: DigRestOrder::Preserve,
            reveal: false,
            enter_tapped: false,
            enters_attacking: false,
            source: DigSource::Library,
        },
        vec![],
        mass_source,
        P0,
    );
    let mut mass_events = Vec::new();
    resolve_ability_chain(mass_runner.state_mut(), &mass_dig, &mut mass_events, 0)
        .expect("uninterrupted deterministic Dig resolves inline");
    assert!(matches!(
        mass_runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(mass_runner.state().active_batch_delivery().is_none());
    assert_eq!(mass_runner.state().objects[&selected_a].zone, Zone::Hand);
    assert_eq!(mass_runner.state().objects[&selected_b].zone, Zone::Hand);
    let mass_tracked = mass_runner
        .state()
        .tracked_object_sets
        .get(
            &mass_runner
                .state()
                .chain_tracked_set_id
                .expect("synchronous mass Dig publishes after its batch"),
        )
        .expect("synchronous mass Dig tracked set exists");
    assert_eq!(mass_tracked, &vec![selected_a, selected_b]);
}

/// W-R2: A Dig's deferred completion can itself start a Library-bottom batch
/// that re-pauses while draining. Its cleanup must survive both pause boundaries
/// and publish exactly once at the true end, regardless of which completion
/// carrier owns the kept-card delivery.
#[test]
fn dig_deferred_reveal_rest_pile_repauses_and_completes_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Deferred Dig Rest-Pile Source", 1, 1)
        .id();
    let kept = scenario
        .add_spell_to_library_top(P0, "Deferred Dig Kept", true)
        .id();
    let rest_a = scenario
        .add_spell_to_library_top(P0, "Deferred Dig Rest A", true)
        .id();
    let rest_b = scenario
        .add_spell_to_library_top(P0, "Deferred Dig Rest B", true)
        .id();
    for (name, destination) in [
        ("Deferred Dig Battlefield Redirect A", Zone::Graveyard),
        ("Deferred Dig Battlefield Redirect B", Zone::Exile),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Battlefield, destination));
    }
    let library_redirect_sources = [
        scenario
            .add_creature(P0, "Deferred Dig Library Redirect A", 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Library, Zone::Graveyard))
            .id(),
        scenario
            .add_creature(P0, "Deferred Dig Library Redirect B", 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Library, Zone::Exile))
            .id(),
    ];

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![kept, rest_a, rest_b];
    let ability = ResolvedAbility::new(
        Effect::Dig {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 3 },
            destination: Some(Zone::Battlefield),
            keep_count: Some(1),
            keep_count_expr: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: Some(Zone::Library),
            rest_order: DigRestOrder::Preserve,
            reveal: true,
            enter_tapped: false,
            enters_attacking: false,
            source: DigSource::Library,
        },
        vec![],
        source,
        P0,
    );
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Dig reaches its kept-card selection");

    let kept_pause = runner
        .act(GameAction::SelectCards { cards: vec![kept] })
        .expect("kept battlefield entry reaches a replacement choice");
    assert!(matches!(
        kept_pause.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        matches!(
            runner
                .state()
                .active_batch_delivery()
                .and_then(|pending| pending.completion.as_ref()),
            Some(BatchCompletion::RevealRestPile {
                delivery_stage: engine::types::game_state::DigDeliveryStage::Kept,
                ..
            })
        ),
        "the kept-card pause must retain a RevealRestPile completion for the deferred rest pile"
    );

    let rest_pause = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("kept-card resolution enters the deferred rest-pile route");
    assert!(matches!(
        rest_pause.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let first_rest_park = runner
        .state()
        .active_batch_delivery()
        .expect("the second rest placement is parked behind the first redirect");
    assert_eq!(first_rest_park.remaining.len(), 1);
    assert!(
        matches!(
            first_rest_park.completion.as_ref(),
            Some(BatchCompletion::RevealRestPile {
                delivery_stage: engine::types::game_state::DigDeliveryStage::Rest,
                ..
            })
        ),
        "the first rest redirect must retain the RevealRestPile completion"
    );
    assert!(runner.state().chain_tracked_set_id.is_none());

    let reparking = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the rest batch re-parks on its remaining library placement");
    assert!(matches!(
        reparking.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        matches!(
            runner
                .state()
                .active_batch_delivery()
                .and_then(|pending| pending.completion.as_ref()),
            Some(BatchCompletion::RevealRestPile {
                delivery_stage: engine::types::game_state::DigDeliveryStage::Rest,
                ..
            })
        ),
        "the re-parked rest delivery must retain its RevealRestPile completion"
    );
    assert!(runner.state().chain_tracked_set_id.is_none());

    for redirect_source in library_redirect_sources {
        runner
            .state_mut()
            .objects
            .get_mut(&redirect_source)
            .expect("synthetic redirect source remains on the battlefield")
            .replacement_definitions
            .clear();
    }
    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the final rest placement drains the deferred completion");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
    assert!(
        matches!(
            runner.state().objects[&kept].zone,
            Zone::Graveyard | Zone::Exile
        ),
        "the kept battlefield entry must take one configured redirect"
    );
    let tracked = runner
        .state()
        .tracked_object_sets
        .get(
            &runner
                .state()
                .chain_tracked_set_id
                .expect("the Dig completion publishes after the true batch end"),
        )
        .expect("the tracked set exists");
    assert!(
        tracked.is_empty(),
        "the kept delivery was redirected away from the requested battlefield destination"
    );
}

fn redirect_self_moved_to(destination: Zone, redirected_to: Zone) -> ReplacementDefinition {
    redirect_moved_to(destination, redirected_to).valid_card(TargetFilter::SelfRef)
}

fn prompt_after_moved_to_exile() -> ReplacementDefinition {
    redirect_moved_to_with_post_effect(Zone::Exile, Zone::Exile)
}

fn scry_after_moved_to_exile() -> ReplacementDefinition {
    let mut replacement = redirect_moved_to(Zone::Exile, Zone::Exile);
    replacement
        .execute
        .as_mut()
        .expect("redirect helper always provides its replacement effect")
        .sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Scry {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )));
    replacement
}

fn proliferate_after_moved_to_exile() -> ReplacementDefinition {
    let mut replacement = redirect_moved_to(Zone::Exile, Zone::Exile);
    replacement
        .execute
        .as_mut()
        .expect("redirect helper always provides its replacement effect")
        .sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Proliferate,
    )));
    replacement
}

fn optional_gain_life_after_moved_to_exile() -> ReplacementDefinition {
    let mut replacement = redirect_moved_to(Zone::Exile, Zone::Exile);
    replacement
        .execute
        .as_mut()
        .expect("redirect helper always provides its replacement effect")
        .sub_ability = Some(Box::new(
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        )
        .optional(),
    ));
    replacement
}

fn redirect_moved_to_with_post_effect(
    destination: Zone,
    redirected_to: Zone,
) -> ReplacementDefinition {
    let mut replacement = redirect_moved_to(destination, redirected_to);
    replacement
        .execute
        .as_mut()
        .expect("redirect helper always provides its replacement effect")
        .sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Choose {
            choice_type: ChoiceType::Labeled {
                options: vec!["first".to_string(), "second".to_string()],
            },
            persist: false,
            selection: TargetSelectionMode::Chosen,
        },
    )));
    replacement
}

fn mana_self_exile_cost_redirect_witness() -> (GameScenario, engine::types::identifiers::ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Mana Self-Exile Redirect Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in [
        "First Mana Self-Exile Redirect",
        "Second Mana Self-Exile Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }

    (scenario, source)
}

/// Drives the real replacement-choice dispatcher through its `Prevented` arm
/// while retaining the paused typed cost-move root created by the normal
/// cost-move pipeline. Zone-change prevention is not yet an engine replacement
/// outcome, so this uses the existing one-shot prevention producer to exercise
/// the shared dispatcher seam.
fn stage_prevented_cost_move(state: &mut GameState, source: engine::types::identifiers::ObjectId) {
    state
        .objects
        .get_mut(&source)
        .expect("mana source exists while its cost move is paused")
        .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Destroy)
        .regeneration_shield()
        .description("Prevent the staged mana cost move".to_string())]
    .into();
    state.pending_replacement = Some(PendingReplacement {
        proposed: ProposedEvent::Destroy {
            object_id: source,
            source: None,
            cant_regenerate: false,
            applied: Default::default(),
        },
        sacrifice_provenance: None,
        candidates: vec![ReplacementId { source, index: 0 }],
        search_found_candidates: Vec::new(),
        depth: 0,
        is_optional: false,
        library_placement: None,
        exile_controller: None,
        exile_duration: None,
        exile_tracking: engine::types::game_state::ZoneDeliveryExileTracking::None,
        excess_recipient: None,
        lifelink_bonus: 0,
        may_cost_paid: false,
        may_cost_remaining: None,
    });
    state.waiting_for = WaitingFor::ReplacementChoice {
        player: P0,
        candidate_count: 1,
        candidates: vec![],
    };
}

#[test]
fn collect_evidence_cost_pauses_for_moved_redirect_before_resuming_its_effect() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Collect Evidence Redirect Source", 1, 1)
        .id();
    let evidence = scenario
        .add_creature_to_graveyard(P0, "Collect Evidence Redirect Fuel", 1, 1)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    for name in [
        "First Collect Evidence Redirect",
        "Second Collect Evidence Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Hand));
    }

    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::CollectEvidenceChoice {
        player: P0,
        minimum_mana_value: 3,
        cards: vec![evidence],
        resume: Box::new(CollectEvidenceResume::Effect {
            pending_ability: Box::new(ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
                vec![],
                source,
                P0,
            )),
        }),
    };

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![evidence],
        })
        .expect("collect-evidence payment should inspect Moved replacements");

    assert!(
        matches!(result.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the selected graveyard-to-exile cost move must pause for competing Moved replacements"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        20,
        "the linked effect must not resolve before the selected cost move settles"
    );
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::CollectEvidencePayment {
            player,
            chosen,
            paused_at_index: 0,
            ..
        }) if *player == P0 && chosen == &vec![evidence]
    ));

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the selected evidence move resumes its typed payment root");
    assert!(matches!(resumed.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(runner.state().objects[&evidence].zone, Zone::Hand);
    assert_eq!(runner.state().players[P0.0 as usize].life, 21);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(
        resumed
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id: P0,
                    action: engine::types::events::PlayerActionKind::CollectEvidence,
                    ..
                }
            ))
            .count(),
        1,
        "the selected evidence payment completes exactly once after the replacement choice"
    );
}

#[test]
fn collect_evidence_cost_completes_when_the_replacement_dispatcher_prevents_its_move() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Prevented Collect Evidence Source", 1, 1)
        .id();
    let evidence = scenario
        .add_creature_to_graveyard(P0, "Prevented Collect Evidence Fuel", 1, 1)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    for name in [
        "First Prevented Collect Evidence Redirect",
        "Second Prevented Collect Evidence Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Hand));
    }

    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::CollectEvidenceChoice {
        player: P0,
        minimum_mana_value: 3,
        cards: vec![evidence],
        resume: Box::new(CollectEvidenceResume::Effect {
            pending_ability: Box::new(ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
                vec![],
                source,
                P0,
            )),
        }),
    };

    runner
        .act(GameAction::SelectCards {
            cards: vec![evidence],
        })
        .expect("collect-evidence payment reaches its replacement pause");
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::CollectEvidencePayment { .. })
    ));

    // A Moved event has no natural prevention producer in the current engine.
    // Re-stage the existing one-shot prevention witness while the typed cost
    // root is parked, exercising the shared `ReplacementPrevented` drain.
    stage_prevented_cost_move(runner.state_mut(), source);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("a fully substituted cost event still resumes collect evidence");

    assert!(matches!(resumed.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(runner.state().objects[&evidence].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P0.0 as usize].life, 21);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(
        resumed
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id: P0,
                    action: engine::types::events::PlayerActionKind::CollectEvidence,
                    ..
                }
            ))
            .count(),
        1,
        "the prevented cost event still completes the evidence payment once"
    );
}

#[test]
fn unless_bounce_cost_pauses_for_moved_redirect_before_avoiding_the_effect() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bounced = scenario
        .add_creature(P0, "Unless Bounce Redirect Witness", 1, 1)
        .id();
    for name in [
        "First Unless Bounce Redirect",
        "Second Unless Bounce Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Hand, Zone::Graveyard));
    }

    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::UnlessBounceChoice {
        player: P0,
        permanents: vec![bounced],
        pending_effect: Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            bounced,
            P0,
        )),
        remaining: 1,
    };

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![bounced],
        })
        .expect("unless bounce payment should inspect Moved replacements");

    assert!(
        matches!(result.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the selected battlefield-to-hand cost move must pause for competing Moved replacements"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        20,
        "the paid unless cost must keep the pending effect avoided while replacement choice is open"
    );
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::UnlessBouncePayment {
            player,
            moved,
            remaining: 1,
            ..
        }) if *player == P0 && *moved == bounced
    ));

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the selected return resumes its typed unless-payment root");
    assert!(matches!(resumed.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(runner.state().objects[&bounced].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        20,
        "the redirected unless cost remains paid, so its avoided effect must not fire"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
}

#[test]
fn unless_bounce_cost_remains_paid_when_the_replacement_dispatcher_prevents_its_move() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bounced = scenario
        .add_creature(P0, "Prevented Unless Bounce Witness", 1, 1)
        .id();
    for name in [
        "First Prevented Unless Bounce Redirect",
        "Second Prevented Unless Bounce Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Hand, Zone::Graveyard));
    }

    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::UnlessBounceChoice {
        player: P0,
        permanents: vec![bounced],
        pending_effect: Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            bounced,
            P0,
        )),
        remaining: 1,
    };

    runner
        .act(GameAction::SelectCards {
            cards: vec![bounced],
        })
        .expect("unless-bounce payment reaches its replacement pause");
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::UnlessBouncePayment { .. })
    ));

    stage_prevented_cost_move(runner.state_mut(), bounced);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("a fully substituted return-to-hand cost still avoids the unless effect");

    assert!(matches!(resumed.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(runner.state().objects[&bounced].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        20,
        "the prevented return was still a paid unless cost, so the effect remains avoided"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
}

#[test]
fn collect_evidence_and_unless_bounce_costs_complete_synchronously_without_replacements() {
    let mut evidence_scenario = GameScenario::new();
    evidence_scenario.at_phase(Phase::PreCombatMain);
    let evidence_source = evidence_scenario
        .add_creature(P0, "Uninterrupted Collect Evidence Source", 1, 1)
        .id();
    let evidence = evidence_scenario
        .add_creature_to_graveyard(P0, "Uninterrupted Collect Evidence Fuel", 1, 1)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let mut evidence_runner = evidence_scenario.build();
    evidence_runner.state_mut().waiting_for = WaitingFor::CollectEvidenceChoice {
        player: P0,
        minimum_mana_value: 3,
        cards: vec![evidence],
        resume: Box::new(CollectEvidenceResume::Effect {
            pending_ability: Box::new(ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
                vec![],
                evidence_source,
                P0,
            )),
        }),
    };
    let evidence_result = evidence_runner
        .act(GameAction::SelectCards {
            cards: vec![evidence],
        })
        .expect("uninterrupted evidence cost resolves synchronously");
    assert!(matches!(
        evidence_result.waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(evidence_runner.state().objects[&evidence].zone, Zone::Exile);
    assert_eq!(evidence_runner.state().players[P0.0 as usize].life, 21);
    assert!(evidence_runner.state().pending_cost_move_resume.is_none());

    let mut bounce_scenario = GameScenario::new();
    bounce_scenario.at_phase(Phase::PreCombatMain);
    let bounced = bounce_scenario
        .add_creature(P0, "Uninterrupted Unless Bounce Witness", 1, 1)
        .id();
    let mut bounce_runner = bounce_scenario.build();
    bounce_runner.state_mut().waiting_for = WaitingFor::UnlessBounceChoice {
        player: P0,
        permanents: vec![bounced],
        pending_effect: Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            bounced,
            P0,
        )),
        remaining: 1,
    };
    let bounce_result = bounce_runner
        .act(GameAction::SelectCards {
            cards: vec![bounced],
        })
        .expect("uninterrupted unless-bounce cost resolves synchronously");
    assert!(matches!(
        bounce_result.waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(bounce_runner.state().objects[&bounced].zone, Zone::Hand);
    assert_eq!(bounce_runner.state().players[P0.0 as usize].life, 20);
    assert!(bounce_runner.state().pending_cost_move_resume.is_none());
}

/// CR 702.21a + CR 701.21 + CR 616.1: A ward payment selecting multiple
/// permanents must leave its unsacrificed suffix parked while each selected
/// sacrifice waits on a competing graveyard replacement choice.
#[test]
fn ward_multi_sacrifice_payment_reparks_each_replacement_before_effect_resolved() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Ward Multi-Sacrifice Effect Source", 1, 1)
        .id();
    let first = scenario
        .add_creature(P0, "First Ward Multi-Sacrifice Redirect", 1, 1)
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    let second = scenario
        .add_creature(P0, "Second Ward Multi-Sacrifice Redirect", 1, 1)
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::WardSacrificeChoice {
        player: P0,
        permanents: vec![first, second],
        pending_effect: Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            P0,
        )),
        remaining: 1,
        min_total_power: Some(2),
    };

    let initial = runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .expect("the first selected ward sacrifice reaches its replacement choice");
    assert!(matches!(
        initial.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert!(
        !initial.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { source_id, .. } if *source_id == source
        )),
        "the ward tail must not resolve before the first replacement choice"
    );

    let after_first = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the first replacement resumes only the second selected ward sacrifice");
    assert!(matches!(
        after_first.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_ne!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert!(
        !after_first.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { source_id, .. } if *source_id == source
        )),
        "the tail remains parked when the resumed suffix pauses again"
    );

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the second replacement completes the parked ward suffix");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
    assert_ne!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert!(runner.state().pending_cost_move_resume.is_none());

    let events = initial
        .events
        .iter()
        .chain(after_first.events.iter())
        .chain(completed.events.iter());
    for object_id in [first, second] {
        assert_eq!(
            events
                .clone()
                .filter(|event| matches!(
                    event,
                    GameEvent::PermanentSacrificed { object_id: sacrificed, .. }
                        if *sacrificed == object_id
                ))
                .count(),
            1,
            "each selected permanent is sacrificed exactly once"
        );
    }
    assert_eq!(
        events
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved { source_id, .. } if *source_id == source
            ))
            .count(),
        1,
        "the ward payment tail resolves exactly once after every selected sacrifice settles"
    );
}

/// CR 702.21a + CR 701.21 + CR 616.1: A sequential ward payment must not
/// surface its next sacrifice prompt until the current replacement choice has
/// settled.
#[test]
fn ward_sequential_sacrifice_payment_reprompts_only_after_replacement_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Ward Sequential Effect Source", 1, 1)
        .id();
    let first = scenario
        .add_creature(P0, "First Ward Sequential Redirect", 1, 1)
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    let second = scenario
        .add_creature(P0, "Second Ward Sequential Sacrifice", 1, 1)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::WardSacrificeChoice {
        player: P0,
        permanents: vec![first, second],
        pending_effect: Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            P0,
        )),
        remaining: 2,
        min_total_power: None,
    };

    let initial = runner
        .act(GameAction::SelectCards { cards: vec![first] })
        .expect("the first sequential ward sacrifice reaches its replacement choice");
    assert!(matches!(
        initial.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        !initial.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { source_id, .. } if *source_id == source
        )),
        "neither the next ward prompt nor the tail may overwrite the replacement pause"
    );

    let reprompt = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the completed first sacrifice reconstructs the next ward choice");
    let WaitingFor::WardSacrificeChoice {
        player,
        permanents,
        remaining,
        ..
    } = &reprompt.waiting_for
    else {
        panic!(
            "the sequential ward suffix must prompt only after replacement resolution, got {:?}",
            reprompt.waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*remaining, 1);
    assert_eq!(permanents, &vec![second]);
    assert!(
        !reprompt.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { source_id, .. } if *source_id == source
        )),
        "the tail waits for the final sequential sacrifice"
    );

    let completed = runner
        .act(GameAction::SelectCards {
            cards: vec![second],
        })
        .expect("the final ward sacrifice resolves synchronously");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
    assert!(runner.state().pending_cost_move_resume.is_none());
    let events = initial
        .events
        .iter()
        .chain(reprompt.events.iter())
        .chain(completed.events.iter());
    for object_id in [first, second] {
        assert_eq!(
            events
                .clone()
                .filter(|event| matches!(
                    event,
                    GameEvent::PermanentSacrificed { object_id: sacrificed, .. }
                        if *sacrificed == object_id
                ))
                .count(),
            1,
            "each sequential ward sacrifice occurs exactly once"
        );
    }
    assert_eq!(
        events
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved { source_id, .. } if *source_id == source
            ))
            .count(),
        1,
        "the final sequential sacrifice reaches the ward tail exactly once"
    );
}

/// CR 702.21a + CR 701.21: Ward sacrifice payments without a replacement
/// choice retain the existing synchronous aggregate and sequential behavior.
#[test]
fn ward_sacrifice_payment_completes_synchronously_without_replacements() {
    let mut aggregate_scenario = GameScenario::new();
    aggregate_scenario.at_phase(Phase::PreCombatMain);
    let aggregate_source = aggregate_scenario
        .add_creature(P0, "Synchronous Aggregate Ward Source", 1, 1)
        .id();
    let aggregate_first = aggregate_scenario
        .add_creature(P0, "Synchronous Aggregate Ward First", 1, 1)
        .id();
    let aggregate_second = aggregate_scenario
        .add_creature(P0, "Synchronous Aggregate Ward Second", 1, 1)
        .id();
    let mut aggregate_runner = aggregate_scenario.build();
    aggregate_runner.state_mut().waiting_for = WaitingFor::WardSacrificeChoice {
        player: P0,
        permanents: vec![aggregate_first, aggregate_second],
        pending_effect: Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            aggregate_source,
            P0,
        )),
        remaining: 1,
        min_total_power: Some(2),
    };
    let aggregate = aggregate_runner
        .act(GameAction::SelectCards {
            cards: vec![aggregate_first, aggregate_second],
        })
        .expect("aggregate ward payment completes synchronously");
    assert!(matches!(aggregate.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(
        aggregate
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved { source_id, .. } if *source_id == aggregate_source
            ))
            .count(),
        1
    );

    let mut sequential_scenario = GameScenario::new();
    sequential_scenario.at_phase(Phase::PreCombatMain);
    let sequential_source = sequential_scenario
        .add_creature(P0, "Synchronous Sequential Ward Source", 1, 1)
        .id();
    let sequential_first = sequential_scenario
        .add_creature(P0, "Synchronous Sequential Ward First", 1, 1)
        .id();
    let sequential_second = sequential_scenario
        .add_creature(P0, "Synchronous Sequential Ward Second", 1, 1)
        .id();
    let mut sequential_runner = sequential_scenario.build();
    sequential_runner.state_mut().waiting_for = WaitingFor::WardSacrificeChoice {
        player: P0,
        permanents: vec![sequential_first, sequential_second],
        pending_effect: Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            sequential_source,
            P0,
        )),
        remaining: 2,
        min_total_power: None,
    };
    let first = sequential_runner
        .act(GameAction::SelectCards {
            cards: vec![sequential_first],
        })
        .expect("first sequential ward payment completes synchronously");
    assert!(matches!(
        first.waiting_for,
        WaitingFor::WardSacrificeChoice {
            remaining: 1,
            ref permanents,
            ..
        } if permanents == &vec![sequential_second]
    ));
    assert!(
        !first.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { source_id, .. } if *source_id == sequential_source
        )),
        "the sequential branch keeps the final ward tail behind its second prompt"
    );
    let final_payment = sequential_runner
        .act(GameAction::SelectCards {
            cards: vec![sequential_second],
        })
        .expect("second sequential ward payment completes the tail");
    assert!(matches!(
        final_payment.waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        first
            .events
            .iter()
            .chain(final_payment.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved { source_id, .. } if *source_id == sequential_source
            ))
            .count(),
        1
    );
}

#[test]
fn village_rites_sacrifice_cost_pauses_for_competing_graveyard_replacements() {
    const VILLAGE_RITES: &str =
        "As an additional cost to cast this spell, sacrifice a creature.\nDraw two cards.";
    const DARKSTEEL_COLOSSUS: &str = "Trample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)\nIndestructible (Effects that say \"destroy\" don't destroy this creature. A creature with indestructible can't be destroyed by damage.)\nIf Darksteel Colossus would be put into a graveyard from anywhere, reveal Darksteel Colossus and shuffle it into its owner's library instead.";
    const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let village_rites = scenario
        .add_spell_to_hand_from_oracle(P0, "Village Rites", true, VILLAGE_RITES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let darksteel = scenario
        .add_creature_from_oracle(P0, "Darksteel Colossus", 11, 11, DARKSTEEL_COLOSSUS)
        .id();
    let rest_in_peace = scenario
        .add_creature(P0, "Rest in Peace", 0, 0)
        .as_enchantment()
        .from_oracle_text(REST_IN_PEACE)
        .id();
    scenario.add_basic_land(P0, ManaColor::Blue);

    let mut runner = scenario.build();
    let initial_state = runner.state().clone();
    let card_id = runner.state().objects[&village_rites].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: village_rites,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Village Rites should announce before its additional cost is selected");
    assert!(
        runner.state().objects[&village_rites].zone == Zone::Hand,
        "the spell object must remain in hand until its cost is fully paid"
    );

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![darksteel],
        })
        .expect("the chosen sacrifice cost should reach its replacement pipeline");

    assert!(
        matches!(result.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the interrupted sacrifice cost must surface its CR 616.1 replacement choice"
    );
    assert!(
        matches!(
            runner.state().pending_cost_move_resume.as_ref(),
            Some(PendingCostMoveResume::SacrificeForCost {
                player,
                chosen,
                paused_at_index: 0,
                ..
            }) if *player == P0 && chosen == &vec![darksteel]
        ),
        "the interrupted sacrifice cost must retain a typed cost-move continuation"
    );
    assert!(
        runner.state().objects[&village_rites].zone == Zone::Hand
            && !result.events.iter().any(
                |event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == village_rites)
            ),
        "the spell must not complete its cast while the sacrifice cost is unpaid"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the engine must not grant priority while the cost is unpaid"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { ref candidates, .. }
                if candidates.iter().any(|candidate| candidate.source_id == rest_in_peace)
        ),
        "Rest in Peace must be one of the material replacement choices"
    );

    let rest_in_peace_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.source_id == rest_in_peace)
            .expect("Rest in Peace replacement is selectable"),
        waiting_for => panic!("expected replacement choice, got {waiting_for:?}"),
    };
    let completed = runner
        .act(GameAction::ChooseReplacement {
            index: rest_in_peace_index,
        })
        .expect("Rest in Peace should replace the sacrifice destination");

    assert_eq!(runner.state().objects[&darksteel].zone, Zone::Exile);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
    assert!(
        completed
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == village_rites)),
        "Village Rites must finish casting once its paid sacrifice reaches exile"
    );

    let mut darksteel_first_runner = GameRunner::from_state(initial_state);
    let card_id = darksteel_first_runner.state().objects[&village_rites].card_id;
    darksteel_first_runner
        .act(GameAction::CastSpell {
            object_id: village_rites,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce the symmetric Village Rites cast");
    darksteel_first_runner
        .act(GameAction::SelectCards {
            cards: vec![darksteel],
        })
        .expect("select Darksteel Colossus for the symmetric sacrifice cost");
    let darksteel_index = match &darksteel_first_runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.source_id == darksteel)
            .expect("Darksteel Colossus replacement is selectable"),
        waiting_for => panic!("expected replacement choice, got {waiting_for:?}"),
    };
    let darksteel_completed = darksteel_first_runner
        .act(GameAction::ChooseReplacement {
            index: darksteel_index,
        })
        .expect("Darksteel Colossus should replace its own sacrifice");
    assert_eq!(
        darksteel_first_runner.state().objects[&darksteel].zone,
        Zone::Library,
        "choosing Darksteel Colossus first must use its library redirect"
    );
    assert!(
        darksteel_completed.events.iter().any(
            |event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == village_rites)
        ),
        "the symmetric redirect must still complete Village Rites exactly once"
    );
}

fn count_two_sacrifice_activation_witness(
    with_departure_observer: bool,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Count-Two Sacrifice Activation Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                2,
            ))),
        )
        .id();
    let first = scenario
        .add_creature(P0, "First Count-Two Sacrifice Witness", 1, 1)
        .id();
    let second = scenario
        .add_creature(P0, "Second Count-Two Sacrifice Witness", 1, 1)
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    let mut runner = scenario.build();
    if with_departure_observer {
        runner
            .state_mut()
            .objects
            .get_mut(&first)
            .expect("the first selected creature exists before the sacrifice")
            .trigger_definitions
            .push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .valid_card(TargetFilter::SelfRef)
                    .origin(Zone::Battlefield)
                    .destination(Zone::Graveyard)
                    .trigger_zones(vec![Zone::Battlefield])
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: TargetFilter::Controller,
                        },
                    )),
            );
    }
    (runner, source, first, second)
}

#[test]
fn count_two_sacrifice_cost_resumes_at_second_object_and_activates_once() {
    let (mut runner, source, first, second) = count_two_sacrifice_activation_witness(false);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("begin the count-two sacrifice activation");

    let initial = runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .expect("the second sacrifice should reach its replacement pipeline");
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::SacrificeForCost {
            chosen,
            paused_at_index: 1,
            ..
        }) if chosen == &vec![first, second]
    ));

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("deliver the second selected sacrifice");
    assert_ne!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(matches!(resumed.waiting_for, WaitingFor::Priority { .. }));

    let events = initial.events.iter().chain(resumed.events.iter());
    assert_eq!(
        events
            .clone()
            .filter(|event| matches!(event, GameEvent::PermanentSacrificed { object_id, .. } if *object_id == first))
            .count(),
        1,
        "the resume must not replay the first selected sacrifice"
    );
    assert_eq!(
        events
            .clone()
            .filter(|event| matches!(event, GameEvent::PermanentSacrificed { object_id, .. } if *object_id == second))
            .count(),
        1,
        "the paused second sacrifice must complete exactly once"
    );
    assert_eq!(
        events
            .filter(|event| matches!(event, GameEvent::AbilityActivated { source_id, .. } if *source_id == source))
            .count(),
        1,
        "the selected activation cost is removed once and the activation proceeds once"
    );
}

#[test]
fn paused_sacrifice_cost_stamps_cross_action_departures_and_collects_dies_once() {
    let (mut runner, source, first, second) = count_two_sacrifice_activation_witness(true);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("begin the observed count-two sacrifice activation");
    let initial = runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .expect("the second sacrifice pauses after the first dies");
    assert!(matches!(
        initial.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the replacement completion must settle the complete sacrifice group");
    assert!(resumed.events.iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, record, .. }
            if *object_id == second && record.co_departed == vec![first]
    )));
    for (object_id, other) in [(first, second), (second, first)] {
        assert!(
            runner.state().zone_changes_this_turn.iter().any(|record| {
                record.object_id == object_id
                    && record.from_zone == Some(Zone::Battlefield)
                    && record.co_departed == vec![other]
            }),
            "the authoritative LKI ledger must retain the complete co-departure group"
        );
    }
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(
                entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if source_id == first
            ))
            .count(),
        1,
        "the deferred first departure trigger is collected once after the full group is stamped"
    );
}

#[test]
fn target_activation_replacement_paused_sacrifice_stages_cost_triggers_until_commit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Targeted Sacrifice Replacement Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Any,
                    damage_source: None,
                    excess: None,
                },
            )
            .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                2,
            ))),
        )
        .id();
    let first = scenario
        .add_creature(P0, "First Targeted Sacrifice Witness", 1, 1)
        .id();
    let second = scenario
        .add_creature(P0, "Second Targeted Sacrifice Witness", 1, 1)
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    let target = scenario.add_creature(P1, "Target", 2, 2).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&first)
        .unwrap()
        .trigger_definitions
        .push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .origin(Zone::Battlefield)
                .destination(Zone::Graveyard)
                .trigger_zones(vec![Zone::Battlefield])
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )),
        );

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("targeted activation starts with target selection");
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        })
        .expect("target is declared before the sacrifice cost");
    runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .expect("second sacrifice reaches the replacement pipeline");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "the first sacrifice event stays inside the pending activation session"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("replacement completion commits the activation");
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(
                entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if source_id == first
            ))
            .count(),
        1,
        "the replacement-paused sacrifice trigger is collected once at activation commit"
    );
}

#[test]
fn self_sacrifice_mana_cost_waits_for_replacement_before_producing_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Self-Sacrifice Mana Replacement Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::SelfRef,
                1,
            ))),
        )
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    let mut runner = scenario.build();

    let initial = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the self-sacrifice mana ability reaches its replacement choice");
    assert!(matches!(
        initial.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        0,
        "mana must not be produced before the sacrifice cost finishes"
    );

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the replacement completion resumes the mana cursor");
    assert_ne!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1
    );
    assert_eq!(
        initial
            .events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "the resumed self-sacrifice cost produces mana exactly once"
    );
}

#[test]
fn selected_sacrifice_mana_cost_resumes_without_repaying_its_prefix() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Selected-Sacrifice Mana Replacement Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                2,
            ))),
        )
        .id();
    let first = scenario
        .add_creature(P0, "First Selected-Sacrifice Mana Witness", 1, 1)
        .id();
    let second = scenario
        .add_creature(P0, "Second Selected-Sacrifice Mana Witness", 1, 1)
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("begin the selected-sacrifice mana ability");
    let initial = runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .expect("the second selected mana sacrifice reaches its replacement choice");
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { cursor, .. })
            if cursor.next_sacrificed == 2
                && cursor.selected_sacrifice_remaining.as_deref() == Some(&[])
    ));
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        0,
        "the mana ability cannot produce its output before every selected sacrifice settles"
    );

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("resuming the selected sacrifice cursor produces mana");
    assert_ne!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1
    );
    let events = initial.events.iter().chain(resumed.events.iter());
    assert_eq!(
        events
            .clone()
            .filter(|event| matches!(event, GameEvent::PermanentSacrificed { object_id, .. } if *object_id == first))
            .count(),
        1,
        "the cursor must not re-pay the first selected sacrifice"
    );
    assert_eq!(
        events
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "the selected sacrifice cursor settles its cost events and produces mana once"
    );
}

#[test]
fn mandatory_single_sacrifice_redirect_completes_without_a_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Mandatory Single Sacrifice Redirect Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::SelfRef,
                1,
            ))),
        )
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .id();
    let mut runner = scenario.build();

    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the unambiguous sacrifice redirect must resolve synchronously");
    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1
    );
}

#[test]
fn foretell_cost_honors_moved_redirect_and_completes_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let foretell_cost = ManaCost::generic(5);
    let foretold = scenario
        .add_spell_to_hand(P0, "Foretell Cost Redirect Witness", false)
        .with_mana_cost(ManaCost::generic(7))
        .with_keyword(Keyword::Foretell(foretell_cost.clone()))
        .id();
    for name in ["First Foretell Redirect", "Second Foretell Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&foretold].card_id;
    let result = runner
        .act(GameAction::Foretell {
            object_id: foretold,
            card_id,
        })
        .expect("foretell special action should pay its cost and consult Moved replacements");

    assert!(
        matches!(result.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the foretell cost move must consult competing Moved redirects"
    );

    let turn_foretold = runner.state().turn_number;
    let json = serde_json::to_string(runner.state()).expect("paused foretell serializes");
    let restored: GameState = serde_json::from_str(&json).expect("paused foretell deserializes");
    assert!(matches!(
        restored.pending_cost_move_resume.as_ref(),
        Some(&PendingCostMoveResume::Foretell {
            player,
            object_id,
            ref cost,
            turn_foretold: stamped_turn,
        }) if player == P0 && object_id == foretold && cost == &foretell_cost && stamped_turn == turn_foretold
    ));
    let mut runner = GameRunner::from_state(restored);

    let result = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the foretell exile");
    let obj = &runner.state().objects[&foretold];
    assert_eq!(obj.zone, Zone::Graveyard);
    assert!(!obj.foretold, "only a card delivered to exile was foretold");
    assert!(
        !obj.face_down,
        "a redirected card must not gain foretell concealment"
    );
    assert!(obj.casting_permissions.is_empty());
    assert!(
        !result.events.iter().any(
            |event| matches!(event, GameEvent::Foretold { object_id, .. } if *object_id == foretold)
        ),
        "a redirected card must not emit Foretold"
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::ReplacementApplied { .. }))
            .count(),
        1,
        "the selected redirect must apply exactly once"
    );
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
}

#[test]
fn foretell_delivery_finalizes_before_a_post_replacement_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let foretell_cost = ManaCost::generic(5);
    let foretold = scenario
        .add_spell_to_hand(P0, "Foretell Post-Effect Witness", false)
        .with_mana_cost(ManaCost::generic(7))
        .with_keyword(Keyword::Foretell(foretell_cost.clone()))
        .id();
    scenario
        .add_creature(P0, "Foretell Post-Effect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(prompt_after_moved_to_exile());
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&foretold].card_id;
    let turn_foretold = runner.state().turn_number;
    let result = runner
        .act(GameAction::Foretell {
            object_id: foretold,
            card_id,
        })
        .expect("foretell should deliver before the replacement post-effect prompts");

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::NamedChoice { ref options, .. }
                if options == &vec!["first".to_string(), "second".to_string()]
        ),
        "the delivered Foretell move must preserve the replacement prompt"
    );
    let object = &runner.state().objects[&foretold];
    assert_eq!(object.zone, Zone::Exile);
    assert!(object.foretold);
    assert!(object.face_down);
    assert!(matches!(
        object.casting_permissions.as_slice(),
        [CastingPermission::Foretold { cost, turn_foretold: stamped_turn }]
            if cost == &foretell_cost && *stamped_turn == turn_foretold
    ));
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::Foretold { object_id, .. } if *object_id == foretold))
            .count(),
        1,
        "delivery must emit exactly one Foretold event before the prompt pauses"
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::ReplacementApplied { .. }))
            .count(),
        1,
        "the identity redirect must apply before its post-effect prompts"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);

    let paused_waiting_for = runner.state().waiting_for.clone();
    let json =
        serde_json::to_string(runner.state()).expect("post-delivery foretell pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("post-delivery foretell pause deserializes");
    assert_eq!(restored.waiting_for, paused_waiting_for);
    assert!(restored.pending_cost_move_resume.is_none());
    let mut runner = GameRunner::from_state(restored);

    let resumed = runner
        .act(GameAction::ChooseOption {
            choice: "first".to_string(),
        })
        .expect("post-replacement choice should remain actionable after serialization");
    let object = &runner.state().objects[&foretold];
    assert_eq!(object.zone, Zone::Exile);
    assert!(object.foretold);
    assert!(object.face_down);
    assert_eq!(object.casting_permissions.len(), 1);
    assert!(
        !resumed.events.iter().any(
            |event| matches!(event, GameEvent::Foretold { object_id, .. } if *object_id == foretold)
        ),
        "resolving the post-effect must not re-finalize Foretell"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
}

#[test]
fn foretell_replacement_pause_then_post_effect_prompt_finalizes_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let foretell_cost = ManaCost::generic(5);
    let foretold = scenario
        .add_spell_to_hand(P0, "Foretell Replacement Resume Witness", false)
        .with_mana_cost(ManaCost::generic(7))
        .with_keyword(Keyword::Foretell(foretell_cost.clone()))
        .id();
    let exile_to_graveyard = scenario
        .add_creature(P0, "Foretell Exile to Graveyard", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard))
        .id();
    scenario
        .add_creature(P0, "Foretell Exile to Exile", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Exile));
    let graveyard_to_exile = scenario
        .add_creature(P0, "Foretell Graveyard to Exile", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to_with_post_effect(
            Zone::Graveyard,
            Zone::Exile,
        ))
        .id();
    scenario
        .add_creature(P0, "Foretell Graveyard to Hand", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Graveyard, Zone::Hand));
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&foretold].card_id;
    let turn_foretold = runner.state().turn_number;
    let initial = runner
        .act(GameAction::Foretell {
            object_id: foretold,
            card_id,
        })
        .expect("competing Moved replacements should pause the Foretell cost move");
    assert!(matches!(
        initial.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let json =
        serde_json::to_string(runner.state()).expect("pre-delivery foretell pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("pre-delivery foretell pause deserializes");
    assert!(matches!(
        restored.pending_cost_move_resume.as_ref(),
        Some(&PendingCostMoveResume::Foretell {
            player,
            object_id,
            ref cost,
            turn_foretold: stamped_turn,
        }) if player == P0 && object_id == foretold && cost == &foretell_cost && stamped_turn == turn_foretold
    ));
    let mut runner = GameRunner::from_state(restored);

    let mut replacement_prompts = 0;
    let mut delivered = None;
    while let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    {
        let expected_source = match replacement_prompts {
            0 => exile_to_graveyard,
            1 => graveyard_to_exile,
            _ => panic!("unexpected additional Foretell replacement prompt"),
        };
        let index = candidates
            .iter()
            .position(|candidate| candidate.source_id == expected_source)
            .expect("the chosen redirect must appear in its CR 616.1 ordering prompt");
        delivered = Some(
            runner
                .act(GameAction::ChooseReplacement { index })
                .expect("apply the selected Foretell redirect"),
        );
        replacement_prompts += 1;
    }
    assert_eq!(
        replacement_prompts, 2,
        "both material Moved replacement collisions must be ordered before delivery"
    );
    let delivered = delivered.expect("the selected graveyard-to-exile redirect must deliver");
    assert!(matches!(
        delivered.waiting_for,
        WaitingFor::NamedChoice { .. }
    ));
    let object = &runner.state().objects[&foretold];
    assert_eq!(object.zone, Zone::Exile);
    assert!(object.foretold);
    assert!(object.face_down);
    assert!(matches!(
        object.casting_permissions.as_slice(),
        [CastingPermission::Foretold { cost, turn_foretold: stamped_turn }]
            if cost == &foretell_cost && *stamped_turn == turn_foretold
    ));
    assert_eq!(
        delivered
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::Foretold { object_id, .. } if *object_id == foretold))
            .count(),
        1
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);

    let resumed = runner
        .act(GameAction::ChooseOption {
            choice: "first".to_string(),
        })
        .expect("the post-effect prompt remains actionable after Foretell completes");
    assert!(!resumed.events.iter().any(
        |event| matches!(event, GameEvent::Foretold { object_id, .. } if *object_id == foretold)
    ));
    assert_eq!(
        runner.state().objects[&foretold].casting_permissions.len(),
        1
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
}

#[test]
fn pitch_exile_cost_honors_moved_redirect_and_completes_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let shoal = scenario
        .add_creature_to_hand(P0, "Nourishing Shoal", 0, 0)
        .as_instant()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green, ManaCostShard::Green],
            generic: 0,
        })
        .with_ability(Effect::GainLife {
            amount: engine::types::ability::QuantityExpr::Ref {
                qty: engine::types::ability::QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            player: TargetFilter::Controller,
        })
        .id();
    let pitched = scenario.add_creature_to_hand(P0, "Green Filler", 2, 2).id();
    scenario
        .add_creature(P0, "Exile Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let shoal_obj = state.objects.get_mut(&shoal).expect("shoal exists");
        shoal_obj
            .casting_options
            .push(SpellCastingOption::alternative_cost(parse_oracle_cost(
                "exile a green card with mana value X from your hand",
            )));
        shoal_obj.color.push(ManaColor::Green);

        let pitched_obj = state
            .objects
            .get_mut(&pitched)
            .expect("pitched card exists");
        pitched_obj.card_types.core_types.push(CoreType::Creature);
        pitched_obj.color.push(ManaColor::Green);
        pitched_obj.mana_cost = ManaCost::generic(3);
    }
    let card_id = runner.state().objects[&shoal].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: shoal,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Nourishing Shoal");
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accept pitch cost");

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![pitched],
        })
        .expect("pay pitch exile cost");

    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Hand),
                to: Zone::Graveyard,
                ..
            } if *object_id == pitched
        )),
        "the redirect must modify the pitch cost's exile event"
    );
    assert_eq!(runner.state().objects[&pitched].zone, Zone::Graveyard);
    assert!(
        !runner.state().stack.is_empty(),
        "the cast must complete after the redirected pitch cost"
    );
}

#[test]
fn multi_card_exile_cost_resumes_after_each_replacement_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_creature_to_hand(P0, "Two-card Pitch Witness", 0, 0)
        .as_instant()
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let first = scenario
        .add_creature_to_hand(P0, "First Green Filler", 2, 2)
        .id();
    let second = scenario
        .add_creature_to_hand(P0, "Second Green Filler", 2, 2)
        .id();
    scenario
        .add_creature(P0, "First Exile Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    scenario
        .add_creature(P0, "Second Exile Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));

    let mut runner = scenario.build();
    {
        let spell_obj = runner
            .state_mut()
            .objects
            .get_mut(&spell)
            .expect("spell exists");
        spell_obj
            .casting_options
            .push(SpellCastingOption::alternative_cost(parse_oracle_cost(
                "exile two green cards from your hand",
            )));
        for object_id in [first, second] {
            let filler = runner
                .state_mut()
                .objects
                .get_mut(&object_id)
                .expect("green filler exists");
            filler.card_types.core_types.push(CoreType::Creature);
            filler.color.push(ManaColor::Green);
        }
    }
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast two-card pitch witness");
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accept two-card pitch cost");
    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .expect("select both green cards");
    assert!(matches!(
        result.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let mut prompts_answered = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("answer the cost-move replacement choice");
        prompts_answered += 1;
        assert!(prompts_answered <= 2, "each selected card pauses once");
    }

    assert_eq!(
        prompts_answered, 2,
        "resume must continue with the next card"
    );
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    assert!(
        !runner.state().stack.is_empty(),
        "the cast must complete after both replacement choices"
    );
}

#[test]
fn return_to_hand_cost_honors_moved_redirect_and_completes_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_creature_to_hand(P0, "Daze Cost Witness", 0, 0)
        .as_instant()
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let returned_land = scenario.add_basic_land(P0, ManaColor::Blue);
    scenario
        .add_creature(P0, "Hand Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Hand, Zone::Exile));

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .expect("spell exists")
        .casting_options
        .push(SpellCastingOption::alternative_cost(parse_oracle_cost(
            "Return a land you control to its owner's hand",
        )));
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Daze cost witness");
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accept return-to-hand cost");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. }
    ));

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![returned_land],
        })
        .expect("pay return-to-hand cost");

    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to: Zone::Exile,
                ..
            } if *object_id == returned_land
        )),
        "the redirect must modify the return-to-hand cost event"
    );
    assert_eq!(runner.state().objects[&returned_land].zone, Zone::Exile);
    assert!(
        !runner.state().stack.is_empty(),
        "the cast must complete after the redirected return-to-hand cost"
    );
}

#[test]
fn self_exile_activation_cost_pauses_for_moved_redirect_without_pending_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature(P0, "Self-Exile Cost Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Exile {
                count: 1,
                zone: None,
                filter: Some(TargetFilter::SelfRef),
            }),
        )
        .id();
    for name in ["First Self-Exile Redirect", "Second Self-Exile Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }

    let mut runner = scenario.build();
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("announce self-exile activation");

    assert!(matches!(
        result.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        runner.state().pending_cast.is_none(),
        "a self-exile activation cost must not use PendingCast to resume"
    );

    let json = serde_json::to_string(runner.state()).expect("paused cost move serializes");
    assert!(
        json.contains("pending_cost_move_resume"),
        "a replacement choice must retain its cost-move continuation on the wire"
    );
    let restored: GameState = serde_json::from_str(&json).expect("paused cost move deserializes");
    assert!(matches!(
        restored.pending_cost_move_resume,
        Some(PendingCostMoveResume::Cast {
            pending: Some(_),
            ..
        })
    ));
    let mut runner = GameRunner::from_state(restored);

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("apply self-exile redirect");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert!(
        !runner.state().stack.is_empty(),
        "the activation must finish after the redirected self-exile cost"
    );
}

#[test]
fn mimeoplasm_forced_exile_cost_resumes_after_redirects_and_tracks_delivered_exiles_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let first = scenario
        .add_creature_to_graveyard(P0, "First Mimeoplasm Witness", 2, 2)
        .id();
    let second = scenario
        .add_creature_to_graveyard(P0, "Second Mimeoplasm Witness", 3, 3)
        .id();
    let mimeoplasm = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Mimeoplasm Forced-Cost Witness",
            5,
            5,
            "As ~ enters, you may exile two creature cards from graveyards. If you do, ~ enters as a copy of one of them, except it has +1/+1 counters equal to the other's power.",
        )
        .id();
    for name in ["First Mimeoplasm Redirect", "Second Mimeoplasm Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Hand));
    }
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Black);

    let mut runner = scenario.build();
    assert!(runner.state().players[P0.0 as usize]
        .graveyard
        .contains(&first));
    assert!(runner.state().players[P0.0 as usize]
        .graveyard
        .contains(&second));
    let mut forced_cost_only =
        runner.state().objects[&mimeoplasm].replacement_definitions[0].clone();
    assert!(matches!(
        &forced_cost_only.mode,
        ReplacementMode::MayCost {
            cost: AbilityCost::Exile { count: 2, .. },
            ..
        }
    ));
    // The printed Oracle parse is the coverage pin. Strip only its independent
    // copy/counter branch so this witness isolates the exact typed two-card MayCost.
    forced_cost_only.execute = None;
    runner
        .state_mut()
        .objects
        .get_mut(&mimeoplasm)
        .expect("Mimeoplasm witness exists")
        .replacement_definitions = vec![forced_cost_only].into();
    runner.cast(mimeoplasm).resolve();
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Mimeoplasm's replacement cost");

    let state = runner.state();
    let Some(PendingCostMoveResume::ReplacementMayCost { remaining, .. }) =
        state.pending_cost_move_resume.as_ref()
    else {
        panic!("the first Mimeoplasm exile must retain its one-card cost tail");
    };
    assert_eq!(remaining.len(), 1);
    let pending = state
        .pending_replacement
        .as_ref()
        .expect("the first inner exile must own its replacement prompt");
    assert_eq!(pending.candidates.len(), 2);
    assert!(matches!(
        &pending.proposed,
        ProposedEvent::ZoneChange {
            from: Zone::Graveyard,
            to: Zone::Exile,
            ..
        }
    ));
    assert!(
        state
            .active_spell_resolution()
            .is_some_and(|ctx| ctx.object_id == mimeoplasm),
        "the outer permanent-spell resolution must survive the inner cost prompt"
    );

    let serialized = serde_json::to_string(runner.state())
        .expect("the nested SpellResolution replacement prompt serializes as v2");
    let restored: GameState = serde_json::from_str(&serialized)
        .expect("the nested SpellResolution replacement prompt restores from v2");
    let mut runner = GameRunner::from_state(restored);

    for prompt in 0..2 {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "expected replacement choice for inner cost move {prompt}, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("apply the forced Mimeoplasm cost redirect");
        if prompt == 0 {
            assert!(
                runner.state().pending_cost_move_resume.is_some(),
                "the first redirected exile must retain the second inner cost move"
            );
            assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
            assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
            assert_eq!(runner.state().objects[&mimeoplasm].zone, Zone::Stack);
            assert!(
                runner
                    .state()
                    .active_spell_resolution()
                    .is_some_and(|ctx| ctx.object_id == mimeoplasm),
                "an inner cost redirect must not consume the outer spell-resolution context"
            );
        } else {
            assert!(
                runner.state().pending_cost_move_resume.is_none(),
                "both forced cost moves must finish before the outer replacement re-enters"
            );
        }
    }

    let state = runner.state();
    assert_eq!(state.objects[&first].zone, Zone::Hand);
    assert_eq!(state.objects[&second].zone, Zone::Hand);
    assert!(
        state
            .cards_exiled_with_source_this_turn
            .get(&mimeoplasm)
            .is_none_or(Vec::is_empty),
        "only cards delivered to exile may be indexed as exiled with Mimeoplasm"
    );
    assert!(
        state
            .exile_links
            .iter()
            .all(|link| link.source_id != mimeoplasm),
        "Mimeoplasm's cost must not create a persistent ExileLink"
    );
    assert_eq!(state.objects[&mimeoplasm].zone, Zone::Battlefield);
    assert!(
        state.active_spell_resolution().is_none(),
        "the outer context is consumed only when Mimeoplasm's own entry completes"
    );
}

#[test]
fn self_return_activation_cost_pauses_for_moved_redirect_without_pending_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature(P0, "Self-Return Cost Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::ReturnToHand {
                count: 1,
                filter: Some(TargetFilter::SelfRef),
                from_zone: None,
            }),
        )
        .id();
    for name in ["First Self-Return Redirect", "Second Self-Return Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Hand, Zone::Exile));
    }

    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("announce self-return activation");

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::ReturnToHand,
                ..
            }
        ),
        "self-return activation should select its return cost before moving: {:?}",
        result.waiting_for
    );
    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![source],
        })
        .expect("select the self-return cost");
    assert!(matches!(
        result.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        runner.state().pending_cast.is_none(),
        "a self-return activation cost must not use PendingCast to resume"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("apply self-return redirect");

    assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    assert!(
        !runner.state().stack.is_empty(),
        "the redirected return-to-hand cost must finish the activation"
    );
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before + 1);
}

#[test]
fn composite_return_cost_resurfaces_each_return_leg() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature(P0, "Two Returns Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::ReturnToHand {
                        count: 1,
                        filter: None,
                        from_zone: None,
                    },
                    AbilityCost::ReturnToHand {
                        count: 1,
                        filter: None,
                        from_zone: None,
                    },
                ],
            }),
        )
        .id();
    let first = scenario.add_basic_land(P0, ManaColor::Blue);
    let second = scenario
        .add_creature(P0, "Second Return Witness", 1, 1)
        .id();

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activate two-return witness");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. }
    ));

    runner
        .act(GameAction::SelectCards { cards: vec![first] })
        .expect("pay first return leg");
    assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
    assert!(
        runner.state().objects[&source].tapped,
        "automatic tap leg is paid once"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. }
    ));

    runner
        .act(GameAction::SelectCards {
            cards: vec![second],
        })
        .expect("pay second return leg");
    assert_eq!(runner.state().objects[&second].zone, Zone::Hand);
    assert!(
        !runner.state().stack.is_empty(),
        "both return legs must complete before the activation reaches the stack"
    );
}

#[test]
fn return_cost_keeps_selected_move_while_residual_self_move_pauses() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature(P0, "Residual Self-Move Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::ReturnToHand {
                        count: 1,
                        filter: None,
                        from_zone: None,
                    },
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                    },
                ],
            }),
        )
        .id();
    let returned = scenario.add_basic_land(P0, ManaColor::Blue);
    for name in ["First Residual Redirect", "Second Residual Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }

    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activate residual self-move witness");
    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![returned],
        })
        .expect("select return before residual self-exile");
    assert!(matches!(
        result.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::Cast { .. })
    ));
    assert_eq!(runner.state().objects[&returned].zone, Zone::Battlefield);

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect residual self-exile");
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&returned].zone, Zone::Hand);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before - 2,
        "the automatic PayLife suffix must resume exactly once before the selected return"
    );
    assert!(
        !runner.state().stack.is_empty(),
        "the selected return must finish after the paused automatic self-move"
    );
}

#[test]
fn modal_activation_self_exile_cost_resumes_after_moved_redirect() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature(P0, "Modal Self-Exile Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Exile {
                count: 1,
                zone: None,
                filter: Some(TargetFilter::SelfRef),
            })
            .with_modal(
                ModalChoice {
                    min_choices: 1,
                    max_choices: 1,
                    mode_count: 1,
                    mode_descriptions: vec!["Gain life".to_string()],
                    ..ModalChoice::default()
                },
                vec![AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )],
            ),
        )
        .id();
    for name in ["First Modal Redirect", "Second Modal Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("announce modal activation");
    let result = runner
        .act(GameAction::SelectModes { indices: vec![0] })
        .expect("select the only mode");
    assert!(matches!(
        result.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect modal activation self-exile cost");
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert!(
        !runner.state().stack.is_empty(),
        "the modal activation must reach the stack after its redirected cost completes"
    );
}

#[test]
fn synthesized_plot_redirect_resumes_as_special_action() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let plotted = scenario
        .add_creature_to_hand(P0, "Synthesized Plot Redirect Witness", 1, 1)
        .id();
    for name in ["First Plot Redirect", "Second Plot Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }

    let mut runner = scenario.build();
    let mut face = CardFace::default();
    face.keywords.push(Keyword::Plot(ManaCost::generic(0)));
    synthesize_plot(&mut face);
    let object = runner
        .state_mut()
        .objects
        .get_mut(&plotted)
        .expect("plot witness exists");
    object.keywords = face.keywords.clone();
    object.base_keywords = face.keywords.clone();
    *Arc::make_mut(&mut object.abilities) = face.abilities.clone();
    *Arc::make_mut(&mut object.base_abilities) = face.abilities;

    let first = runner
        .act(GameAction::ActivateAbility {
            source_id: plotted,
            ability_index: 0,
        })
        .expect("start synthesized plot special action");
    assert!(matches!(
        first.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let second = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect plotted self-exile");

    assert_eq!(runner.state().objects[&plotted].zone, Zone::Graveyard);
    assert!(runner.state().objects[&plotted]
        .casting_permissions
        .iter()
        .any(|permission| matches!(permission, CastingPermission::Plotted { .. })));
    assert!(
        runner.state().stack.is_empty(),
        "plot must never use the stack"
    );
    assert!(
        first
            .events
            .iter()
            .chain(second.events.iter())
            .all(|event| !matches!(event, GameEvent::AbilityActivated { .. })),
        "plot is a special action and must not emit AbilityActivated"
    );
}

#[test]
fn mana_self_exile_cost_redirect_serializes_and_resumes_mana_payment_once() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&source].card_id;
    runner.state_mut().pending_cast = Some(Box::new(PendingCast::new(
        source,
        card_id,
        ResolvedAbility::new(
            Effect::Unimplemented {
                name: "Mana Payment Witness".to_string(),
                description: None,
            },
            vec![],
            source,
            P0,
        ),
        ManaCost::generic(1),
    )));
    let ability = runner.state().objects[&source].abilities[0].clone();
    let mut initial_events = Vec::new();
    let initial = activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut initial_events,
        ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        },
        None,
    )
    .expect("the mana ability activation should reach its self-exile cost");

    assert!(
        matches!(initial, WaitingFor::ReplacementChoice { .. }),
        "a mana self-exile cost must consult competing Moved redirects"
    );
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment {
            pending,
            cursor,
        }) if matches!(&pending.resume, ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        })
            && cursor.remaining.is_empty()
    ));
    let json = serde_json::to_string(runner.state())
        .expect("a paused mana self-exile replacement choice serializes");
    assert!(
        json.contains("ReplacementChoice"),
        "the replacement choice must remain serialized while mana payment is paused"
    );
    let restored: GameState = serde_json::from_str(&json)
        .expect("a paused mana self-exile replacement choice deserializes");
    let mut runner = GameRunner::from_state(restored);

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the mana self-exile cost");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the resumed activation must produce its mana exactly once"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "resuming the cost move must not repay the earlier tap component"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(
                |event| matches!(event, GameEvent::ManaAdded { player_id, .. } if *player_id == P0)
            )
            .count(),
        1,
        "the resumed activation must not produce mana twice"
    );
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment {
            player,
            convoke_mode: None,
        } if player == P0
    ));
}

#[test]
fn mana_self_exile_cost_redirect_serializes_and_resumes_unless_payment_once() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    let ability = runner.state().objects[&source].abilities[0].clone();
    let unless_cost = AbilityCost::Mana {
        cost: ManaCost::generic(1),
    };
    let pending_effect = ResolvedAbility::new(
        Effect::Unimplemented {
            name: "Unless Payment Witness".to_string(),
            description: None,
        },
        vec![],
        source,
        P0,
    );
    let resume = ManaAbilityResume::UnlessPayment {
        outer_player: Some(P0),
        cost: Box::new(unless_cost.clone()),
        pending_effect: Box::new(pending_effect.clone()),
        trigger_event: None,
        effect_description: Some("unless payment witness".to_string()),
        remaining: vec![P1],
    };
    let mut initial_events = Vec::new();
    let initial = activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut initial_events,
        resume,
        None,
    )
    .expect("the mana ability activation should reach its self-exile cost");

    assert!(matches!(initial, WaitingFor::ReplacementChoice { .. }));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment {
            pending,
            cursor,
        }) if matches!(
            &pending.resume,
            ManaAbilityResume::UnlessPayment {
                outer_player: Some(P0),
                cost,
                pending_effect: paused_effect,
                trigger_event: None,
                effect_description: Some(description),
                remaining,
            } if cost.as_ref() == &unless_cost
                && paused_effect.as_ref() == &pending_effect
                && description == "unless payment witness"
                && remaining == &vec![P1]
        ) && cursor.remaining.is_empty()
    ));

    let json = serde_json::to_string(runner.state())
        .expect("a paused unless-payment mana activation serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("a paused unless-payment mana activation deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the mana self-exile cost");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the resumed activation must produce its mana exactly once"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "resuming the cost move must not repay the earlier tap component"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    match &resumed.waiting_for {
        WaitingFor::UnlessPayment {
            player,
            cost,
            pending_effect: resumed_effect,
            trigger_event,
            effect_description,
            remaining,
        } => {
            assert_eq!(*player, P0);
            assert_eq!(cost, &unless_cost);
            assert_eq!(resumed_effect.as_ref(), &pending_effect);
            assert!(trigger_event.is_none());
            assert_eq!(
                effect_description.as_deref(),
                Some("unless payment witness")
            );
            assert_eq!(remaining, &vec![P1]);
        }
        other => panic!("expected exact UnlessPayment resume, got {other:?}"),
    }
}

#[test]
fn auto_tap_cost_move_redirect_preserves_outer_mana_payment() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let spell = scenario
        .add_spell_to_hand(P0, "Auto-Tap Cost-Move Payment Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    let announced = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("manual spell payment should reach its mana window");
    assert!(matches!(
        announced.waiting_for,
        WaitingFor::ManaPayment { .. }
    ));

    let paused = runner
        .act(GameAction::PassPriority)
        .expect("auto-tap must surface a mana source's replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor })
            if matches!(pending.resume, ManaAbilityResume::ManaPayment {
                outer_player: Some(P0),
                convoke_mode: None,
            })
                && cursor.remaining.is_empty()
    ));
    assert!(runner.state().pending_cast.is_some());
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "the spell's mana cost must not be spent before the source move settles"
    );

    let json = serde_json::to_string(runner.state())
        .expect("the auto-payment replacement pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the auto-payment replacement pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the auto-tapped mana source's exile cost");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment {
            player,
            convoke_mode: None,
        } if player == P0
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the source resumes once and leaves its mana available to the outer payment"
    );

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the outer spell payment resumes after the source move");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "resuming the outer payment must not repay the mana source's tap prefix"
    );
}

#[test]
fn auto_tap_cost_move_redirect_preserves_outer_unless_payment() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let unless_cost = AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                },
            },
            AbilityCost::Mana {
                cost: ManaCost::generic(1),
            },
        ],
    };
    let pending_effect = ResolvedAbility::new(
        Effect::Unimplemented {
            name: "Auto-Tap Unless Payment Witness".to_string(),
            description: None,
        },
        vec![],
        source,
        P0,
    );
    runner.state_mut().waiting_for = WaitingFor::UnlessPayment {
        player: P0,
        cost: unless_cost.clone(),
        pending_effect: Box::new(pending_effect.clone()),
        trigger_event: None,
        effect_description: Some("auto-tap unless payment witness".to_string()),
        remaining: vec![P1],
    };

    let paused = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("auto-tap must preserve an unless payment while the source move pauses");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor }) if matches!(
            &pending.resume,
            ManaAbilityResume::UnlessPayment {
                outer_player: Some(P0),
                cost,
                pending_effect: paused_effect,
                trigger_event: None,
                effect_description: Some(description),
                remaining,
            } if cost.as_ref() == &unless_cost
                && paused_effect.as_ref() == &pending_effect
                && description == "auto-tap unless payment witness"
                && remaining == &vec![P1]
        ) && cursor.remaining.is_empty()
            && cursor.resolution_mode == ManaAbilityCostResolutionMode::AutoResolved
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        1,
        "the colored prefix may be produced, but the unsettled generic source must prevent spending the unless cost"
    );

    let json = serde_json::to_string(runner.state())
        .expect("the auto unless-payment replacement pause serializes");
    let restored: GameState = serde_json::from_str(&json)
        .expect("the auto unless-payment replacement pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the auto-tapped source's exile cost");
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::UnlessPayment {
            player,
            ref cost,
            ref remaining,
            ..
        } if player == P0 && cost == &unless_cost && remaining == &vec![P1]
    ));

    let paid = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the restored unless payment should spend the resumed mana");
    assert!(matches!(paid.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
}

#[test]
fn effect_pay_cost_auto_tap_redirect_serializes_exact_cost_and_trailing_effect_once() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    };
    let mut ability = ResolvedAbility::new(
        Effect::PayCost {
            cost: AbilityCost::Mana { cost: cost.clone() },
            scale: None,
            payer: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));

    let starting_life = runner.state().players[P0.0 as usize].life;
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("effect payment should pause only for the replacement choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            &pending.resume,
            ManaAbilityResume::EffectPayCost {
                payer: P0,
                ability: paused_ability,
                cost: paused_cost,
                ..
            } if paused_ability.as_ref() == &ability
                && paused_cost.as_ref() == &AbilityCost::Mana { cost: cost.clone() }
        )
    ));

    let json =
        serde_json::to_string(runner.state()).expect("paused effect-cost mana payment serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("paused effect-cost mana payment deserializes");
    let mut runner = GameRunner::from_state(restored);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirected source cost resumes the exact outer effect cost");

    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        starting_life + 1,
        "the trailing effect must resume exactly once after the outer cost is paid"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
}

/// The plan's §"Exact no-ledger split" ordering, measured at a DURABLE-LEDGER
/// root: *"For a completed root, invoke the wrapper immediately after mana
/// production/activation completion and **before** `resume_mana_ability_root`."*
///
/// The witness is an `ManaAbilityResume::EffectPayCost` root — the one root
/// family whose resume is not inert: `pay_ability_cost_for_resolution` spends
/// the pool and then `resolve_effect_pay_cost_rider` runs the trailing effect,
/// all inside `resume_mana_ability_root`. Two distinguishable observers pin the
/// boundary: OT watches the frame's own tap, OL watches the RIDER's life gain.
///
/// With settlement before the resume, the frame's batch is exactly its own tap
/// events, so OT is a one-member batch that needs no CR 603.3b ordering prompt,
/// and the rider's life-gain event is not part of the frame at all. Restoring
/// baseline's resume-then-settle order sweeps the rider's event into the same
/// completed-frame batch, producing a two-member group and an `OrderTriggers`
/// prompt — the exact durable-state difference this slice owns.
#[test]
fn durable_ledger_effect_pay_cost_root_settles_before_its_resume_runs_the_rider() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let observer_t = scenario
        .add_creature(P0, "OT Source-Tap Observer", 0, 0)
        .as_enchantment()
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::Taps)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                        player: TargetFilter::Controller,
                    },
                ))
                .valid_card(TargetFilter::SpecificObject { id: source })
                .trigger_zones(vec![Zone::Battlefield]),
        )
        .id();
    let observer_l = scenario
        .add_creature(P0, "OL Life-Gain Observer", 0, 0)
        .as_enchantment()
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::LifeGained)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                ))
                .valid_card(TargetFilter::Any)
                .trigger_zones(vec![Zone::Battlefield]),
        )
        .id();
    let mut runner = scenario.build();

    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    };
    let mut ability = ResolvedAbility::new(
        Effect::PayCost {
            cost: AbilityCost::Mana { cost: cost.clone() },
            scale: None,
            payer: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("effect payment pauses only for the replacement choice");
    // Positive reach guard: the DURABLE ledger really is live, and the root
    // really is the effect-payment family whose resume is not inert.
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor })
            if matches!(&pending.resume, ManaAbilityResume::EffectPayCost { payer: P0, .. })
                && !cursor.deferred_cost_events.is_empty()
    ));
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "no context is materialized while the replacement choice is live"
    );
    let life_before = runner.state().players[P0.0 as usize].life;

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the redirected source cost resumes the exact outer effect cost");

    // The frame settled BEFORE the rider ran, so its batch is exactly OT.
    assert!(
        !matches!(resumed.waiting_for, WaitingFor::OrderTriggers { .. }),
        "a one-member completed-frame batch needs no CR 603.3b ordering prompt, got {:?}",
        resumed.waiting_for
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before + 1,
        "the rider ran exactly once, after the frame settled"
    );
    let stacked: Vec<ObjectId> = runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match entry.kind {
            StackEntryKind::TriggeredAbility { .. } => Some(entry.source_id),
            _ => None,
        })
        .collect();
    assert!(
        stacked.contains(&observer_t),
        "OT was collected by the completed mana frame: {stacked:?}"
    );
    assert!(
        stacked.contains(&observer_l),
        "and OL was collected for the rider's own event: {stacked:?}"
    );
    assert_eq!(
        stacked.len(),
        2,
        "each observer is placed exactly once: {stacked:?}"
    );
}

#[test]
fn effect_pay_cost_rider_waits_for_scry_post_effect_before_typed_root_settles() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Effect PayCost Scry Ordering Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    let scry_card = scenario.add_card_to_library_top(P0, "Effect PayCost Scry Ordering Card");
    scenario
        .add_creature(P0, "Effect PayCost Scry Ordering Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(scry_after_moved_to_exile());

    let mut runner = scenario.build();
    let mut ability = ResolvedAbility::new(
        Effect::PayCost {
            cost: AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                },
            },
            scale: None,
            payer: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));
    let life_before = runner.state().players[P0.0 as usize].life;
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("effect PayCost reaches its source-cost replacement post-effect");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ScryChoice { player: P0, ref cards } if cards == &vec![scry_card]
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
            if matches!(pending.resume, ManaAbilityResume::EffectPayCost { .. })
    ));
    assert!(
        runner.state().active_ability_continuation().is_none(),
        "only replacement post-effect work may drain before the typed Effect::PayCost root"
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before);

    let json = serde_json::to_string(runner.state())
        .expect("the interactive post-effect and typed EffectPayCost root serialize together");
    let restored: GameState = serde_json::from_str(&json)
        .expect("the interactive post-effect and typed EffectPayCost root deserialize together");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::SelectCards {
            cards: vec![scry_card],
        })
        .expect("settling Scry completes the typed root before releasing the PayCost rider");

    let mana_added = resumed
        .events
        .iter()
        .position(
            |event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source),
        )
        .expect("the source produces its mana while the typed root settles");
    let rider_life = resumed
        .events
        .iter()
        .position(|event| matches!(event, GameEvent::LifeChanged { player_id, amount } if *player_id == P0 && *amount == 1))
        .expect("the trailing PayCost rider resolves once");
    assert!(
        mana_added < rider_life,
        "the trailing PayCost rider must remain parked until the Scry post-effect and typed mana root complete"
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before + 1);
    assert!(runner.state().pending_cost_move_resume.is_none());
}

fn assert_repeated_interactive_activation_cost(
    mut runner: GameRunner,
    source: engine::types::identifiers::ObjectId,
    chosen: [engine::types::identifiers::ObjectId; 2],
    one_of: bool,
    expected_kind: impl Fn(&PayCostKind) -> bool,
) {
    let activated = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the activation starts its interactive cost payment");
    if one_of {
        assert!(matches!(
            activated.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { .. }
        ));
        runner
            .act(GameAction::ChooseActivationCostBranch { index: 0 })
            .expect("the only disjunctive cost branch is payable");
    } else {
        assert!(matches!(
            activated.waiting_for,
            WaitingFor::PayCost { ref kind, .. } if expected_kind(kind)
        ));
    }

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { ref kind, .. } if expected_kind(kind)
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen[0]],
        })
        .expect("the first repeated interactive cost leg is paid");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { ref kind, .. } if expected_kind(kind)
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen[1]],
        })
        .expect("the second repeated interactive cost leg is paid");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the activation reaches the stack only after both selected cost legs"
    );
}

fn repeated_discard_activation_witness(
    one_of: bool,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    [engine::types::identifiers::ObjectId; 2],
) {
    let discard = AbilityCost::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        filter: None,
        selection: CardSelectionMode::Chosen,
        self_scope: DiscardSelfScope::FromHand,
    };
    let cost = AbilityCost::Composite {
        costs: vec![discard.clone(), discard],
    };
    let cost = if one_of {
        AbilityCost::OneOf { costs: vec![cost] }
    } else {
        cost
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Repeated Discard Activation Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(cost),
        )
        .id();
    let first = scenario.add_card_to_hand(P0, "First Repeated Discard Witness");
    let second = scenario.add_card_to_hand(P0, "Second Repeated Discard Witness");
    (scenario.build(), source, [first, second])
}

fn repeated_sacrifice_activation_witness(
    one_of: bool,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    [engine::types::identifiers::ObjectId; 2],
) {
    let sacrifice = AbilityCost::Sacrifice(SacrificeCost::count(
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
        1,
    ));
    let cost = AbilityCost::Composite {
        costs: vec![sacrifice.clone(), sacrifice],
    };
    let cost = if one_of {
        AbilityCost::OneOf { costs: vec![cost] }
    } else {
        cost
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Repeated Sacrifice Activation Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(cost),
        )
        .id();
    let first = scenario
        .add_creature(P0, "First Repeated Sacrifice Witness", 1, 1)
        .as_artifact()
        .id();
    let second = scenario
        .add_creature(P0, "Second Repeated Sacrifice Witness", 1, 1)
        .as_artifact()
        .id();
    (scenario.build(), source, [first, second])
}

fn repeated_exile_activation_witness(
    one_of: bool,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    [engine::types::identifiers::ObjectId; 2],
) {
    let exile = AbilityCost::Exile {
        count: 1,
        zone: Some(Zone::Hand),
        filter: None,
    };
    let cost = AbilityCost::Composite {
        costs: vec![exile.clone(), exile],
    };
    let cost = if one_of {
        AbilityCost::OneOf { costs: vec![cost] }
    } else {
        cost
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Repeated Exile Activation Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(cost),
        )
        .id();
    let first = scenario.add_card_to_hand(P0, "First Repeated Exile Witness");
    let second = scenario.add_card_to_hand(P0, "Second Repeated Exile Witness");
    (scenario.build(), source, [first, second])
}

fn repeated_unattach_activation_witness(
    one_of: bool,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    [engine::types::identifiers::ObjectId; 2],
) {
    let unattach = AbilityCost::UnattachFrom {
        filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
        count: 1,
    };
    let cost = AbilityCost::Composite {
        costs: vec![unattach.clone(), unattach],
    };
    let cost = if one_of {
        AbilityCost::OneOf { costs: vec![cost] }
    } else {
        cost
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Repeated Unattach Activation Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(cost),
        )
        .id();
    let first = scenario
        .add_creature(P0, "First Repeated Unattach Witness", 0, 1)
        .as_artifact()
        .id();
    let second = scenario
        .add_creature(P0, "Second Repeated Unattach Witness", 0, 1)
        .as_artifact()
        .id();
    let mut runner = scenario.build();
    for attachment in [first, second] {
        runner
            .state_mut()
            .objects
            .get_mut(&attachment)
            .unwrap()
            .attached_to = Some(AttachTarget::Object(source));
        runner
            .state_mut()
            .objects
            .get_mut(&source)
            .unwrap()
            .attachments
            .push(attachment);
    }
    (runner, source, [first, second])
}

#[test]
fn repeated_and_one_of_interactive_activation_costs_surface_each_unpaid_leg() {
    for one_of in [false, true] {
        let (runner, source, chosen) = repeated_discard_activation_witness(one_of);
        assert_repeated_interactive_activation_cost(runner, source, chosen, one_of, |kind| {
            matches!(kind, PayCostKind::Discard)
        });

        let (runner, source, chosen) = repeated_sacrifice_activation_witness(one_of);
        assert_repeated_interactive_activation_cost(runner, source, chosen, one_of, |kind| {
            matches!(kind, PayCostKind::Sacrifice)
        });

        let (runner, source, chosen) = repeated_exile_activation_witness(one_of);
        assert_repeated_interactive_activation_cost(runner, source, chosen, one_of, |kind| {
            matches!(kind, PayCostKind::ExileFromZone { .. })
        });

        let (runner, source, chosen) = repeated_unattach_activation_witness(one_of);
        assert_repeated_interactive_activation_cost(runner, source, chosen, one_of, |kind| {
            matches!(kind, PayCostKind::UnattachFrom { .. })
        });
    }
}

#[test]
fn mana_selected_exile_cost_redirect_resumes_after_the_paid_prefix_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Mana Selected-Exile Redirect Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 2,
                        zone: Some(Zone::Battlefield),
                        filter: None,
                    },
                ],
            }),
        )
        .id();
    let selected = scenario
        .add_creature(P0, "Selected Exile Payment", 1, 1)
        .id();
    let second_selected = scenario
        .add_creature(P0, "Second Selected Exile Payment", 1, 1)
        .id();
    for name in [
        "First Mana Selected-Exile Redirect",
        "Second Mana Selected-Exile Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("start the selected-exile mana ability");
    let initial = runner
        .act(GameAction::SelectCards {
            cards: vec![selected, second_selected],
        })
        .expect("select the creature for the mana ability exile cost");

    assert!(
        matches!(initial.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "a selected mana-exile cost must consult competing Moved redirects"
    );
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { cursor, .. })
            if cursor.remaining.len() == 1
                && cursor.selected_exile_remaining.as_deref() == Some(&[second_selected])
    ));
    let json = serde_json::to_string(runner.state())
        .expect("a paused selected mana-exile replacement choice serializes");
    let restored: GameState = serde_json::from_str(&json)
        .expect("a paused selected mana-exile replacement choice deserializes");
    let mut runner = GameRunner::from_state(restored);

    let after_first_redirect = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the first selected mana-exile cost");

    assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&second_selected].zone,
        Zone::Battlefield
    );
    assert!(matches!(
        after_first_redirect.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the second selected mana-exile cost");

    assert_eq!(
        runner.state().objects[&second_selected].zone,
        Zone::Graveyard
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the selected-exile activation must produce exactly one mana after resuming"
    );
    assert_eq!(
        initial
            .events
            .iter()
            .chain(after_first_redirect.events.iter())
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the selected-exile resume must not replay the paid tap prefix"
    );
    assert_eq!(
        initial
            .events
            .iter()
            .chain(after_first_redirect.events.iter())
            .chain(resumed.events.iter())
            .filter(
                |event| matches!(event, GameEvent::ManaAdded { player_id, .. } if *player_id == P0)
            )
            .count(),
        1,
        "the selected-exile resume must not produce mana twice"
    );
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
}

#[test]
fn effect_pay_cost_composite_mana_life_suffix_serializes_and_rides_once() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    let mana = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    };
    let cost = AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana { cost: mana.clone() },
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
        ],
    };
    let mut ability = ResolvedAbility::new(
        Effect::PayCost {
            cost: cost.clone(),
            scale: None,
            payer: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));

    let life_before = runner.state().players[P0.0 as usize].life;
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("the composite effect cost reaches the mana source replacement choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "neither the later life cost nor the rider may run before the typed mana root settles"
    );
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            &pending.resume,
            ManaAbilityResume::EffectPayCost { cost: paused_cost, .. }
                if paused_cost.as_ref() == &cost
        )
    ));

    let json = serde_json::to_string(runner.state())
        .expect("the complete composite effect-cost root serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the complete composite effect-cost root deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirected mana-source cost resumes the full Composite suffix");

    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before - 1,
        "the exact order is mana, PayLife once, then the +1-life rider once"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the source's paid tap prefix is never replayed"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
}

#[test]
fn effect_pay_cost_composite_mana_life_prevention_serializes_and_rides_once() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    let mana = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    };
    let cost = AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana { cost: mana },
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
        ],
    };
    let mut ability = ResolvedAbility::new(
        Effect::PayCost {
            cost,
            scale: None,
            payer: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));

    let life_before = runner.state().players[P0.0 as usize].life;
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("the composite effect cost reaches a mana-source cost move pause");

    // No ZoneChange replacement currently yields `Prevented`, so stage the
    // canonical one-shot Prevented producer while retaining the real paused
    // ManaAbilityPayment root. This drives the actual replacement-choice
    // dispatcher branch that a future cost-move prevention will use.
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("mana source exists")
        .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Destroy)
        .regeneration_shield()
        .description("Prevent the staged cost move".to_string())]
    .into();
    runner.state_mut().pending_replacement = Some(PendingReplacement {
        proposed: ProposedEvent::Destroy {
            object_id: source,
            source: None,
            cant_regenerate: false,
            applied: Default::default(),
        },
        sacrifice_provenance: None,
        candidates: vec![ReplacementId { source, index: 0 }],
        search_found_candidates: Vec::new(),
        depth: 0,
        is_optional: false,
        library_placement: None,
        exile_controller: None,
        exile_duration: None,
        exile_tracking: engine::types::game_state::ZoneDeliveryExileTracking::None,
        excess_recipient: None,
        lifelink_bonus: 0,
        may_cost_paid: false,
        may_cost_remaining: None,
    });
    runner.state_mut().waiting_for = WaitingFor::ReplacementChoice {
        player: P0,
        candidate_count: 1,
        candidates: vec![],
    };

    let json = serde_json::to_string(runner.state())
        .expect("the prevented composite effect-cost root serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the prevented composite effect-cost root deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the Prevented dispatcher resumes the complete typed cost root");

    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before - 1,
        "prevention still settles mana then PayLife once before the rider"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the prevented cost move cannot replay the source's paid tap prefix"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
}

#[test]
fn paused_mana_cost_events_create_observer_triggers_once_and_preserve_order_resume() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let observer_trigger = |amount| {
        TriggerDefinition::new(TriggerMode::Taps)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: amount },
                    player: TargetFilter::Controller,
                },
            ))
            .valid_card(TargetFilter::Any)
            .trigger_zones(vec![Zone::Battlefield])
    };
    for (name, amount) in [
        ("First Cost Event Observer", 1),
        ("Second Cost Event Observer", 2),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_trigger_definition(observer_trigger(amount));
    }
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&source].card_id;
    runner.state_mut().pending_cast = Some(Box::new(PendingCast::new(
        source,
        card_id,
        ResolvedAbility::new(
            Effect::Unimplemented {
                name: "Cost event observer outer payment".to_string(),
                description: None,
            },
            vec![],
            source,
            P0,
        ),
        ManaCost::generic(1),
    )));
    let ability = runner.state().objects[&source].abilities[0].clone();
    let mut initial_events = Vec::new();
    activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut initial_events,
        ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        },
        None,
    )
    .expect("the source pauses after its tap cost");
    assert!(runner.state().stack.is_empty());

    let json = serde_json::to_string(runner.state()).expect("paused cost event batch serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("paused cost event batch deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("resume the typed cost event settlement");

    // A completed mana frame is a cost-payment micro-frame inside somebody
    // else's action, not a CR 603.3b release boundary. Its ordinary observers
    // are collected exactly once and deferred; ordering and announcement belong
    // to the owner's own boundary, so the resumed action returns straight to the
    // outer payment with the queue intact and nothing on the stack.
    assert!(
        matches!(
            resumed.waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ),
        "a completed mana micro-frame returns to its owner's payment, not to CR 603.3b ordering: \
         {:?}",
        resumed.waiting_for
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(
        runner.state().stack.is_empty(),
        "no observer may be announced from inside the payment that produced its events"
    );
    let queued_amounts = |state: &GameState| -> Vec<i32> {
        state
            .deferred_triggers
            .iter()
            .map(|context| match &context.pending.ability.effect {
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value },
                    ..
                } => *value,
                other => panic!("unexpected deferred observer effect {other:?}"),
            })
            .collect()
    };
    assert_eq!(
        queued_amounts(runner.state()),
        vec![1, 2],
        "each actual observer trigger is collected exactly once, in the collector's APNAP order, \
         not once per pause and resume"
    );

    // The queue is durable across the returned prompt: it is engine state, not
    // action-local coordination.
    let json = serde_json::to_string(runner.state())
        .expect("the deferred observer release group serializes at the outer payment prompt");
    let across: GameState = serde_json::from_str(&json)
        .expect("the deferred observer release group deserializes at the outer payment prompt");
    assert_eq!(queued_amounts(&across), vec![1, 2]);
    assert!(across.stack.is_empty());

    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "the resumed mana production itself is emitted once"
    );
}

#[test]
fn nested_costed_mana_source_serializes_parent_cursor_and_finishes_outer_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let outer = scenario
        .add_creature(P0, "Outer Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Mana {
                        cost: ManaCost::generic(1),
                    },
                ],
            }),
        )
        .id();
    let inner = scenario
        .add_creature(P0, "Inner Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in [
        "First Nested Source Redirect",
        "Second Nested Source Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Nested Mana Payment Target", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the nested witness must announce a real pending cast");
    assert!(matches!(
        cast.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    let outer_ability = runner.state().objects[&outer].abilities[0].clone();
    let mut initial_events = Vec::new();
    let paused = activate_mana_ability(
        runner.state_mut(),
        outer,
        P0,
        0,
        &outer_ability,
        &mut initial_events,
        ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        },
        None,
    )
    .expect("the inner source pauses on its self-exile cost");
    assert!(matches!(paused, WaitingFor::ReplacementChoice { .. }));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor })
            if pending.source_id == inner
                && cursor.parent.as_ref().is_some_and(|parent| {
                    parent.lifecycle == ManaAbilityCostParentLifecycle::Suspended
                        && parent.pending.source_id == outer
                        && matches!(parent.cursor.remaining.as_slice(), [AbilityCost::Mana { .. }])
                })
    ));

    let json =
        serde_json::to_string(runner.state()).expect("the suspended parent mana cursor serializes");
    assert!(
        json.contains("Suspended"),
        "the serialized parent frame must retain its typed re-entry ownership"
    );
    let restored: GameState =
        serde_json::from_str(&json).expect("the suspended parent mana cursor deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect inner self-exile and resume the exact parent cursor");

    assert_eq!(runner.state().objects[&inner].zone, Zone::Graveyard);
    assert!(runner.state().objects[&outer].tapped);
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == outer))
            .count(),
        1,
        "the outer tap prefix is retained by the parent cursor rather than replayed"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == inner))
            .count(),
        1,
        "the inner source's tap cost is paid once across the replacement pause"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Battlefield),
                    to: Zone::Graveyard,
                    ..
                } if *object_id == inner
            ))
            .count(),
        1,
        "the redirected inner self-exile cost is delivered once"
    );
    for source_id in [inner, outer] {
        assert_eq!(
            initial_events
                .iter()
                .chain(resumed.events.iter())
                .filter(|event| matches!(event, GameEvent::ManaAdded { source_id: id, .. } if *id == source_id))
                .count(),
            1,
            "each nested mana ability produces exactly once"
        );
    }

    runner
        .act(GameAction::PassPriority)
        .expect("the outer spell payment consumes the outer mana once");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

/// Which permanent the ordinary observer of the A/B/C/D topology watches.
///
/// This is the ONLY axis the sibling row changes, which is what makes it the
/// discriminator for the ancestor-prefix finding: observer E fires on A's tap,
/// which lives in the *suspended parent's* prefix, not in the paused child's
/// local ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostileObserverAxis {
    /// Observer D: source-filtered `Taps` on the paused child B.
    TapsB,
    /// Observer E: source-filtered `Taps` on the suspended parent A.
    TapsA,
}

struct HostileAbcdFixture {
    scenario: GameScenario,
    source_a: ObjectId,
    source_b: ObjectId,
    source_c: ObjectId,
    observer: ObjectId,
    observer_gain: i32,
}

/// The plan's four-permanent hostile topology, shared byte-for-byte by the
/// direct-Priority row, its observer-axis sibling, and both masked-root
/// controls. Only `axis` differs between them.
fn hostile_abcd_fixture(axis: HostileObserverAxis) -> HostileAbcdFixture {
    fn targetless_reflexive_gain(amount: i32) -> AbilityDefinition {
        let mut reflexive = AbilityDefinition::new(
            AbilityKind::Database,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: amount },
                player: TargetFilter::Controller,
            },
        );
        reflexive.condition = Some(engine::types::ability::AbilityCondition::WhenYouDo);
        reflexive
    }

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A — noncreature ARTIFACT so no summoning sickness and no creature-tier
    // ambiguity in source selection. `{T}, {2}: Add {G}`, plus a targetless true
    // `WhenYouDo` rider gaining 5.
    let source_a = scenario
        .add_creature(P0, "A Root Green Source", 1, 1)
        .as_artifact()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Mana {
                        cost: ManaCost::generic(2),
                    },
                ],
            })
            .sub_ability(targetless_reflexive_gain(5)),
        )
        .id();

    // B — noncreature LAND. Its land card tier deterministically precedes C's
    // artifact tier, and its reflexive continuation classifies it
    // `HasIrreversibleContinuation`; both penalties stay in tier zero. No
    // auto-tap ordering code is touched — the fixture must EARN B-before-C by
    // reaching the pause, and the assertions in each row are what prove it did.
    let source_b = scenario
        .add_creature(P0, "B Paused Reflexive Land", 1, 1)
        .as_land()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![ManaSpendRestriction::ActivateOnly],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            })
            .sub_ability(targetless_reflexive_gain(3)),
        )
        .id();

    // C — noncreature artifact, `{T}: Add {C}`, no continuation.
    let source_c = scenario
        .add_creature(P0, "C Synchronous Source", 1, 1)
        .as_artifact()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![ManaSpendRestriction::ActivateOnly],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id();

    // Two COMPETING redirects for B's self-exile, so the exile raises a real
    // `ReplacementChoice` rather than applying silently.
    for name in ["B Redirect One", "B Redirect Two"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }

    let (observer_name, watched, observer_gain) = match axis {
        HostileObserverAxis::TapsB => ("D B-Tap Observer", source_b, 2),
        HostileObserverAxis::TapsA => ("E A-Tap Observer", source_a, 4),
    };
    let observer = scenario
        .add_creature(P0, observer_name, 0, 0)
        .as_enchantment()
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::Taps)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed {
                            value: observer_gain,
                        },
                        player: TargetFilter::Controller,
                    },
                ))
                .valid_card(TargetFilter::SpecificObject { id: watched })
                .trigger_zones(vec![Zone::Battlefield]),
        )
        .id();

    HostileAbcdFixture {
        scenario,
        source_a,
        source_b,
        source_c,
        observer,
        observer_gain,
    }
}

/// CR 605.3a-c + CR 603.12 + CR 601.2h — the plan's **direct-priority A/B/C/D
/// hostile regression**, replacing the blocked `EndContinuousEffect` row.
///
/// `EndContinuousEffect` cannot host this proof and was not weakened to match
/// the trace it actually produces: `pay_non_cast_mana_cost`'s automatic planner
/// pre-funds a costed source LEAF-FIRST, so B is parentless and C is already
/// committed before A starts, and no live suspended parent with a LATER
/// synchronous child is ever formed. `game/casting_costs.rs` and the
/// special-action protocol are out of scope, so the root moves instead.
///
/// The reachable root is a real player action: `ActivateAbility` at
/// `WaitingFor::Priority`. A becomes a genuine parentless cursor with no
/// `PendingCast`, no resolution stack entry, and no unless-payment owner. Its
/// `{2}` Mana component asks automatic payment, which selects the LAND B before
/// the artifact C; B's `{T}, Exile this` hits two competing exile replacements
/// and serializes the whole nested cursor tree with A recursively `Suspended`.
///
/// What this row discriminates that a ledger-shape unit test cannot: the
/// **prepared parent snapshot's wiring**. At the pause, A's suspended parent
/// ledger must hold exactly A's own tap and no B tap, and B's frame-local
/// ledger exactly B's tap and no A tap. Replacing
/// `parent_snapshot_with_current_cost_events(..)` at the
/// `resolve_mana_ability_excluding` call site with a plain `parent.cloned()`
/// leaves A's parent prefix EMPTY and fails assertion (7); restoring the
/// pre-fix clone-and-scan child ledger puts A's tap into B's ledger and fails
/// assertion (6).
///
/// SCOPE: this row lands the plan's assertions 1-16 for the DIRECT-Priority
/// root. Its observer-E sibling
/// (`direct_priority_mana_root_orders_the_ancestor_prefix_observer_with_both_reflexives`)
/// and the masked cast-root control
/// (`masked_cast_root_mana_batch_stays_queued_until_the_spell_is_announced`)
/// carry the rest of this topology's plan requirements. The second masked
/// control — an `UnlessPayment` resolution owner over the same A/B/C/D fixture —
/// is `masked_unless_payment_root_mana_batch_stays_queued_until_the_owner_settles`.
#[test]
fn direct_priority_mana_root_suspends_its_parent_and_keeps_ledgers_disjoint() {
    let HostileAbcdFixture {
        scenario,
        source_a,
        source_b,
        source_c,
        observer,
        observer_gain: _,
    } = hostile_abcd_fixture(HostileObserverAxis::TapsB);
    let observer_d = observer;
    let mut runner = scenario.build();

    // (1) Reach guards for the ROOT itself: a real empty-stack Priority with no
    // cast, resolution, or payment owner. Without these the row could pass from
    // some other production chronology.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().pending_cast.is_none());
    assert!(runner.state().pending_cost_move_resume.is_none());
    let life_before = runner.state().players[P0.0 as usize].life;
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);

    // (2) The production root action.
    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: source_a,
            ability_index: 0,
        })
        .expect("A's {2} Mana component pays through B and pauses on its exile replacement");

    // (3) A real replacement pause, not a synthesized one.
    assert!(
        matches!(paused.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "expected B's exile ReplacementChoice, got {:?}",
        paused.waiting_for
    );

    // (4)-(7) The nested cursor tree, its owner, and the two DISJOINT ledgers.
    assert_nested_hostile_pause(runner.state(), source_a, source_b);

    // (5) Tapped-state reach guards for the intended chronology: A and B are
    // paid, C has not been reached yet, and B is still on the battlefield
    // pending its replacement choice.
    assert!(runner.state().objects[&source_a].tapped, "A paid its tap");
    assert!(runner.state().objects[&source_b].tapped, "B paid its tap");
    assert!(
        !runner.state().objects[&source_c].tapped,
        "C must be untapped at the pause — a tapped C means the leaf-first \
         planner chronology, not the nested-parent one this row exists to prove"
    );
    assert_eq!(runner.state().objects[&source_b].zone, Zone::Battlefield);

    // (11) Nothing has resolved yet.
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before);

    // (6) The mandatory durable branch. `current_action_event_start` is
    // `#[serde(skip)]`, so A's one-event parent prefix must survive on the
    // serialized ledger itself, not on the marker.
    let json = serde_json::to_string(runner.state()).expect("paused cursor tree serializes");
    let restored: GameState = serde_json::from_str(&json).expect("paused cursor tree restores");
    let mut runner = GameRunner::from_state(restored);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_nested_hostile_pause(runner.state(), source_a, source_b);
    assert!(runner.state().objects[&source_a].tapped);
    assert!(runner.state().objects[&source_b].tapped);
    assert!(!runner.state().objects[&source_c].tapped);

    // (7) Resume through the normal action.
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the replacement choice resumes B, then A's later synchronous C");

    // (8) B moved exactly once, to the redirected zone.
    assert_eq!(runner.state().objects[&source_b].zone, Zone::Graveyard);
    assert_eq!(
        resumed
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::ZoneChanged { object_id, .. } if *object_id == source_b
            ))
            .count(),
        1,
        "no duplicate exile or move delivery across the pause/resume boundary"
    );

    // (9) C then taps exactly once, and A produces its green mana. B's and C's
    // colorless mana was spent on A's {2}; only A's green remains.
    assert!(
        runner.state().objects[&source_c].tapped,
        "A's remaining generic mana must come from the LATER synchronous child C"
    );
    let pool = &runner.state().players[P0.0 as usize].mana_pool;
    assert_eq!(
        pool.total(),
        1,
        "only A's own production remains in the pool"
    );
    assert_eq!(pool.count_color(ManaType::Green), 1);

    // (10) Every paid cost and every production happened exactly once across
    // the pause/resume boundary.
    let all_events: Vec<&GameEvent> = paused.events.iter().chain(resumed.events.iter()).collect();
    for (label, id) in [("A", source_a), ("B", source_b), ("C", source_c)] {
        assert_eq!(
            all_events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::PermanentTapped { object_id, .. } if *object_id == id
                ))
                .count(),
            1,
            "{label} paid its tap cost exactly once"
        );
        assert_eq!(
            all_events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::ManaAdded { source_id, .. } if *source_id == id
                ))
                .count(),
            1,
            "{label} produced mana exactly once"
        );
    }

    // (11, after resume) Still nothing resolved: neither reflexive nor D.
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "no reflexive rider and no observer may resolve before the batch is ordered"
    );
    // (12) ONE empty-stack `OrderTriggers` group holding exactly three members:
    // B's reflexive rider, D's B-tap observer, and A's reflexive rider — the
    // last of which only exists after C completed and A produced its mana.
    //
    // (13) This exact three-member, empty-stack shape is the joint revert
    // discriminator. Under the old inherited-ledger behaviour C scans A's cloned
    // ancestor batch before A finishes, exposing an early incomplete or
    // duplicated group; under a split collection seam D is already a stack entry
    // and only the two reflexives remain to order. Neither revert can produce
    // one empty-stack order prompt containing all three.
    let WaitingFor::OrderTriggers {
        player: order_player,
        triggers: ref group,
    } = resumed.waiting_for
    else {
        panic!(
            "the completed root must expose one CR 603.3b ordering group, got {:?}",
            resumed.waiting_for
        );
    };
    assert_eq!(order_player, P0);
    assert!(
        runner.state().stack.is_empty(),
        "no member of the batch may be dispatched separately: the stack must still \
         be empty when the single ordering group is offered"
    );
    let members: Vec<ObjectId> = group.iter().map(|summary| summary.source_id).collect();
    assert_eq!(
        members.len(),
        3,
        "expected exactly B's reflexive, D's observer, and A's reflexive; got {members:?}"
    );
    for (label, id) in [("A", source_a), ("B", source_b), ("D", observer_d)] {
        assert_eq!(
            members.iter().filter(|member| **member == id).count(),
            1,
            "{label} must appear in the single ordering group exactly once: {members:?}"
        );
    }

    // (14) Order the group through the real action and identify the resulting
    // stack entries by source.
    let ordered = runner
        .act(GameAction::OrderTriggers {
            order: vec![0, 1, 2],
        })
        .expect("the three-member group orders through the real CR 603.3b action");
    assert!(
        matches!(ordered.waiting_for, WaitingFor::Priority { player } if player == P0),
        "ordering the whole group returns priority, got {:?}",
        ordered.waiting_for
    );
    let announced: Vec<ObjectId> = runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match entry.kind {
            StackEntryKind::TriggeredAbility { source_id, .. } => Some(source_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        announced.len(),
        3,
        "exactly three triggered entries, no duplicates and no preexisting D entry: {announced:?}"
    );
    for (label, id) in [("A", source_a), ("B", source_b), ("D", observer_d)] {
        assert_eq!(
            announced.iter().filter(|entry| **entry == id).count(),
            1,
            "{label} announced exactly once: {announced:?}"
        );
    }
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "announcement alone resolves nothing"
    );
    assert!(runner.state().deferred_triggers.is_empty());

    // (15) Clone at the post-order point for two normal-action branches.
    let post_order = runner.state().clone();

    // (15a) Resolve all three: 5 + 3 + 2, each exactly once.
    let mut resolve_all = GameRunner::from_state(post_order.clone());
    for _ in 0..3 {
        resolve_all
            .act(GameAction::PassPriority)
            .expect("P0 passes priority to resolve the next triggered ability");
        resolve_all
            .act(GameAction::PassPriority)
            .expect("P1 passes priority to resolve the next triggered ability");
    }
    assert!(
        resolve_all.state().stack.is_empty(),
        "all three triggered abilities resolve through the normal priority path"
    );
    assert_eq!(
        resolve_all.state().players[P0.0 as usize].life,
        life_before + 10,
        "the 5, 3 and 2 life effects each occur exactly once"
    );

    // (15b) Counter one identified reflexive through the normal stack path and
    // resolve the other two: the countered effect must not occur while the other
    // two occur exactly once.
    let mut countered = GameRunner::from_state(post_order);
    let top = countered
        .state()
        .stack
        .last()
        .expect("the ordered group left three entries on the stack");
    let countered_source = match top.kind {
        StackEntryKind::TriggeredAbility { source_id, .. } => source_id,
        ref other => panic!("unexpected top-of-stack entry {other:?}"),
    };
    let countered_gain = if countered_source == source_a {
        5
    } else if countered_source == source_b {
        3
    } else {
        2
    };
    let top_id = top.id;
    let counter_ability = ResolvedAbility::new(
        Effect::Counter {
            target: TargetFilter::StackAbility {
                controller: None,
                tag: None,
                kind: None,
            },
            source_rider: None,
            countered_spell_zone: None,
        },
        vec![TargetRef::Object(top_id)],
        observer_d,
        P0,
    );
    engine::game::effects::counter::resolve(
        countered.state_mut(),
        &counter_ability,
        &mut Vec::new(),
    )
    .expect("the identified entry is countered through the production counter resolver");
    assert_eq!(
        countered
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(
                entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if source_id == countered_source
            ))
            .count(),
        0,
        "the countered entry leaves the stack"
    );
    for _ in 0..2 {
        countered
            .act(GameAction::PassPriority)
            .expect("P0 passes priority to resolve a surviving triggered ability");
        countered
            .act(GameAction::PassPriority)
            .expect("P1 passes priority to resolve a surviving triggered ability");
    }
    assert!(countered.state().stack.is_empty());
    assert_eq!(
        countered.state().players[P0.0 as usize].life,
        life_before + 10 - countered_gain,
        "the countered reflexive's effect does not occur while the other two occur exactly once"
    );
}

/// CR 603.3b + CR 605.3a — the plan's **observer-axis sibling** of the hostile
/// direct-Priority row, and the positive-and-revert discriminator for the
/// ancestor-prefix finding.
///
/// It changes exactly one thing: the ordinary observer watches A's tap instead
/// of B's. A's tap lives in the SUSPENDED PARENT's retained prefix, not in the
/// paused child B's frame-local ledger, so the completed root can only collect
/// observer E if the parent-snapshot suffix augmentation actually preserved
/// A's first-action tap across the pause. Reverting that augmentation leaves no
/// collector holding A's tap: E never fires, the single ordering group has only
/// two members, and E's distinguishable +4 never occurs. A ledger-shape-only
/// unit test cannot close this — the shapes look identical until a real
/// observer has to match out of them.
#[test]
fn direct_priority_mana_root_orders_the_ancestor_prefix_observer_with_both_reflexives() {
    let HostileAbcdFixture {
        scenario,
        source_a,
        source_b,
        source_c,
        observer: observer_e,
        observer_gain,
    } = hostile_abcd_fixture(HostileObserverAxis::TapsA);
    let mut runner = scenario.build();

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().pending_cast.is_none());
    let life_before = runner.state().players[P0.0 as usize].life;

    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: source_a,
            ability_index: 0,
        })
        .expect("A's {2} Mana component pays through B and pauses on its exile replacement");
    assert!(
        matches!(paused.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "expected B's exile ReplacementChoice, got {:?}",
        paused.waiting_for
    );
    // The identical chronology reach guards: same nested cursor tree, same two
    // DISJOINT ledgers, same untapped C.
    assert_nested_hostile_pause(runner.state(), source_a, source_b);
    assert!(runner.state().objects[&source_a].tapped);
    assert!(runner.state().objects[&source_b].tapped);
    assert!(!runner.state().objects[&source_c].tapped);
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before);

    let json = serde_json::to_string(runner.state()).expect("paused cursor tree serializes");
    let restored: GameState = serde_json::from_str(&json).expect("paused cursor tree restores");
    let mut runner = GameRunner::from_state(restored);
    assert_nested_hostile_pause(runner.state(), source_a, source_b);
    assert!(!runner.state().objects[&source_c].tapped);

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the replacement choice resumes B, then A's later synchronous C");
    assert!(runner.state().objects[&source_c].tapped);
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before);

    // ONE empty-stack ordering group holding exactly B's reflexive, E's A-tap
    // observer, and A's reflexive.
    let WaitingFor::OrderTriggers {
        player: order_player,
        triggers: ref group,
    } = resumed.waiting_for
    else {
        panic!(
            "the completed root must expose one CR 603.3b ordering group, got {:?}",
            resumed.waiting_for
        );
    };
    assert_eq!(order_player, P0);
    assert!(
        runner.state().stack.is_empty(),
        "no member of the batch may be dispatched separately"
    );
    let members: Vec<ObjectId> = group.iter().map(|summary| summary.source_id).collect();
    assert_eq!(
        members.len(),
        3,
        "expected exactly B's reflexive, E's A-tap observer, and A's reflexive; got {members:?}"
    );
    for (label, id) in [("A", source_a), ("B", source_b), ("E", observer_e)] {
        assert_eq!(
            members.iter().filter(|member| **member == id).count(),
            1,
            "{label} must appear in the single ordering group exactly once: {members:?}"
        );
    }

    runner
        .act(GameAction::OrderTriggers {
            order: vec![0, 1, 2],
        })
        .expect("the three-member group orders through the real CR 603.3b action");
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
            .count(),
        3
    );
    for _ in 0..3 {
        runner.act(GameAction::PassPriority).expect("P0 passes");
        runner.act(GameAction::PassPriority).expect("P1 passes");
    }
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before + 5 + 3 + observer_gain,
        "E's distinct effect occurs exactly once together with the two reflexive effects"
    );
}

/// CR 601.2h + CR 603.3b — the plan's **masked cast-root control** for the same
/// A/B/C/D topology.
///
/// A completed mana frame is a cost-payment micro-frame inside somebody else's
/// action, so it may not release its own batch. With a real `PendingCast` and a
/// live `WaitingFor::ManaPayment` owner, the whole three-member batch must stay
/// in `state.deferred_triggers` — no ordering prompt, no triggered stack entry,
/// and specifically no observer-D entry — until the spell is actually announced.
/// Only the cast finalizer may expose the single ordering group, and it must
/// expose exactly those three above the already-announced spell.
///
/// The `deferred_triggers`-size assertion alone does not close the unified-seam
/// finding; the explicit "no D on the stack" assertion is the one that fails
/// under a separate fresh dispatch of the ordinary half.
#[test]
fn masked_cast_root_mana_batch_stays_queued_until_the_spell_is_announced() {
    let HostileAbcdFixture {
        mut scenario,
        source_a,
        source_b,
        source_c,
        observer: observer_d,
        observer_gain,
    } = hostile_abcd_fixture(HostileObserverAxis::TapsB);
    let spell = scenario
        .add_spell_to_hand(P0, "Masked Cast Root Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let card_id = runner.state().objects[&spell].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the masked owner announces a real pending cast in manual payment mode");
    assert!(
        matches!(cast.waiting_for, WaitingFor::ManaPayment { player: P0, .. }),
        "expected a live ManaPayment owner, got {:?}",
        cast.waiting_for
    );
    assert!(runner.state().pending_cast.is_some());

    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: source_a,
            ability_index: 0,
        })
        .expect("A is manually activated during the cast's mana payment");
    assert!(
        matches!(paused.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "expected the same nested B pause under the masked owner, got {:?}",
        paused.waiting_for
    );
    assert_nested_hostile_pause_cursor_tree(runner.state(), source_a, source_b);
    assert!(
        !runner.state().objects[&source_c].tapped,
        "C must be untapped at the pause under the masked owner too"
    );

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the replacement choice resumes B, then A's later synchronous C");
    assert!(runner.state().objects[&source_c].tapped);

    // The masked contract: the owner is still live, and the WHOLE batch is
    // queued rather than released.
    assert!(
        matches!(
            resumed.waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ),
        "the cast owner must retain the action, got {:?}",
        resumed.waiting_for
    );
    assert!(
        !matches!(resumed.waiting_for, WaitingFor::OrderTriggers { .. }),
        "a masked mana frame may not open CR 603.3b ordering"
    );
    assert!(
        !runner
            .state()
            .stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })),
        "no member of the batch — and specifically not observer D — may reach the stack \
         while the cast owner is live"
    );
    let queued: Vec<ObjectId> = runner
        .state()
        .deferred_triggers
        .iter()
        .map(|context| context.pending.source_id)
        .collect();
    assert_eq!(
        queued.len(),
        3,
        "one deferred queue holding exactly B's reflexive, D's observer, and A's reflexive: \
         {queued:?}"
    );
    for (label, id) in [("A", source_a), ("B", source_b), ("D", observer_d)] {
        assert_eq!(
            queued.iter().filter(|member| **member == id).count(),
            1,
            "{label} is queued exactly once: {queued:?}"
        );
    }
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before);

    // Complete the payment through the real action protocol. Only the cast
    // finalizer may release the group.
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "A's green mana is available to pay the spell's single green pip"
    );
    let finalized = runner.act(GameAction::PassPriority).expect(
        "the manual payment finalizes from the available green mana and announces the spell",
    );

    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::Spell { .. })),
        "the spell is announced exactly once before the batch is released"
    );
    let WaitingFor::OrderTriggers {
        triggers: ref group,
        ..
    } = finalized.waiting_for
    else {
        panic!(
            "the cast finalizer must expose the single ordering group, got {:?}",
            finalized.waiting_for
        );
    };
    let members: Vec<ObjectId> = group.iter().map(|summary| summary.source_id).collect();
    assert_eq!(members.len(), 3, "{members:?}");
    for (label, id) in [("A", source_a), ("B", source_b), ("D", observer_d)] {
        assert_eq!(
            members.iter().filter(|member| **member == id).count(),
            1,
            "{label} appears in the released group exactly once: {members:?}"
        );
    }
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
            .count(),
        0,
        "no trigger entry may be on the stack before the group is ordered"
    );

    runner
        .act(GameAction::OrderTriggers {
            order: vec![0, 1, 2],
        })
        .expect("the released group orders above the announced spell");
    let stack_kinds: Vec<bool> = runner
        .state()
        .stack
        .iter()
        .map(|entry| matches!(entry.kind, StackEntryKind::Spell { .. }))
        .collect();
    assert_eq!(stack_kinds.iter().filter(|is_spell| **is_spell).count(), 1);
    assert!(
        stack_kinds[0],
        "CR 603.3 places the newly ordered triggers ABOVE the spell that was already announced"
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before);
    for _ in 0..3 {
        runner.act(GameAction::PassPriority).expect("P0 passes");
        runner.act(GameAction::PassPriority).expect("P1 passes");
    }
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before + 5 + 3 + observer_gain,
        "each masked batch member resolves exactly once after the owner released it"
    );
}

/// CR 118.12 + CR 603.3b + CR 608.2 — the plan's **masked resolution-root
/// control** for the same A/B/C/D topology, and the sibling that session 7's
/// report recorded as the one remaining owed member of this family.
///
/// The masked cast-root control proves a live `PendingCast` masks the batch.
/// This row proves the *other* masking owner the plan names: a real resolution
/// that has stopped at `WaitingFor::UnlessPayment`. There is no `PendingCast`
/// here at all — the owner is a live `resolution_stack` frame — so the two
/// controls cannot share a bug: a completed mana frame may not release its own
/// batch under EITHER owner, and only the owner's own settlement may.
///
/// The `deferred_triggers`-size assertion alone does not close the unified-seam
/// finding; the explicit "no D on the stack" assertion is the one that fails
/// under a separate fresh dispatch of the ordinary half. The unpaid punishment
/// is -100 life, so a resolution that leaked past its unless-payment would be
/// impossible to confuse with the batch's own +1/+5/+3/+2.
///
/// FIXTURE NOTE, deliberately recorded rather than absorbed: the punisher
/// carries an "if you do" rider, so the paid branch runs an ability chain and
/// the owner reaches its own post-action settlement, which is what releases the
/// group. A rider-FREE unless-payment currently has no settlement convergence
/// at all — `finish_successful_unless_payment` runs the pipeline only when it
/// resolved a sub-ability, so a bare paid cost lands on `Priority` with the
/// three contexts still queued. That gap is exactly what the plan's settled-
/// Priority convergence wrapper (`run_post_action_pipeline_from_settled_priority`
/// plus the `engine_payment_choices` readiness hook) closes, and it is still
/// owed; this row deliberately does not encode the gap as a contract.
#[test]
fn masked_unless_payment_root_mana_batch_stays_queued_until_the_owner_settles() {
    let HostileAbcdFixture {
        mut scenario,
        source_a,
        source_b,
        source_c,
        observer: observer_d,
        observer_gain,
    } = hostile_abcd_fixture(HostileObserverAxis::TapsB);

    // "You lose 100 life unless you pay {1}. If you do, you gain 1 life." The
    // punishment is unmissable if the owner ever leaked past its payment, and
    // the "if you do" rider is what carries the resolution to its own
    // completion — this row's release boundary.
    let mut paid_rider = AbilityDefinition::new(
        AbilityKind::Database,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    );
    paid_rider.condition = Some(engine::types::ability::AbilityCondition::EffectOutcome {
        signal: engine::types::ability::EffectOutcomeSignal::OptionalEffectPerformed,
    });
    let mut punisher = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 100 },
            target: Some(TargetFilter::Controller),
        },
    )
    .sub_ability(paid_rider);
    punisher.unless_pay = Some(UnlessPayModifier {
        cost: AbilityCost::Mana {
            cost: ManaCost::generic(1),
        },
        payer: TargetFilter::Controller,
    });
    let spell = scenario
        .add_spell_to_hand(P0, "Masked Resolution Root Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .with_ability_definition(punisher)
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the free witness spell is announced");
    runner.act(GameAction::PassPriority).expect("P0 passes");
    let owner = runner
        .act(GameAction::PassPriority)
        .expect("P1 passes and the witness resolves into its unless-payment");
    assert!(
        matches!(
            owner.waiting_for,
            WaitingFor::UnlessPayment { player: P0, .. }
        ),
        "expected a live UnlessPayment resolution owner, got {:?}",
        owner.waiting_for
    );
    // Positive reach guard for the axis that distinguishes this control from the
    // cast-root one: the masking owner here is a paused resolution, not a cast.
    assert!(
        runner.state().pending_cast.is_none(),
        "no PendingCast may mask this batch — the owner is the resolution itself"
    );
    let WaitingFor::UnlessPayment {
        pending_effect: ref parked,
        ref cost,
        ..
    } = owner.waiting_for
    else {
        unreachable!()
    };
    assert_eq!(
        parked.source_id, spell,
        "the parked punisher is the witness spell's own resolution"
    );
    assert_eq!(
        cost,
        &AbilityCost::Mana {
            cost: ManaCost::generic(1)
        }
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before);

    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: source_a,
            ability_index: 0,
        })
        .expect("A is manually activated during the unless payment (CR 118.12)");
    assert!(
        matches!(paused.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "expected the same nested B pause under the resolution owner, got {:?}",
        paused.waiting_for
    );
    assert_nested_hostile_pause_cursor_tree(runner.state(), source_a, source_b);
    assert!(
        !runner.state().objects[&source_c].tapped,
        "C must be untapped at the pause under the resolution owner too"
    );

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the replacement choice resumes B, then A's later synchronous C");
    assert!(runner.state().objects[&source_c].tapped);

    // The masked contract: the resolution owner is still live, and the WHOLE
    // batch is queued rather than released.
    assert!(
        matches!(
            resumed.waiting_for,
            WaitingFor::UnlessPayment { player: P0, .. }
        ),
        "the resolution owner must retain the action, got {:?}",
        resumed.waiting_for
    );
    assert!(
        !matches!(resumed.waiting_for, WaitingFor::OrderTriggers { .. }),
        "a masked mana frame may not open CR 603.3b ordering"
    );
    assert!(
        !runner
            .state()
            .stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })),
        "no member of the batch — and specifically not observer D — may reach the stack \
         while the resolution owner is live"
    );
    let queued: Vec<ObjectId> = runner
        .state()
        .deferred_triggers
        .iter()
        .map(|context| context.pending.source_id)
        .collect();
    assert_eq!(
        queued.len(),
        3,
        "one deferred queue holding exactly B's reflexive, D's observer, and A's reflexive: \
         {queued:?}"
    );
    for (label, id) in [("A", source_a), ("B", source_b), ("D", observer_d)] {
        assert_eq!(
            queued.iter().filter(|member| **member == id).count(),
            1,
            "{label} is queued exactly once: {queued:?}"
        );
    }
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "A's green mana is available to pay the unless cost"
    );

    // Only the resolution owner's own settlement may release the group.
    let settled = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the unless cost is paid from A's green mana and the punisher is prevented");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        0,
        "the green pip really paid the unless cost"
    );
    let WaitingFor::OrderTriggers {
        triggers: ref group,
        ..
    } = settled.waiting_for
    else {
        panic!(
            "the settled resolution owner must expose the single ordering group, got {:?}",
            settled.waiting_for
        );
    };
    let members: Vec<ObjectId> = group.iter().map(|summary| summary.source_id).collect();
    assert_eq!(members.len(), 3, "{members:?}");
    for (label, id) in [("A", source_a), ("B", source_b), ("D", observer_d)] {
        assert_eq!(
            members.iter().filter(|member| **member == id).count(),
            1,
            "{label} appears in the released group exactly once: {members:?}"
        );
    }
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
            .count(),
        0,
        "no trigger entry may be on the stack before the group is ordered"
    );
    // The paid branch already ran to completion: the +1 "if you do" rider is the
    // owner's own last instruction and it happened BEFORE the release, while the
    // -100 punishment never did.
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before + 1);

    runner
        .act(GameAction::OrderTriggers {
            order: vec![0, 1, 2],
        })
        .expect("the released group orders once the resolution owner has settled");
    for _ in 0..3 {
        runner.act(GameAction::PassPriority).expect("P0 passes");
        runner.act(GameAction::PassPriority).expect("P1 passes");
    }
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before + 1 + 5 + 3 + observer_gain,
        "each masked batch member resolves exactly once, and the paid-for punisher never does"
    );
}

/// The plan's assertions (4)-(7) at the hostile pause, factored so the
/// in-memory and the restored-from-JSON branches assert byte-identical
/// properties. The two ledger assertions are the discriminating pair: they fail
/// in OPPOSITE directions for the two reverts this row exists to catch.
fn assert_nested_hostile_pause_cursor_tree(
    state: &GameState,
    source_a: ObjectId,
    source_b: ObjectId,
) {
    let Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor }) =
        state.pending_cost_move_resume.as_ref()
    else {
        panic!(
            "expected a nested ManaAbilityPayment owner, got {:?}",
            state.pending_cost_move_resume
        );
    };
    assert_eq!(pending.source_id, source_b, "the paused child is B");

    let parent = cursor
        .parent
        .as_ref()
        .expect("B's cursor must carry the nested A parent");
    assert_eq!(
        parent.lifecycle,
        ManaAbilityCostParentLifecycle::Suspended,
        "pausing B recursively suspends A"
    );
    assert_eq!(parent.pending.source_id, source_a, "the parent is A");
    assert!(
        parent.cursor.remaining.iter().any(|cost| matches!(
            cost,
            AbilityCost::Mana {
                cost: ManaCost::Cost { generic: 2, .. }
            }
        )),
        "A still owes its {{2}} Mana component, so a LATER synchronous child follows: {:?}",
        parent.cursor.remaining
    );

    let tap_of = |events: &[GameEvent], id: ObjectId| {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::PermanentTapped { object_id, .. } if *object_id == id
                )
            })
            .count()
    };
    // (6) B's FRAME-LOCAL ledger: exactly B's own tap, and no ancestor data.
    // Restoring the pre-fix clone-and-scan child ledger puts A's tap here too.
    assert_eq!(
        tap_of(&cursor.deferred_cost_events, source_b),
        1,
        "B's local ledger owns exactly its own tap: {:?}",
        cursor.deferred_cost_events
    );
    assert_eq!(
        tap_of(&cursor.deferred_cost_events, source_a),
        0,
        "a child may never scan its ancestor's events: {:?}",
        cursor.deferred_cost_events
    );
    // (7) A's PREPARED parent snapshot: exactly A's pre-child suffix. Reverting
    // the snapshot augmentation at the `resolve_mana_ability_excluding` call
    // site leaves this empty.
    assert_eq!(
        tap_of(&parent.cursor.deferred_cost_events, source_a),
        1,
        "the prepared synchronous parent captured A's pre-child suffix: {:?}",
        parent.cursor.deferred_cost_events
    );
    assert_eq!(
        tap_of(&parent.cursor.deferred_cost_events, source_b),
        0,
        "and never the child's own events: {:?}",
        parent.cursor.deferred_cost_events
    );
}

/// The direct-Priority root additionally owns the plan's "no competing owner"
/// half of assertion (4). The masked-root controls deliberately do NOT assert
/// it: a live `PendingCast` or resolution owner is exactly what they exist to
/// mask the batch behind.
fn assert_nested_hostile_pause(state: &GameState, source_a: ObjectId, source_b: ObjectId) {
    assert_nested_hostile_pause_cursor_tree(state, source_a, source_b);
    assert!(state.pending_cast.is_none());
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
    assert!(!matches!(
        state.waiting_for,
        WaitingFor::OrderTriggers { .. }
    ));
}

// ---------------------------------------------------------------------------
// Round-9 finding 2: the required NO-PAUSE matrix. One synchronous fixture, all
// three real roots, both colour halves — six action rows through the single
// typed completed-mana-frame seam, plus the `TapLandForMana` regression that
// exercises the one changed match arm none of the six reach.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum NoPauseColorAxis {
    /// N produces `{G}` outright: the activation completes in one action.
    Fixed,
    /// N produces one mana of any colour: the activation returns
    /// `WaitingFor::ChooseManaColor` first, and the choice action completes it.
    AnyOneColor,
}

struct NoPauseFixture {
    scenario: GameScenario,
    source_n: ObjectId,
    observer_o: ObjectId,
}

/// The plan's synchronous no-pause fixture: source N `{T}: Add {G}` with a
/// targetless true `WhenYouDo` rider gaining 5, and a source-filtered ordinary
/// `Taps` observer O of N gaining 2. No replacement, no deferred life cost, no
/// other pause is reachable, so N's durable cost ledger is provably empty and
/// every row exercises the EMPTY-LEDGER half of the completed-frame seam.
fn no_pause_mana_fixture(axis: NoPauseColorAxis) -> NoPauseFixture {
    let mut reflexive = AbilityDefinition::new(
        AbilityKind::Database,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 5 },
            player: TargetFilter::Controller,
        },
    );
    reflexive.condition = Some(engine::types::ability::AbilityCondition::WhenYouDo);

    let produced = match axis {
        NoPauseColorAxis::Fixed => ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        },
        NoPauseColorAxis::AnyOneColor => ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: vec![ManaColor::Green, ManaColor::Blue],
            contribution: ManaContribution::Base,
        },
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source_n = scenario
        .add_creature(P0, "N Synchronous Green Source", 1, 1)
        .as_artifact()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced,
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap)
            .sub_ability(reflexive),
        )
        .id();
    let observer_o = scenario
        .add_creature(P0, "O N-Tap Observer", 0, 0)
        .as_enchantment()
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::Taps)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                        player: TargetFilter::Controller,
                    },
                ))
                .valid_card(TargetFilter::SpecificObject { id: source_n })
                .trigger_zones(vec![Zone::Battlefield]),
        )
        .id();

    NoPauseFixture {
        scenario,
        source_n,
        observer_o,
    }
}

/// The plan's shared per-boundary assertions for the no-pause matrix: neither
/// member is separately pushed, the deferred queue holds exactly N's reflexive
/// and O once each, and no life has been gained yet.
fn assert_exactly_two_contexts_queued_and_unstacked(
    state: &GameState,
    source_n: ObjectId,
    observer_o: ObjectId,
    life_before: i32,
) {
    assert!(
        !state
            .stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })),
        "no member of the pair may be separately pushed before its owner releases it"
    );
    let queued: Vec<ObjectId> = state
        .deferred_triggers
        .iter()
        .map(|context| context.pending.source_id)
        .collect();
    assert_eq!(
        queued.len(),
        2,
        "exactly N's reflexive and O, once each: {queued:?}"
    );
    for (label, id) in [("N", source_n), ("O", observer_o)] {
        assert_eq!(
            queued.iter().filter(|member| **member == id).count(),
            1,
            "{label} is queued exactly once: {queued:?}"
        );
    }
    assert_eq!(
        state.players[P0.0 as usize].life, life_before,
        "no trigger may resolve before the release group is ordered"
    );
}

/// The plan's terminal assertions: exactly one group of exactly the two
/// contexts, nothing on the stack before ordering, then the two distinguishable
/// effects exactly once each for +7.
fn assert_single_group_of_two_then_resolve_for_seven(
    runner: &mut GameRunner,
    group_wait: &WaitingFor,
    source_n: ObjectId,
    observer_o: ObjectId,
    life_before: i32,
) {
    let WaitingFor::OrderTriggers {
        triggers: ref group,
        ..
    } = group_wait
    else {
        panic!("expected the single release group, got {group_wait:?}");
    };
    let members: Vec<ObjectId> = group.iter().map(|summary| summary.source_id).collect();
    assert_eq!(members.len(), 2, "{members:?}");
    for (label, id) in [("N", source_n), ("O", observer_o)] {
        assert_eq!(
            members.iter().filter(|member| **member == id).count(),
            1,
            "{label} appears in the released group exactly once: {members:?}"
        );
    }
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
            .count(),
        0,
        "no trigger entry may be on the stack before the group is ordered"
    );

    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("the released group orders");
    for _ in 0..3 {
        runner.act(GameAction::PassPriority).expect("P0 passes");
        runner.act(GameAction::PassPriority).expect("P1 passes");
    }
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before + 5 + 2,
        "the distinguishable 5- and 2-life effects each happen exactly once"
    );
}

/// Root 1 of the plan's no-pause matrix: a direct `ActivateAbility` from
/// `WaitingFor::Priority` with an EMPTY durable ledger.
///
/// Baseline had no seam here at all: a `Priority` resume with no deferred cost
/// events fell straight through `finish_mana_ability_cost_payment` to the
/// generic post-action scan, which dispatched observer O on its own while N's
/// synthetic reflexive was materialized by the mana frame. Routing the empty
/// ledger through `collect_completed_mana_frame_events` makes the two one batch.
/// Restoring the `has_deferred_cost_events` gate splits them and the
/// exactly-two group assertion fails.
#[test]
fn direct_priority_no_pause_root_releases_its_reflexive_and_observer_as_one_group() {
    let NoPauseFixture {
        scenario,
        source_n,
        observer_o,
    } = no_pause_mana_fixture(NoPauseColorAxis::Fixed);
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().pending_cast.is_none());

    let acted = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N is activated directly from Priority");

    // The durable ledger really was empty: nothing paused.
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "positive reach guard: the fixture is synchronous, so no cursor survives"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "N's mana is available immediately (CR 605.3b)"
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before);

    // The direct root exposes ONE empty-stack ordering group after the action.
    assert!(
        runner
            .state()
            .stack
            .iter()
            .all(|entry| !matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })),
        "CR 603.3b ordering happens before any entry is placed"
    );
    assert_single_group_of_two_then_resolve_for_seven(
        &mut runner,
        &acted.waiting_for,
        source_n,
        observer_o,
        life_before,
    );
}

/// Root 2 of the plan's no-pause matrix: manual activation during a real
/// spell's `WaitingFor::ManaPayment`. The owner retains the action and one
/// two-context queue until the spell is announced, and only the cast finalizer
/// exposes the group — above the announced spell.
#[test]
fn masked_cast_no_pause_root_queues_both_contexts_until_the_spell_is_announced() {
    let NoPauseFixture {
        mut scenario,
        source_n,
        observer_o,
    } = no_pause_mana_fixture(NoPauseColorAxis::Fixed);
    let spell = scenario
        .add_spell_to_hand(P0, "No-Pause Masked Cast Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let card_id = runner.state().objects[&spell].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the masked owner announces a real pending cast in manual payment mode");
    assert!(matches!(
        cast.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));

    let activated = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N is manually activated during the cast's mana payment");
    assert!(
        matches!(
            activated.waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ),
        "the cast owner must retain the action, got {:?}",
        activated.waiting_for
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "N's mana is available immediately, before any trigger resolves"
    );
    assert_exactly_two_contexts_queued_and_unstacked(
        runner.state(),
        source_n,
        observer_o,
        life_before,
    );

    let finalized = runner
        .act(GameAction::PassPriority)
        .expect("the manual payment finalizes and announces the spell");
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::Spell { .. })),
        "the spell is announced exactly once before the batch is released"
    );
    assert_single_group_of_two_then_resolve_for_seven(
        &mut runner,
        &finalized.waiting_for,
        source_n,
        observer_o,
        life_before,
    );
}

/// Root 3 of the plan's no-pause matrix: manual activation during a real
/// `UnlessPayment` resolution owner. Same fixture, same queue, and the group is
/// exposed only once that owner has settled.
#[test]
fn masked_resolution_no_pause_root_queues_both_contexts_until_the_owner_settles() {
    let NoPauseFixture {
        mut scenario,
        source_n,
        observer_o,
    } = no_pause_mana_fixture(NoPauseColorAxis::Fixed);
    let spell = scenario
        .add_spell_to_hand(P0, "No-Pause Masked Resolution Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .with_ability_definition(unless_pay_one_punisher())
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the free witness spell is announced");
    runner.act(GameAction::PassPriority).expect("P0 passes");
    let owner = runner
        .act(GameAction::PassPriority)
        .expect("P1 passes and the witness resolves into its unless-payment");
    assert!(
        matches!(
            owner.waiting_for,
            WaitingFor::UnlessPayment { player: P0, .. }
        ),
        "expected a live UnlessPayment resolution owner, got {:?}",
        owner.waiting_for
    );
    assert!(runner.state().pending_cast.is_none());

    let activated = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N is manually activated during the unless payment (CR 118.12)");
    assert!(
        matches!(
            activated.waiting_for,
            WaitingFor::UnlessPayment { player: P0, .. }
        ),
        "the resolution owner must retain the action, got {:?}",
        activated.waiting_for
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1
    );
    assert_exactly_two_contexts_queued_and_unstacked(
        runner.state(),
        source_n,
        observer_o,
        life_before,
    );

    let settled = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the unless cost is paid from N's green mana");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        0,
        "the green pip really paid the unless cost"
    );
    assert_single_group_of_two_then_resolve_for_seven(
        &mut runner,
        &settled.waiting_for,
        source_n,
        // The paid branch's +1 rider ran before the release, so the two batch
        // members are measured against the post-rider baseline.
        observer_o,
        life_before + 1,
    );
}

/// "You lose 100 life unless you pay {1}. If you do, you gain 1 life." — the
/// resolution-owner witness shared by the hostile masked control and the
/// no-pause resolution root. The rider is what carries the resolution to its
/// own completion, which is this family's release boundary; see
/// `masked_unless_payment_root_mana_batch_stays_queued_until_the_owner_settles`
/// for the recorded gap in the rider-free shape.
fn unless_pay_one_punisher() -> AbilityDefinition {
    let mut paid_rider = AbilityDefinition::new(
        AbilityKind::Database,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    );
    paid_rider.condition = Some(engine::types::ability::AbilityCondition::EffectOutcome {
        signal: engine::types::ability::EffectOutcomeSignal::OptionalEffectPerformed,
    });
    let mut punisher = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 100 },
            target: Some(TargetFilter::Controller),
        },
    )
    .sub_ability(paid_rider);
    punisher.unless_pay = Some(UnlessPayModifier {
        cost: AbilityCost::Mana {
            cost: ManaCost::generic(1),
        },
        payer: TargetFilter::Controller,
    });
    punisher
}

/// The plan's colour-half reach guard: the activation action returns
/// `ChooseManaColor` after collecting O but WITHOUT ordering or stacking it, and
/// the reflexive does not exist yet because no mana has been produced.
fn assert_color_prompt_holds_only_the_observer(
    state: &GameState,
    source_n: ObjectId,
    observer_o: ObjectId,
    life_before: i32,
) {
    assert!(
        !matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }),
        "the colour prompt may not be preceded by a CR 603.3b ordering pass"
    );
    assert!(
        !state
            .stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })),
        "O may not be separately pushed while the colour choice is open"
    );
    let queued: Vec<ObjectId> = state
        .deferred_triggers
        .iter()
        .map(|context| context.pending.source_id)
        .collect();
    assert_eq!(
        queued,
        vec![observer_o],
        "the already-paid cost range was collected into exactly O; N's reflexive \
         cannot exist yet because no mana has been produced"
    );
    assert!(
        !queued.contains(&source_n),
        "no reflexive before production: {queued:?}"
    );
    assert_eq!(state.players[P0.0 as usize].life, life_before);
}

/// Colour half of root 1: the direct `ActivateAbility` root whose `AnyOneColor`
/// production returns `ChooseManaColor` first. Both halves of the seam run —
/// the pre-prompt collection in `finish_mana_ability_cost_payment` and the
/// post-choice collection in `handle_choose_mana_color` — and the release group
/// is identical to the fixed-colour row's.
#[test]
fn direct_priority_color_choice_root_releases_the_same_two_context_group() {
    let NoPauseFixture {
        scenario,
        source_n,
        observer_o,
    } = no_pause_mana_fixture(NoPauseColorAxis::AnyOneColor);
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let prompted = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N's AnyOneColor production opens the colour choice");
    assert!(
        matches!(
            prompted.waiting_for,
            WaitingFor::ChooseManaColor { player: P0, .. }
        ),
        "expected ChooseManaColor, got {:?}",
        prompted.waiting_for
    );
    assert_color_prompt_holds_only_the_observer(runner.state(), source_n, observer_o, life_before);

    let chosen = runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("the choice action produces the chosen mana and materializes the reflexive");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1
    );
    assert_single_group_of_two_then_resolve_for_seven(
        &mut runner,
        &chosen.waiting_for,
        source_n,
        observer_o,
        life_before,
    );
}

/// Colour half of root 2: the same two halves under a live `PendingCast`. The
/// cast owner retains the action across BOTH halves and only the finalizer
/// exposes the group.
#[test]
fn masked_cast_color_choice_root_releases_the_same_two_context_group() {
    let NoPauseFixture {
        mut scenario,
        source_n,
        observer_o,
    } = no_pause_mana_fixture(NoPauseColorAxis::AnyOneColor);
    let spell = scenario
        .add_spell_to_hand(P0, "Colour-Half Masked Cast Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the masked owner announces a real pending cast");

    let prompted = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N is manually activated during the cast's mana payment");
    assert!(
        matches!(
            prompted.waiting_for,
            WaitingFor::ChooseManaColor { player: P0, .. }
        ),
        "expected ChooseManaColor, got {:?}",
        prompted.waiting_for
    );
    assert_color_prompt_holds_only_the_observer(runner.state(), source_n, observer_o, life_before);

    let chosen = runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("the choice action completes N under the still-live cast owner");
    assert!(
        matches!(
            chosen.waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ),
        "the cast owner must retain the action across the colour choice, got {:?}",
        chosen.waiting_for
    );
    assert_exactly_two_contexts_queued_and_unstacked(
        runner.state(),
        source_n,
        observer_o,
        life_before,
    );

    let finalized = runner
        .act(GameAction::PassPriority)
        .expect("the manual payment finalizes and announces the spell");
    assert!(runner
        .state()
        .stack
        .iter()
        .any(|entry| matches!(entry.kind, StackEntryKind::Spell { .. })));
    assert_single_group_of_two_then_resolve_for_seven(
        &mut runner,
        &finalized.waiting_for,
        source_n,
        observer_o,
        life_before,
    );
}

/// Colour half of root 3: the same two halves under a live `UnlessPayment`
/// resolution owner.
#[test]
fn masked_resolution_color_choice_root_releases_the_same_two_context_group() {
    let NoPauseFixture {
        mut scenario,
        source_n,
        observer_o,
    } = no_pause_mana_fixture(NoPauseColorAxis::AnyOneColor);
    let spell = scenario
        .add_spell_to_hand(P0, "Colour-Half Masked Resolution Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .with_ability_definition(unless_pay_one_punisher())
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the free witness spell is announced");
    runner.act(GameAction::PassPriority).expect("P0 passes");
    let owner = runner
        .act(GameAction::PassPriority)
        .expect("P1 passes and the witness resolves into its unless-payment");
    assert!(matches!(
        owner.waiting_for,
        WaitingFor::UnlessPayment { player: P0, .. }
    ));

    let prompted = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N is manually activated during the unless payment");
    assert!(
        matches!(
            prompted.waiting_for,
            WaitingFor::ChooseManaColor { player: P0, .. }
        ),
        "expected ChooseManaColor, got {:?}",
        prompted.waiting_for
    );
    assert_color_prompt_holds_only_the_observer(runner.state(), source_n, observer_o, life_before);

    let chosen = runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("the choice action completes N under the still-live resolution owner");
    assert!(
        matches!(
            chosen.waiting_for,
            WaitingFor::UnlessPayment { player: P0, .. }
        ),
        "the resolution owner must retain the action across the colour choice, got {:?}",
        chosen.waiting_for
    );
    assert_exactly_two_contexts_queued_and_unstacked(
        runner.state(),
        source_n,
        observer_o,
        life_before,
    );

    let settled = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the unless cost is paid from N's green mana");
    assert_single_group_of_two_then_resolve_for_seven(
        &mut runner,
        &settled.waiting_for,
        source_n,
        observer_o,
        life_before + 1,
    );
}

/// The plan's required `ManaPayment` + `TapLandForMana` action regression.
///
/// The six-row no-pause matrix's manual-cast `ActivateAbility` row does not
/// execute the ADJACENT changed match arm, so this row drives the real
/// engine-authored `GameAction::TapLandForMana` instead — never
/// `ActivateAbility`, never `handle_tap_land_for_mana`, never a production
/// helper directly.
///
/// Explicit revert discriminator: restoring that one arm's baseline immediate
/// `process_triggers(state, &mana_events)` dispatches or stages O separately
/// while L's reflexive stays under the cast guard, so the immediate exact-two
/// deferred queue, the empty trigger stack, and the later exact-two single group
/// cannot all hold. This row must fail under that one-arm revert even when the
/// adjacent `ManaPayment` `ActivateAbility` routing remains correct.
#[test]
fn manual_cast_tap_land_for_mana_defers_its_reflexive_and_observer_as_one_group() {
    let mut reflexive = AbilityDefinition::new(
        AbilityKind::Database,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 5 },
            player: TargetFilter::Controller,
        },
    );
    reflexive.condition = Some(engine::types::ability::AbilityCondition::WhenYouDo);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land_l = scenario
        .add_creature(P0, "L Reflexive Green Land", 1, 1)
        .as_land()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap)
            .sub_ability(reflexive),
        )
        .id();
    let observer_o = scenario
        .add_creature(P0, "O L-Tap Observer", 0, 0)
        .as_enchantment()
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::Taps)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                        player: TargetFilter::Controller,
                    },
                ))
                .valid_card(TargetFilter::SpecificObject { id: land_l })
                .trigger_zones(vec![Zone::Battlefield]),
        )
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Tap-Land Payment Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let card_id = runner.state().objects[&spell].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("a real one-green cast reaches manual mana payment");
    assert!(
        matches!(cast.waiting_for, WaitingFor::ManaPayment { player: P0, .. }),
        "expected the real ManaPayment owner, got {:?}",
        cast.waiting_for
    );

    // The selection must be ENGINE-AUTHORED, not hand-built.
    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&land_l)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the engine authors a ManaSourceSelection for L at the payment prompt");
    assert_eq!(selection.source.object_id, land_l);
    assert_eq!(
        selection.ability_index,
        Some(0),
        "the selection names L's own {{T}}: Add {{G}} ability"
    );

    let tapped = runner
        .act(GameAction::TapLandForMana { selection })
        .expect("the engine-authored land tap is submitted as its own action");

    assert!(runner.state().objects[&land_l].tapped, "L is tapped");
    assert_eq!(
        tapped
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::TappedForMana { source_id, .. } if *source_id == land_l
            ))
            .count(),
        1,
        "exactly one source-identifiable TappedForMana(L): {:?}",
        tapped.events
    );
    assert_eq!(
        tapped
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::ManaAdded { .. }))
            .count(),
        1,
        "exactly one base ManaAdded occurrence: {:?}",
        tapped.events
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "L's green mana is present and usable immediately"
    );
    assert!(
        runner.state().pending_cast.is_some(),
        "the pending cast is still live"
    );
    assert!(
        matches!(
            tapped.waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ),
        "the cast owner retains the action, got {:?}",
        tapped.waiting_for
    );
    assert!(
        !matches!(tapped.waiting_for, WaitingFor::OrderTriggers { .. }),
        "a masked land tap may not open CR 603.3b ordering"
    );
    assert_exactly_two_contexts_queued_and_unstacked(
        runner.state(),
        land_l,
        observer_o,
        life_before,
    );
    assert!(
        runner.state().deferred_triggers.iter().all(|context| {
            !engine::game::mana_abilities::is_triggered_mana_ability(
                &context.pending.ability,
                context.pending.trigger_event.as_ref(),
            )
        }),
        "neither queued context is an accepted triggered-mana context"
    );

    let finalized = runner
        .act(GameAction::PassPriority)
        .expect("the manual payment spends L's green pip and announces the spell");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        0,
        "the green pip is consumed by the real cast"
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(entry.kind, StackEntryKind::Spell { .. }))
            .count(),
        1,
        "the spell is announced exactly once"
    );
    assert_single_group_of_two_then_resolve_for_seven(
        &mut runner,
        &finalized.waiting_for,
        land_l,
        observer_o,
        life_before,
    );
}

// ---------------------------------------------------------------------------
// `ManaAdded` immediacy: the plan's "targetless nonmodal `ManaAdded`" classifier
// row, driven as real actions over the no-pause fixture. A third permanent M
// observes N's `ManaAdded` with a targetless nonmodal mana body, so the complete
// classifier accepts it and the immediate backend resolves it stacklessly inside
// the completed mana frame — its bonus mana is in the pool before the frame's
// owner is resumed, and it never becomes a queue or stack member.
// ---------------------------------------------------------------------------

/// Attach permanent M to the no-pause fixture: a `TriggerMode::ManaAdded`
/// observer whose executed body is `body`. `OncePerTurn` is mandatory — M's own
/// mana body emits a further `ManaAdded`, and CR 603.2h is what stops the
/// fixed point from re-triggering M off its own production.
fn add_mana_added_observer(scenario: &mut GameScenario, label: &str, body: Effect) -> ObjectId {
    scenario
        .add_creature(P0, label, 0, 0)
        .as_enchantment()
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::ManaAdded)
                .execute(AbilityDefinition::new(AbilityKind::Database, body))
                .constraint(TriggerConstraint::OncePerTurn)
                .trigger_zones(vec![Zone::Battlefield]),
        )
        .id()
}

/// M's accepted body: one colorless mana, targetless and nonmodal, so
/// `build_target_slots` is empty and `is_triggered_mana_ability` holds.
fn mana_added_bonus_mana_body() -> Effect {
    Effect::Mana {
        produced: ManaProduction::Colorless {
            count: QuantityExpr::Fixed { value: 1 },
        },
        restrictions: vec![],
        grants: vec![],
        expiry: None,
        target: None,
    }
}

fn count_mana_added_from(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(
            |event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source),
        )
        .count()
}

/// How many pips in P0's pool were produced by `source`.
///
/// Provenance rather than the raw event vector is the right witness for an
/// accepted occurrence: `collect_mana_action_trigger_batch` resolves the
/// accepted body into its own frame-local dispatch buffer, exactly as baseline
/// `process_triggers` does, so an inline body's own `ManaAdded` deliberately
/// never receives a live occurrence identity in the reducer's public vector.
/// The produced pip and its `source_id` are the durable record.
fn pips_produced_by(state: &GameState, source: ObjectId) -> usize {
    state.players[P0.0 as usize]
        .mana_pool
        .mana
        .iter()
        .filter(|unit| unit.source_id == source)
        .count()
}

/// The plan's `ManaAdded` immediacy row at the direct-`Priority` root.
///
/// M is accepted by the complete classifier, so `TriggerPlacement::TriggeredManaImmediate`
/// resolves it inside N's completed mana frame: the colorless pip exists in the
/// same action, M is never appended to `state.deferred_triggers`, no
/// `TriggeredAbility` entry is ever pushed for it, and the single release group
/// is still exactly N's reflexive plus O.
#[test]
fn direct_priority_mana_added_bonus_resolves_inline_without_queue_or_stack() {
    let NoPauseFixture {
        mut scenario,
        source_n,
        observer_o,
    } = no_pause_mana_fixture(NoPauseColorAxis::Fixed);
    let bonus_m = add_mana_added_observer(
        &mut scenario,
        "M Mana-Added Bonus",
        mana_added_bonus_mana_body(),
    );
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let acted = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N is activated directly from Priority");

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "N's own base mana (CR 605.3b)"
    );
    // The immediacy claim: M's accepted body already ran, stacklessly, inside
    // the same completed mana frame.
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        1,
        "M's accepted triggered mana is spendable in the SAME action (CR 605.4a)"
    );
    assert_eq!(
        pips_produced_by(runner.state(), bonus_m),
        1,
        "M produces exactly once — CR 603.2h stops it observing its own ManaAdded"
    );
    assert_eq!(
        pips_produced_by(runner.state(), source_n),
        1,
        "and N's base production happens exactly once"
    );
    assert_eq!(
        count_mana_added_from(&acted.events, source_n),
        1,
        "N's own base ManaAdded is a live action event exactly once"
    );
    assert!(
        !runner
            .state()
            .deferred_triggers
            .iter()
            .any(|context| context.pending.source_id == bonus_m),
        "an accepted triggered-mana context is never appended to the ordinary queue"
    );
    if let WaitingFor::OrderTriggers {
        triggers: ref group,
        ..
    } = acted.waiting_for
    {
        assert!(
            !group.iter().any(|summary| summary.source_id == bonus_m),
            "and it is never a member of the released ordinary group"
        );
    }
    assert_single_group_of_two_then_resolve_for_seven(
        &mut runner,
        &acted.waiting_for,
        source_n,
        observer_o,
        life_before,
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. }))
            .count(),
        0,
        "M never occupied the stack at any point"
    );
}

/// The pure-axis positive reach guard for the row above: the ONLY difference is
/// M's executed body. A `GainLife` body makes `is_triggered_mana_ability` false,
/// so the same firing `ManaAdded` event, the same matcher and the same
/// `OncePerTurn` constraint now produce an ORDINARY deferred context — proving
/// the fixture really does couple M to N's production, and that immediacy is a
/// property of the accepted mana body rather than of the fixture's topology.
#[test]
fn direct_priority_mana_added_nonmana_body_is_deferred_as_an_ordinary_trigger() {
    let NoPauseFixture {
        mut scenario,
        source_n,
        observer_o,
    } = no_pause_mana_fixture(NoPauseColorAxis::Fixed);
    let bonus_m = add_mana_added_observer(
        &mut scenario,
        "M Mana-Added Life",
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
    );
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let acted = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N is activated directly from Priority");

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        0,
        "a nonmana body produces no mana at all"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "and nothing resolves before the release group is ordered"
    );

    let WaitingFor::OrderTriggers {
        triggers: ref group,
        ..
    } = acted.waiting_for
    else {
        panic!("expected one release group, got {:?}", acted.waiting_for);
    };
    let members: Vec<ObjectId> = group.iter().map(|summary| summary.source_id).collect();
    assert_eq!(
        members.len(),
        3,
        "the rejected M joins N's reflexive and O in the SAME ordinary group: {members:?}"
    );
    for (label, id) in [("N", source_n), ("O", observer_o), ("M", bonus_m)] {
        assert_eq!(
            members.iter().filter(|member| **member == id).count(),
            1,
            "{label} appears exactly once: {members:?}"
        );
    }

    runner
        .act(GameAction::OrderTriggers {
            order: vec![0, 1, 2],
        })
        .expect("the released group orders");
    for _ in 0..4 {
        runner.act(GameAction::PassPriority).expect("P0 passes");
        runner.act(GameAction::PassPriority).expect("P1 passes");
    }
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before + 5 + 2 + 3,
        "all three distinguishable effects happen exactly once each"
    );
}

/// The immediacy payoff at a masked root: M's bonus colorless pip must be
/// spendable by the very cast whose `WaitingFor::ManaPayment` masks the frame.
/// The witness spell costs `{1}{G}`, which is unpayable unless M resolved
/// stacklessly inside N's completed frame — reverting `settles_completed_frame`
/// to baseline's `is_ultimate_root && has_deferred_cost_events` leaves the
/// empty-ledger `ManaPayment` shape with no collection at all, so M never fires
/// and the generic pip cannot be paid.
#[test]
fn masked_cast_mana_added_bonus_is_spendable_before_the_spell_is_announced() {
    let NoPauseFixture {
        mut scenario,
        source_n,
        observer_o,
    } = no_pause_mana_fixture(NoPauseColorAxis::Fixed);
    let bonus_m = add_mana_added_observer(
        &mut scenario,
        "M Mana-Added Bonus",
        mana_added_bonus_mana_body(),
    );
    let spell = scenario
        .add_spell_to_hand(P0, "Mana-Added Immediacy Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let card_id = runner.state().objects[&spell].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the masked owner announces a real pending cast in manual payment mode");
    assert!(matches!(
        cast.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));

    let activated = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N is manually activated during the cast's mana payment");
    assert!(
        matches!(
            activated.waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ),
        "the cast owner must retain the action, got {:?}",
        activated.waiting_for
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        1,
        "M's accepted bonus is in the pool BEFORE the payment owner resumes"
    );
    assert_eq!(
        pips_produced_by(runner.state(), bonus_m),
        1,
        "the colorless pip really is M's, not a second pip of N's"
    );
    assert!(
        !runner
            .state()
            .deferred_triggers
            .iter()
            .any(|context| context.pending.source_id == bonus_m),
        "an accepted triggered-mana context is never appended to the ordinary queue"
    );
    assert_exactly_two_contexts_queued_and_unstacked(
        runner.state(),
        source_n,
        observer_o,
        life_before,
    );

    let finalized = runner
        .act(GameAction::PassPriority)
        .expect("the manual payment spends BOTH pips and announces the spell");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        0
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        0,
        "the generic pip is paid by M's bonus mana"
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(entry.kind, StackEntryKind::Spell { .. }))
            .count(),
        1,
        "the spell is announced exactly once"
    );
    assert_single_group_of_two_then_resolve_for_seven(
        &mut runner,
        &finalized.waiting_for,
        source_n,
        observer_o,
        life_before,
    );
}

// ---------------------------------------------------------------------------
// Delayed-trigger families: the plan's `WhenNextEvent` one-shot control and the
// duration-bearing `WheneverEvent` persistence control. Both prove the mana
// frame's COMBINED collector materializes normal and delayed contexts together
// in one APNAP batch — `collect_pending_and_delayed_triggers_for_batch` — rather
// than letting the generic delayed pass discover them separately after the
// frame has already claimed its live occurrences.
// ---------------------------------------------------------------------------

/// A free witness spell that installs one real delayed trigger through
/// `Effect::CreateDelayedTrigger`, and the second `{T}: Add {G}` source used to
/// prove one-shot removal versus duration-bearing persistence.
struct DelayedFrameFixture {
    scenario: GameScenario,
    source_n: ObjectId,
    observer_o: ObjectId,
    installer: ObjectId,
    source_n2: ObjectId,
}

/// The embedded matcher both delayed rows install. `valid_card` is mandatory:
/// without it `taps_for_mana_card_matches` requires the tapping permanent to BE
/// the delayed trigger's own source, which an installer in the graveyard never
/// is. `Any` (rather than a specific id) is what makes the one-shot row's second
/// tap a non-vacuous probe — N2 would match, and only removal stops it.
fn delayed_taps_for_mana_matcher() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::TapsForMana).valid_card(TargetFilter::Any)
}

fn delayed_frame_fixture(condition: DelayedTriggerCondition) -> DelayedFrameFixture {
    let NoPauseFixture {
        mut scenario,
        source_n,
        observer_o,
    } = no_pause_mana_fixture(NoPauseColorAxis::Fixed);
    let installer = scenario
        .add_spell_to_hand(P0, "Delayed Trigger Installer", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .with_ability_definition(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CreateDelayedTrigger {
                condition,
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 4 },
                        player: TargetFilter::Controller,
                    },
                )),
                uses_tracked_set: false,
            },
        ))
        .id();
    // A second, otherwise identical mana source with NO rider and NO observer,
    // so a later tap is a clean probe for whether the delayed source is still
    // installed.
    let source_n2 = scenario
        .add_creature(P0, "N2 Plain Green Source", 1, 1)
        .as_artifact()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id();

    DelayedFrameFixture {
        scenario,
        source_n,
        observer_o,
        installer,
        source_n2,
    }
}

/// Cast and resolve the free installer, then assert exactly one delayed trigger
/// is installed and return the caller's life total at that point.
fn resolve_delayed_installer(runner: &mut GameRunner, installer: ObjectId) -> i32 {
    let card_id = runner.state().objects[&installer].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: installer,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the free installer is announced");
    runner.act(GameAction::PassPriority).expect("P0 passes");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes and the installer resolves");
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "exactly one real delayed trigger is installed by the resolved effect"
    );
    runner.state().players[P0.0 as usize].life
}

/// Order and resolve a released group of exactly `expected` members, then assert
/// the total life delta.
fn resolve_group_of(
    runner: &mut GameRunner,
    group_wait: &WaitingFor,
    expected: &[ObjectId],
    life_before: i32,
    total_gain: i32,
) {
    let WaitingFor::OrderTriggers {
        triggers: ref group,
        ..
    } = group_wait
    else {
        panic!("expected one release group, got {group_wait:?}");
    };
    let members: Vec<ObjectId> = group.iter().map(|summary| summary.source_id).collect();
    assert_eq!(
        members.len(),
        expected.len(),
        "the frame releases exactly one combined APNAP batch: {members:?}"
    );
    for id in expected {
        assert_eq!(
            members.iter().filter(|member| *member == id).count(),
            1,
            "{id:?} is a member exactly once: {members:?}"
        );
    }
    runner
        .act(GameAction::OrderTriggers {
            order: (0..members.len()).collect(),
        })
        .expect("the released group orders");
    for _ in 0..(members.len() + 1) {
        runner.act(GameAction::PassPriority).expect("P0 passes");
        runner.act(GameAction::PassPriority).expect("P1 passes");
    }
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before + total_gain,
        "each distinguishable effect happens exactly once"
    );
}

/// The plan's synchronous delayed one-shot `TappedForMana` row.
///
/// A real `Effect::CreateDelayedTrigger` carrying `WhenNextEvent` with an
/// embedded `TriggerMode::TapsForMana` is installed by a resolved spell. N's
/// fixed-colour activation then emits the base tap plus one source-identifiable
/// `TappedForMana`, and the completed mana frame's COMBINED collector must
/// return the delayed context in the SAME APNAP batch as N's reflexive and the
/// ordinary observer O — one group of three, each effect once, for +11.
///
/// The one-shot half is measured twice: the instance is removed from
/// `state.delayed_triggers` exactly once, and a second identical tap by N2
/// afterwards produces no further firing at all.
#[test]
fn synchronous_delayed_one_shot_taps_for_mana_joins_the_frame_batch_once() {
    let DelayedFrameFixture {
        scenario,
        source_n,
        observer_o,
        installer,
        source_n2,
    } = delayed_frame_fixture(DelayedTriggerCondition::WhenNextEvent {
        trigger: Box::new(delayed_taps_for_mana_matcher()),
        or_trigger: None,
        lifetime: DelayedTriggerLifetime::default(),
    });
    let mut runner = scenario.build();
    let life_before = resolve_delayed_installer(&mut runner, installer);

    // Positive reach guard on the INSTALLED shape, before anything fires.
    let installed = &runner.state().delayed_triggers[0];
    assert!(
        matches!(
            installed.condition,
            DelayedTriggerCondition::WhenNextEvent { ref trigger, .. }
                if trigger.mode == TriggerMode::TapsForMana
        ),
        "the installed condition is the real embedded TapsForMana one-shot: {:?}",
        installed.condition
    );
    assert!(installed.one_shot, "CR 603.7: it is a one-shot");
    assert_eq!(
        installed.source_id, installer,
        "install origin is preserved"
    );

    let acted = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N is activated directly from Priority");
    assert_eq!(
        acted
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::TappedForMana { source_id, .. } if *source_id == source_n
            ))
            .count(),
        1,
        "exactly one source-identifiable TappedForMana reach event"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the matched one-shot instance is removed exactly once: {:?}",
        runner.state().delayed_triggers
    );
    resolve_group_of(
        &mut runner,
        &acted.waiting_for,
        &[source_n, observer_o, installer],
        life_before,
        5 + 2 + 4,
    );

    // The generic delayed pass creates none: a second identical tap after the
    // one-shot was consumed produces no further delayed firing.
    let life_after_group = runner.state().players[P0.0 as usize].life;
    let second = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n2,
            ability_index: 0,
        })
        .expect("N2 taps for mana with no delayed source installed");
    assert!(
        !matches!(second.waiting_for, WaitingFor::OrderTriggers { .. }),
        "no trigger at all may be released by the second tap, got {:?}",
        second.waiting_for
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_after_group,
        "the consumed one-shot cannot fire a second time"
    );
}

/// The plan's duration-bearing `WheneverEvent` persistence row, deliberately
/// separate from the one-shot control above.
///
/// The same combined collector must place the delayed context in the frame's one
/// APNAP batch, but the duration-bearing source stays INSTALLED afterwards, and
/// a later matching event proves persistence by firing exactly one more time.
#[test]
fn duration_bearing_delayed_whenever_event_stays_installed_and_fires_again() {
    let DelayedFrameFixture {
        scenario,
        source_n,
        observer_o,
        installer,
        source_n2,
    } = delayed_frame_fixture(DelayedTriggerCondition::WheneverEvent {
        trigger: Box::new(delayed_taps_for_mana_matcher()),
        expiry: WheneverEventExpiry::default(),
    });
    let mut runner = scenario.build();
    let life_before = resolve_delayed_installer(&mut runner, installer);
    assert!(
        !runner.state().delayed_triggers[0].one_shot,
        "CR 603.7b: a duration-bearing WheneverEvent is not a one-shot"
    );

    let acted = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N is activated directly from Priority");
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the duration-bearing source remains installed while its context fires"
    );
    resolve_group_of(
        &mut runner,
        &acted.waiting_for,
        &[source_n, observer_o, installer],
        life_before,
        5 + 2 + 4,
    );

    // Persistence: a later matching event fires it exactly one additional time.
    // N2 carries no rider and no observer, so the delayed context is the batch's
    // only member and CR 603.3b needs no ordering prompt for it.
    let life_after_group = runner.state().players[P0.0 as usize].life;
    let second = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n2,
            ability_index: 0,
        })
        .expect("N2 taps for mana while the duration-bearing source is still installed");
    assert!(
        !matches!(second.waiting_for, WaitingFor::OrderTriggers { .. }),
        "a one-member batch needs no ordering prompt, got {:?}",
        second.waiting_for
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(
                |entry| matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. })
                    && entry.controller == P0
            )
            .count(),
        1,
        "the persistent delayed source fired exactly one additional time"
    );
    runner.act(GameAction::PassPriority).expect("P0 passes");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes and the second firing resolves");
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_after_group + 4,
        "and its distinguishable effect happens exactly once more"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "and it is still installed after firing again"
    );
}

fn two_source_assist_replacement_witness(
    mana_cost: ManaCost,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    [engine::types::identifiers::ObjectId; 2],
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let helper_sources = [
        scenario
            .add_creature(P1, "First Two-Source Assist Mana Witness", 1, 1)
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Blue],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::Exile {
                            count: 1,
                            zone: None,
                            filter: Some(TargetFilter::SelfRef),
                        },
                    ],
                }),
            )
            .id(),
        scenario
            .add_creature(P1, "Second Two-Source Assist Mana Witness", 1, 1)
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Blue],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            )
            .id(),
    ];
    for name in [
        "First Two-Source Assist Redirect",
        "Second Two-Source Assist Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Two-Source Assist Payment Witness", true)
        .with_mana_cost(mana_cost)
        .with_keyword(Keyword::Assist)
        .id();
    (scenario.build(), spell, helper_sources)
}

#[test]
fn committed_assist_retries_remaining_sources_after_serialized_ordinary_pause() {
    let (mut runner, spell, helper_sources) =
        two_source_assist_replacement_witness(ManaCost::generic(2));
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the Assist spell reaches its helper offer");
    runner
        .act(GameAction::ChooseAssistPlayer { player: Some(P1) })
        .expect("choose the helper");
    runner
        .act(GameAction::CommitAssistPayment { generic: 2 })
        .expect("commit the two generic helper contribution");
    runner
        .act(GameAction::PassPriority)
        .expect("the first helper source pauses on its replacement choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let json = serde_json::to_string(runner.state())
        .expect("the ordinary two-source Assist pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the ordinary two-source Assist pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the first helper source completes without losing the remaining plan");
    assert_eq!(
        runner.state().objects[&helper_sources[0]].zone,
        Zone::Graveyard
    );
    assert_eq!(runner.state().players[P1.0 as usize].mana_pool.total(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));

    runner
        .act(GameAction::PassPriority)
        .expect("PaymentStarted must tap the remaining helper source before spending");
    assert_eq!(
        runner.state().objects[&helper_sources[1]].zone,
        Zone::Battlefield
    );
    assert!(runner.state().objects[&helper_sources[1]].tapped);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
}

#[test]
fn committed_assist_retries_remaining_sources_after_serialized_phyrexian_pause() {
    let (mut runner, spell, helper_sources) =
        two_source_assist_replacement_witness(ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianGreen],
            generic: 2,
        });
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the Assist Phyrexian spell reaches its helper offer");
    runner
        .act(GameAction::ChooseAssistPlayer { player: Some(P1) })
        .expect("choose the helper");
    runner
        .act(GameAction::CommitAssistPayment { generic: 2 })
        .expect("commit the two generic helper contribution");
    runner
        .act(GameAction::PassPriority)
        .expect("the caster chooses the Phyrexian shard before helper payment");
    runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![engine::types::game_state::ShardChoice::PayLife],
        })
        .expect("the first helper source pauses after the submitted Phyrexian choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let json = serde_json::to_string(runner.state())
        .expect("the Phyrexian two-source Assist pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the Phyrexian two-source Assist pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the exact Phyrexian root retries the remaining helper source plan");
    assert_eq!(
        runner.state().objects[&helper_sources[0]].zone,
        Zone::Graveyard
    );
    assert_eq!(
        runner.state().objects[&helper_sources[1]].zone,
        Zone::Battlefield
    );
    assert!(runner.state().objects[&helper_sources[1]].tapped);
    assert_eq!(runner.state().players[P0.0 as usize].life, 18);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
}

#[test]
fn committed_assist_phyrexian_choice_serializes_helper_cost_pause_and_charges_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let helper_source = scenario
        .add_creature(P1, "Assist Helper Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in [
        "First Assist Helper Redirect",
        "Second Assist Helper Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Assist Phyrexian Costed Source Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianGreen],
            generic: 1,
        })
        .with_keyword(Keyword::Assist)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let assist = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("Assist spell reaches the helper choice");
    assert!(matches!(
        assist.waiting_for,
        WaitingFor::AssistChoosePlayer { player: P0, .. }
    ));
    runner
        .act(GameAction::ChooseAssistPlayer { player: Some(P1) })
        .expect("choose the helper");
    runner
        .act(GameAction::CommitAssistPayment { generic: 1 })
        .expect("commit one generic from the helper");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    let phyrexian = runner
        .act(GameAction::PassPriority)
        .expect("the caster must choose the Phyrexian payment");
    assert!(matches!(
        phyrexian.waiting_for,
        WaitingFor::PhyrexianPayment { player: P0, .. }
    ));
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .map(|pending| pending.assist_state),
        Some(engine::types::game_state::AssistState::Committed {
            helper: P1,
            generic: 1
        })
    ));

    let paused = runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![engine::types::game_state::ShardChoice::PayLife],
        })
        .expect("submitted Phyrexian choice starts the committed helper payment");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            pending.resume,
            ManaAbilityResume::PhyrexianCastPayment {
                caster: P0,
                ref choices,
            } if choices == &vec![engine::types::game_state::ShardChoice::PayLife]
        )
    ));
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .map(|pending| pending.assist_state),
        Some(engine::types::game_state::AssistState::PaymentStarted {
            helper: P1,
            generic: 1
        })
    ));
    assert_eq!(
        runner.state().players[P1.0 as usize].mana_pool.total(),
        0,
        "a helper pause cannot spend mana before its source resolves"
    );

    let json = serde_json::to_string(runner.state())
        .expect("the committed Assist + submitted Phyrexian root serializes");
    let restored: GameState = serde_json::from_str(&json)
        .expect("the committed Assist + submitted Phyrexian root deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("helper source replacement retries the exact submitted choices");

    assert_eq!(runner.state().objects[&helper_source].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        18,
        "the submitted PayLife choice is retained and paid exactly once"
    );
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == helper_source))
            .count(),
        1,
        "the helper source's paid tap prefix is never replayed"
    );
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == helper_source))
            .count(),
        1,
        "the helper produces and spends its committed mana once"
    );
}

#[test]
fn caster_phyrexian_finalization_serializes_costed_source_pause_and_retries_choices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Caster Phyrexian Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in [
        "First Caster Phyrexian Redirect",
        "Second Caster Phyrexian Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Caster Phyrexian Costed Source Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianGreen],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the caster reaches the mana-payment window");
    assert!(matches!(
        cast.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    // The regular announcement above creates the real stack entry and
    // `PendingCast`. Enter the exact LifeOnly-shard finalization prompt here
    // so the witness exercises submitted-choice retry without relying on a
    // source-selection heuristic to reach the prompt first.
    runner.state_mut().waiting_for = WaitingFor::PhyrexianPayment {
        player: P0,
        spell_object: spell,
        shards: vec![engine::types::game_state::PhyrexianShard {
            shard_index: 0,
            color: ManaColor::Green,
            options: engine::types::game_state::ShardOptions::LifeOnly,
        }],
    };

    let paused = runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![engine::types::game_state::ShardChoice::PayLife],
        })
        .expect("caster's generic source pauses after the submitted Phyrexian choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            pending.resume,
            ManaAbilityResume::PhyrexianCastPayment {
                caster: P0,
                ref choices,
            } if choices == &vec![engine::types::game_state::ShardChoice::PayLife]
        )
    ));
    assert!(runner.state().pending_cast.is_some());

    let json = serde_json::to_string(runner.state())
        .expect("caster Phyrexian costed-source pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("caster Phyrexian costed-source pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the typed caster Phyrexian root retries its submitted choice");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P0.0 as usize].life, 18);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the caster source's cost is paid once across the replacement pause"
    );
}

#[test]
fn committed_assist_mana_payment_serializes_helper_redirect_and_charges_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let helper_source = scenario
        .add_creature(P1, "Assist Ordinary Helper Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in [
        "First Ordinary Assist Helper Redirect",
        "Second Ordinary Assist Helper Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Assist Ordinary Costed Source Witness", true)
        .with_mana_cost(ManaCost::generic(1))
        .with_keyword(Keyword::Assist)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let offered = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the Assist spell reaches its helper offer");
    assert!(matches!(
        offered.waiting_for,
        WaitingFor::AssistChoosePlayer { player: P0, .. }
    ));
    runner
        .act(GameAction::ChooseAssistPlayer { player: Some(P1) })
        .expect("choose the assisting player");
    runner
        .act(GameAction::CommitAssistPayment { generic: 1 })
        .expect("commit the helper's generic contribution");

    let paused = runner
        .act(GameAction::PassPriority)
        .expect("the helper's cost move pauses on its Moved replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            pending.resume,
            ManaAbilityResume::ManaPayment {
                outer_player: Some(P0),
                convoke_mode: None,
            }
        )
    ));
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .map(|pending| pending.assist_state),
        Some(engine::types::game_state::AssistState::PaymentStarted {
            helper: P1,
            generic: 1,
        })
    ));

    let json =
        serde_json::to_string(runner.state()).expect("the ordinary Assist helper pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the ordinary Assist helper pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the helper's typed outer payment root resumes after redirect");

    assert_eq!(runner.state().objects[&helper_source].zone, Zone::Graveyard);
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .map(|pending| pending.assist_state),
        Some(engine::types::game_state::AssistState::PaymentStarted {
            helper: P1,
            generic: 1,
        })
    ));
    assert_eq!(
        runner.state().players[P1.0 as usize].mana_pool.total(),
        1,
        "the resumed helper produces its committed mana before the outer spend"
    );

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the original Assist payment finishes the cast");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
    assert_eq!(runner.state().players[P1.0 as usize].mana_pool.total(), 0);
    assert!(!matches!(
        completed.waiting_for,
        WaitingFor::AssistChoosePlayer { .. } | WaitingFor::AssistPayment { .. }
    ));

    let events = paused
        .events
        .iter()
        .chain(resumed.events.iter())
        .chain(completed.events.iter());
    assert_eq!(
        events
            .clone()
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == helper_source))
            .count(),
        1,
        "the helper's tap cost is retained across serialization and never replayed"
    );
    assert_eq!(
        events
            .clone()
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == helper_source))
            .count(),
        1,
        "the helper produces exactly one mana for its committed Assist contribution"
    );
    assert_eq!(
        events
            .filter(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
            .count(),
        1,
        "the outer spell finalizes exactly once without another Assist offer"
    );
}

#[test]
fn nested_composite_effect_cost_serializes_all_suffixes_and_rider_once() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].energy = 2;
    let mana = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    };
    let cost = AbilityCost::Composite {
        costs: vec![
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana { cost: mana.clone() },
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                    },
                ],
            },
            AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 2 },
            },
        ],
    };
    let mut ability = ResolvedAbility::new(
        Effect::PayCost {
            cost: cost.clone(),
            scale: None,
            payer: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));

    let expected_resume_cost = AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana { cost: mana },
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 2 },
            },
        ],
    };

    let life_before = runner.state().players[P0.0 as usize].life;
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("the nested cost reaches the source's replacement pause");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            &pending.resume,
            ManaAbilityResume::EffectPayCost { cost: paused_cost, .. }
                if paused_cost.as_ref() == &expected_resume_cost
        )
    ));

    let json =
        serde_json::to_string(runner.state()).expect("the nested composite cost root serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the nested composite cost root deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the redirected source resumes every enclosing composite suffix");

    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert_eq!(runner.state().players[P0.0 as usize].energy, 0);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before - 1,
        "PayLife settles once before the +1-life rider settles once"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::LifeChanged { amount: -2, .. }))
            .count(),
        1,
        "the nested PayLife suffix is paid exactly once"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::LifeChanged { amount: 1, .. }))
            .count(),
        1,
        "the rider runs exactly once after the complete nested cost"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the source's paid tap prefix is not replayed"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
}

#[test]
fn mana_cost_post_replacement_named_choice_serializes_and_resumes_outer_payment_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Post-Effect Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    scenario
        .add_creature(P0, "Mana Cost Post-Effect Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(prompt_after_moved_to_exile());
    let spell = scenario
        .add_spell_to_hand(P0, "Post-Effect Mana Payment Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the spell reaches its mana-payment window");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    let ability = runner.state().objects[&source].abilities[0].clone();
    let mut activation_events = Vec::new();
    let paused = activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut activation_events,
        ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        },
        None,
    )
    .expect("the mandatory replacement delivers and reaches its post-effect prompt");
    assert!(matches!(
        paused,
        WaitingFor::NamedChoice { ref options, .. }
            if options == &vec!["first".to_string(), "second".to_string()]
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    assert_eq!(
        activation_events
            .iter()
            .filter(|event| matches!(event, GameEvent::ReplacementApplied { .. }))
            .count(),
        1,
        "the mandatory identity replacement applies exactly once before prompting"
    );
    assert!(runner.state().pending_replacement.is_none());
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor }) if matches!(
            pending.resume,
            ManaAbilityResume::ManaPayment {
                outer_player: Some(P0),
                convoke_mode: None,
            }
        ) && cursor.remaining.is_empty()
    ));
    assert!(runner.state().pending_cast.is_some());

    let json = serde_json::to_string(runner.state())
        .expect("the post-effect prompt retains the typed mana-cost cursor on the wire");
    let restored: GameState = serde_json::from_str(&json)
        .expect("the post-effect prompt restores with the typed mana-cost cursor");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseOption {
            choice: "first".to_string(),
        })
        .expect("answering the post-effect prompt resumes the parked mana-cost root");

    assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the parked cursor produces mana exactly once after the post-effect"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the original outer mana payment spends the resumed mana once");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);

    let events = activation_events
        .iter()
        .chain(resumed.events.iter())
        .chain(completed.events.iter());
    assert_eq!(
        events
            .clone()
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the post-effect pause cannot replay the mana source's paid tap cost"
    );
    assert_eq!(
        events
            .clone()
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "the post-effect pause cannot produce mana twice"
    );
    assert_eq!(
        events
            .filter(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
            .count(),
        1,
        "the original spell finalizes once after the post-effect prompt"
    );
}

#[test]
fn self_return_mana_cost_post_effect_serializes_without_advancing_planned_sources() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Self-Return Post-Effect Mana Source", 1, 1)
        .as_land()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::ReturnToHand {
                        count: 1,
                        filter: Some(TargetFilter::SelfRef),
                        from_zone: None,
                    },
                ],
            }),
        )
        .id();
    let deferred_source = scenario
        .add_creature(P0, "Deferred Planned Mana Source", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id();
    scenario
        .add_creature(P0, "Self-Return Post-Effect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to_with_post_effect(Zone::Hand, Zone::Hand));
    let spell = scenario
        .add_spell_to_hand(P0, "Two-Source Auto-Tap Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the spell reaches manual mana payment");
    let paused = runner
        .act(GameAction::PassPriority)
        .expect("the first auto-tapped source reaches its replacement post-effect");

    assert!(matches!(
        paused.waiting_for,
        WaitingFor::NamedChoice { ref options, .. }
            if options == &vec!["first".to_string(), "second".to_string()]
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
    assert!(
        !runner.state().objects[&deferred_source].tapped,
        "the live post-effect prompt must stop the rest of the auto-tap plan"
    );
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { cursor, .. })
            if cursor.resolution_mode == ManaAbilityCostResolutionMode::AutoResolved
    ));
    assert!(runner.state().pending_replacement.is_none());

    let json = serde_json::to_string(runner.state())
        .expect("the self-return post-effect pause serializes");
    assert!(
        json.contains("AutoResolved"),
        "the serialized cursor must retain its typed auto-resolution mode"
    );
    let restored: GameState =
        serde_json::from_str(&json).expect("the self-return post-effect pause deserializes");
    let mut runner = GameRunner::from_state(restored);

    let resumed = runner
        .act(GameAction::ChooseOption {
            choice: "first".to_string(),
        })
        .expect("the post-effect response resumes the typed self-return cost root");
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert!(
        !runner.state().objects[&deferred_source].tapped,
        "resuming the first source must not advance the next planned source"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the resumed source produces exactly its own mana before the outer payment continues"
    );

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the remaining planned source completes the outer payment");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().objects[&deferred_source].tapped);
    assert!(runner.state().pending_cost_move_resume.is_none());

    for (object_id, label) in [
        (source, "self-return source"),
        (deferred_source, "deferred planned source"),
    ] {
        assert_eq!(
            paused
                .events
                .iter()
                .chain(resumed.events.iter())
                .chain(completed.events.iter())
                .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id: tapped, .. } if *tapped == object_id))
                .count(),
            1,
            "the {label} is tapped exactly once across the paused auto-tap plan"
        );
        assert_eq!(
            paused
                .events
                .iter()
                .chain(resumed.events.iter())
                .chain(completed.events.iter())
                .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == object_id))
                .count(),
            1,
            "the {label} produces mana exactly once across the paused auto-tap plan"
        );
    }
}

#[test]
fn prevented_mana_cost_move_serializes_and_restores_mana_payment_root() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let spell = scenario
        .add_spell_to_hand(P0, "Prevented Mana Payment Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the spell reaches its manual mana-payment window");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment {
            player: P0,
            convoke_mode: None,
        }
    ));
    assert!(matches!(
        runner.state().pending_cast.as_deref(),
        Some(pending) if pending.object_id == spell && pending.card_id == card_id
    ));
    let ability = runner.state().objects[&source].abilities[0].clone();
    let mut initial_events = Vec::new();
    let paused = activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut initial_events,
        ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        },
        None,
    )
    .expect("the mana payment root pauses on its source cost move");
    assert!(matches!(paused, WaitingFor::ReplacementChoice { .. }));
    stage_prevented_cost_move(runner.state_mut(), source);

    let json = serde_json::to_string(runner.state())
        .expect("the staged prevented mana-payment root serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the staged prevented mana-payment root deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the prevented dispatcher restores the exact mana-payment prompt");

    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment {
            player: P0,
            convoke_mode: None,
        }
    ));
    assert!(matches!(
        runner.state().pending_cast.as_deref(),
        Some(pending) if pending.object_id == spell && pending.card_id == card_id
    ));
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "prevention must not replay the source's paid tap prefix"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "prevention resumes mana production exactly once"
    );
}

#[test]
fn prevented_mana_cost_move_serializes_and_restores_unless_payment_root() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    let ability = runner.state().objects[&source].abilities[0].clone();
    let unless_cost = AbilityCost::Mana {
        cost: ManaCost::generic(1),
    };
    let pending_effect = ResolvedAbility::new(
        Effect::Unimplemented {
            name: "Prevented Unless Payment Witness".to_string(),
            description: None,
        },
        vec![],
        source,
        P0,
    );
    let mut initial_events = Vec::new();
    let paused = activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut initial_events,
        ManaAbilityResume::UnlessPayment {
            outer_player: Some(P0),
            cost: Box::new(unless_cost.clone()),
            pending_effect: Box::new(pending_effect.clone()),
            trigger_event: None,
            effect_description: Some("prevented unless payment witness".to_string()),
            remaining: vec![P1],
        },
        None,
    )
    .expect("the unless-payment root pauses on its source cost move");
    assert!(matches!(paused, WaitingFor::ReplacementChoice { .. }));
    stage_prevented_cost_move(runner.state_mut(), source);

    let json = serde_json::to_string(runner.state())
        .expect("the staged prevented unless-payment root serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the staged prevented unless-payment root deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the prevented dispatcher restores the exact unless-payment prompt");

    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::UnlessPayment {
            player: P0,
            ref cost,
            pending_effect: ref resumed_effect,
            trigger_event: None,
            effect_description: Some(ref description),
            ref remaining,
        } if cost == &unless_cost
            && resumed_effect.as_ref() == &pending_effect
            && description == "prevented unless payment witness"
            && remaining == &vec![P1]
    ));
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "prevention must not replay the source's paid tap prefix"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "prevention resumes mana production exactly once"
    );
}

#[test]
fn paused_mana_cost_events_scan_current_and_deferred_observers_once() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let observer = |mode, amount| {
        TriggerDefinition::new(mode)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: amount },
                    player: TargetFilter::Controller,
                },
            ))
            .valid_card(TargetFilter::Any)
            .trigger_zones(vec![Zone::Battlefield])
    };
    scenario
        .add_creature(P1, "Deferred Tap Observer", 0, 0)
        .as_enchantment()
        .with_trigger_definition(observer(TriggerMode::Taps, 1));
    scenario
        .add_creature(P0, "Current Mana Observer", 0, 0)
        .as_enchantment()
        .with_trigger_definition(observer(TriggerMode::ManaAdded, 2));

    let mut runner = scenario.build();
    let ability = runner.state().objects[&source].abilities[0].clone();
    let mut initial_events = Vec::new();
    activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut initial_events,
        ManaAbilityResume::Priority,
        None,
    )
    .expect("the source pauses after its initial tap event");

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the replacement resume settles both event batches");
    assert!(matches!(resumed.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(runner.state().stack.len(), 2);
    for amount in [1, 2] {
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .filter(|entry| matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { ability, .. }
                        if matches!(
                            &ability.effect,
                            Effect::GainLife {
                                amount: QuantityExpr::Fixed { value },
                                ..
                            } if *value == amount
                        )
                ))
                .count(),
            1,
            "the observer for amount {amount} is placed exactly once"
        );
    }
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the deferred tap event remains exactly-once owned by the cursor"
    );
    assert_eq!(
        resumed
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "the current ManaAdded event is emitted and scanned exactly once"
    );
}

#[test]
fn pre_phyrexian_auto_tap_redirect_preserves_manual_payment_root_without_forcing_prompt() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let spell = scenario
        .add_spell_to_hand(P0, "Pre-Phyrexian Auto-Tap Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianGreen],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("a manual cast reaches its normal mana-payment window");
    let paused = runner
        .act(GameAction::PassPriority)
        .expect("pre-Phyrexian auto-tap reaches the source-cost replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            pending.resume,
            ManaAbilityResume::ManaPayment {
                outer_player: Some(P0),
                convoke_mode: None,
            }
        )
    ));

    let json = serde_json::to_string(runner.state())
        .expect("the pre-Phyrexian source-cost pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the pre-Phyrexian source-cost pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirecting the source cost preserves the manual payment root");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment {
            player: P0,
            convoke_mode: None,
        }
    ));
    let phyrexian = runner
        .act(GameAction::PassPriority)
        .expect("the preserved root computes the real Phyrexian payment prompt");
    assert!(matches!(
        phyrexian.waiting_for,
        WaitingFor::PhyrexianPayment { player: P0, .. }
    ));
    let completed = runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![engine::types::game_state::ShardChoice::PayLife],
        })
        .expect("the real submitted Phyrexian choice finalizes the original cast");

    assert_eq!(runner.state().players[P0.0 as usize].life, 18);
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .chain(phyrexian.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the pre-Phyrexian source cost is paid exactly once"
    );
}

#[test]
fn automatic_phyrexian_cast_retries_the_original_payment_after_source_cost_redirect() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let spell = scenario
        .add_spell_to_hand(P0, "Automatic Phyrexian Root Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianGreen],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let paused = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("automatic casting reaches the source-cost replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
            if matches!(
                pending.resume,
                ManaAbilityResume::FinalizePendingManaPayment { player: P0 }
            )
    ));

    let phyrexian = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirecting the automatic source cost resumes the original cast");
    assert!(matches!(
        phyrexian.waiting_for,
        WaitingFor::PhyrexianPayment { player: P0, .. }
    ));

    let completed = runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![engine::types::game_state::ShardChoice::PayLife],
        })
        .expect("the automatic Phyrexian cast completes after the replacement answer");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(runner.state().players[P0.0 as usize].life, 18);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(phyrexian.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
            .count(),
        1,
        "the automatic cast finalizes exactly once"
    );
}

#[test]
fn automatic_ordinary_cast_retries_the_original_payment_after_source_cost_redirect() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let spell = scenario
        .add_spell_to_hand(P0, "Automatic Ordinary Root Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let paused = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("automatic casting reaches the source-cost replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
            if matches!(
                pending.resume,
                ManaAbilityResume::FinalizePendingManaPayment { player: P0 }
            )
    ));

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirecting the automatic source cost resumes the original cast");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
            .count(),
        1,
        "the automatic cast finalizes exactly once"
    );
}

#[test]
fn automatic_phyrexian_activation_retries_after_source_cost_redirect() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let activator = scenario
        .add_creature(P0, "Automatic Activation Root Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::PhyrexianGreen],
                    generic: 0,
                },
            }),
        )
        .id();

    let mut runner = scenario.build();
    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: activator,
            ability_index: 0,
        })
        .expect("automatic activation reaches the source-cost replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
            if matches!(
                pending.resume,
                ManaAbilityResume::FinalizePendingManaPayment { player: P0 }
            )
    ));

    let phyrexian = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirecting the automatic source cost resumes the activation");
    assert!(matches!(
        phyrexian.waiting_for,
        WaitingFor::PhyrexianPayment { player: P0, .. }
    ));

    let completed = runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![engine::types::game_state::ShardChoice::PayLife],
        })
        .expect("the automatic activation completes after the replacement answer");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P0.0 as usize].life, 18);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(phyrexian.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::AbilityActivated { source_id, .. } if *source_id == activator))
            .count(),
        1,
        "the automatic activation reaches the stack exactly once"
    );
}

#[test]
fn automatic_ordinary_activation_retries_after_source_cost_redirect() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let activator = scenario
        .add_creature(P0, "Automatic Ordinary Activation Root Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                },
            }),
        )
        .id();

    let mut runner = scenario.build();
    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: activator,
            ability_index: 0,
        })
        .expect("ordinary activation reaches the source-cost replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
            if matches!(
                pending.resume,
                ManaAbilityResume::FinalizePendingManaPayment { player: P0 }
            )
    ));

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirecting the ordinary source cost resumes the activation");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&activator].zone, Zone::Battlefield);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::AbilityActivated { source_id, .. } if *source_id == activator))
            .count(),
        1,
        "the ordinary activation reaches the stack exactly once"
    );
}

#[test]
fn targeted_mana_tap_hand_exile_cost_retries_after_source_cost_redirect_without_replaying_exile() {
    let (mut scenario, mana_source) = mana_self_exile_cost_redirect_witness();
    let activator = scenario
        .add_creature(P0, "Targeted Exile Cost Activation Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                    damage_source: None,
                    excess: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![ManaCostShard::Green],
                            generic: 0,
                        },
                    },
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: Some(Zone::Hand),
                        filter: None,
                    },
                ],
            }),
        )
        .id();
    let target = scenario
        .add_creature(P1, "Targeted Exile Cost Target", 1, 1)
        .id();
    let fuel = scenario.add_card_to_hand(P0, "Targeted Exile Cost Fuel");

    let mut runner = scenario.build();
    let target_selection = runner
        .act(GameAction::ActivateAbility {
            source_id: activator,
            ability_index: 0,
        })
        .expect("the targeted activation announces before paying its costs");
    assert!(matches!(
        target_selection.waiting_for,
        WaitingFor::TargetSelection { player: P0, .. }
    ));
    let select_exile = runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        })
        .expect("target selection surfaces the non-self hand exile cost first");
    assert!(matches!(
        select_exile.waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::ExileFromZone { .. },
            ..
        }
    ));

    let fuel_paused = runner
        .act(GameAction::SelectCards { cards: vec![fuel] })
        .expect("the selected hand-exile cost reaches its redirect choice");
    assert!(matches!(
        fuel_paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let source_paused = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the paid hand-exile prefix advances to the mana-source redirect");
    assert!(matches!(
        source_paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the mana-root resume must not replay the already paid hand-exile cost");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&fuel].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&mana_source].zone, Zone::Graveyard);
    assert!(
        runner.state().objects[&activator].tapped,
        "the post-mana tap suffix is paid exactly once"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());

    let stack_entry = runner
        .state()
        .stack
        .back()
        .expect("the activation reaches the stack after all cost suffixes settle");
    let StackEntryKind::ActivatedAbility { ability, .. } = &stack_entry.kind else {
        panic!(
            "expected activated ability on the stack, got {:?}",
            stack_entry.kind
        );
    };
    assert_eq!(ability.targets, vec![TargetRef::Object(target)]);
    assert_eq!(
        fuel_paused
            .events
            .iter()
            .chain(source_paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if *object_id == fuel))
            .count(),
        1,
        "the selected hand-exile cost cannot replay after the mana source resumes"
    );
    assert_eq!(
        target_selection
            .events
            .iter()
            .chain(select_exile.events.iter())
            .chain(fuel_paused.events.iter())
            .chain(source_paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::AbilityActivated { source_id, .. } if *source_id == activator))
            .count(),
        1,
        "the target-first activation is announced exactly once"
    );
}

#[test]
fn committed_assist_source_cost_pause_rejects_cast_cancellation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let helper_source = scenario
        .add_creature(P1, "Assist Cancellation Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in [
        "First Assist Cancellation Redirect",
        "Second Assist Cancellation Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Assist Cancellation Witness", true)
        .with_mana_cost(ManaCost::generic(1))
        .with_keyword(Keyword::Assist)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the Assist spell reaches its helper offer");
    runner
        .act(GameAction::ChooseAssistPlayer { player: Some(P1) })
        .expect("choose the assisting player");
    runner
        .act(GameAction::CommitAssistPayment { generic: 1 })
        .expect("commit the helper contribution");
    runner
        .act(GameAction::PassPriority)
        .expect("the helper source reaches its replacement choice");
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the helper source resumes the committed Assist payment");
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));

    let cancelled = runner.act(GameAction::CancelCast);
    assert!(matches!(
        cancelled,
        Err(engine::game::engine::EngineError::ActionNotAllowed(_))
    ));
    assert_eq!(runner.state().objects[&helper_source].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P1.0 as usize].mana_pool.total(), 1);
    assert!(runner.state().pending_cast.is_some());

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the non-cancellable committed Assist payment still finalizes");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(runner.state().players[P1.0 as usize].mana_pool.total(), 0);
}

#[test]
fn mana_cost_scry_post_effect_serializes_until_answered_then_resumes_root_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Scry Post-Effect Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    let scry_card = scenario.add_card_to_library_top(P0, "Scry Post-Effect Card");
    scenario
        .add_creature(P0, "Scry Post-Effect Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(scry_after_moved_to_exile());
    let spell = scenario
        .add_spell_to_hand(P0, "Scry Post-Effect Mana Payment Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the spell reaches manual mana payment");
    let ability = runner.state().objects[&source].abilities[0].clone();
    let mut activation_events = Vec::new();
    let paused = activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut activation_events,
        ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        },
        None,
    )
    .expect("the mandatory replacement delivers and reaches its Scry post-effect");
    assert!(matches!(
        paused,
        WaitingFor::ScryChoice { player: P0, ref cards } if cards == &vec![scry_card]
    ));
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert!(runner.state().pending_cost_move_resume.is_some());

    let json = serde_json::to_string(runner.state())
        .expect("the Scry post-effect retains the parked cost root on the wire");
    let restored: GameState =
        serde_json::from_str(&json).expect("the Scry post-effect restores the parked cost root");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::SelectCards {
            cards: vec![scry_card],
        })
        .expect("answering Scry resumes the parked mana-cost root");

    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the mana source resolves only after the Scry answer"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the original outer payment spends the resumed mana");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(
        activation_events
            .iter()
            .chain(resumed.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "the Scry post-effect cannot replay mana production"
    );
}

#[test]
fn mana_cost_proliferate_post_effect_serializes_until_answered_then_resumes_root_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Proliferate Post-Effect Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    let proliferate_target = scenario
        .add_creature(P0, "Proliferate Post-Effect Target", 1, 1)
        .id();
    scenario.with_counter(proliferate_target, CounterType::Plus1Plus1, 1);
    scenario
        .add_creature(P0, "Proliferate Post-Effect Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(proliferate_after_moved_to_exile());
    let spell = scenario
        .add_spell_to_hand(P0, "Proliferate Post-Effect Mana Payment Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the spell reaches manual mana payment");
    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the real mana-ability action reaches its Proliferate post-effect");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ProliferateChoice { player: P0, .. }
    ));
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert!(runner.state().pending_cost_move_resume.is_some());

    let json = serde_json::to_string(runner.state())
        .expect("the Proliferate post-effect retains the parked cost root on the wire");
    let restored: GameState = serde_json::from_str(&json)
        .expect("the Proliferate post-effect restores the parked cost root");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(proliferate_target)],
        })
        .expect("answering Proliferate resumes the parked mana-cost root");

    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(
        runner.state().objects[&proliferate_target].counters[&CounterType::Plus1Plus1],
        2,
        "the interactive post-effect settles before the outer mana root resumes"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the mana source resolves only after the Proliferate answer"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the original outer payment spends the resumed mana");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
            .count(),
        1,
        "the Proliferate post-effect resumes the outer cast exactly once"
    );
}

#[test]
fn optional_post_effect_settles_before_resuming_the_parked_mana_root() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Optional Post-Effect Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    scenario
        .add_creature(P0, "Optional Post-Effect Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(optional_gain_life_after_moved_to_exile());
    let spell = scenario
        .add_spell_to_hand(P0, "Optional Post-Effect Mana Payment Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the spell reaches manual mana payment");
    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the mana ability's mandatory redirect reaches its optional post-effect");
    assert!(
        matches!(
            paused.waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "expected optional post-effect choice, got {:?}",
        paused.waiting_for
    );
    assert!(runner.state().pending_cost_move_resume.is_some());
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);

    let resumed = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("answering the optional post-effect resumes the parked mana root");
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(runner.state().players[P0.0 as usize].life, 21);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the source resolves only after the optional effect is fully answered"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the original payment spends the resumed mana");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
            .count(),
        1,
        "the outer cast resumes exactly once after the optional post-effect"
    );
}

#[test]
fn delve_mana_payment_honors_moved_redirect_without_linking_redirected_fuel() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Delve Redirect Payment Witness", true)
        .with_mana_cost(ManaCost::generic(1))
        .with_keyword(Keyword::Delve)
        .id();
    let fuel = scenario
        .add_spell_to_graveyard(P0, "Redirected Delve Fuel", true)
        .id();
    for name in ["First Delve Exile Redirect", "Second Delve Exile Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Hand));
    }

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let announced = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("delve spell reaches its mana-payment window");
    assert!(matches!(
        announced.waiting_for,
        WaitingFor::ManaPayment {
            player: P0,
            convoke_mode: Some(engine::types::game_state::ConvokeMode::Delve),
        }
    ));

    let paused = runner
        .act(GameAction::TapForConvoke {
            object_id: fuel,
            mana_type: engine::types::mana::ManaType::Colorless,
        })
        .expect("delve fuel must consult competing Moved redirects");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirected delve fuel restores the mana-payment root");
    assert_eq!(runner.state().objects[&fuel].zone, Zone::Hand);
    assert!(
        !runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.exiled_id == fuel && link.source_id == spell),
        "fuel redirected away from exile must not be linked as exiled with the spell"
    );
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment {
            player: P0,
            convoke_mode: Some(engine::types::game_state::ConvokeMode::Delve),
        }
    ));

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("redirected delve fuel still pays its generic cost component");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

#[test]
fn delve_murktide_link_tracks_only_fuel_delivered_to_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Murktide Regent", true)
        .with_mana_cost(ManaCost::generic(2))
        .with_keyword(Keyword::Delve)
        .id();
    let delivered_fuel = scenario
        .add_spell_to_graveyard(P0, "Delivered Delve Fuel", true)
        .id();
    let redirected_fuel = scenario
        .add_spell_to_graveyard(P0, "Redirected Murktide Fuel", true)
        .id();
    let first_redirect = scenario
        .add_creature(P0, "First Murktide Exile Redirect", 0, 0)
        .as_enchantment()
        .id();
    let second_redirect = scenario
        .add_creature(P0, "Second Murktide Exile Redirect", 0, 0)
        .as_enchantment()
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("Murktide-shaped delve spell reaches mana payment");
    runner
        .act(GameAction::TapForConvoke {
            object_id: delivered_fuel,
            mana_type: engine::types::mana::ManaType::Colorless,
        })
        .expect("first delve fuel is delivered to exile");

    for redirect in [first_redirect, second_redirect] {
        runner
            .state_mut()
            .objects
            .get_mut(&redirect)
            .expect("redirect source remains on the battlefield")
            .replacement_definitions = vec![redirect_moved_to(Zone::Exile, Zone::Hand)].into();
    }

    let paused = runner
        .act(GameAction::TapForConvoke {
            object_id: redirected_fuel,
            mana_type: engine::types::mana::ManaType::Colorless,
        })
        .expect("second delve fuel must consult competing Moved redirects");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirected fuel resumes the Murktide-shaped mana payment");
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment {
            player: P0,
            convoke_mode: Some(engine::types::game_state::ConvokeMode::Delve),
        }
    ));
    assert_eq!(runner.state().objects[&delivered_fuel].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&redirected_fuel].zone, Zone::Hand);
    let tracked_ids: Vec<_> = runner
        .state()
        .exile_links
        .iter()
        .filter(|link| link.source_id == spell)
        .map(|link| link.exiled_id)
        .collect();
    assert_eq!(tracked_ids, vec![delivered_fuel]);
    assert_eq!(
        runner
            .state()
            .cards_exiled_with_source_this_turn
            .get(&spell)
            .cloned()
            .unwrap_or_default(),
        vec![delivered_fuel],
        "Murktide's tracked set contains precisely its delivered exile"
    );

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("both delve components pay the generic mana after redirect");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

/// W-L1 (red first): Cascade's bottom placement must be a replaceable
/// Library-destination move. The unmodified raw mover cannot surface this
/// CR 616.1 choice, so this witness is expected to fail until tranche L1.
#[test]
fn cascade_bottom_batch_pauses_for_library_redirect_before_completion() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Cascade Library Redirect Source", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let misses = [
        scenario
            .add_spell_to_library_top(P0, "Cascade Miss One", true)
            .with_mana_cost(ManaCost::generic(1))
            .id(),
        scenario
            .add_spell_to_library_top(P0, "Cascade Miss Two", true)
            .with_mana_cost(ManaCost::generic(2))
            .id(),
        scenario
            .add_spell_to_library_top(P0, "Cascade Miss Three", true)
            .with_mana_cost(ManaCost::generic(3))
            .id(),
    ];
    let redirect_sources = [
        scenario
            .add_creature(P0, "Cascade Library To Graveyard", 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Library, Zone::Graveyard))
            .id(),
        scenario
            .add_creature(P0, "Cascade Library To Exile", 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Library, Zone::Exile))
            .id(),
    ];

    let mut runner = scenario.build();
    let ability = ResolvedAbility::new(Effect::Cascade, vec![], source, P0);
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("cascade should reach its library-bottom cleanup");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the first cascade bottom placement must surface its competing Moved redirects"
    );
    let parked_order = runner
        .state()
        .active_batch_delivery()
        .expect("the remaining randomized cascade suffix must be batch-owned")
        .remaining
        .clone();
    assert_eq!(
        parked_order.len(),
        misses.len() - 1,
        "the parked batch owns every unattempted miss after the first redirect choice"
    );
    assert!(
        !initial_events.iter().any(|event| matches!(
            event,
            GameEvent::CascadeMissed { source_id, .. } if *source_id == source
        )),
        "cascade completion must wait for every bottom placement to settle"
    );

    let redirected = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choosing one Library redirect delivers the first cascade miss");
    let redirected_id = misses
        .iter()
        .copied()
        .find(|id| !parked_order.contains(id))
        .expect("the first attempted miss is outside the parked suffix");
    assert_ne!(
        runner.state().objects[&redirected_id].zone,
        Zone::Library,
        "the chosen redirect suppresses the original bottom placement"
    );
    assert!(
        matches!(redirected.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the remaining batch suffix must re-pause while the Library redirects remain active"
    );
    for redirect_source in redirect_sources {
        runner
            .state_mut()
            .objects
            .get_mut(&redirect_source)
            .expect("synthetic redirect source remains on the battlefield")
            .replacement_definitions
            .clear();
    }
    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the now-unredirected parked cascade suffix drains");
    let library: Vec<_> = runner.state().players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(
        &library[library.len() - parked_order.len()..],
        parked_order.as_slice(),
        "the batch drain must retain the already-randomized suffix order"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(redirected.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::CascadeMissed { source_id, .. } if *source_id == source
            ))
            .count(),
        1,
        "cascade completion fires exactly once after the full batch settles"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(redirected.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::Cascade,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "Cascade's resolution event fires exactly once after the full no-hit batch settles"
    );
}

/// W-166 (red first): Cascade's one-card Library-to-Exile delivery must park
/// the loop before its hit offer or miss-tail runs when CR 616.1 requires a
/// replacement choice.
#[test]
fn cascade_library_exile_redirect_pauses_before_hit_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Cascade Exile Redirect Source", 1, 1)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let miss = scenario
        .add_spell_to_library_top(P0, "Cascade Redirect Miss", true)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Cascade Redirect Hit", false)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario
        .add_creature(P0, "Cascade Exile To Hand Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(
            redirect_moved_to(Zone::Exile, Zone::Hand)
                .valid_card(TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant))),
        );
    scenario
        .add_creature(P0, "Cascade Exile To Graveyard Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(
            redirect_moved_to(Zone::Exile, Zone::Graveyard)
                .valid_card(TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant))),
        );
    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![miss, hit];
    let ability = ResolvedAbility::new(Effect::Cascade, vec![], source, P0);
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("cascade reaches its first replacement-safe exile");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&miss].zone, Zone::Library);
    assert_eq!(runner.state().objects[&hit].zone, Zone::Library);
    assert!(
        !initial_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Cascade,
                source_id,
                ..
            } if *source_id == source
        )),
        "the hit offer tail must not precede the replacement choice"
    );

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the settled first exile resumes the cascade loop");
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::CastOffer {
            player: P0,
            kind:
                engine::types::game_state::CastOfferKind::Cascade {
                    hit_card,
                    exiled_misses,
                    source_mv: 4,
                    source_id,
                },
        } if hit_card == hit && exiled_misses.is_empty() && source_id == source
    ));
    assert_eq!(runner.state().objects[&miss].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&hit].zone, Zone::Exile);
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Cascade,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the settled loop exposes the cascade tail exactly once"
    );
}

/// W-166-REG: Without replacements Cascade still finds its first eligible hit,
/// carries every prior miss, and puts both cards on the library bottom when the
/// controller declines the cast offer.
#[test]
fn cascade_exile_loop_stays_synchronous_without_replacements() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Synchronous Cascade Exile Source", 1, 1)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let miss = scenario
        .add_spell_to_library_top(P0, "Synchronous Cascade Miss", true)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Synchronous Cascade Hit", true)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![miss, hit];
    let mut events = Vec::new();
    resolve_ability_chain(
        runner.state_mut(),
        &ResolvedAbility::new(Effect::Cascade, vec![], source, P0),
        &mut events,
        0,
    )
    .expect("unredirected cascade resolves synchronously to its offer");
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::CastOffer {
            kind:
                engine::types::game_state::CastOfferKind::Cascade {
                    hit_card,
                    exiled_misses,
                    source_mv: 4,
                    source_id,
                },
            ..
        } if *hit_card == hit && exiled_misses == &vec![miss] && *source_id == source
    ));
    assert_eq!(runner.state().objects[&miss].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&hit].zone, Zone::Exile);

    let declined = runner
        .act(GameAction::CascadeChoice {
            choice: engine::types::actions::CastChoice::Decline,
        })
        .expect("declined cascade puts its hit and miss on the library bottom");
    assert!(matches!(
        declined.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().players[P0.0 as usize].library.len(), 2);
    assert_eq!(runner.state().objects[&miss].zone, Zone::Library);
    assert_eq!(runner.state().objects[&hit].zone, Zone::Library);
    assert_eq!(
        events
            .iter()
            .chain(declined.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Cascade,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the synchronous cascade tail resolves exactly once"
    );
}

/// W-167 (red first): a cast-from-zone exile delivery must park before it grants
/// the lingering permission or emits its resolution event when CR 616.1 requires
/// the affected card's controller to choose a replacement.
#[test]
fn cast_from_zone_exile_redirect_pauses_before_lingering_permission_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Cast-From-Zone Redirect Source", 1, 1)
        .id();
    let card = scenario
        .add_spell_to_library_top(P0, "Cast-From-Zone Redirect Card", true)
        .id();
    scenario
        .add_creature(P0, "Cast-From-Zone Exile To Hand", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Hand));
    scenario
        .add_creature(P0, "Cast-From-Zone Exile To Graveyard", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![card];
    let ability = ResolvedAbility::new(
        Effect::CastFromZone {
            target: TargetFilter::ParentTarget,
            without_paying_mana_cost: true,
            mode: CardPlayMode::Cast,
            cast_transformed: false,
            alt_ability_cost: None,
            constraint: None,
            duration: None,
            driver: CastFromZoneDriver::LingeringPermission,
            mana_spend_permission: None,
        },
        vec![TargetRef::Object(card)],
        source,
        P0,
    );
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("CastFromZone reaches its replacement-safe exile delivery");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&card].zone, Zone::Library);
    assert!(runner.state().objects[&card].casting_permissions.is_empty());
    assert!(
        !initial_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::CastFromZone,
                source_id,
                ..
            } if *source_id == source
        )),
        "the lingering-permission tail must not precede the replacement choice"
    );

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the selected exile redirect settles the CastFromZone delivery");
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
    assert!(
        runner.state().objects[&card].casting_permissions.is_empty(),
        "an exile permission must not attach when the card did not reach exile"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::CastFromZone,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the settled CastFromZone tail resolves exactly once"
    );
}

/// W-167-REG: an unredirected CastFromZone exile delivery remains synchronous
/// and grants exactly the same permission as the prior raw mover.
#[test]
fn cast_from_zone_exile_delivery_stays_synchronous_and_grants_permission() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Synchronous Cast-From-Zone Source", 1, 1)
        .id();
    let card = scenario
        .add_spell_to_library_top(P0, "Synchronous Cast-From-Zone Card", true)
        .id();
    let second_card = scenario
        .add_spell_to_library_top(P0, "Second Synchronous Cast-From-Zone Card", true)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![card, second_card];
    let ability = ResolvedAbility::new(
        Effect::CastFromZone {
            target: TargetFilter::ParentTarget,
            without_paying_mana_cost: true,
            mode: CardPlayMode::Cast,
            cast_transformed: false,
            alt_ability_cost: None,
            constraint: None,
            duration: None,
            driver: CastFromZoneDriver::LingeringPermission,
            mana_spend_permission: None,
        },
        vec![TargetRef::Object(card), TargetRef::Object(second_card)],
        source,
        P0,
    );
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("unredirected CastFromZone resolves synchronously");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    for card in [card, second_card] {
        assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
        assert!(runner.state().objects[&card]
            .casting_permissions
            .iter()
            .any(|permission| matches!(permission, CastingPermission::ExileWithAltCost { .. })));
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::CastFromZone,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the synchronous CastFromZone path resolves exactly once"
    );
}

/// W-L3 (red first): PutAtLibraryPosition must keep its requested top ordering
/// while routing every placement through the Library replacement consult.
#[test]
fn put_on_top_batch_redirects_and_preserves_chosen_order_without_redirects() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Put On Top Redirect Source", 1, 1)
        .id();
    let marker = scenario
        .add_spell_to_library_top(P0, "Existing Library Marker", true)
        .id();
    let first = scenario
        .add_spell_to_hand(P0, "First Chosen Top Card", true)
        .id();
    let second = scenario
        .add_spell_to_hand(P0, "Second Chosen Top Card", true)
        .id();
    let redirect_sources = [
        scenario
            .add_creature(P0, "Put On Top To Graveyard", 0, 0)
            .as_enchantment()
            .id(),
        scenario
            .add_creature(P0, "Put On Top To Exile", 0, 0)
            .as_enchantment()
            .id(),
    ];
    let base_state = scenario.build().state().clone();
    let ability = ResolvedAbility::new(
        Effect::PutAtLibraryPosition {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 0 },
            position: engine::types::ability::LibraryPosition::Top,
        },
        vec![TargetRef::Object(first), TargetRef::Object(second)],
        source,
        P0,
    );

    let mut control = GameRunner::from_state(base_state.clone());
    let mut control_events = Vec::new();
    resolve_ability_chain(control.state_mut(), &ability, &mut control_events, 0)
        .expect("unredirected placement resolves synchronously");
    assert_eq!(
        control.state().players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![first, second, marker],
        "top placement preserves the chosen order when no redirect applies"
    );

    let mut redirected = GameRunner::from_state(base_state);
    for redirect_source in redirect_sources {
        redirected
            .state_mut()
            .objects
            .get_mut(&redirect_source)
            .expect("synthetic redirect source remains on the battlefield")
            .replacement_definitions =
            vec![redirect_moved_to(Zone::Library, Zone::Graveyard)].into();
    }
    let mut redirected_events = Vec::new();
    resolve_ability_chain(redirected.state_mut(), &ability, &mut redirected_events, 0)
        .expect("put-on-top reaches its first library placement");

    assert!(
        matches!(
            redirected.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the first top placement must surface its competing Moved redirects"
    );
    assert!(
        redirected.state().active_batch_delivery().is_some(),
        "the remaining placement must be carried by the batch across the pause"
    );
    let parked_order = redirected
        .state()
        .active_batch_delivery()
        .expect("the remaining top placement is batch-owned")
        .remaining
        .clone();
    assert!(
        !redirected_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::PutAtLibraryPosition,
                source_id,
                ..
            } if *source_id == source
        )),
        "PutAtLibraryPosition must not complete before every placement settles"
    );
    let first_redirect = redirected
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choosing the first top-placement redirect delivers that card");
    let redirected_id = [first, second]
        .into_iter()
        .find(|id| !parked_order.contains(id))
        .expect("the attempted top card is outside the parked suffix");
    assert_eq!(
        redirected.state().objects[&redirected_id].zone,
        Zone::Graveyard,
        "the replacement suppresses placement at the old top position"
    );
    assert!(
        matches!(
            first_redirect.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the remaining top-placement batch must re-pause while redirects remain active"
    );
    for redirect_source in redirect_sources {
        redirected
            .state_mut()
            .objects
            .get_mut(&redirect_source)
            .expect("synthetic redirect source remains on the battlefield")
            .replacement_definitions
            .clear();
    }
    let completed = redirected
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the now-unredirected top-placement suffix drains");
    assert_eq!(
        redirected.state().players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![parked_order[0], marker],
        "the remaining top placement drains after the redirected card without reordering"
    );
    assert_eq!(
        redirected_events
            .iter()
            .chain(first_redirect.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::PutAtLibraryPosition,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "PutAtLibraryPosition completes exactly once after the whole batch settles"
    );
}

/// W-L2: A declined Discover keeps its hit and chain tail parked until the
/// replacement-aware miss batch has settled.
#[test]
fn discover_bottom_batch_pauses_before_its_hit_and_continuation_complete() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Discover Library Redirect Source", 1, 1)
        .id();
    let miss_a = scenario
        .add_spell_to_library_top(P0, "Discover Miss One", true)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let miss_b = scenario
        .add_spell_to_library_top(P0, "Discover Miss Two", true)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Discover Hit", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let redirect_sources = [
        scenario
            .add_creature(P0, "Discover Library To Graveyard", 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Library, Zone::Graveyard))
            .id(),
        scenario
            .add_creature(P0, "Discover Library To Exile", 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Library, Zone::Exile))
            .id(),
    ];

    let mut runner = scenario.build();
    let library = &mut runner.state_mut().players[P0.0 as usize].library;
    library.clear();
    library.push_back(miss_a);
    library.push_back(miss_b);
    library.push_back(hit);
    let mut ability = ResolvedAbility::new(
        Effect::Discover {
            mana_value_limit: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("discover should offer its eligible hit");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CastOffer {
            kind: engine::types::game_state::CastOfferKind::Discover { hit_card, .. },
            ..
        } if hit_card == hit
    ));
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    assert!(
        !initial_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::Discover,
                source_id,
                ..
            } if *source_id == source
        )),
        "the Discover resolution event must wait for the miss batch"
    );
    assert!(
        !initial_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::GainLife,
                source_id,
                ..
            } if *source_id == source
        )),
        "the discover chain tail must wait behind the cast offer"
    );

    let paused = runner
        .act(GameAction::DiscoverChoice {
            choice: engine::types::actions::CastChoice::Decline,
        })
        .expect("declined discover starts the replacement-aware miss batch");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Exile,
        "the raw hit-to-hand instruction waits until the miss batch completes"
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    let parked_order = runner
        .state()
        .active_batch_delivery()
        .expect("the remaining randomized discover misses are batch-owned")
        .remaining
        .clone();
    let first_redirect = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the first discover miss is redirected before the batch completes");
    let redirected_id = [miss_a, miss_b]
        .into_iter()
        .find(|id| !parked_order.contains(id))
        .expect("the first attempted miss is outside the parked suffix");
    assert_ne!(runner.state().objects[&redirected_id].zone, Zone::Library);
    assert!(
        matches!(
            first_redirect.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the remaining discover miss must re-pause while its redirects remain active"
    );
    for redirect_source in redirect_sources {
        runner
            .state_mut()
            .objects
            .get_mut(&redirect_source)
            .expect("synthetic redirect source remains on the battlefield")
            .replacement_definitions
            .clear();
    }
    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the now-unredirected discover suffix and hit-to-hand tail drain");
    let library: Vec<_> = runner.state().players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(library, parked_order);
    assert_eq!(runner.state().objects[&hit].zone, Zone::Hand);
    assert_eq!(runner.state().players[P0.0 as usize].life, 21);
    assert_eq!(
        initial_events
            .iter()
            .chain(first_redirect.events.iter())
            .chain(paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::Discover,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "Discover completes exactly once after the full batch settles"
    );
    assert_eq!(
        paused
            .events
            .iter()
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::GainLife,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the discover continuation completes exactly once after the batch settles"
    );
}

/// W-D1 (red first): a declined Discover's printed hit-to-hand instruction is
/// a replacement-aware delivery. A Hand redirect must park before the Discover
/// completion and its chained tail run.
#[test]
fn discover_declined_hit_to_hand_redirect_pauses_before_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Discover Hand Redirect Source", 1, 1)
        .id();
    let miss = scenario
        .add_spell_to_library_top(P0, "Discover Hand Redirect Miss", true)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Discover Hand Redirect Hit", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    for (name, destination) in [
        ("Discover Hand To Graveyard", Zone::Graveyard),
        ("Discover Hand To Exile", Zone::Exile),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Hand, destination));
    }

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![miss, hit];
    let mut ability = ResolvedAbility::new(
        Effect::Discover {
            mana_value_limit: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("Discover reaches its cast offer");

    let paused = runner
        .act(GameAction::DiscoverChoice {
            choice: engine::types::actions::CastChoice::Decline,
        })
        .expect("the synchronous miss batch reaches the replaceable Hand delivery");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&hit].zone, Zone::Exile);
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    assert!(
        !paused.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::Discover,
                source_id,
                ..
            } if *source_id == source
        )),
        "the Discover tail must not run before the Hand redirect choice"
    );

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the chosen Hand redirect resumes the typed completion tail");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(runner.state().objects[&hit].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P0.0 as usize].life, 21);
    assert_eq!(
        initial_events
            .iter()
            .chain(paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::Discover,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "Discover completes exactly once after its redirected Hand delivery"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::GainLife,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the Discover continuation runs exactly once after its redirected Hand delivery"
    );
}

/// W-D3 (red first): a declined Discover can park once while bottoming its
/// miss, then again while delivering its hit to Hand. The two typed tails must
/// preserve that order and complete exactly once.
#[test]
fn discover_declined_miss_and_hit_redirects_pause_in_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Discover Compound Redirect Source", 1, 1)
        .id();
    let miss = scenario
        .add_spell_to_library_top(P0, "Discover Compound Redirect Miss", true)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Discover Compound Redirect Hit", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    for (name, destination, redirected_to) in [
        (
            "Discover Compound Library To Graveyard",
            Zone::Library,
            Zone::Graveyard,
        ),
        (
            "Discover Compound Library To Exile",
            Zone::Library,
            Zone::Exile,
        ),
        (
            "Discover Compound Hand To Graveyard",
            Zone::Hand,
            Zone::Graveyard,
        ),
        ("Discover Compound Hand To Exile", Zone::Hand, Zone::Exile),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(destination, redirected_to));
    }

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![miss, hit];
    let ability = ResolvedAbility::new(
        Effect::Discover {
            mana_value_limit: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("Discover reaches its cast offer");

    let miss_paused = runner
        .act(GameAction::DiscoverChoice {
            choice: engine::types::actions::CastChoice::Decline,
        })
        .expect("the miss bottom placement reaches its replacement choice");
    assert!(matches!(
        miss_paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&hit].zone, Zone::Exile);

    let hand_paused = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the resolved miss reaches the replaceable hit-to-Hand delivery");
    assert!(matches!(
        hand_paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&miss].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&hit].zone, Zone::Exile);
    assert!(
        !hand_paused.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::Discover,
                source_id,
                ..
            } if *source_id == source
        )),
        "the Discover completion waits for the second replacement choice"
    );

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the redirected Hand delivery completes Discover");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(runner.state().objects[&hit].zone, Zone::Graveyard);
    assert_eq!(
        initial_events
            .iter()
            .chain(miss_paused.events.iter())
            .chain(hand_paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::Discover,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the two sequential replacement pauses run Discover's tail exactly once"
    );
}

/// W-D2 (red first): a rejected cast during Discover resolution routes its hit
/// through the same replacement-aware Hand delivery. Its synchronous miss batch
/// must propagate that completion pause instead of restoring priority over it.
#[test]
fn discover_rejected_cast_hit_to_hand_redirect_pauses_before_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Discover Rejection Redirect Source", 1, 1)
        .id();
    let target = scenario
        .add_creature(P1, "Discover Rejection Target", 1, 1)
        .id();
    let miss = scenario
        .add_spell_to_library_top(P0, "Discover Rejection Miss", true)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Discover Rejection X Hit", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .from_oracle_text("Destroy target creature.")
        .id();
    for (name, destination) in [
        ("Discover Rejection Hand To Graveyard", Zone::Graveyard),
        ("Discover Rejection Hand To Exile", Zone::Exile),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Hand, destination));
    }

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![miss, hit];
    let ability = ResolvedAbility::new(
        Effect::Discover {
            mana_value_limit: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("Discover reaches its cast offer");

    let selecting_target = runner
        .act(GameAction::DiscoverChoice {
            choice: engine::types::actions::CastChoice::Cast,
        })
        .expect("the legal Discover hit starts its during-resolution cast");
    assert!(matches!(
        selecting_target.waiting_for,
        WaitingFor::TargetSelection { player: P0, .. }
    ));
    match &mut runner.state_mut().waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. } => pending_cast.ability.chosen_x = Some(2),
        waiting_for => panic!("expected the target-selection cast, got {waiting_for:?}"),
    }

    let paused = runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        })
        .expect("the seeded resulting mana value rejects the Discover cast");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&hit].zone, Zone::Exile);
    assert!(runner.state().stack.iter().all(|entry| entry.id != hit));
    assert!(
        !paused.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::Discover,
                source_id,
                ..
            } if *source_id == source
        )),
        "priority and EffectResolved must wait for the Hand redirect choice"
    );

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the redirected rejected hit completes its priority tail");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(runner.state().objects[&hit].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&miss].zone, Zone::Library);
    assert_eq!(
        initial_events
            .iter()
            .chain(selecting_target.events.iter())
            .chain(paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::Discover,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the rejected cast emits Discover completion exactly once after its Hand delivery"
    );
}

/// W-REG: An uninterrupted Discover rejection still sends the hit to Hand and
/// returns priority synchronously, with the usual single Discover completion.
#[test]
fn discover_rejected_cast_without_redirect_stays_synchronous() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Uninterrupted Discover Rejection Source", 1, 1)
        .id();
    let target = scenario
        .add_creature(P1, "Uninterrupted Discover Rejection Target", 1, 1)
        .id();
    let miss = scenario
        .add_spell_to_library_top(P0, "Uninterrupted Discover Rejection Miss", true)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Uninterrupted Discover Rejection X Hit", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .from_oracle_text("Destroy target creature.")
        .id();

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![miss, hit];
    let ability = ResolvedAbility::new(
        Effect::Discover {
            mana_value_limit: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("Discover reaches its cast offer");
    runner
        .act(GameAction::DiscoverChoice {
            choice: engine::types::actions::CastChoice::Cast,
        })
        .expect("the legal Discover hit starts its during-resolution cast");
    match &mut runner.state_mut().waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. } => pending_cast.ability.chosen_x = Some(2),
        waiting_for => panic!("expected the target-selection cast, got {waiting_for:?}"),
    }

    let completed = runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        })
        .expect("the seeded resulting mana value rejects the Discover cast");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(runner.state().objects[&hit].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&miss].zone, Zone::Library);
    assert_eq!(
        initial_events
            .iter()
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::Discover,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the uninterrupted rejection retains the existing one-event completion"
    );
}

/// W-REG: In the absence of a Library-destination replacement, all three
/// migrated effect paths finish synchronously with their ordinary ordering and
/// completion behavior intact.
#[test]
fn library_effect_placements_stay_synchronous_without_redirects() {
    let mut cascade_scenario = GameScenario::new();
    cascade_scenario.at_phase(Phase::PreCombatMain);
    let cascade_source = cascade_scenario
        .add_creature(P0, "Uninterrupted Cascade", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    for (name, mana_value) in [("Cascade Miss A", 1), ("Cascade Miss B", 2)] {
        cascade_scenario
            .add_spell_to_library_top(P0, name, true)
            .with_mana_cost(ManaCost::generic(mana_value))
            .id();
    }
    let mut cascade = cascade_scenario.build();
    let mut cascade_events = Vec::new();
    resolve_ability_chain(
        cascade.state_mut(),
        &ResolvedAbility::new(Effect::Cascade, vec![], cascade_source, P0),
        &mut cascade_events,
        0,
    )
    .expect("uninterrupted cascade resolves");
    assert!(matches!(
        cascade.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(cascade.state().players[P0.0 as usize].library.len(), 2);
    assert_eq!(
        cascade_events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::CascadeMissed { source_id, .. } if *source_id == cascade_source
            ))
            .count(),
        1
    );

    let mut discover_scenario = GameScenario::new();
    discover_scenario.at_phase(Phase::PreCombatMain);
    let discover_source = discover_scenario
        .add_creature(P0, "Uninterrupted Discover", 1, 1)
        .id();
    let discover_miss = discover_scenario
        .add_spell_to_library_top(P0, "Discover Miss", true)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let discover_hit = discover_scenario
        .add_spell_to_library_top(P0, "Discover Hit", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut discover = discover_scenario.build();
    discover.state_mut().players[P0.0 as usize].library = im::vector![discover_miss, discover_hit];
    let mut discover_events = Vec::new();
    resolve_ability_chain(
        discover.state_mut(),
        &ResolvedAbility::new(
            Effect::Discover {
                mana_value_limit: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            discover_source,
            P0,
        ),
        &mut discover_events,
        0,
    )
    .expect("uninterrupted discover reaches its offer");
    let discover_completed = discover
        .act(GameAction::DiscoverChoice {
            choice: engine::types::actions::CastChoice::Decline,
        })
        .expect("declined uninterrupted discover resolves");
    assert!(matches!(
        discover_completed.waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(discover.state().objects[&discover_hit].zone, Zone::Hand);
    assert_eq!(discover.state().objects[&discover_miss].zone, Zone::Library);
    assert_eq!(
        discover_events
            .iter()
            .chain(discover_completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::Discover,
                    source_id,
                    ..
                } if *source_id == discover_source
            ))
            .count(),
        1
    );

    let mut put_scenario = GameScenario::new();
    put_scenario.at_phase(Phase::PreCombatMain);
    let put_source = put_scenario
        .add_creature(P0, "Uninterrupted Put", 1, 1)
        .id();
    let marker = put_scenario
        .add_spell_to_library_top(P0, "Library Marker", true)
        .id();
    let first = put_scenario.add_spell_to_hand(P0, "First Top", true).id();
    let second = put_scenario.add_spell_to_hand(P0, "Second Top", true).id();
    let mut put = put_scenario.build();
    let mut put_events = Vec::new();
    resolve_ability_chain(
        put.state_mut(),
        &ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 0 },
                position: engine::types::ability::LibraryPosition::Top,
            },
            vec![TargetRef::Object(first), TargetRef::Object(second)],
            put_source,
            P0,
        ),
        &mut put_events,
        0,
    )
    .expect("uninterrupted top placement resolves");
    assert_eq!(
        put.state().players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![first, second, marker]
    );
    assert_eq!(
        put_events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::PutAtLibraryPosition,
                    source_id,
                    ..
                } if *source_id == put_source
            ))
            .count(),
        1
    );
}

/// W-R2-TOP (red first): PutOnTopOrBottom's selected permanent must take the
/// replacement-aware Library delivery before its chained resolution tail runs.
#[test]
fn put_on_top_or_bottom_redirect_pauses_before_continuation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Top Or Bottom Redirect Source", 1, 1)
        .id();
    let target = scenario
        .add_creature(P0, "Top Or Bottom Redirect Target", 1, 1)
        .id();
    for (name, destination) in [
        ("Top Or Bottom Library To Graveyard", Zone::Graveyard),
        ("Top Or Bottom Library To Exile", Zone::Exile),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Library, destination));
    }

    let mut runner = scenario.build();
    let mut ability = ResolvedAbility::new(
        Effect::PutOnTopOrBottom {
            target: TargetFilter::Any,
            chooser: TargetFilter::ParentTargetOwner,
        },
        vec![TargetRef::Object(target)],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("top-or-bottom reaches the owner choice");

    let paused = runner
        .act(GameAction::ChooseTopOrBottom { top: true })
        .expect("the Library delivery reaches its replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the redirected Library delivery resumes its continuation");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(runner.state().objects[&target].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P0.0 as usize].life, 21);
    assert_eq!(
        initial_events
            .iter()
            .chain(paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::GainLife,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the chained continuation runs exactly once after the redirected delivery"
    );
}

/// W-R2-DIG (red first): a Dig kept card moving out of the library must settle
/// its replacement-aware destination before the tracked-set publication and
/// continuation tail run.
#[test]
fn dig_kept_nonbattlefield_redirect_pauses_before_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Dig Kept Redirect Source", 1, 1)
        .id();
    let kept = scenario
        .add_spell_to_library_top(P0, "Dig Kept Redirect Card", true)
        .id();
    let rest = scenario
        .add_spell_to_library_top(P0, "Dig Kept Redirect Rest", true)
        .id();
    for (name, destination) in [
        ("Dig Kept Hand To Graveyard", Zone::Graveyard),
        ("Dig Kept Hand To Exile", Zone::Exile),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Hand, destination));
    }

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![kept, rest];
    let mut ability = ResolvedAbility::new(
        Effect::Dig {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 2 },
            destination: Some(Zone::Hand),
            keep_count: Some(1),
            keep_count_expr: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: Some(Zone::Graveyard),
            rest_order: DigRestOrder::Preserve,
            reveal: true,
            enter_tapped: false,
            enters_attacking: false,
            source: DigSource::Library,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("Dig reaches its selection");

    let paused = runner
        .act(GameAction::SelectCards { cards: vec![kept] })
        .expect("the kept Hand delivery reaches its replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&kept].zone, Zone::Library);
    assert_eq!(runner.state().objects[&rest].zone, Zone::Library);
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    assert!(runner.state().chain_tracked_set_id.is_none());

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the redirected kept delivery completes the Dig tail");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(runner.state().objects[&kept].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&rest].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P0.0 as usize].life, 21);
    let tracked = runner
        .state()
        .tracked_object_sets
        .get(
            &runner
                .state()
                .chain_tracked_set_id
                .expect("Dig publishes its kept set after the delivery settles"),
        )
        .expect("Dig tracked set exists");
    assert!(
        tracked.is_empty(),
        "a kept card redirected away from its requested destination is absent from the settled delivery"
    );
    assert!(
        runner.state().last_zone_changed_ids.is_empty(),
        "a redirected kept card must not satisfy a requested-destination reflexive gate"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::GainLife,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the Dig continuation runs exactly once after the redirected kept delivery"
    );
}

/// W-R2-REG: The two R2 effect paths preserve their synchronous no-replacement
/// behavior, including continuation delivery and the requested library position.
#[test]
fn r2_effect_zone_moves_stay_synchronous_without_redirects() {
    let mut top_scenario = GameScenario::new();
    top_scenario.at_phase(Phase::PreCombatMain);
    let top_source = top_scenario
        .add_creature(P0, "Synchronous Top Or Bottom Source", 1, 1)
        .id();
    let top_target = top_scenario
        .add_creature(P0, "Synchronous Top Or Bottom Target", 1, 1)
        .id();
    let mut top_runner = top_scenario.build();
    let mut top_ability = ResolvedAbility::new(
        Effect::PutOnTopOrBottom {
            target: TargetFilter::Any,
            chooser: TargetFilter::ParentTargetOwner,
        },
        vec![TargetRef::Object(top_target)],
        top_source,
        P0,
    );
    top_ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        top_source,
        P0,
    )));
    let mut top_events = Vec::new();
    resolve_ability_chain(top_runner.state_mut(), &top_ability, &mut top_events, 0)
        .expect("top-or-bottom reaches its choice");
    let top_completed = top_runner
        .act(GameAction::ChooseTopOrBottom { top: true })
        .expect("unredirected top-or-bottom settles inline");
    assert!(matches!(
        top_completed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(top_runner.state().objects[&top_target].zone, Zone::Library);
    assert_eq!(top_runner.state().players[P0.0 as usize].life, 21);

    let mut dig_scenario = GameScenario::new();
    dig_scenario.at_phase(Phase::PreCombatMain);
    let dig_source = dig_scenario
        .add_creature(P0, "Synchronous Dig Kept Source", 1, 1)
        .id();
    let kept = dig_scenario
        .add_spell_to_library_top(P0, "Synchronous Dig Kept Card", true)
        .id();
    let rest = dig_scenario
        .add_spell_to_library_top(P0, "Synchronous Dig Kept Rest", true)
        .id();
    let mut dig_runner = dig_scenario.build();
    dig_runner.state_mut().players[P0.0 as usize].library = im::vector![kept, rest];
    let mut dig_ability = ResolvedAbility::new(
        Effect::Dig {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 2 },
            destination: Some(Zone::Hand),
            keep_count: Some(1),
            keep_count_expr: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: Some(Zone::Graveyard),
            rest_order: DigRestOrder::Preserve,
            reveal: true,
            enter_tapped: false,
            enters_attacking: false,
            source: DigSource::Library,
        },
        vec![],
        dig_source,
        P0,
    );
    dig_ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        dig_source,
        P0,
    )));
    let mut dig_events = Vec::new();
    resolve_ability_chain(dig_runner.state_mut(), &dig_ability, &mut dig_events, 0)
        .expect("Dig reaches its selection");
    let dig_completed = dig_runner
        .act(GameAction::SelectCards { cards: vec![kept] })
        .expect("unredirected Dig kept delivery settles inline");
    assert!(matches!(
        dig_completed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(dig_runner.state().objects[&kept].zone, Zone::Hand);
    assert_eq!(dig_runner.state().objects[&rest].zone, Zone::Graveyard);
    assert_eq!(dig_runner.state().players[P0.0 as usize].life, 21);
}

fn per_color_exile_ability(
    source_id: engine::types::identifiers::ObjectId,
    pool: Vec<engine::types::identifiers::ObjectId>,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::ForEachCategory {
            category: IterationCategory::Color,
            chooser: Chooser::Controller,
            action: ForEachCategoryAction::ExileFromPool {
                zone: Zone::Library,
                up_to: true,
            },
        },
        pool.into_iter().map(TargetRef::Object).collect(),
        source_id,
        P0,
    )
}

/// W-R3 (red first): a per-category exile's tracked-set extension and next
/// member prompt must wait for its replacement-aware exile delivery.
#[test]
fn per_category_exile_redirect_pauses_before_next_member() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Per-Category Exile Redirect Source", 1, 1)
        .id();
    let white = scenario.add_card_to_library_top(P0, "Per-Category White Card");
    let blue = scenario.add_card_to_library_top(P0, "Per-Category Blue Card");
    for (name, destination) in [
        ("Per-Category Exile To Graveyard", Zone::Graveyard),
        ("Per-Category Exile To Hand", Zone::Hand),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, destination));
    }

    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&white).unwrap().color = vec![ManaColor::White];
    runner.state_mut().objects.get_mut(&blue).unwrap().color = vec![ManaColor::Blue];
    let ability = per_color_exile_ability(source, vec![white, blue]);
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("the first color member reaches its choice");

    let paused = runner
        .act(GameAction::SelectCards { cards: vec![white] })
        .expect("the selected exile reaches a replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&white].zone, Zone::Library);
    let tracked = runner
        .state()
        .tracked_object_sets
        .get(
            &runner
                .state()
                .chain_tracked_set_id
                .expect("per-category resolution starts a tracked set"),
        )
        .expect("per-category tracked set exists");
    assert!(
        tracked.is_empty(),
        "the batch tail has not published the exile"
    );

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the redirected exile resumes the iteration tail");
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ChooseFromZoneChoice { ref cards, .. } if cards == &vec![blue]
    ));
    assert_eq!(runner.state().objects[&white].zone, Zone::Graveyard);
    let tracked = runner
        .state()
        .tracked_object_sets
        .get(
            &runner
                .state()
                .chain_tracked_set_id
                .expect("the settled exile publishes to the tracked set"),
        )
        .expect("the tracked set exists after the batch settles");
    assert_eq!(tracked, &vec![white]);

    let completed = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining the final category member completes the iteration");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(runner.state().objects[&blue].zone, Zone::Library);
    assert_eq!(
        initial_events
            .iter()
            .chain(paused.events.iter())
            .chain(resumed.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::ChooseFromZone,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the per-category iteration tail resolves exactly once"
    );
}

/// W-R3-REG: without a redirect, per-category exiles settle inline and advance
/// to the next category member before finishing the iteration.
#[test]
fn per_category_exile_stays_synchronous_without_redirects() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Synchronous Per-Category Exile Source", 1, 1)
        .id();
    let white = scenario.add_card_to_library_top(P0, "Synchronous Per-Category White");
    let blue = scenario.add_card_to_library_top(P0, "Synchronous Per-Category Blue");
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&white).unwrap().color = vec![ManaColor::White];
    runner.state_mut().objects.get_mut(&blue).unwrap().color = vec![ManaColor::Blue];
    let ability = per_color_exile_ability(source, vec![white, blue]);
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("the first color member reaches its choice");

    let first = runner
        .act(GameAction::SelectCards { cards: vec![white] })
        .expect("the white exile settles inline");
    assert!(matches!(
        first.waiting_for,
        WaitingFor::ChooseFromZoneChoice { ref cards, .. } if cards == &vec![blue]
    ));
    assert_eq!(runner.state().objects[&white].zone, Zone::Exile);

    let completed = runner
        .act(GameAction::SelectCards { cards: vec![blue] })
        .expect("the blue exile completes the iteration");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(runner.state().objects[&blue].zone, Zone::Exile);
    let tracked = runner
        .state()
        .tracked_object_sets
        .get(
            &runner
                .state()
                .chain_tracked_set_id
                .expect("per-category exiles publish one shared tracked set"),
        )
        .expect("the shared tracked set exists");
    assert_eq!(tracked, &vec![white, blue]);
    assert_eq!(
        initial_events
            .iter()
            .chain(first.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::ChooseFromZone,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the synchronous per-category iteration resolves exactly once"
    );
}

/// W-R4 (red first): selected drawn cards must settle their replacement-aware
/// Library delivery before the remaining cards' life payment or resolution event.
#[test]
fn drawn_this_turn_topdeck_redirect_pauses_before_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Drawn-This-Turn Redirect Source", 1, 1)
        .id();
    let topdecked = scenario.add_card_to_hand(P0, "Drawn-This-Turn Topdecked");
    let kept = scenario.add_card_to_hand(P0, "Drawn-This-Turn Kept");
    for (name, destination) in [
        ("Drawn-This-Turn Library To Graveyard", Zone::Graveyard),
        ("Drawn-This-Turn Library To Exile", Zone::Exile),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Library, destination));
    }

    let mut runner = scenario.build();
    engine::game::effects::drawn_this_turn_choice::record_drawn_card(
        runner.state_mut(),
        P0,
        topdecked,
    );
    engine::game::effects::drawn_this_turn_choice::record_drawn_card(runner.state_mut(), P0, kept);
    let ability = ResolvedAbility::new(
        Effect::ChooseDrawnThisTurnPayOrTopdeck {
            count: QuantityExpr::Fixed { value: 2 },
            life_payment: QuantityExpr::Fixed { value: 4 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("drawn-this-turn effect reaches its selection");

    let paused = runner
        .act(GameAction::SelectCards {
            cards: vec![topdecked],
        })
        .expect("the Library delivery reaches its replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&topdecked].zone, Zone::Hand);
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    assert_eq!(
        initial_events
            .iter()
            .chain(paused.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::ChooseDrawnThisTurnPayOrTopdeck,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        0,
        "the resolution event waits behind the replacement choice"
    );

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the redirected Library delivery runs the payment tail");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(runner.state().objects[&topdecked].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&kept].zone, Zone::Hand);
    assert_eq!(runner.state().players[P0.0 as usize].life, 16);
    assert_eq!(runner.state().last_effect_count, Some(1));
    assert_eq!(
        initial_events
            .iter()
            .chain(paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::ChooseDrawnThisTurnPayOrTopdeck,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the payment tail emits one resolution event after the replacement settles"
    );
}

/// W-R4-REG: reverse request construction preserves the selected order when
/// each ordered Library placement inserts at the top.
#[test]
fn drawn_this_turn_topdeck_preserves_selected_library_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Synchronous Drawn-This-Turn Source", 1, 1)
        .id();
    let prior_top = scenario.add_card_to_library_top(P0, "Drawn-This-Turn Prior Top");
    let first = scenario.add_card_to_hand(P0, "Drawn-This-Turn First");
    let second = scenario.add_card_to_hand(P0, "Drawn-This-Turn Second");
    let kept = scenario.add_card_to_hand(P0, "Drawn-This-Turn Kept");
    let mut runner = scenario.build();
    for object_id in [first, second, kept] {
        engine::game::effects::drawn_this_turn_choice::record_drawn_card(
            runner.state_mut(),
            P0,
            object_id,
        );
    }
    let ability = ResolvedAbility::new(
        Effect::ChooseDrawnThisTurnPayOrTopdeck {
            count: QuantityExpr::Fixed { value: 3 },
            life_payment: QuantityExpr::Fixed { value: 4 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("drawn-this-turn effect reaches its selection");

    let completed = runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .expect("the unredirected Library placements settle inline");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![first, second, prior_top],
        "first-selected remains topmost after reverse index-zero placements"
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, 16);
    assert_eq!(runner.state().last_effect_count, Some(2));
    assert_eq!(
        initial_events
            .iter()
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::ChooseDrawnThisTurnPayOrTopdeck,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the synchronous payment tail emits one resolution event"
    );
}

/// W-163-A (red first): a directly targeted sacrifice that pauses on the first
/// replacement choice retains both the selected suffix and its terminal event.
#[test]
fn targeted_sacrifice_reparks_replacement_before_terminal_effect_resolved() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Targeted Sacrifice Resume Source", 1, 1)
        .as_enchantment()
        .id();
    let first = scenario
        .add_creature(P0, "Targeted Sacrifice First Redirect", 1, 1)
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    let second = scenario
        .add_creature(P0, "Targeted Sacrifice Second Redirect", 1, 1)
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    let ability = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 2 },
            min_count: 0,
        },
        vec![TargetRef::Object(first), TargetRef::Object(second)],
        source,
        P0,
    );
    let mut runner = scenario.build();
    let mut initial_events = Vec::new();

    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("the first selected sacrifice reaches its replacement choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        !initial_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Sacrifice,
                source_id,
                ..
            } if *source_id == source
        )),
        "the terminal event must wait for the parked selected suffix"
    );

    let first_resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the first replacement delivers and re-parks the second sacrifice");
    assert!(matches!(
        first_resumed.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&first].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert!(
        !initial_events
            .iter()
            .chain(first_resumed.events.iter())
            .any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Sacrifice,
                    source_id,
                    ..
                } if *source_id == source
            )),
        "the tail must remain parked across a second replacement choice"
    );

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the remaining selected sacrifice and terminal tail resolve");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(runner.state().objects[&second].zone, Zone::Exile);
    assert_eq!(runner.state().last_effect_count, Some(2));
    assert_eq!(
        initial_events
            .iter()
            .chain(first_resumed.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Sacrifice,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the directly targeted sacrifice finishes exactly once after both replacements"
    );
}

/// W-163-B: the mandatory-all sacrifice fast path remains synchronous when no
/// replacement decision is needed.
#[test]
fn mandatory_all_sacrifice_completes_synchronously() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Mandatory-All Sacrifice Source", 1, 1)
        .as_enchantment()
        .id();
    let first = scenario
        .add_creature(P0, "Mandatory-All Sacrifice First", 1, 1)
        .id();
    let second = scenario
        .add_creature(P0, "Mandatory-All Sacrifice Second", 1, 1)
        .id();
    let ability = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature()),
            count: QuantityExpr::Fixed { value: 2 },
            min_count: 0,
        },
        vec![],
        source,
        P0,
    );
    let mut runner = scenario.build();
    let mut events = Vec::new();

    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("mandatory-all sacrifice resolves inline");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    assert_eq!(runner.state().last_effect_count, Some(2));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Sacrifice,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1
    );
}

/// W-163-C: a sacrifice selected through `EffectZoneChoice` keeps its tracked
/// set and chained tail behind the replacement boundary.
#[test]
fn effect_zone_sacrifice_replacement_preserves_tracked_set_and_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Effect-Zone Sacrifice Source", 1, 1)
        .as_enchantment()
        .id();
    let redirected = scenario
        .add_creature(P0, "Effect-Zone Sacrifice Redirect", 1, 1)
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    scenario.add_creature(P0, "Effect-Zone Sacrifice Unchosen", 1, 1);
    let ability = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature()),
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
        vec![],
        source,
        P0,
    )
    .sub_ability(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    ));
    let mut runner = scenario.build();
    let mut initial_events = Vec::new();

    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("sacrifice prompts for one creature");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::EffectZoneChoice {
            effect_kind: EffectKind::Sacrifice,
            ..
        }
    ));

    let paused = runner
        .act(GameAction::SelectCards {
            cards: vec![redirected],
        })
        .expect("selected sacrifice reaches its replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(runner.state().chain_tracked_set_id.is_none());
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("replacement delivery resumes the tracked-set publish and rider");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
    let tracked = runner
        .state()
        .tracked_object_sets
        .get(
            &runner
                .state()
                .chain_tracked_set_id
                .expect("selected sacrifice publishes a fresh tracked set after delivery"),
        )
        .expect("the published selected-sacrifice set exists");
    assert_eq!(tracked, &vec![redirected]);
    assert_eq!(runner.state().players[P0.0 as usize].life, 21);
    assert_eq!(
        initial_events
            .iter()
            .chain(paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Sacrifice,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the selected sacrifice emits one terminal event after its replacement settles"
    );
}

/// W-163-D: Exploit emits its per-creature event and terminal event only after
/// the replacement-delivered sacrifice has actually completed.
#[test]
fn exploit_replacement_preserves_creature_exploited_follow_up() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let exploiter = scenario
        .add_creature(P0, "Exploit Replacement Source", 1, 1)
        .id();
    let victim = scenario
        .add_creature(P0, "Exploit Replacement Victim", 1, 1)
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    let ability = ResolvedAbility::new(
        Effect::Exploit {
            target: TargetFilter::Any,
        },
        vec![TargetRef::Object(victim)],
        exploiter,
        P0,
    );
    let mut runner = scenario.build();
    let mut initial_events = Vec::new();

    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("exploit reaches the replacement choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(!initial_events
        .iter()
        .any(|event| matches!(event, GameEvent::CreatureExploited { .. })));

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("replacement delivery completes exploit");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(
        initial_events
            .iter()
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::CreatureExploited {
                    exploiter: event_exploiter,
                    sacrificed,
                } if *event_exploiter == exploiter && *sacrificed == victim
            ))
            .count(),
        1,
        "the exploit follow-up is emitted once after delivery"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Exploit,
                    source_id,
                    ..
                } if *source_id == exploiter
            ))
            .count(),
        1
    );
}

/// W-163-E: the terminal sweep of choose-and-sacrifice-rest keeps its complete
/// unchosen set and terminal event across a replacement choice.
#[test]
fn choose_and_sacrifice_rest_replacement_preserves_terminal_sweep() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Choose-and-Sacrifice-Rest Source", 1, 1)
        .as_enchantment()
        .id();
    let victim = scenario
        .add_creature(P0, "Choose-and-Sacrifice-Rest Victim", 1, 1)
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    let ability = ResolvedAbility::new(
        Effect::ChooseAndSacrificeRest {
            categories: vec![],
            chooser_scope: CategoryChooserScope::EachPlayerSelf,
            choose_filter: TargetFilter::Typed(TypedFilter::creature()),
            sacrifice_filter: TargetFilter::Typed(TypedFilter::creature()),
            total_power_cap: None,
            keeper_constraint: None,
        },
        vec![],
        source,
        P0,
    );
    let mut runner = scenario.build();
    let mut initial_events = Vec::new();

    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("terminal unchosen sweep reaches its replacement choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        !initial_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChooseAndSacrificeRest,
                source_id,
                ..
            } if *source_id == source
        )),
        "the terminal event must wait for the unchosen sacrifice delivery"
    );

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("replacement delivery finishes the unchosen sweep");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(runner.state().objects[&victim].zone, Zone::Exile);
    assert_eq!(
        initial_events
            .iter()
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ChooseAndSacrificeRest,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1
    );
}

/// W-164-A (red first): a hand card selected by an EffectZoneChoice must take
/// the Library replacement path before the choice's terminal effect and rider.
#[test]
fn effect_zone_put_on_top_hand_redirect_pauses_before_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Effect-Zone Put-On-Top Source", 1, 1)
        .id();
    let selected = scenario
        .add_spell_to_hand(P0, "Effect-Zone Put-On-Top Redirect", true)
        .id();
    let redirect_sources = [
        scenario
            .add_creature(P0, "Effect-Zone Put-On-Top To Graveyard", 0, 0)
            .as_enchantment()
            .id(),
        scenario
            .add_creature(P0, "Effect-Zone Put-On-Top To Exile", 0, 0)
            .as_enchantment()
            .id(),
    ];
    let ability = ResolvedAbility::new(
        Effect::PutAtLibraryPosition {
            target: TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
            ),
            count: QuantityExpr::Fixed { value: 1 },
            position: engine::types::ability::LibraryPosition::Top,
        },
        vec![],
        source,
        P0,
    )
    .sub_ability(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    ));
    let mut runner = scenario.build();
    for (redirect_source, destination) in redirect_sources
        .into_iter()
        .zip([Zone::Graveyard, Zone::Exile])
    {
        runner
            .state_mut()
            .objects
            .get_mut(&redirect_source)
            .expect("synthetic redirect source remains on the battlefield")
            .replacement_definitions = vec![redirect_moved_to(Zone::Library, destination)].into();
    }
    let mut initial_events = Vec::new();

    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("put-on-top prompts for the hand card");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::EffectZoneChoice {
            effect_kind: EffectKind::PutAtLibraryPosition,
            ..
        }
    ));

    let paused = runner
        .act(GameAction::SelectCards {
            cards: vec![selected],
        })
        .expect("selected hand card reaches the Library replacement choice");
    assert!(
        matches!(paused.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "expected a replacement choice, got {:?}",
        paused.waiting_for
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    assert!(
        !paused.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::PutAtLibraryPosition,
                source_id,
                ..
            } if *source_id == source
        )),
        "the terminal effect must remain parked behind the replacement choice"
    );

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the chosen replacement delivers the card and drains the tail");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P0.0 as usize].life, 21);
    assert_eq!(
        initial_events
            .iter()
            .chain(paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::PutAtLibraryPosition,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the terminal effect fires exactly once after replacement delivery"
    );
}

/// A resolver-created PutAtLibraryPosition choice must reject a repeated card
/// before mutating its source zone, while preserving selection-order placement
/// for a distinct follow-up choice.
#[test]
fn effect_zone_put_at_library_position_rejects_duplicate_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Put-On-Top Duplicate Selection Source", 1, 1)
        .id();
    let first = scenario
        .add_spell_to_hand(P0, "Put-On-Top Duplicate First", true)
        .id();
    let second = scenario
        .add_spell_to_hand(P0, "Put-On-Top Duplicate Second", true)
        .id();
    let marker = scenario
        .add_spell_to_library_top(P0, "Put-On-Top Duplicate Marker", true)
        .id();
    let ability = ResolvedAbility::new(
        Effect::PutAtLibraryPosition {
            target: TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
            ),
            count: QuantityExpr::Fixed { value: 2 },
            position: engine::types::ability::LibraryPosition::Top,
        },
        vec![],
        source,
        P0,
    );
    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library = im::vector![marker];
    let mut initial_events = Vec::new();

    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("PutAtLibraryPosition reaches its real resolver-created choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::EffectZoneChoice {
            effect_kind: EffectKind::PutAtLibraryPosition,
            ..
        }
    ));

    let duplicate = runner
        .act(GameAction::SelectCards {
            cards: vec![first, first],
        })
        .expect_err("a repeated card is not a legal resolution-time choice");
    assert!(
        matches!(duplicate, engine::game::EngineError::InvalidAction(message) if message == "Selected cards must be distinct")
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::EffectZoneChoice {
            effect_kind: EffectKind::PutAtLibraryPosition,
            ..
        }
    ));
    assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&second].zone, Zone::Hand);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![marker],
        "rejected input must not mutate the library"
    );

    let completed = runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .expect("distinct eligible cards resolve the existing choice");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(runner.state().objects[&first].zone, Zone::Library);
    assert_eq!(runner.state().objects[&second].zone, Zone::Library);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![first, second, marker],
        "distinct selection retains the requested top order"
    );
}

/// W-164-B: a mixed hand/library selection preserves the raw synchronous order
/// when only the hand members use the replacement-aware delivery path.
#[test]
fn effect_zone_put_at_library_position_mixed_sources_preserves_legacy_library_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Effect-Zone Mixed Put-On-Top Source", 1, 1)
        .id();
    let hand_first = scenario
        .add_spell_to_hand(P0, "Effect-Zone Mixed Hand First", true)
        .id();
    let library_first = scenario
        .add_spell_to_library_top(P0, "Effect-Zone Mixed Library First", true)
        .id();
    let hand_second = scenario
        .add_spell_to_hand(P0, "Effect-Zone Mixed Hand Second", true)
        .id();
    let library_second = scenario
        .add_spell_to_library_top(P0, "Effect-Zone Mixed Library Second", true)
        .id();
    let marker = scenario
        .add_spell_to_library_top(P0, "Effect-Zone Mixed Marker", true)
        .id();
    let mut base_state = scenario.build().state().clone();
    base_state.players[P0.0 as usize].library = im::vector![library_first, library_second, marker];

    for (position, expected) in [
        (
            engine::types::ability::LibraryPosition::Top,
            vec![
                hand_first,
                library_first,
                hand_second,
                library_second,
                marker,
            ],
        ),
        (
            engine::types::ability::LibraryPosition::Bottom,
            vec![
                marker,
                hand_first,
                library_first,
                hand_second,
                library_second,
            ],
        ),
        (
            engine::types::ability::LibraryPosition::NthFromTop { n: 2 },
            vec![
                hand_first,
                library_second,
                hand_second,
                library_first,
                marker,
            ],
        ),
    ] {
        let mut runner = GameRunner::from_state(base_state.clone());
        runner.state_mut().waiting_for = WaitingFor::EffectZoneChoice {
            player: P0,
            cards: vec![hand_first, library_first, hand_second, library_second],
            count: 4,
            min_count: 0,
            up_to: false,
            source_id: source,
            effect_kind: EffectKind::PutAtLibraryPosition,
            zone: Zone::Hand,
            destination: None,
            enter_tapped: EtbTapState::Unspecified,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            face_down_profile: None,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            count_param: 0,
            library_position: Some(position),
            is_cost_payment: false,
            enters_modified_if: None,
            duration: None,
        };

        let completed = runner
            .act(GameAction::SelectCards {
                cards: vec![hand_first, library_first, hand_second, library_second],
            })
            .expect("mixed-source placement resolves synchronously without replacements");
        assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(
            runner.state().players[P0.0 as usize]
                .library
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            expected,
            "the split delivery matches the prior raw selection-order placement"
        );
    }
}

/// W-168 (red first): a tracked-pile cloak must park before its detach/manifest
/// tail or `EffectResolved` when CR 616.1 requires an exile-redirect choice.
/// After the selected redirect settles, only the member that actually reached
/// exile may enter face down under the CR 701.58a cloak profile.
#[test]
fn cloak_tracked_exile_redirect_pauses_before_manifest_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Cloak Redirect Source", 1, 1)
        .id();
    let redirected = scenario
        .add_creature(P0, "Redirected Cloak Member", 2, 2)
        .id();
    let exiled = scenario.add_creature(P0, "Exiled Cloak Member", 3, 3).id();
    let redirect_sources = [
        scenario
            .add_creature(P0, "Cloak Exile To Hand", 0, 0)
            .as_enchantment()
            .id(),
        scenario
            .add_creature(P0, "Cloak Exile To Graveyard", 0, 0)
            .as_enchantment()
            .id(),
    ];

    let mut runner = scenario.build();
    for (redirect_source, redirected_to) in [
        (redirect_sources[0], Zone::Hand),
        (redirect_sources[1], Zone::Graveyard),
    ] {
        runner
            .state_mut()
            .objects
            .get_mut(&redirect_source)
            .expect("synthetic redirect source remains on the battlefield")
            .replacement_definitions = vec![redirect_moved_to(Zone::Exile, redirected_to)
            .valid_card(TargetFilter::SpecificObject { id: redirected })]
        .into();
    }
    let tracked_set = engine::types::identifiers::TrackedSetId(0);
    runner
        .state_mut()
        .tracked_object_sets
        .insert(tracked_set, vec![redirected, exiled]);
    runner.state_mut().chain_tracked_set_id = Some(tracked_set);
    let ability = ResolvedAbility::new(
        Effect::Cloak {
            target: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 0 },
            object_source: Some(TargetFilter::TrackedSet { id: tracked_set }),
            // CR 110.2a: P0-owned/P0-controlled pile — pins the None-snapshot
            // park/re-park path (owner default, behavior-identical).
            enters_under: None,
        },
        vec![],
        source,
        P0,
    );

    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("cloak reaches its replacement-safe exile batch");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&redirected].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&exiled].zone, Zone::Battlefield);
    assert!(
        !runner.state().objects[&redirected].face_down
            && !runner.state().objects[&exiled].face_down,
        "the manifest tail must not run before the redirect choice"
    );
    assert!(
        !initial_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Cloak,
                source_id,
                ..
            } if *source_id == source
        )),
        "Cloak must not resolve before the exile batch settles"
    );

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the selected exile redirect settles the tracked cloak batch");
    assert!(matches!(resumed.waiting_for, WaitingFor::Priority { .. }));
    assert!(matches!(
        runner.state().objects[&redirected].zone,
        Zone::Hand | Zone::Graveyard
    ));
    assert!(
        !runner.state().objects[&redirected].face_down,
        "a card redirected away from exile must not be re-manifested"
    );
    assert_eq!(runner.state().objects[&exiled].zone, Zone::Battlefield);
    assert!(
        runner.state().objects[&exiled].face_down,
        "the unredirected member must cloak from exile"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Cloak,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the settled cloak tail resolves exactly once"
    );
}

/// W-168-REG: an unredirected tracked-pile cloak remains synchronous and keeps
/// the prior two zone changes per member plus the face-down ward-{2} outcome.
#[test]
fn cloak_tracked_exile_delivery_stays_synchronous_and_cloaks_every_member() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Synchronous Cloak Source", 1, 1)
        .id();
    let first = scenario
        .add_creature(P0, "First Synchronous Cloak Member", 2, 2)
        .id();
    let second = scenario
        .add_creature(P0, "Second Synchronous Cloak Member", 3, 3)
        .id();

    let mut runner = scenario.build();
    let tracked_set = engine::types::identifiers::TrackedSetId(0);
    runner
        .state_mut()
        .tracked_object_sets
        .insert(tracked_set, vec![first, second]);
    runner.state_mut().chain_tracked_set_id = Some(tracked_set);
    let ability = ResolvedAbility::new(
        Effect::Cloak {
            target: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 0 },
            object_source: Some(TargetFilter::TrackedSet { id: tracked_set }),
            // CR 110.2a: P0-owned/P0-controlled pile — pins the None-snapshot
            // synchronous path (owner default, behavior-identical).
            enters_under: None,
        },
        vec![],
        source,
        P0,
    );

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("unredirected tracked cloak resolves synchronously");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    for member in [first, second] {
        let object = &runner.state().objects[&member];
        assert_eq!(object.zone, Zone::Battlefield);
        assert!(object.face_down);
        assert_eq!(object.power, Some(2));
        assert_eq!(object.toughness, Some(2));
        assert!(object.keywords.iter().any(|keyword| matches!(
            keyword,
            Keyword::Ward(cost) if *cost == engine::types::keywords::WardCost::Mana(ManaCost::generic(2))
        )));
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Exile,
                    ..
                } if [first, second].contains(object_id)
            ))
            .count(),
        2,
        "every tracked member has one battlefield-to-exile event"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Battlefield,
                    ..
                } if [first, second].contains(object_id)
            ))
            .count(),
        2,
        "every settled exile member has one face-down battlefield entry"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Cloak,
                    source_id,
                    ..
                } if *source_id == source
            ))
            .count(),
        1,
        "the synchronous cloak tail resolves exactly once"
    );
}

/// W-169 (red first): a revealed explore land's replaceable Library→Hand move
/// must settle before the Explore trigger event or a chained continuation runs.
#[test]
fn explore_land_redirect_pauses_before_explore_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let explorer = scenario
        .add_creature(P0, "Explore Redirect Source", 1, 1)
        .id();
    let land = scenario.add_card_to_library_top(P0, "Explore Redirect Land");
    for (name, destination) in [
        ("Explore Hand To Graveyard", Zone::Graveyard),
        ("Explore Hand To Exile", Zone::Exile),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Hand, destination));
    }

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&land)
        .expect("revealed land exists")
        .card_types
        .core_types
        .push(CoreType::Land);
    let mut ability = ResolvedAbility::new(Effect::Explore, vec![], explorer, P0);
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        explorer,
        P0,
    )));
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("explore reaches its replacement-safe land delivery");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&land].zone, Zone::Library);
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    assert!(
        !initial_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Explore,
                source_id,
                ..
            } if *source_id == explorer
        )),
        "the explore tail must not precede the replacement choice"
    );

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the selected redirect settles the explore land delivery");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P0.0 as usize].life, 21);
    assert_eq!(
        initial_events
            .iter()
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Explore,
                    source_id,
                    ..
                } if *source_id == explorer
            ))
            .count(),
        1,
        "a redirected land still completes exactly one explore"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::GainLife,
                    source_id,
                    ..
                } if *source_id == explorer
            ))
            .count(),
        1,
        "the chained continuation runs exactly once after the explore tail"
    );
}

/// W-169-REG: without a redirect, an explore land remains synchronous while the
/// nonland branch keeps its existing counter-then-choice behavior.
#[test]
fn explore_land_delivery_stays_synchronous_and_nonland_path_is_unchanged() {
    let mut land_scenario = GameScenario::new();
    land_scenario.at_phase(Phase::PreCombatMain);
    let land_explorer = land_scenario
        .add_creature(P0, "Synchronous Explore Land Source", 1, 1)
        .id();
    let land = land_scenario.add_card_to_library_top(P0, "Synchronous Explore Land");
    let mut land_runner = land_scenario.build();
    land_runner
        .state_mut()
        .objects
        .get_mut(&land)
        .expect("revealed land exists")
        .card_types
        .core_types
        .push(CoreType::Land);
    let land_ability = ResolvedAbility::new(Effect::Explore, vec![], land_explorer, P0);
    let mut land_events = Vec::new();
    resolve_ability_chain(land_runner.state_mut(), &land_ability, &mut land_events, 0)
        .expect("unredirected land explore resolves synchronously");

    assert!(matches!(
        land_runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(land_runner.state().objects[&land].zone, Zone::Hand);
    assert!(
        !land_runner.state().objects[&land_explorer]
            .counters
            .contains_key(&CounterType::Plus1Plus1),
        "a land explore does not add a +1/+1 counter"
    );

    let mut nonland_scenario = GameScenario::new();
    nonland_scenario.at_phase(Phase::PreCombatMain);
    let nonland_explorer = nonland_scenario
        .add_creature(P0, "Synchronous Explore Nonland Source", 1, 1)
        .id();
    let nonland = nonland_scenario
        .add_spell_to_library_top(P0, "Synchronous Explore Nonland", true)
        .id();
    let mut nonland_runner = nonland_scenario.build();
    let nonland_ability = ResolvedAbility::new(Effect::Explore, vec![], nonland_explorer, P0);
    let mut nonland_events = Vec::new();
    resolve_ability_chain(
        nonland_runner.state_mut(),
        &nonland_ability,
        &mut nonland_events,
        0,
    )
    .expect("nonland explore keeps its counter-then-choice path");

    assert_eq!(
        nonland_runner.state().objects[&nonland_explorer].counters[&CounterType::Plus1Plus1],
        1
    );
    assert!(matches!(
        nonland_runner.state().waiting_for,
        WaitingFor::DigChoice { ref cards, .. } if cards == &vec![nonland]
    ));
}

/// W-170 (red first): the no-host ReturnAsAura graveyard instruction must park
/// its resolution tail until CR 616.1 chooses and settles the replacement.
#[test]
fn return_as_aura_no_target_redirect_pauses_before_resolution_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario
        .add_creature(P0, "Return-As-Aura Redirect Host", 2, 2)
        .id();
    for name in [
        "Return-As-Aura Graveyard To Exile A",
        "Return-As-Aura Graveyard To Exile B",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Graveyard, Zone::Exile));
    }

    let mut runner = scenario.build();
    runner.state_mut().last_zone_changed_ids.push(host);
    let ability = ResolvedAbility::new(
        Effect::ReturnAsAura {
            enchant_filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
            grants: vec![ContinuousModification::RemoveAllAbilities],
        },
        vec![],
        host,
        P0,
    );
    let mut initial_events = Vec::new();
    engine::game::effects::return_as_aura::resolve(
        runner.state_mut(),
        &ability,
        &mut initial_events,
    )
    .expect("return-as-Aura reaches its replacement-safe no-host delivery");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
    assert!(
        !initial_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ReturnAsAura,
                source_id,
                ..
            } if *source_id == host
        )),
        "the ReturnAsAura tail must not precede the replacement choice"
    );

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the selected replacement settles the ReturnAsAura zone change");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&host].zone, Zone::Exile);
    assert_eq!(
        initial_events
            .iter()
            .chain(completed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ReturnAsAura,
                    source_id,
                    ..
                } if *source_id == host
            ))
            .count(),
        1,
        "the settled replacement delivery runs the ReturnAsAura tail exactly once"
    );
}

/// W-170-REG: the unredirected no-host path stays synchronous, strips the
/// returned host's live trigger snapshot, and the one-host attachment path is
/// unchanged.
#[test]
fn return_as_aura_no_target_stays_synchronous_and_attach_path_is_unchanged() {
    let mut no_target_scenario = GameScenario::new();
    no_target_scenario.at_phase(Phase::PreCombatMain);
    let no_target_host = no_target_scenario
        .add_creature(P0, "Return-As-Aura No-Target Host", 2, 2)
        .id();
    let mut no_target_runner = no_target_scenario.build();
    no_target_runner
        .state_mut()
        .objects
        .get_mut(&no_target_host)
        .expect("returned host exists")
        .trigger_definitions
        .push(TriggerDefinition::new(TriggerMode::ChangesZone));
    no_target_runner
        .state_mut()
        .last_zone_changed_ids
        .push(no_target_host);
    let no_target_ability = ResolvedAbility::new(
        Effect::ReturnAsAura {
            enchant_filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
            grants: vec![ContinuousModification::RemoveAllAbilities],
        },
        vec![],
        no_target_host,
        P0,
    );
    let mut no_target_events = Vec::new();
    engine::game::effects::return_as_aura::resolve(
        no_target_runner.state_mut(),
        &no_target_ability,
        &mut no_target_events,
    )
    .expect("unredirected no-host ReturnAsAura resolves synchronously");

    assert!(matches!(
        no_target_runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(
        no_target_runner.state().objects[&no_target_host].zone,
        Zone::Graveyard
    );
    let GameEvent::ZoneChanged { record, .. } = no_target_events
        .iter()
        .find(|event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if *object_id == no_target_host))
        .expect("the no-host move emits its zone-change record")
    else {
        panic!("expected a no-host ZoneChanged event");
    };
    assert!(
        record.trigger_definitions.is_empty(),
        "the no-host move snapshots the aura-stripped live trigger definitions"
    );
    assert_eq!(
        no_target_events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ReturnAsAura,
                    source_id,
                    ..
                } if *source_id == no_target_host
            ))
            .count(),
        1,
        "the synchronous no-host path resolves exactly once"
    );

    let mut attach_scenario = GameScenario::new();
    attach_scenario.at_phase(Phase::PreCombatMain);
    let attach_host = attach_scenario
        .add_creature(P0, "Return-As-Aura Attach Host", 2, 2)
        .id();
    let target = attach_scenario
        .add_creature(P0, "Return-As-Aura Attach Target", 1, 1)
        .id();
    let mut attach_runner = attach_scenario.build();
    attach_runner
        .state_mut()
        .last_zone_changed_ids
        .push(attach_host);
    let attach_ability = ResolvedAbility::new(
        Effect::ReturnAsAura {
            enchant_filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            grants: vec![],
        },
        vec![],
        attach_host,
        P0,
    );
    let mut attach_events = Vec::new();
    engine::game::effects::return_as_aura::resolve(
        attach_runner.state_mut(),
        &attach_ability,
        &mut attach_events,
    )
    .expect("one-host ReturnAsAura attaches synchronously");

    assert_eq!(
        attach_runner.state().objects[&attach_host].attached_to,
        Some(AttachTarget::Object(target))
    );
    assert_eq!(
        attach_runner.state().objects[&attach_host].zone,
        Zone::Battlefield
    );
    assert_eq!(
        attach_events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ReturnAsAura,
                    source_id,
                    ..
                } if *source_id == attach_host
            ))
            .count(),
        1,
        "the unchanged attach path resolves exactly once"
    );
}

/// W-171 (red first): accepting the CR 903.9a commander return must let competing
/// Command-destination redirects park their CR 616.1 ordering prompt. The selected
/// redirect genuinely puts the commander into exile, so the next SBA check correctly
/// offers one fresh return choice; declining that fresh choice proves the ledger stops
/// a duplicate prompt for the same exile stay.
#[test]
fn commander_zone_return_redirect_pauses_and_reoffers_only_for_fresh_exile_arrival() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let commander = scenario
        .add_creature_to_graveyard(P0, "Commander Return Redirect Witness", 2, 2)
        .id();
    scenario.with_commander(commander);
    for name in [
        "Commander Command To Exile Redirect A",
        "Commander Command To Exile Redirect B",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Command, Zone::Exile));
    }

    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    let mut setup_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        commander,
        Zone::Graveyard,
        &mut setup_events,
    );
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut setup_events);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CommanderZoneChoice {
            commander_id,
            current_zone: Zone::Graveyard,
            ..
        } if commander_id == commander
    ));

    let paused = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the commander return is valid");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(
        runner.state().objects[&commander].zone,
        Zone::Graveyard,
        "the commander must remain in its source zone while CR 616.1 is parked"
    );

    let settled = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the chosen redirect settles the commander return");
    assert_eq!(runner.state().objects[&commander].zone, Zone::Exile);
    assert!(matches!(
        settled.waiting_for,
        WaitingFor::CommanderZoneChoice {
            commander_id,
            current_zone: Zone::Exile,
            ..
        } if commander_id == commander
    ));

    let declined = runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the fresh exile return is valid");
    assert!(matches!(
        declined.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&commander].zone, Zone::Exile);
    assert!(
        runner
            .state()
            .commander_declined_zone_return
            .contains(&commander),
        "declining suppresses a duplicate offer while the commander stays in exile"
    );
}

/// W-171-REG: the ordinary accept path remains synchronous, and declining keeps
/// the commander in its zone with the existing same-stay ledger behavior.
#[test]
fn commander_zone_return_stays_synchronous_and_decline_is_unchanged() {
    let mut accept_scenario = GameScenario::new();
    accept_scenario.at_phase(Phase::PreCombatMain);
    let accepted_commander = accept_scenario
        .add_creature_to_graveyard(P0, "Synchronous Commander Return", 2, 2)
        .id();
    accept_scenario.with_commander(accepted_commander);
    let mut accept_runner = accept_scenario.build();
    accept_runner.state_mut().format_config.command_zone = true;
    let mut accept_setup_events = Vec::new();
    engine::game::zones::move_to_zone(
        accept_runner.state_mut(),
        accepted_commander,
        Zone::Graveyard,
        &mut accept_setup_events,
    );
    engine::game::sba::check_state_based_actions(
        accept_runner.state_mut(),
        &mut accept_setup_events,
    );

    let accepted = accept_runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("unredirected commander return is valid");
    assert!(matches!(
        accepted.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(
        accept_runner.state().objects[&accepted_commander].zone,
        Zone::Command
    );
    assert!(accepted.events.iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Graveyard),
            to: Zone::Command,
            ..
        } if *object_id == accepted_commander
    )));
    let mut recheck_events = Vec::new();
    engine::game::sba::check_state_based_actions(accept_runner.state_mut(), &mut recheck_events);
    assert!(matches!(
        accept_runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));

    let mut decline_scenario = GameScenario::new();
    decline_scenario.at_phase(Phase::PreCombatMain);
    let declined_commander = decline_scenario
        .add_creature_to_graveyard(P0, "Declined Commander Return", 2, 2)
        .id();
    decline_scenario.with_commander(declined_commander);
    let mut decline_runner = decline_scenario.build();
    decline_runner.state_mut().format_config.command_zone = true;
    let mut decline_setup_events = Vec::new();
    engine::game::zones::move_to_zone(
        decline_runner.state_mut(),
        declined_commander,
        Zone::Graveyard,
        &mut decline_setup_events,
    );
    engine::game::sba::check_state_based_actions(
        decline_runner.state_mut(),
        &mut decline_setup_events,
    );

    let declined = decline_runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the commander return is valid");
    assert!(matches!(
        declined.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(
        decline_runner.state().objects[&declined_commander].zone,
        Zone::Graveyard
    );
    assert!(
        decline_runner
            .state()
            .commander_declined_zone_return
            .contains(&declined_commander),
        "declining preserves the existing same-stay ledger behavior"
    );
}

/// W-173 (red first): CR 903.9b replaces a commander bounce before the Hand
/// arrival event. A Warped Devotion-shaped observer therefore cannot trigger
/// when the owner chooses the command zone, but does trigger after a decline.
#[test]
fn commander_hand_return_replaces_bounce_before_warped_devotion_can_observe_it() {
    let mut accept_scenario = GameScenario::new();
    accept_scenario.at_phase(Phase::PreCombatMain);
    let accepted_commander = accept_scenario
        .add_creature(P0, "Commander Hand Replacement Accept", 2, 2)
        .id();
    accept_scenario.with_commander(accepted_commander);
    let accepted_observer = accept_scenario
        .add_creature(P0, "Warped Devotion Witness", 0, 0)
        .as_enchantment()
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::Typed(TypedFilter::permanent()))
                .origin(Zone::Battlefield)
                .destination(Zone::Hand)
                .trigger_zones(vec![Zone::Battlefield])
                .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
        )
        .id();
    let mut accept_runner = accept_scenario.build();
    accept_runner.state_mut().format_config.command_zone = true;
    let mut accept_setup_events = Vec::new();
    engine::game::zones::move_to_zone(
        accept_runner.state_mut(),
        accepted_commander,
        Zone::Battlefield,
        &mut accept_setup_events,
    );
    let accept_bounce = ResolvedAbility::new(
        Effect::Bounce {
            target: TargetFilter::Any,
            destination: None,
            selection: BounceSelection::Targeted,
        },
        vec![TargetRef::Object(accepted_commander)],
        accepted_observer,
        P0,
    );
    let mut accept_events = Vec::new();
    resolve_ability_chain(
        accept_runner.state_mut(),
        &accept_bounce,
        &mut accept_events,
        0,
    )
    .expect("the bounce reaches the command-zone replacement choice");
    assert!(matches!(
        accept_runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(
        accept_runner.state().objects[&accepted_commander].zone,
        Zone::Battlefield,
        "the original Hand move stays proposed until CR 903.9b is chosen"
    );

    let accepted = accept_runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the CR 903.9b replacement is valid");
    assert_eq!(
        accept_runner.state().objects[&accepted_commander].zone,
        Zone::Command
    );
    assert!(
        !accepted.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to: Zone::Hand,
                ..
            } if *object_id == accepted_commander
        )),
        "accepting CR 903.9b emits no battlefield-to-Hand event"
    );
    assert!(
        !accept_runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == accepted_observer),
        "Warped Devotion cannot trigger when the commander never reaches Hand"
    );

    let mut decline_scenario = GameScenario::new();
    decline_scenario.at_phase(Phase::PreCombatMain);
    let declined_commander = decline_scenario
        .add_creature(P0, "Commander Hand Replacement Decline", 2, 2)
        .id();
    decline_scenario.with_commander(declined_commander);
    let declined_observer = decline_scenario
        .add_creature(P0, "Warped Devotion Decline Witness", 0, 0)
        .as_enchantment()
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::Typed(TypedFilter::permanent()))
                .origin(Zone::Battlefield)
                .destination(Zone::Hand)
                .trigger_zones(vec![Zone::Battlefield])
                .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
        )
        .id();
    let mut decline_runner = decline_scenario.build();
    decline_runner.state_mut().format_config.command_zone = true;
    let mut decline_setup_events = Vec::new();
    engine::game::zones::move_to_zone(
        decline_runner.state_mut(),
        declined_commander,
        Zone::Battlefield,
        &mut decline_setup_events,
    );
    let decline_bounce = ResolvedAbility::new(
        Effect::Bounce {
            target: TargetFilter::Any,
            destination: None,
            selection: BounceSelection::Targeted,
        },
        vec![TargetRef::Object(declined_commander)],
        declined_observer,
        P0,
    );
    let mut decline_events = Vec::new();
    resolve_ability_chain(
        decline_runner.state_mut(),
        &decline_bounce,
        &mut decline_events,
        0,
    )
    .expect("the bounce reaches the command-zone replacement choice");
    let declined = decline_runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining the CR 903.9b replacement is valid");
    assert_eq!(
        decline_runner.state().objects[&declined_commander].zone,
        Zone::Hand
    );
    assert!(declined.events.iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Battlefield),
            to: Zone::Hand,
            ..
        } if *object_id == declined_commander
    )));
    assert!(
        decline_runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == declined_observer),
        "Warped Devotion triggers after a declined Hand replacement"
    );
}

/// W-173: CR 903.9b also replaces a library return before both the library
/// arrival observer (Wan Shi Tong-shaped) and the normal library shuffle tail.
#[test]
fn commander_library_return_skips_library_arrival_and_shuffle_when_replaced() {
    let mut accept_scenario = GameScenario::new();
    accept_scenario.at_phase(Phase::PreCombatMain);
    let accepted_commander = accept_scenario
        .add_creature(P0, "Commander Library Replacement Accept", 2, 2)
        .id();
    accept_scenario.with_commander(accepted_commander);
    let accepted_observer = accept_scenario
        .add_creature(P0, "Wan Shi Tong Library Witness", 0, 0)
        .as_enchantment()
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::ChangesZoneAll)
                .destination(Zone::Library)
                .trigger_zones(vec![Zone::Battlefield])
                .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
        )
        .id();
    let mut accept_runner = accept_scenario.build();
    accept_runner.state_mut().format_config.command_zone = true;
    let mut accept_setup_events = Vec::new();
    engine::game::zones::move_to_zone(
        accept_runner.state_mut(),
        accepted_commander,
        Zone::Battlefield,
        &mut accept_setup_events,
    );
    let accept_library_return = ResolvedAbility::new(
        Effect::Bounce {
            target: TargetFilter::Any,
            destination: Some(Zone::Library),
            selection: BounceSelection::Targeted,
        },
        vec![TargetRef::Object(accepted_commander)],
        accepted_observer,
        P0,
    );
    let mut accept_events = Vec::new();
    resolve_ability_chain(
        accept_runner.state_mut(),
        &accept_library_return,
        &mut accept_events,
        0,
    )
    .expect("the library return reaches the command-zone replacement choice");
    let accepted = accept_runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the command-zone replacement is valid");
    assert_eq!(
        accept_runner.state().objects[&accepted_commander].zone,
        Zone::Command
    );
    assert!(
        !accepted.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Library,
                ..
            } if *object_id == accepted_commander
        )),
        "accepting CR 903.9b emits no library-arrival event"
    );
    assert!(
        !accepted.events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::ShuffledLibrary,
                ..
            }
        )),
        "a replaced library move must not run the delivery shuffle tail"
    );
    assert!(
        !accept_runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == accepted_observer),
        "a Wan Shi Tong-shaped observer cannot trigger without a library arrival"
    );

    let mut decline_scenario = GameScenario::new();
    decline_scenario.at_phase(Phase::PreCombatMain);
    let declined_commander = decline_scenario
        .add_creature(P0, "Commander Library Replacement Decline", 2, 2)
        .id();
    decline_scenario.with_commander(declined_commander);
    let declined_observer = decline_scenario
        .add_creature(P0, "Wan Shi Tong Decline Witness", 0, 0)
        .as_enchantment()
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::ChangesZoneAll)
                .destination(Zone::Library)
                .trigger_zones(vec![Zone::Battlefield])
                .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
        )
        .id();
    let mut decline_runner = decline_scenario.build();
    decline_runner.state_mut().format_config.command_zone = true;
    let mut decline_setup_events = Vec::new();
    engine::game::zones::move_to_zone(
        decline_runner.state_mut(),
        declined_commander,
        Zone::Battlefield,
        &mut decline_setup_events,
    );
    let decline_library_return = ResolvedAbility::new(
        Effect::Bounce {
            target: TargetFilter::Any,
            destination: Some(Zone::Library),
            selection: BounceSelection::Targeted,
        },
        vec![TargetRef::Object(declined_commander)],
        declined_observer,
        P0,
    );
    let mut decline_events = Vec::new();
    resolve_ability_chain(
        decline_runner.state_mut(),
        &decline_library_return,
        &mut decline_events,
        0,
    )
    .expect("the library return reaches the command-zone replacement choice");
    let declined = decline_runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining the command-zone replacement is valid");
    assert_eq!(
        decline_runner.state().objects[&declined_commander].zone,
        Zone::Library
    );
    assert!(declined.events.iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged {
            object_id,
            to: Zone::Library,
            ..
        } if *object_id == declined_commander
    )));
    assert!(declined.events.iter().any(|event| matches!(
        event,
        GameEvent::PlayerPerformedAction {
            action: PlayerActionKind::ShuffledLibrary,
            ..
        }
    )));
    assert!(
        decline_runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == declined_observer),
        "the observer triggers after a real library arrival"
    );
}

/// W-173: CR 903.9b is the CR 614.5 exception. After it changes a Hand move
/// to Command, a competing Command-to-Hand redirect modifies the same event
/// and legally offers the commander replacement again.
#[test]
fn commander_hand_return_rearms_after_a_competing_redirect_recreates_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let commander = scenario
        .add_creature(P0, "Commander Repeat-Replacement Witness", 2, 2)
        .id();
    scenario.with_commander(commander);
    let redirect_source = scenario
        .add_creature(P0, "Command To Hand Redirect Witness", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Command, Zone::Hand))
        .id();
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    let mut setup_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        commander,
        Zone::Battlefield,
        &mut setup_events,
    );
    let bounce = ResolvedAbility::new(
        Effect::Bounce {
            target: TargetFilter::Any,
            destination: None,
            selection: BounceSelection::Targeted,
        },
        vec![TargetRef::Object(commander)],
        redirect_source,
        P0,
    );
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &bounce, &mut events, 0)
        .expect("the initial commander replacement choice is available");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice {
            candidate_count: 2,
            ..
        }
    ));
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        unreachable!("the optional prompt was asserted above");
    };
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.source_id)
            .collect::<Vec<_>>(),
        vec![commander, commander],
        "the lone initial candidate is the commander's accept/decline choice: {candidates:?}"
    );

    let reapplied = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the initial commander replacement is valid");
    assert!(
        matches!(
            reapplied.waiting_for,
            WaitingFor::ReplacementChoice {
                candidate_count: 2,
                ..
            }
        ),
        "the modified event must re-offer the commander replacement"
    );
    assert_eq!(
        runner.state().objects[&commander].zone,
        Zone::Battlefield,
        "the re-armed replacement parks before the modified event is delivered"
    );

    let settled = runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining the re-armed commander replacement is valid");
    assert!(matches!(
        settled.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(
        runner.state().objects[&commander].zone,
        Zone::Hand,
        "the competing redirect's recreated Hand destination is delivered after the second decline"
    );
}

/// W-173: In a material CR 616.1 ordering prompt, selecting the commander
/// rule chooses its turn to apply; its CR 903.9b "may" choice remains separate.
#[test]
fn commander_hand_return_keeps_its_may_choice_inside_cr_616_ordering() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let commander = scenario
        .add_creature(P0, "Commander Ordering Witness", 2, 2)
        .id();
    scenario.with_commander(commander);
    let redirect_source = scenario
        .add_creature(P0, "Hand To Command Redirect Witness", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Hand, Zone::Command))
        .id();
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    let mut setup_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        commander,
        Zone::Battlefield,
        &mut setup_events,
    );
    let bounce = ResolvedAbility::new(
        Effect::Bounce {
            target: TargetFilter::Any,
            destination: None,
            selection: BounceSelection::Targeted,
        },
        vec![TargetRef::Object(commander)],
        redirect_source,
        P0,
    );
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &bounce, &mut events, 0)
        .expect("the material replacement ordering choice is available");
    let WaitingFor::ReplacementChoice {
        candidate_count,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("CR 616.1 must present a replacement ordering prompt");
    };
    assert_eq!(*candidate_count, 2);
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.source_id)
            .collect::<Vec<_>>(),
        vec![commander, redirect_source]
    );

    let selected = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("selecting the commander rule's place in the ordering is valid");
    assert!(matches!(
        selected.waiting_for,
        WaitingFor::ReplacementChoice {
            candidate_count: 2,
            ..
        }
    ));
    assert_eq!(
        runner.state().objects[&commander].zone,
        Zone::Battlefield,
        "ordering selection must not silently accept the optional commander replacement"
    );

    let declined = runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining the commander rule after ordering it is valid");
    assert!(matches!(
        declined.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(
        runner.state().objects[&commander].zone,
        Zone::Command,
        "the still-applicable Hand-to-Command redirect resolves after the decline"
    );
}

// ---------------------------------------------------------------------------
// The accepted-pause families: `optional` (`OptionalEffectChoice`) and
// `optional_for` (`OpponentMayChoice`).
//
// These are the first shapes the complete classifier accepts that do NOT
// complete synchronously. The occurrence pauses mid-body inside the completed
// mana frame, its continuation carrier holds the frame's own resume root, and
// the answering action's readiness hook in `engine_payment_choices` partitions
// the stored emission batches, runs the accepted tail, and resumes that exact
// mana frame once.
//
// The widening owns a real behaviour delta: baseline `is_triggered_mana_ability`
// returns true for an all-mana `optional` body, so baseline resolved it inline
// and left `waiting_for` set with no continuation at all.
// ---------------------------------------------------------------------------

/// M's accepted body, made optional: still one colorless mana, still targetless
/// and nonmodal, so `build_target_slots` stays empty and the baseline
/// acceptance gate still holds — the ONLY difference from
/// `mana_added_bonus_mana_body` is the "you may".
fn optional_mana_added_bonus_observer(scenario: &mut GameScenario, label: &str) -> ObjectId {
    scenario
        .add_creature(P0, label, 0, 0)
        .as_enchantment()
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::ManaAdded)
                .execute(
                    AbilityDefinition::new(AbilityKind::Database, mana_added_bonus_mana_body())
                        .optional(),
                )
                .constraint(TriggerConstraint::OncePerTurn)
                .trigger_zones(vec![Zone::Battlefield]),
        )
        .id()
}

/// The pause itself, asserted before it is answered: N's activation returns
/// `OptionalEffectChoice`, not a wait belonging to the payment, and NOTHING of
/// the frame has been released — M owns no pip, no queue slot and no stack
/// entry, while the frame's two ordinary observers are already queued
/// undispatched behind it.
fn assert_optional_pause_is_open_and_nothing_released(
    runner: &GameRunner,
    bonus_m: ObjectId,
    source_n: ObjectId,
    observer_o: ObjectId,
    waiting_for: &WaitingFor,
    life_before: i32,
) {
    assert!(
        matches!(waiting_for, WaitingFor::OptionalEffectChoice { .. }),
        "the accepted body's own pause is the action's wait, got {waiting_for:?}"
    );
    assert_eq!(
        pips_produced_by(runner.state(), bonus_m),
        0,
        "an accepted occurrence produces nothing while its own decision is open"
    );
    assert_eq!(
        pips_produced_by(runner.state(), source_n),
        1,
        "N's base production already happened — the frame really is mid-settlement"
    );
    assert!(
        !runner
            .state()
            .deferred_triggers
            .iter()
            .any(|context| context.pending.source_id == bonus_m),
        "a paused accepted occurrence is never queued on the ordinary authority"
    );
    assert_exactly_two_contexts_queued_and_unstacked(
        runner.state(),
        source_n,
        observer_o,
        life_before,
    );
}

/// Accept: the resumed body's mana reaches the pool inside the answering
/// action, and readiness then resumes the suspended mana frame exactly once —
/// the frame's own release group is still exactly N's reflexive plus O.
#[test]
fn accepted_optional_mana_body_pauses_the_frame_and_resumes_it_on_accept() {
    let NoPauseFixture {
        mut scenario,
        source_n,
        observer_o,
    } = no_pause_mana_fixture(NoPauseColorAxis::Fixed);
    let bonus_m = optional_mana_added_bonus_observer(&mut scenario, "M Optional Mana-Added Bonus");
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N is activated directly from Priority");
    assert_optional_pause_is_open_and_nothing_released(
        &runner,
        bonus_m,
        source_n,
        observer_o,
        &paused.waiting_for,
        life_before,
    );

    let resumed = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P0 accepts the accepted occurrence's own optional body");
    assert_eq!(
        pips_produced_by(runner.state(), bonus_m),
        1,
        "the resumed accepted body produces exactly once (CR 605.4a)"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        1,
        "and the bonus is spendable from the answering action onward"
    );
    assert!(
        !runner
            .state()
            .deferred_triggers
            .iter()
            .any(|context| context.pending.source_id == bonus_m),
        "resumption never demotes the occurrence to the ordinary queue"
    );
    assert_single_group_of_two_then_resolve_for_seven(
        &mut runner,
        &resumed.waiting_for,
        source_n,
        observer_o,
        life_before,
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. }))
            .count(),
        0,
        "M never occupied the stack across the pause"
    );
}

/// Decline: the same resumption runs, the same single group is released, and
/// the ONLY difference is that M produced nothing. This is the pure-axis
/// control for the accept row — it proves the frame's resumption is owned by
/// readiness rather than by the body having produced mana.
#[test]
fn accepted_optional_mana_body_declined_still_resumes_the_frame_once() {
    let NoPauseFixture {
        mut scenario,
        source_n,
        observer_o,
    } = no_pause_mana_fixture(NoPauseColorAxis::Fixed);
    let bonus_m = optional_mana_added_bonus_observer(&mut scenario, "M Optional Mana-Added Bonus");
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N is activated directly from Priority");
    assert_optional_pause_is_open_and_nothing_released(
        &runner,
        bonus_m,
        source_n,
        observer_o,
        &paused.waiting_for,
        life_before,
    );

    let resumed = runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("P0 declines the accepted occurrence's own optional body");
    assert_eq!(
        pips_produced_by(runner.state(), bonus_m),
        0,
        "a declined body produces nothing"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        0,
        "and nothing colorless reaches the pool by any other route"
    );
    assert_single_group_of_two_then_resolve_for_seven(
        &mut runner,
        &resumed.waiting_for,
        source_n,
        observer_o,
        life_before,
    );
}

/// The same accepted body under `optional_for` (`OpponentMayChoice`): the "you
/// may" is routed to the opponent, so the pause belongs to a NON-ACTIVATOR while
/// the mana frame it suspends belongs to P0.
///
/// This is the second accepted-pause family and the second readiness hook in
/// `engine_payment_choices`. Its reducer arm returns the `ActionResult`
/// directly, without the ordinary post-action pipeline — see the assertion on
/// the resumed wait, which records exactly where full settled-Priority
/// convergence is still owed.
fn opponent_may_mana_added_bonus_observer(scenario: &mut GameScenario, label: &str) -> ObjectId {
    let mut body =
        AbilityDefinition::new(AbilityKind::Database, mana_added_bonus_mana_body()).optional();
    body.optional_for = Some(OpponentMayScope::AnyOpponent);
    scenario
        .add_creature(P0, label, 0, 0)
        .as_enchantment()
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::ManaAdded)
                .execute(body)
                .constraint(TriggerConstraint::OncePerTurn)
                .trigger_zones(vec![Zone::Battlefield]),
        )
        .id()
}

#[test]
fn accepted_opponent_may_mana_body_pauses_on_the_nonactivator_and_resumes_the_frame() {
    let NoPauseFixture {
        mut scenario,
        source_n,
        observer_o,
    } = no_pause_mana_fixture(NoPauseColorAxis::Fixed);
    let bonus_m =
        opponent_may_mana_added_bonus_observer(&mut scenario, "M Opponent-May Mana-Added Bonus");
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: source_n,
            ability_index: 0,
        })
        .expect("N is activated directly from Priority");
    let WaitingFor::OpponentMayChoice { player, .. } = paused.waiting_for else {
        panic!(
            "the accepted body's opponent-may pause is the action's wait, got {:?}",
            paused.waiting_for
        );
    };
    assert_eq!(
        player, P1,
        "CR 608.2d: the decision belongs to the opponent, not to the frame's activator"
    );
    assert_eq!(
        pips_produced_by(runner.state(), bonus_m),
        0,
        "nothing is produced while the opponent's decision is open"
    );
    assert_exactly_two_contexts_queued_and_unstacked(
        runner.state(),
        source_n,
        observer_o,
        life_before,
    );

    let resumed = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P1 accepts");
    assert_eq!(
        pips_produced_by(runner.state(), bonus_m),
        1,
        "the resumed accepted body produces exactly once, into its own controller's pool"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        1,
        "CR 605.4a: the bonus belongs to M's controller, not to the accepting opponent"
    );
    assert_single_group_of_two_then_resolve_for_seven(
        &mut runner,
        &resumed.waiting_for,
        source_n,
        observer_o,
        life_before,
    );
}
