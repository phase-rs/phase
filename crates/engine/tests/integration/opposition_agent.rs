//! End-to-end coverage for Opposition Agent's coupled search-control,
//! found-card replacement, exile play permission, and nested replacement resume.

use engine::ai_support::{legal_actions_for_viewer, legal_actions_full};
use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CardPlayMode, CastingPermission, ControlWindow, Effect,
    FilterProp, ManaSpendPermission, PlayerFilter, QuantityExpr, ReplacementDefinition,
    ReplacementMode, ReplacementPlayerScope, SearchFoundModifier, SearchSelectionConstraint,
    StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, PendingSearchFoundContinuation, ScheduledTurnControl,
    ScheduledTurnControlLifecycle, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::SearchFoundDisposition;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::{ProhibitionScope, StaticMode};
use engine::types::zones::{EtbTapState, Zone};

const OPPOSITION_AGENT: &str = "Flash\n\
You control your opponents while they're searching their libraries.\n\
While an opponent is searching their library, they exile each card they find. You may play those cards for as long as they remain exiled, and you may spend mana as though it were mana of any color to cast them.";

const TEST_TUTOR: &str =
    "Search your library for a card, put that card into your hand, then shuffle.";
const REVEAL_TUTOR: &str =
    "Search your library for a card, reveal it, put it into your hand, then shuffle.";
const P2: PlayerId = PlayerId(2);

fn search_found_execute(modifier: SearchFoundModifier) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ApplySearchFoundReplacement { modifier },
    )
}

fn setup() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let agent = scenario
        .add_creature_from_oracle(P0, "Opposition Agent", 3, 2, OPPOSITION_AGENT)
        .id();
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let found = scenario
        .add_spell_to_library_top(P1, "Found Red Instant", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text("You gain 1 life.")
        .id();
    scenario.add_card_to_library_top(P1, "Library Filler");
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, agent, tutor, found)
}

fn resolve_tutor_to_search(runner: &mut GameRunner, tutor: ObjectId) {
    let outcome = runner.cast(tutor).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::SearchChoice { player: P1, .. }
    ));
}

fn found_permission(state: &engine::types::game_state::GameState, found: ObjectId) -> bool {
    state.objects[&found]
        .casting_permissions
        .iter()
        .any(|permission| {
            matches!(
                permission,
                CastingPermission::PlayFromExile {
                    granted_to: P0,
                    mana_spend_permission: Some(ManaSpendPermission::AnyColor),
                    ..
                }
            )
        })
}

fn setup_optional_search_found() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Optional Search Replacement", 1, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::SearchFound)
                .mode(ReplacementMode::Optional { decline: None })
                .valid_player(ReplacementPlayerScope::AnyPlayer)
                .execute(search_found_execute(SearchFoundModifier {
                    destination: Zone::Exile,
                    play_mode: CardPlayMode::Play,
                    mana_spend_permission: Some(ManaSpendPermission::AnyColor),
                })),
        )
        .id();
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let found = scenario
        .add_spell_to_library_top(P1, "Found Card", true)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, source, tutor, found)
}

fn setup_two_optional_search_found() -> (GameRunner, [ObjectId; 2], ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_creature(P0, "First Optional Search Replacement", 1, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::SearchFound)
                .mode(ReplacementMode::Optional { decline: None })
                .valid_player(ReplacementPlayerScope::AnyPlayer)
                .description("Exile with the first source".to_string())
                .execute(search_found_execute(SearchFoundModifier {
                    destination: Zone::Exile,
                    play_mode: CardPlayMode::Play,
                    mana_spend_permission: Some(ManaSpendPermission::AnyColor),
                })),
        )
        .id();
    let second = scenario
        .add_creature(P1, "Second Optional Search Replacement", 1, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::SearchFound)
                .mode(ReplacementMode::Optional { decline: None })
                .valid_player(ReplacementPlayerScope::AnyPlayer)
                .description("Exile with the second source".to_string())
                .execute(search_found_execute(SearchFoundModifier {
                    destination: Zone::Exile,
                    play_mode: CardPlayMode::Play,
                    mana_spend_permission: None,
                })),
        )
        .id();
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let found = scenario
        .add_spell_to_library_top(P1, "Found Card", true)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, [first, second], tutor, found)
}

fn setup_two_mandatory_search_found() -> (GameRunner, [ObjectId; 2], ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_creature_from_oracle(P0, "First Opposition Agent", 3, 2, OPPOSITION_AGENT)
        .id();
    let second = scenario
        .add_creature_from_oracle(P0, "Second Opposition Agent", 3, 2, OPPOSITION_AGENT)
        .id();
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let found = scenario
        .add_spell_to_library_top(P1, "Found Card", true)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, [first, second], tutor, found)
}

fn setup_mandatory_and_optional_search_found(
) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mandatory = scenario
        .add_creature_from_oracle(P0, "Opposition Agent", 3, 2, OPPOSITION_AGENT)
        .id();
    let optional = scenario
        .add_creature(P1, "Optional Search Replacement", 1, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::SearchFound)
                .mode(ReplacementMode::Optional { decline: None })
                .valid_player(ReplacementPlayerScope::AnyPlayer)
                .description("Exile with the optional source".to_string())
                .execute(search_found_execute(SearchFoundModifier {
                    destination: Zone::Exile,
                    play_mode: CardPlayMode::Play,
                    mana_spend_permission: None,
                })),
        )
        .id();
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let found = scenario
        .add_spell_to_library_top(P1, "Found Card", true)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, mandatory, optional, tutor, found)
}

fn search_ability(
    count: i32,
    source_zones: Vec<Zone>,
    player_scope: Option<PlayerFilter>,
    delivery: Option<Zone>,
) -> AbilityDefinition {
    let mut search = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SearchLibrary {
            filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                FilterProp::Named {
                    name: "Found Card".to_string(),
                },
            ])),
            count: QuantityExpr::Fixed { value: count },
            reveal: false,
            target_player: None,
            selection_constraint: SearchSelectionConstraint::None,
            split: None,
            source_zones,
        },
    );
    if let Some(destination) = delivery {
        search = search.sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination,
                target: TargetFilter::ParentTarget,
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
    }
    if let Some(scope) = player_scope {
        search = search.player_scope(scope);
    }
    search
}

/// CR 723.2 + CR 723.4 + CR 723.5 + CR 701.23a + CR 609.4b: the semantic
/// searcher remains P1, but P0 is the only authorized submitter. The found card
/// is exiled instead of entering P1's hand, and P0 can cast its red cost using
/// only blue mana.
#[test]
fn controls_search_exiles_found_card_and_spends_mana_as_any_color() {
    let (mut runner, agent, tutor, found) = setup();
    resolve_tutor_to_search(&mut runner, tutor);

    let full = legal_actions_full(runner.state());
    let controller_view = legal_actions_for_viewer(runner.state(), P0);
    let searched_player_view = legal_actions_for_viewer(runner.state(), P1);
    assert_eq!(controller_view, full, "P0 must receive P1's search actions");
    assert!(
        searched_player_view.0.is_empty(),
        "P1 must not receive actions while P0 controls this search"
    );

    let selection = GameAction::SelectCards { cards: vec![found] };
    assert!(
        apply(runner.state_mut(), P1, selection.clone()).is_err(),
        "the searched player cannot submit the controller's decision"
    );
    runner
        .act(selection)
        .expect("the search controller submits the found-card choice");

    assert_eq!(runner.state().objects[&found].zone, Zone::Exile);
    assert!(found_permission(runner.state(), found));

    let mut departure_events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        agent,
        Zone::Graveyard,
        &mut departure_events,
    );
    assert!(
        found_permission(runner.state(), found),
        "the bound permission survives its source leaving the battlefield"
    );

    // The tutor finishes with P1 holding priority. P1 passes, then P0 casts the
    // red instant from exile using the single blue mana in P0's pool.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ) {
        runner
            .act(GameAction::PassPriority)
            .expect("P1 passes priority to P0");
    }
    let card_id = runner.state().objects[&found].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: found,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Opposition Agent permission casts red spell with blue mana");
    assert_eq!(runner.state().objects[&found].zone, Zone::Stack);
}

/// CR 611.3d + CR 609.4b: Opposition Agent's persistent play permission and
/// mana-spend concession are exercised through the real targeted Expedite cast
/// pipeline after priority passes explicitly from P1 to P0.
#[test]
fn casts_verbatim_expedite_after_p1_to_p0_priority_handoff() {
    const EXPEDITE: &str = "Target creature gains haste until end of turn. Draw a card.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Opposition Agent", 3, 2, OPPOSITION_AGENT);
    let target = scenario.add_creature(P0, "Expedite Target", 2, 2).id();
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let expedite = scenario
        .add_spell_to_library_top(P1, "Expedite", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text(EXPEDITE)
        .id();
    scenario.add_card_to_library_top(P1, "P1 Library Filler");
    scenario.add_card_to_library_top(P0, "P0 Draw Card");
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards {
            cards: vec![expedite],
        })
        .expect("P0 controls P1's search and selects Expedite");
    assert_eq!(runner.state().objects[&expedite].zone, Zone::Exile);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority explicitly to P0");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));

    let outcome = runner.cast(expedite).target_object(target).resolve();

    outcome.assert_zone(&[expedite], Zone::Graveyard);
    outcome.assert_hand_drawn(P0, 1);
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "the blue mana paid Expedite's red cost"
    );
    assert!(
        outcome.state().objects[&target].has_keyword(&Keyword::Haste),
        "Expedite's targeted haste effect must resolve"
    );
}

/// Paired hostile control: the exile-play permission alone does not make blue
/// mana payable for Expedite's red cost when the any-color concession is absent.
#[test]
fn expedited_cast_with_blue_mana_fails_without_mana_spend_permission() {
    const EXPEDITE: &str = "Target creature gains haste until end of turn. Draw a card.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Search Controller Without Mana Concession", 3, 2)
        .with_static_definition(StaticDefinition::new(
            StaticMode::ControlPlayersDuringOwnLibrarySearch {
                who: ProhibitionScope::Opponents,
            },
        ))
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::SearchFound)
                .valid_player(ReplacementPlayerScope::Opponent)
                .execute(search_found_execute(SearchFoundModifier {
                    destination: Zone::Exile,
                    play_mode: CardPlayMode::Play,
                    mana_spend_permission: None,
                })),
        );
    let target = scenario.add_creature(P0, "Expedite Target", 2, 2).id();
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let expedite = scenario
        .add_spell_to_library_top(P1, "Expedite", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text(EXPEDITE)
        .id();
    scenario.add_card_to_library_top(P1, "P1 Library Filler");
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards {
            cards: vec![expedite],
        })
        .expect("P0 controls P1's search and selects Expedite");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority explicitly to P0");

    assert!(
        runner
            .cast(expedite)
            .target_object(target)
            .try_resolve()
            .is_err(),
        "blue mana must not pay Expedite's red cost without the concession"
    );
    assert_eq!(runner.state().objects[&expedite].zone, Zone::Exile);
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 1);
    assert!(!runner.state().objects[&target].has_keyword(&Keyword::Haste));
}

/// CR 614.6 + CR 701.23a: Opposition Agent replaces the found-card event
/// before the tutor's reveal instruction observes that set. The exiled card is
/// public by zone, but it is not reported or retained as a card revealed by the
/// original tutor instruction.
#[test]
fn replacement_modified_card_is_absent_from_reveal_tutor_event_and_memory() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Opposition Agent", 3, 2, OPPOSITION_AGENT);
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Reveal Tutor", false, REVEAL_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let found = scenario
        .add_spell_to_library_top(P1, "Found Card", true)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    resolve_tutor_to_search(&mut runner, tutor);
    let result = runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("the controlled reveal tutor resolves through SearchFound");

    assert_eq!(runner.state().objects[&found].zone, Zone::Exile);
    assert!(
        !result.events.iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { card_ids, .. } if card_ids.contains(&found)
        )),
        "the modified card must not reach reveal-event observers"
    );
    assert!(
        !runner.state().last_revealed_ids.contains(&found)
            && !runner.state().revealed_cards.contains(&found),
        "the modified card must not remain in reveal visibility memory"
    );
}

/// CR 614.1a + CR 701.23a: SearchFound replacement matching is a generic
/// own-library-search seam. It applies according to `valid_player` even when no
/// Opposition Agent player-control static exists; the semantic searcher still
/// submits the search choice.
#[test]
fn generic_any_player_search_found_replacement_does_not_require_search_control_static() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Generic Search Replacement", 1, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::SearchFound)
                .valid_player(ReplacementPlayerScope::AnyPlayer)
                .execute(search_found_execute(SearchFoundModifier {
                    destination: Zone::Exile,
                    play_mode: CardPlayMode::Play,
                    mana_spend_permission: None,
                })),
        );
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let found = scenario
        .add_spell_to_library_top(P1, "Found Card", true)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    resolve_tutor_to_search(&mut runner, tutor);
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(runner.state(), P1),
        P1,
        "without a player-control static, the searcher remains authorized"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("generic SearchFound replacement applies to an own-library search");

    assert_eq!(runner.state().objects[&found].zone, Zone::Exile);
    assert!(runner.state().objects[&found]
        .casting_permissions
        .iter()
        .any(|permission| matches!(
            permission,
            CastingPermission::PlayFromExile {
                granted_to: P0,
                mana_spend_permission: None,
                ..
            }
        )));
}

/// CR 614.5: accepting an optional generic SearchFound replacement consumes
/// its single opportunity for this event using the exact modifier and source
/// incarnation snapshotted when the choice was offered, even if the live source
/// disappears before resume.
#[test]
fn optional_search_found_accept_uses_bound_candidate_after_source_departure() {
    let (mut runner, source, tutor, found) = setup_optional_search_found();
    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("the optional SearchFound replacement offers its choice");
    let WaitingFor::ReplacementChoice {
        candidate_count,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("optional SearchFound must surface a replacement choice");
    };
    assert_eq!(*candidate_count, 2);
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[1].description, "Decline");
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .expect("replacement remains parked")
            .search_found_candidates
            .len(),
        1,
        "the optional prompt must carry one bound candidate"
    );

    runner.state_mut().objects.remove(&source);
    runner
        .state_mut()
        .battlefield
        .retain(|object_id| *object_id != source);
    let accepted = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept resumes from the bound candidate without a live source");

    assert_eq!(runner.state().objects[&found].zone, Zone::Exile);
    assert!(accepted.events.iter().any(|event| matches!(
        event,
        GameEvent::ReplacementApplied {
            source_id,
            event_type,
        } if *source_id == source && event_type == "SearchFound"
    )));
    assert!(runner.state().objects[&found]
        .casting_permissions
        .iter()
        .any(|permission| matches!(
            permission,
            CastingPermission::PlayFromExile {
                granted_to: P0,
                source_id: Some(permission_source),
                mana_spend_permission: Some(ManaSpendPermission::AnyColor),
                ..
            } if *permission_source == source
        )));
}

/// CR 614.5: the definition's `may` permits declining this SearchFound
/// replacement. Recording its single opportunity leaves the original event
/// intact without offering the same effect again.
#[test]
fn optional_search_found_decline_delivers_original_without_reoffering() {
    let (mut runner, _source, tutor, found) = setup_optional_search_found();
    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("the optional SearchFound replacement offers its choice");
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline resumes the original found-card delivery");

    assert_eq!(runner.state().objects[&found].zone, Zone::Hand);
    assert!(runner.state().objects[&found]
        .casting_permissions
        .is_empty());
    assert!(runner.state().pending_replacement.is_none());
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
}

#[test]
fn out_of_range_single_optional_search_found_choice_is_rejected_without_state_change() {
    let (mut runner, _source, tutor, found) = setup_optional_search_found();
    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("the optional SearchFound replacement offers accept and decline");
    let before = serde_json::to_string(runner.state()).expect("serialize parked optional prompt");

    let error = runner
        .act(GameAction::ChooseReplacement { index: 2 })
        .expect_err("index 2 is outside the two-option accept/decline prompt");

    assert!(error.to_string().contains("outside 0..2"));
    assert_eq!(
        serde_json::to_string(runner.state()).expect("serialize rejected optional prompt"),
        before,
        "a hostile optional index must not consume or mutate the parked event"
    );
    assert_eq!(runner.state().objects[&found].zone, Zone::Library);
}

#[test]
fn out_of_range_multi_mandatory_search_found_choice_is_rejected_without_state_change() {
    let (mut runner, _sources, tutor, found) = setup_two_mandatory_search_found();
    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("two mandatory SearchFound replacements offer an ordering choice");
    let before = serde_json::to_string(runner.state()).expect("serialize mandatory prompt");

    let error = runner
        .act(GameAction::ChooseReplacement { index: 2 })
        .expect_err("index equal to mandatory candidate count cannot mean original delivery");

    assert!(error.to_string().contains("outside 0..2"));
    assert_eq!(
        serde_json::to_string(runner.state()).expect("serialize rejected mandatory prompt"),
        before
    );
    assert_eq!(runner.state().objects[&found].zone, Zone::Library);
}

/// CR 616.1: one mandatory and one optional SearchFound replacement expose
/// only their exact source choices. The optional definition's `may` does not
/// create an original-delivery branch while a mandatory effect still applies.
#[test]
fn mandatory_and_optional_search_found_prompt_preserves_provenance_without_original_branch() {
    let (mut runner, mandatory, optional, tutor, found) =
        setup_mandatory_and_optional_search_found();
    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("the production tutor reaches mixed SearchFound ordering");

    let WaitingFor::ReplacementChoice {
        candidate_count,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("mixed SearchFound replacements must offer an ordering choice");
    };
    assert_eq!(*candidate_count, 2);
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.source_id)
            .collect::<Vec<_>>(),
        vec![mandatory, optional]
    );
    assert!(candidates
        .iter()
        .all(|candidate| candidate.source_id != ObjectId(0)));
    let optional_index = candidates
        .iter()
        .position(|candidate| candidate.source_id == optional)
        .expect("optional source keeps its exact prompt provenance");

    let outcome = runner
        .act(GameAction::ChooseReplacement {
            index: optional_index,
        })
        .expect("the selected optional source replaces the found-card delivery");
    assert_eq!(runner.state().objects[&found].zone, Zone::Exile);
    assert!(runner.state().objects[&found]
        .casting_permissions
        .iter()
        .any(|permission| matches!(
            permission,
            CastingPermission::PlayFromExile {
                granted_to: P1,
                source_id: Some(source_id),
                mana_spend_permission: None,
                ..
            } if *source_id == optional
        )));
    assert!(outcome.events.iter().any(|event| matches!(
        event,
        GameEvent::ReplacementApplied {
            source_id,
            event_type,
        } if *source_id == optional && event_type == "SearchFound"
    )));
}

#[test]
fn out_of_range_mixed_search_found_choice_is_rejected_without_state_change() {
    let (mut runner, _mandatory, _optional, tutor, found) =
        setup_mandatory_and_optional_search_found();
    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("mixed SearchFound replacements offer an ordering choice");
    let before = serde_json::to_string(runner.state()).expect("serialize mixed prompt");

    let error = runner
        .act(GameAction::ChooseReplacement { index: 2 })
        .expect_err("mixed prompts cannot expose the all-optional original branch");

    assert!(error.to_string().contains("outside 0..2"));
    assert_eq!(
        serde_json::to_string(runner.state()).expect("serialize rejected mixed prompt"),
        before
    );
    assert_eq!(runner.state().objects[&found].zone, Zone::Library);
}

/// CR 614.5 + CR 616.1: when every applicable found-card replacement definition
/// says `may`, the affected player may decline the complete ordered set. Each
/// effect gets its single opportunity, then the unchanged found-card event
/// reaches the tutor's printed hand destination without re-offering a source.
#[test]
fn two_optional_search_found_replacements_can_all_be_declined() {
    let (mut runner, sources, tutor, found) = setup_two_optional_search_found();
    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("two optional SearchFound sources offer an ordering choice");

    let WaitingFor::ReplacementChoice {
        candidate_count,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("two optional SearchFound sources must surface a replacement choice");
    };
    assert_eq!(*candidate_count, 3);
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.source_id)
            .collect::<Vec<_>>(),
        vec![sources[0], sources[1], ObjectId(0)]
    );
    assert_eq!(
        candidates[2].description,
        "Use the original found-card destination"
    );
    assert!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .expect("choice remains parked")
            .search_found_candidates
            .iter()
            .all(|candidate| candidate.is_optional),
        "optional status must remain bound per offered candidate"
    );

    let checkpoint = serde_json::to_string(runner.state())
        .expect("the multi-optional SearchFound prompt serializes");
    *runner.state_mut() = serde_json::from_str(&checkpoint)
        .expect("the multi-optional SearchFound prompt restores with optionality intact");

    runner
        .act(GameAction::ChooseReplacement { index: 2 })
        .expect("the original-destination branch declines every optional source");

    assert_eq!(runner.state().objects[&found].zone, Zone::Hand);
    assert!(runner.state().objects[&found]
        .casting_permissions
        .is_empty());
    assert!(runner.state().pending_replacement.is_none());
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
}

/// CR 614.5 + CR 616.1: selecting one of two optional SearchFound sources accepts
/// that exact frozen candidate. Its modifier and permission provenance win, and
/// the unselected source is no longer applicable after the event leaves the
/// original disposition.
#[test]
fn two_optional_search_found_replacements_apply_only_the_selected_source() {
    let (mut runner, sources, tutor, found) = setup_two_optional_search_found();
    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("two optional SearchFound sources offer an ordering choice");
    let selected_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.source_id == sources[1])
            .expect("the second source is an offered candidate"),
        other => panic!("expected replacement choice, got {other:?}"),
    };

    let outcome = runner
        .act(GameAction::ChooseReplacement {
            index: selected_index,
        })
        .expect("selecting one optional source resumes the found-card event");

    assert_eq!(runner.state().objects[&found].zone, Zone::Exile);
    assert!(runner.state().objects[&found]
        .casting_permissions
        .iter()
        .any(|permission| matches!(
            permission,
            CastingPermission::PlayFromExile {
                granted_to: P1,
                source_id: Some(source_id),
                mana_spend_permission: None,
                ..
            } if *source_id == sources[1]
        )));
    assert!(!runner.state().objects[&found]
        .casting_permissions
        .iter()
        .any(|permission| matches!(
            permission,
            CastingPermission::PlayFromExile {
                source_id: Some(source_id),
                ..
            } if *source_id == sources[0]
        )));
    assert_eq!(
        outcome
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::ReplacementApplied {
                    event_type,
                    ..
                } if event_type == "SearchFound"
            ))
            .count(),
        1
    );
    assert!(outcome.events.iter().any(|event| matches!(
        event,
        GameEvent::ReplacementApplied {
            source_id,
            event_type,
        } if *source_id == sources[1] && event_type == "SearchFound"
    )));
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
}

/// CR 723.1a: an active turn-control effect and a live search-control static
/// are ordered by creation time. A future scheduled control is never active.
#[test]
fn newest_active_player_control_effect_authorizes_search_decisions() {
    let (mut runner, agent, tutor, _found) = setup();
    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .state_mut()
        .objects
        .get_mut(&agent)
        .unwrap()
        .timestamp = 50;
    runner.state_mut().turn_decision_controller = Some(P1);
    runner
        .state_mut()
        .scheduled_turn_controls
        .push(ScheduledTurnControl {
            target_player: P1,
            controller: P1,
            timestamp: 100,
            lifecycle: ScheduledTurnControlLifecycle::Active,
            grant_extra_turn_after: false,
            window: ControlWindow::NextTurn,
        });
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(runner.state(), P1),
        P1,
        "the newer active turn-control effect must beat the older Agent"
    );

    runner.state_mut().scheduled_turn_controls[0].timestamp = 25;
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(runner.state(), P1),
        P0,
        "the newer Agent static must beat the older active turn control"
    );

    runner.state_mut().scheduled_turn_controls[0].timestamp = 100;
    runner.state_mut().scheduled_turn_controls[0].lifecycle =
        ScheduledTurnControlLifecycle::Pending;
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(runner.state(), P1),
        P0,
        "a future scheduled control must not authorize before its window begins"
    );
}

#[test]
fn found_land_can_be_played_after_the_search() {
    let (mut runner, _agent, tutor, found) = setup();
    {
        let card = runner.state_mut().objects.get_mut(&found).unwrap();
        card.card_types.core_types.clear();
        card.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Land);
    }
    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("the controlled search exiles the found land");
    assert!(found_permission(runner.state(), found));

    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    let card_id = runner.state().objects[&found].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: found,
            card_id,
        })
        .expect("CardPlayMode::Play permits the exiled land play");
    assert_eq!(runner.state().objects[&found].zone, Zone::Battlefield);
}

/// CR 723.1a / CR 723.5: control is evaluated live. If the Agent leaves during
/// the search, authority immediately falls back to the searched player and the
/// now-absent replacement cannot exile the found card.
#[test]
fn source_departure_during_search_hands_control_back_to_searcher() {
    let (mut runner, agent, tutor, found) = setup();
    resolve_tutor_to_search(&mut runner, tutor);

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), agent, Zone::Graveyard, &mut events);
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(runner.state(), P1),
        P1
    );

    apply(
        runner.state_mut(),
        P1,
        GameAction::SelectCards { cards: vec![found] },
    )
    .expect("P1 regains authority after Opposition Agent leaves");
    assert_eq!(runner.state().objects[&found].zone, Zone::Hand);
    assert!(!found_permission(runner.state(), found));
}

/// CR 101.4 + CR 701.23a + CR 616.1: a scoped APNAP search that pauses on
/// SearchFound ordering must resume the exact serialized scoped protocol and
/// advance to the next searcher before simultaneous delivery.
#[test]
fn scoped_apnap_search_resumes_through_serialized_search_found_choice() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let first_agent = scenario
        .add_creature_from_oracle(P0, "First Opposition Agent", 3, 2, OPPOSITION_AGENT)
        .id();
    let second_agent = scenario
        .add_creature_from_oracle(P0, "Second Opposition Agent", 3, 2, OPPOSITION_AGENT)
        .id();
    let search = scenario
        .add_spell_to_hand(P0, "Scoped Search", false)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(search_ability(
            1,
            vec![Zone::Library],
            Some(PlayerFilter::Opponent),
            Some(Zone::Battlefield),
        ))
        .id();
    let p1_found = scenario
        .add_spell_to_library_top(P1, "Found Card", true)
        .id();
    let p2_found = scenario
        .add_spell_to_library_top(P2, "Found Card", true)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&first_agent)
        .unwrap()
        .timestamp = 10;
    runner
        .state_mut()
        .objects
        .get_mut(&second_agent)
        .unwrap()
        .timestamp = 20;

    let outcome = runner.cast(search).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::SearchChoice { player: P1, cards, .. } if cards.contains(&p1_found)
    ));
    drop(outcome);

    runner
        .act(GameAction::SelectCards {
            cards: vec![p1_found],
        })
        .expect("the controlling Agent submits P1's scoped search");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    assert!(runner
        .state()
        .pending_search_found_batch
        .as_ref()
        .is_some_and(|batch| {
            matches!(&batch.continuation, PendingSearchFoundContinuation::Scoped)
        }));

    let serialized = serde_json::to_string(runner.state()).expect("serialize scoped pause");
    *runner.state_mut() = serde_json::from_str(&serialized).expect("restore scoped pause");
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("SearchFound choice resumes the serialized scoped search");
    assert_eq!(runner.state().objects[&p1_found].zone, Zone::Exile);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::SearchChoice { player: P2, ref cards, .. } if cards.contains(&p2_found)
    ));

    runner
        .act(GameAction::SelectCards {
            cards: vec![p2_found],
        })
        .expect("the controlling Agent submits P2's later APNAP search");
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the second SearchFound choice completes the scoped protocol");
    assert_eq!(runner.state().objects[&p2_found].zone, Zone::Exile);
    assert!(runner.state().pending_scoped_library_search.is_none());
    assert!(runner.state().pending_search_found_batch.is_none());
}

/// A malformed restored scoped state is a recoverable action error: the live
/// replacement prompt and exact SearchFound batch remain parked for a repaired
/// state to retry instead of panicking or dropping the selected card.
#[test]
fn malformed_serialized_scoped_resume_returns_error_without_stranding_batch() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "First Agent", 3, 2, OPPOSITION_AGENT);
    scenario.add_creature_from_oracle(P0, "Second Agent", 3, 2, OPPOSITION_AGENT);
    let search = scenario
        .add_spell_to_hand(P0, "Scoped Search", false)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(search_ability(
            1,
            vec![Zone::Library],
            Some(PlayerFilter::Opponent),
            Some(Zone::Battlefield),
        ))
        .id();
    let found = scenario
        .add_spell_to_library_top(P1, "Found Card", true)
        .id();
    scenario.add_spell_to_library_top(P2, "Found Card", true);
    let mut runner = scenario.build();
    let outcome = runner.cast(search).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::SearchChoice { player: P1, .. }
    ));
    drop(outcome);
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("two Agents park SearchFound ordering");

    let serialized = serde_json::to_string(runner.state()).expect("serialize scoped pause");
    *runner.state_mut() = serde_json::from_str(&serialized).expect("restore scoped pause");
    runner.state_mut().pending_scoped_library_search = None;
    let error = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect_err("missing scoped continuation must be a controlled engine error");
    assert!(error.to_string().contains("missing scoped search resume"));
    assert_eq!(runner.state().objects[&found].zone, Zone::Library);
    assert!(runner.state().pending_search_found_batch.is_some());
    assert!(runner.state().pending_replacement.is_some());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
}

/// CR 701.23a: when Library remains an effective zone, search control and the
/// selected Agent replacement cover every found card from the mixed-zone
/// instruction, not only the card physically found in the library.
#[test]
fn mixed_zone_search_exiles_hand_graveyard_and_library_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Opposition Agent", 3, 2, OPPOSITION_AGENT);
    let search = scenario
        .add_spell_to_hand(P1, "Mixed Search", false)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(search_ability(
            3,
            vec![Zone::Hand, Zone::Graveyard, Zone::Library],
            None,
            None,
        ))
        .id();
    let hand = scenario.add_spell_to_hand(P1, "Found Card", true).id();
    let graveyard = scenario.add_spell_to_graveyard(P1, "Found Card", true).id();
    let library = scenario
        .add_spell_to_library_top(P1, "Found Card", true)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let outcome = runner.cast(search).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::SearchChoice { player: P1, cards, .. }
            if [hand, graveyard, library].iter().all(|id| cards.contains(id))
    ));
    drop(outcome);
    runner
        .act(GameAction::SelectCards {
            cards: vec![hand, graveyard, library],
        })
        .expect("the Agent controls the mixed-zone found set");
    for found in [hand, graveyard, library] {
        assert_eq!(runner.state().objects[&found].zone, Zone::Exile);
        assert!(found_permission(runner.state(), found));
    }
}

/// CR 609.3 + CR 701.23a: if Library is muzzled out of a mixed-zone search,
/// the nonlibrary search still reaches a real SearchChoice, but Opposition
/// Agent neither controls that choice nor replaces the selected card.
#[test]
fn library_muzzled_mixed_zone_search_reaches_nonlibrary_choice_without_agent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Opposition Agent", 3, 2, OPPOSITION_AGENT);
    scenario
        .add_creature(P0, "Search Muzzle", 0, 1)
        .with_static(StaticMode::CantSearchLibrary {
            cause: ProhibitionScope::Opponents,
        });
    let search = scenario
        .add_spell_to_hand(P1, "Mixed Search", false)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(search_ability(
            1,
            vec![Zone::Hand, Zone::Graveyard, Zone::Library],
            None,
            None,
        ))
        .id();
    let hand = scenario.add_spell_to_hand(P1, "Found Card", true).id();
    let graveyard = scenario.add_spell_to_graveyard(P1, "Found Card", true).id();
    let library = scenario
        .add_spell_to_library_top(P1, "Found Card", true)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let outcome = runner.cast(search).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::SearchChoice { player: P1, cards, .. }
            if cards.contains(&hand) && cards.contains(&graveyard) && !cards.contains(&library)
    ));
    assert!(outcome.state().library_search_control.is_none());
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(outcome.state(), P1),
        P1
    );
    drop(outcome);
    runner
        .act(GameAction::SelectCards { cards: vec![hand] })
        .expect("P1 retains the nonlibrary search decision");
    assert_eq!(runner.state().objects[&hand].zone, Zone::Hand);
    assert!(!found_permission(runner.state(), hand));
}

/// CR 616.1: the found card's owner (the searched player in this fixture)
/// chooses the replacement identity while the newest Agent controls that
/// choice; the selected source, not the controlling Agent, determines the
/// persistent permission's grantee and provenance.
#[test]
fn two_agents_preserve_selected_replacement_id_and_permission_grantee() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let older = scenario
        .add_creature_from_oracle(P0, "Older Opposition Agent", 3, 2, OPPOSITION_AGENT)
        .id();
    let newer = scenario
        .add_creature_from_oracle(P2, "Newer Opposition Agent", 3, 2, OPPOSITION_AGENT)
        .id();
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let found = scenario
        .add_spell_to_library_top(P1, "Found Card", true)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&older)
        .unwrap()
        .timestamp = 10;
    runner
        .state_mut()
        .objects
        .get_mut(&newer)
        .unwrap()
        .timestamp = 20;
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let outcome = runner.cast(tutor).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::SearchChoice { player: P1, .. }
    ));
    drop(outcome);
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(runner.state(), P1),
        P2,
        "the newest Agent controls P1's search decisions"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("P2 submits the choice while controlling P1");

    let candidates = &runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("two Agents require replacement ordering")
        .search_found_candidates;
    let older_index = candidates
        .iter()
        .position(|candidate| candidate.replacement_id.source == older)
        .expect("older Agent candidate is preserved by ReplacementId");
    let selected_id = candidates[older_index].replacement_id;
    assert_eq!(selected_id.source, older);
    assert_eq!(candidates[older_index].modifier.granted_to, P0);

    runner
        .act(GameAction::ChooseReplacement { index: older_index })
        .expect("P2 submits P1's selection of the older Agent replacement");
    assert_eq!(runner.state().objects[&found].zone, Zone::Exile);
    assert!(runner.state().objects[&found]
        .casting_permissions
        .iter()
        .any(|permission| matches!(
            permission,
            CastingPermission::PlayFromExile {
                granted_to: P0,
                source_id: Some(source_id),
                ..
            } if *source_id == selected_id.source
        )));
    assert!(!runner.state().objects[&found]
        .casting_permissions
        .iter()
        .any(|permission| matches!(
            permission,
            CastingPermission::PlayFromExile { granted_to: P2, .. }
        )));
}

fn optional_exile_to_graveyard_redirect() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .mode(ReplacementMode::Optional { decline: None })
        .destination_zone(Zone::Exile)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Graveyard,
                target: TargetFilter::Any,
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

/// CR 616.1: Opposition Agent's SearchFound replacement can synchronously
/// produce a Library→Exile zone event that has its own replacement choice. The
/// exact search batch must survive that nested pause, and declining the redirect
/// must grant permission only after the card actually reaches exile.
#[test]
fn nested_zone_replacement_pause_resumes_search_and_grants_permission() {
    let (mut runner, _agent, tutor, found) = setup();
    let redirect = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(90_001),
        P0,
        "Optional Exile Redirect".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&redirect)
        .expect("redirect source exists")
        .replacement_definitions
        .push(optional_exile_to_graveyard_redirect());

    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("the controlled search selection reaches the nested replacement");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(runner.state().pending_search_found_batch.is_some());
    assert_eq!(runner.state().objects[&found].zone, Zone::Library);

    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining the zone redirect resumes the saved search batch");
    assert_eq!(runner.state().objects[&found].zone, Zone::Exile);
    assert!(found_permission(runner.state(), found));
    assert!(runner.state().pending_search_found_batch.is_none());
    assert!(runner.state().pending_batch_deliveries.is_none());

    // The replacement pipeline's accepted SearchFound event remains bound, not
    // reverted to an original survivor that the tutor could put into P1's hand.
    assert!(!matches!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .map(|pending| &pending.proposed),
        Some(engine::types::proposed_event::ProposedEvent::SearchFound {
            disposition: SearchFoundDisposition::Original,
            ..
        })
    ));
}

/// CR 400.2 + CR 723.4: while a SearchFound exile is paused inside a nested
/// zone-change replacement, the completion's hidden-library object id is
/// visible only to the searcher and that player's search controller.
#[test]
fn nested_search_found_completion_redacts_hidden_object_from_third_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Opposition Agent", 3, 2, OPPOSITION_AGENT);
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let found = scenario
        .add_spell_to_library_top(P1, "Private Found Card", true)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    let redirect = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(90_003),
        P0,
        "Optional Exile Redirect".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&redirect)
        .expect("redirect source exists")
        .replacement_definitions
        .push(optional_exile_to_graveyard_redirect());

    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("the selected card reaches the nested zone replacement pause");

    let completion_object = |viewer| {
        let view = filter_state_for_viewer(runner.state(), viewer);
        match view
            .pending_batch_deliveries
            .expect("nested pause retains its completion")
            .completion
            .expect("nested pause carries SearchFound completion")
        {
            engine::types::game_state::BatchCompletion::SearchFoundZoneDelivery {
                object_id,
                ..
            } => object_id,
            other => panic!("unexpected nested completion: {other:?}"),
        }
    };
    assert_eq!(completion_object(P0), found);
    assert_eq!(completion_object(P1), found);
    assert_eq!(completion_object(P2), ObjectId(0));

    let authoritative_object = match runner
        .state()
        .pending_batch_deliveries
        .as_ref()
        .and_then(|pending| pending.completion.as_ref())
        .expect("authoritative nested completion remains parked")
    {
        engine::types::game_state::BatchCompletion::SearchFoundZoneDelivery {
            object_id, ..
        } => *object_id,
        other => panic!("unexpected authoritative completion: {other:?}"),
    };
    assert_eq!(authoritative_object, found);
}

/// CR 616.1 + CR 608.2c: a two-card found set can pause independently for
/// each replaced card's nested zone move. Normal search finalization runs once
/// after the exact suffix drains.
#[test]
fn multi_card_search_resumes_across_two_nested_zone_replacement_pauses() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Opposition Agent", 3, 2, OPPOSITION_AGENT);
    let tutor = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Double Tutor",
            false,
            "Search your library for two cards, put them into your hand, then shuffle.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let first = scenario
        .add_spell_to_library_top(P1, "First Found", true)
        .from_oracle_text("You gain 1 life.")
        .id();
    let second = scenario
        .add_spell_to_library_top(P1, "Second Found", true)
        .from_oracle_text("You gain 1 life.")
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    let redirect = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(90_002),
        P0,
        "Optional Exile Redirect".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&redirect)
        .unwrap()
        .replacement_definitions
        .push(optional_exile_to_graveyard_redirect());

    resolve_tutor_to_search(&mut runner, tutor);
    runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .expect("controlled two-card search reaches first nested pause");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("first decline resumes exact suffix");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("second decline completes the search continuation");

    for found in [first, second] {
        assert_eq!(runner.state().objects[&found].zone, Zone::Exile);
        assert!(found_permission(runner.state(), found));
    }
    assert!(runner.state().pending_search_found_batch.is_none());
    assert!(runner.state().pending_continuation.is_none());
}
