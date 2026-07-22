use engine::database::synthesis::synthesize_plot;
use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::AttachTarget;
use engine::game::mana_abilities::activate_mana_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle_cost::parse_oracle_cost;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, BounceSelection, CardPlayMode, CardSelectionMode,
    CastFromZoneDriver, CastingPermission, CategoryChooserScope, ChoiceType, Chooser,
    ContinuousModification, DigSource, DiscardSelfScope, Effect, EffectKind, FilterProp,
    ForEachCategoryAction, IterationCategory, ManaContribution, ManaProduction, ModalChoice,
    QuantityExpr, QuantityRef, ReplacementDefinition, ReplacementMode, ResolvedAbility,
    SacrificeCost, SpellCastingOption, TargetFilter, TargetRef, TargetSelectionMode,
    TriggerDefinition, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{
    BatchCompletion, CastPaymentMode, CollectEvidenceResume, GameState,
    ManaAbilityCostParentLifecycle, ManaAbilityCostResolutionMode, ManaAbilityResume, PayCostKind,
    PendingCast, PendingCostMoveResume, PendingReplacement, StackEntryKind, WaitingFor,
};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
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
            reveal: true,
            enter_tapped: false,
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
            reveal: true,
            enter_tapped: false,
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
            reveal: false,
            enter_tapped: false,
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
            reveal: false,
            enter_tapped: false,
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

/// W-R2: A `RevealRestPile` already deferred behind a kept-card replacement can
/// itself start a Library-bottom batch that re-pauses while draining. Its cleanup
/// must survive both pause boundaries and publish exactly once at the true end.
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
            reveal: true,
            enter_tapped: false,
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
    assert!(matches!(
        runner
            .state()
            .active_batch_delivery()
            .and_then(|pending| pending.completion.as_ref()),
        Some(BatchCompletion::RevealRestPile { .. })
    ));

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
    assert!(matches!(
        first_rest_park.completion.as_ref(),
        Some(BatchCompletion::RevealRestPile { .. })
    ));
    assert!(runner.state().chain_tracked_set_id.is_none());

    let reparking = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the rest batch re-parks on its remaining library placement");
    assert!(matches!(
        reparking.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner
            .state()
            .active_batch_delivery()
            .and_then(|pending| pending.completion.as_ref()),
        Some(BatchCompletion::RevealRestPile { .. })
    ));
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
    assert_eq!(tracked, &vec![kept]);
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

    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::OrderTriggers { ref triggers, .. } if triggers.len() == 2
    ));
    assert!(runner.state().pending_cost_move_resume.is_none());
    let ordered = runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("both observer triggers remain orderable after the cost settles");
    assert!(matches!(
        ordered.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(
        runner.state().stack.len(),
        2,
        "each actual observer trigger is collected exactly once, not once per pause and resume"
    );
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
            reveal: true,
            enter_tapped: false,
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
    assert_eq!(tracked, &vec![kept]);
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
            reveal: true,
            enter_tapped: false,
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
