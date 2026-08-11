//! Regression for GitHub issue #4253 — Sanar, Innovative First-Year's Vivid
//! ability revealed nonlands into hand before the per-color exile loop, so
//! `ForEachCategoryExile` saw an empty Library pool.
//!
//! CR 701.20b: revealed cards stay in the library until an effect moves them.
//! Sanar's "for each of those colors, you may exile a card of that color from
//! among the revealed cards" requires the reveal-until step to leave the pile
//! in the library.
//!
//! This file also owns the runtime coverage for the follow-on defect ("exiling
//! more than one card at a time isn't working"): Sanar's trailing "you may cast
//! the exiled cards this turn" exiled and granted a cast permission to EVERY
//! revealed card instead of only the per-color picks. Two seams were wrong:
//!
//! 1. the parser did not treat `ForEachCategory { action: ExileFromPool }` as a
//!    tracked-set publisher, so the cast anaphor stayed `ParentTarget`; and
//! 2. `cast_from_zone::resolve` read `ability.targets` unconditionally, so even
//!    a tracked-set-bound cast step saw whatever the chain seam had injected
//!    (for Sanar, the whole reveal window).
//!
//! Because seam 2 is shared by every `CastFromZone` with a tracked-set filter
//! (51 cards), the general building-block fixtures live here too rather than in
//! a new top-level test binary.

use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, CardPlayMode, CastFromZoneDriver, CastingPermission, Chooser, ControllerRef,
    Effect, ForEachCategoryAction, IterationCategory, QuantityExpr, QuantityRef, ResolvedAbility,
    RevealUntilDisposition, TargetFilter, ThisWayCause, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::{EtbTapState, Zone};

const SANAR_VIVID_ORACLE: &str = "Reveal cards from the top of your library until you reveal X \
nonland cards, where X is the number of colors among permanents you control. For each of those \
colors, you may exile a card of that color from among the revealed cards. Then shuffle. You may \
cast the exiled cards this turn.";

fn distinct_colors_count() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::DistinctColorsAmongPermanents {
            filter: TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You)),
        },
    }
}

fn sanar_vivid_chain(source: ObjectId) -> ResolvedAbility {
    let mut ability = ResolvedAbility::new(
        Effect::RevealUntil {
            player: TargetFilter::Controller,
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Non(Box::new(TypeFilter::Land))],
                controller: None,
                properties: vec![],
            }),
            count: distinct_colors_count(),
            matched_disposition: RevealUntilDisposition::RevealOnly,
            kept_destination: Zone::Library,
            rest_destination: Zone::Library,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            kept_optional_to: None,
            enters_under: None,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::ForEachCategory {
            category: IterationCategory::Color,
            chooser: Chooser::Controller,
            action: ForEachCategoryAction::ExileFromPool {
                zone: Zone::Library,
                up_to: true,
            },
        },
        vec![],
        source,
        P0,
    )));
    ability
}

#[test]
fn sanar_vivid_parses_library_reveal_then_per_color_exile() {
    let def = parse_effect_chain(SANAR_VIVID_ORACLE, AbilityKind::Spell);
    assert!(
        matches!(
            &*def.effect,
            Effect::RevealUntil {
                matched_disposition: RevealUntilDisposition::RevealOnly,
                kept_destination: Zone::Library,
                rest_destination: Zone::Library,
                ..
            }
        ),
        "reveal-until must keep the pile in the library, got {:?}",
        def.effect
    );
    let exile = def
        .sub_ability
        .as_ref()
        .expect("Vivid must chain into per-color exile");
    assert!(
        matches!(
            exile.effect.as_ref(),
            Effect::ForEachCategory {
                category: IterationCategory::Color,
                action: ForEachCategoryAction::ExileFromPool {
                    zone: Zone::Library,
                    up_to: true,
                    ..
                },
                ..
            }
        ),
        "expected ForEachCategory(ExileFromPool), got {:?}",
        exile.effect
    );
    // CR 608.2c + CR 607.2a: "you may cast the exiled cards this turn" is an
    // anaphor on the cards the per-category exile published, so it must bind to
    // the chain-local tracked set restricted to the members whose producer
    // action was an exile — NOT `ParentTarget` (which the resolver reads out of
    // `ability.targets`, i.e. the whole reveal window). Reverting the parser
    // half of the fix leaves `ParentTarget` here.
    let cast = find_cast_from_zone(&def).expect("Vivid must chain into a cast grant");
    assert_eq!(
        cast,
        &TargetFilter::TrackedSetFiltered {
            id: TrackedSetId(0),
            filter: Box::new(TargetFilter::Any),
            caused_by: Some(ThisWayCause::Exiled),
        },
        "the cast anaphor must bind to the exiled members of the chain tracked set"
    );
}

/// First `Effect::CastFromZone` target filter reachable through `sub_ability`.
fn find_cast_from_zone(
    ability: &engine::types::ability::AbilityDefinition,
) -> Option<&TargetFilter> {
    let mut cursor = Some(ability);
    while let Some(current) = cursor {
        if let Effect::CastFromZone { target, .. } = current.effect.as_ref() {
            return Some(target);
        }
        cursor = current.sub_ability.as_deref();
    }
    None
}

/// Every casting permission currently recorded anywhere in the game, as
/// `(object, permission count)` for objects that carry at least one.
fn granted_objects(state: &GameState) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = state
        .objects
        .iter()
        .filter(|(_, obj)| !obj.casting_permissions.is_empty())
        .map(|(id, _)| *id)
        .collect();
    ids.sort();
    ids
}

/// CR 401.1: number of cards a player owns in their library.
fn library_size(state: &GameState, player: engine::types::player::PlayerId) -> usize {
    state
        .objects
        .values()
        .filter(|obj| obj.owner == player && obj.zone == Zone::Library)
        .count()
}

/// Build the Sanar board: two differently-coloured permanents (so X = 2) and an
/// 8-card library whose reveal-until window is 5 cards — one blue nonland, three
/// lands, one red nonland — over three cards the window never reaches.
struct SanarBoard {
    spell: ObjectId,
    blue_spell: ObjectId,
    red_spell: ObjectId,
    window_lands: [ObjectId; 3],
    deep: [ObjectId; 3],
}

fn sanar_board() -> (engine::game::scenario::GameRunner, SanarBoard) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let blue_perm = scenario.add_creature(P0, "Blue Bear", 2, 2).id();
    let red_perm = scenario.add_creature(P0, "Red Bear", 2, 2).id();

    // Library, added bottom-first so the final top→bottom order is:
    //   Blue Spell | Land 1 | Land 2 | Land 3 | Red Spell | Deep 1..3
    let deep = [
        scenario.add_card_to_library_top(P0, "Deep Three"),
        scenario.add_card_to_library_top(P0, "Deep Two"),
        scenario.add_card_to_library_top(P0, "Deep One"),
    ];
    let red_spell = scenario.add_card_to_library_top(P0, "Red Spell");
    let window_lands = [
        scenario.add_card_to_library_top(P0, "Window Land Three"),
        scenario.add_card_to_library_top(P0, "Window Land Two"),
        scenario.add_card_to_library_top(P0, "Window Land One"),
    ];
    let blue_spell = scenario.add_card_to_library_top(P0, "Blue Spell");

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Vivid Body", false, SANAR_VIVID_ORACLE)
        .id();

    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.objects.get_mut(&blue_perm).unwrap().color = vec![ManaColor::Blue];
    state.objects.get_mut(&red_perm).unwrap().color = vec![ManaColor::Red];
    state.objects.get_mut(&blue_spell).unwrap().color = vec![ManaColor::Blue];
    state.objects.get_mut(&red_spell).unwrap().color = vec![ManaColor::Red];
    for id in [blue_spell, red_spell] {
        state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Sorcery];
    }
    for id in window_lands.iter().chain(deep.iter()) {
        state.objects.get_mut(id).unwrap().card_types.core_types = vec![CoreType::Land];
    }

    (
        runner,
        SanarBoard {
            spell,
            blue_spell,
            red_spell,
            window_lands,
            deep,
        },
    )
}

/// Answer the current `ChooseFromZoneChoice` through the production action
/// handler, asserting the offer first. `picks` empty declines the optional pick
/// (CR 608.2d).
fn answer_zone_choice(
    runner: &mut engine::game::scenario::GameRunner,
    expected_offer: &[ObjectId],
    picks: &[ObjectId],
) {
    let offered = match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { cards, up_to, .. } => {
            assert!(*up_to, "\"you may exile\" is an optional per-member pick");
            cards.clone()
        }
        other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
    };
    assert_eq!(
        offered, expected_offer,
        "the per-color pool must be exactly the matching revealed cards"
    );
    runner
        .act(GameAction::SelectCards {
            cards: picks.to_vec(),
        })
        .expect("the per-category pick must be accepted");
}

/// CR 608.2c + CR 607.2a + CR 601.3 (issue: "exiling more than one card at a
/// time isn't working"). The reported symptom was that Sanar exiled every
/// revealed card and granted every one of them a cast permission.
///
/// Reverting EITHER half of the fix flips this test:
/// * without the parser half the cast step stays `ParentTarget`, so
///   `cast_from_zone::resolve` reads the injected reveal window;
/// * without the engine half the tracked-set filter is parsed but
///   `ability.targets` is still what the resolver collects.
///
/// Either way all five revealed cards end up exiled and granted.
#[test]
fn sanar_vivid_grants_only_the_per_color_picks() {
    let (mut runner, board) = sanar_board();
    let library_before = library_size(runner.state(), P0);
    assert_eq!(library_before, 8, "8-card library staged");

    let outcome = runner.cast(board.spell).accept_optional().resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ChooseFromZoneChoice { .. }
        ),
        "the per-color exile loop must reach an interactive pick, got {:?}",
        outcome.final_waiting_for()
    );

    // WUBRG iteration: blue member first, then red. This is the engine's own
    // canonical color order, not a rule — CR 105.1 only names the five colors
    // and mandates no iteration order.
    answer_zone_choice(&mut runner, &[board.blue_spell], &[board.blue_spell]);
    answer_zone_choice(&mut runner, &[board.red_spell], &[board.red_spell]);
    runner.advance_until_stack_empty();

    // Only the two picks left the library.
    assert_eq!(
        runner.state().objects[&board.blue_spell].zone,
        Zone::Exile,
        "the blue pick is exiled"
    );
    assert_eq!(
        runner.state().objects[&board.red_spell].zone,
        Zone::Exile,
        "the red pick is exiled"
    );
    for id in board.window_lands.iter().chain(board.deep.iter()) {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Library,
            "revealed-but-not-picked and never-revealed cards must be shuffled back into the \
             library, not exiled ({id:?})"
        );
    }
    assert_eq!(
        library_size(runner.state(), P0),
        library_before - 2,
        "the library must shrink by exactly the two picks"
    );

    // CR 601.3: the permission set, compared as a whole rather than spot-checked.
    assert_eq!(
        granted_objects(runner.state()),
        {
            let mut expected = vec![board.blue_spell, board.red_spell];
            expected.sort();
            expected
        },
        "exactly the two exiled picks may be cast"
    );
}

/// CR 608.2d + CR 601.3: declining every optional per-colour pick must grant
/// nothing at all and leave the whole reveal window in the library.
///
/// This is the fail-closed arm of the tracked-set binding: `ForEachCategory`
/// publishes a FRESH (empty) chain set before its first prompt, so
/// `chain_tracked_set_id` is `Some(empty)` and the cast step's sentinel
/// resolves to that empty set rather than skipping to some older non-empty
/// set. Under the pre-fix `ability.targets` read, the injected reveal window
/// was granted regardless of what the player picked.
#[test]
fn sanar_vivid_declining_every_pick_grants_nothing() {
    let (mut runner, board) = sanar_board();
    let library_before = library_size(runner.state(), P0);

    let outcome = runner.cast(board.spell).accept_optional().resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ChooseFromZoneChoice { .. }
        ),
        "reach-guard: the per-color exile loop must actually prompt, got {:?}",
        outcome.final_waiting_for()
    );

    answer_zone_choice(&mut runner, &[board.blue_spell], &[]);
    answer_zone_choice(&mut runner, &[board.red_spell], &[]);
    runner.advance_until_stack_empty();

    assert_eq!(
        granted_objects(runner.state()),
        Vec::<ObjectId>::new(),
        "declining every pick must leave no casting permission anywhere"
    );
    for id in [board.blue_spell, board.red_spell]
        .iter()
        .chain(board.window_lands.iter())
        .chain(board.deep.iter())
    {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Library,
            "nothing was exiled, so every revealed card stays in the library ({id:?})"
        );
    }
    assert_eq!(
        library_size(runner.state(), P0),
        library_before,
        "library size is unchanged"
    );
}

const PRAETORS_GRASP_ORACLE: &str = "Search target opponent's library for a card and exile it \
face down. Then that player shuffles. You may play that card for as long as it remains exiled.";

/// CR 608.2c + CR 601.3: the Praetor's Grasp shape —
/// `SearchLibrary → ChangeZone{chosen card → Exile} → Shuffle → CastFromZone`.
///
/// The upstream instruction picks ONE card interactively; the cast anaphor then
/// binds to the tracked set that `ChangeZone` published. This is the shape most
/// at risk from the engine half of the fix, because the chosen card reaches the
/// cast step only through the tracked set now — if the intrinsic binding missed
/// it, the chosen card would be stranded in exile with no permission.
#[test]
fn praetors_grasp_shape_grants_the_exiled_card_and_strands_nothing() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let stolen = scenario.add_card_to_library_top(P1, "Stolen Card");
    let other_a = scenario.add_card_to_library_top(P1, "Opponent Card A");
    let other_b = scenario.add_card_to_library_top(P1, "Opponent Card B");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Grasp Body", false, PRAETORS_GRASP_ORACLE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_player(P1)
        .search_first_legal()
        .accept_optional()
        .resolve();

    let found = outcome
        .state()
        .objects
        .iter()
        .filter(|(_, obj)| obj.owner == P1 && obj.zone == Zone::Exile)
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    assert_eq!(
        found.len(),
        1,
        "exactly one opponent card is exiled by the search, got {found:?}"
    );
    let exiled = found[0];
    assert!(
        [stolen, other_a, other_b].contains(&exiled),
        "the exiled card must be one of the staged library cards"
    );

    // CR 601.3: the grant landed on the moved card — and only on it.
    assert_eq!(
        granted_objects(outcome.state()),
        vec![exiled],
        "the play permission must land on the exiled card and nothing else"
    );
    for id in [stolen, other_a, other_b] {
        if id != exiled {
            assert_eq!(
                outcome.state().objects[&id].zone,
                Zone::Library,
                "unsearched cards stay in the opponent's library ({id:?})"
            );
        }
    }
}

/// CR 608.2c + CR 607.2a building-block fixture for `tracked_set_cast_candidates`:
/// a `TrackedSetFiltered` cast anaphor must apply BOTH of its gates — the
/// producer-action cause AND the inner type filter — to every published member.
///
/// No shipping card currently emits `CastFromZone { TrackedSetFiltered { filter:
/// Typed(..), caused_by: Exiled } }` (the parser rewrite always produces
/// `filter: Any`), but the resolver arm this test drives is the same one all 51
/// cause-filtered cards use, and `change_zone` already ships the typed form. So
/// the capability is tested at the building-block level, per CLAUDE.md, with a
/// hand-seeded chain set standing in for the upstream publisher.
///
/// Three members, all in the published set:
/// * an instant stamped `Exiled` — matches both gates, gets the permission;
/// * a creature stamped `Exiled` — right cause, wrong type;
/// * an instant with NO cause stamp (a non-exile publisher merged it in) —
///   right type, wrong cause.
#[test]
fn tracked_set_cast_anaphor_applies_both_cause_and_type_gates() {
    let mut scenario = GameScenario::new_n_player(2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Grant Source", 1, 1).id();
    let matching = scenario.add_spell_to_hand(P0, "Exiled Instant", true).id();
    let wrong_type = scenario.add_card_to_hand(P0, "Exiled Creature");
    let wrong_cause = scenario.add_spell_to_hand(P0, "Merged Instant", true).id();
    let mut runner = scenario.build();

    let state = runner.state_mut();
    for id in [matching, wrong_type, wrong_cause] {
        state.objects.get_mut(&id).unwrap().zone = Zone::Exile;
    }
    state
        .objects
        .get_mut(&wrong_type)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];
    state
        .tracked_object_sets
        .insert(TrackedSetId(1), vec![matching, wrong_type, wrong_cause]);
    state.tracked_set_member_causes.insert(
        TrackedSetId(1),
        [
            (matching, ThisWayCause::Exiled),
            (wrong_type, ThisWayCause::Exiled),
        ]
        .into_iter()
        .collect(),
    );
    state.next_tracked_set_id = 2;
    state.chain_tracked_set_id = Some(TrackedSetId(1));

    let cast_filter = TargetFilter::TrackedSetFiltered {
        id: TrackedSetId(0),
        filter: Box::new(TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Instant],
                    controller: None,
                    properties: vec![],
                }),
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Sorcery],
                    controller: None,
                    properties: vec![],
                }),
            ],
        }),
        caused_by: Some(ThisWayCause::Exiled),
    };
    let ability = ResolvedAbility::new(
        Effect::CastFromZone {
            target: cast_filter,
            without_paying_mana_cost: true,
            mode: CardPlayMode::Cast,
            cast_transformed: false,
            alt_ability_cost: None,
            constraint: None,
            duration: None,
            driver: CastFromZoneDriver::default(),
            mana_spend_permission: None,
        },
        vec![],
        source,
        P0,
    );
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("the cast grant must resolve");

    assert_eq!(
        granted_objects(runner.state()),
        vec![matching],
        "only the member satisfying BOTH the Exiled cause and the instant/sorcery filter may be \
         cast"
    );
    assert!(
        matches!(
            runner.state().objects[&matching]
                .casting_permissions
                .first(),
            Some(CastingPermission::ExileWithAltCost { .. })
        ),
        "reach-guard: the matching member really did receive a free-cast permission"
    );
}

#[test]
fn sanar_vivid_per_color_exile_offers_revealed_library_cards() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let white_perm = scenario.add_creature(P0, "White Bear", 2, 2).id();
    let red_perm = scenario.add_creature(P0, "Red Bear", 2, 2).id();

    // Library top → bottom: land, red sorcery, land, white sorcery.
    let _bottom = scenario.add_card_to_library_top(P0, "Bottom Marker");
    let white_spell = scenario.add_card_to_library_top(P0, "White Bolt");
    let _land2 = scenario.add_card_to_library_top(P0, "Land Two");
    let red_spell = scenario.add_card_to_library_top(P0, "Red Bolt");
    let _land1 = scenario.add_card_to_library_top(P0, "Land One");

    let source = scenario.add_creature(P0, "Sanar Source", 1, 1).id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&white_perm)
        .unwrap()
        .color = vec![ManaColor::White];
    runner.state_mut().objects.get_mut(&red_perm).unwrap().color = vec![ManaColor::Red];

    for (id, core) in [
        (red_spell, CoreType::Sorcery),
        (white_spell, CoreType::Sorcery),
        (_land1, CoreType::Land),
        (_land2, CoreType::Land),
    ] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types = vec![core];
    }
    runner
        .state_mut()
        .objects
        .get_mut(&red_spell)
        .unwrap()
        .color = vec![ManaColor::Red];
    runner
        .state_mut()
        .objects
        .get_mut(&white_spell)
        .unwrap()
        .color = vec![ManaColor::White];

    let ability = sanar_vivid_chain(source);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Sanar Vivid chain must resolve through per-color exile");

    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { cards, up_to, .. } => {
            assert!(*up_to, "you may exile is optional per color");
            assert_eq!(
                cards,
                &vec![white_spell],
                "WUBRG iteration offers the white sorcery first while it remains in the library"
            );
        }
        other => panic!("expected ChooseFromZoneChoice for the first color member, got {other:?}"),
    }

    assert_eq!(
        runner.state().objects[&red_spell].zone,
        Zone::Library,
        "revealed cards must stay in the library until exiled"
    );
    assert_eq!(
        runner.state().objects[&white_spell].zone,
        Zone::Library,
        "revealed cards must stay in the library until exiled"
    );
}

const EXILE_THEN_COUNTERS_THEN_CAST: &str = "Exile the top two cards of your library. Put a \
+1/+1 counter on each creature you control. You may cast the exiled cards this turn.";

/// CR 608.2c + CR 607.2a hostile multi-authority fixture: two publishers merge
/// into ONE chain tracked set, and only one of them exiled.
///
/// `publish_tracked_set` EXTENDS the chain set, so the `PutCounterAll` clause
/// merges the countered battlefield creature into the same set the exile
/// published. A BARE `TrackedSet{0}` cast binding therefore reads the creature
/// too — and `grant_lingering_permissions` routes every non-exile-zone target
/// through a `ZoneMoveRequest::effect(.., Zone::Exile, ..)` batch, which rips
/// the creature off the battlefield and grants it a cast permission.
///
/// This is the fixture that makes `caused_by: Some(Exiled)` load-bearing rather
/// than decorative: with a bare binding the Board Bear ends up in Exile with a
/// permission; with the cause filter it stays on the battlefield with none.
#[test]
fn non_exile_publisher_members_are_excluded_from_the_cast_anaphor() {
    let mut scenario = GameScenario::new_n_player(2, 23);
    scenario.at_phase(Phase::PreCombatMain);

    let bear = scenario.add_creature(P0, "Board Bear", 2, 2).id();
    let deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let second = scenario.add_card_to_library_top(P0, "Exiled Two");
    let first = scenario.add_card_to_library_top(P0, "Exiled One");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Impulse Body", false, EXILE_THEN_COUNTERS_THEN_CAST)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).accept_optional().resolve();

    // Reach-guard: the counter clause really ran, so the bear really was merged
    // into the chain set (CR 122.1). Without this the negative below is vacuous.
    assert_eq!(
        outcome.counters(bear, engine::types::counter::CounterType::Plus1Plus1),
        1,
        "reach-guard: the PutCounterAll clause must have resolved and merged the bear into the \
         chain tracked set"
    );

    assert_eq!(
        outcome.zone_of(bear),
        Zone::Battlefield,
        "the countered creature was never exiled, so the cast anaphor must not drag it into exile"
    );
    assert_eq!(
        granted_objects(outcome.state()),
        {
            let mut expected = vec![first, second];
            expected.sort();
            expected
        },
        "only the two exiled library cards may be cast"
    );
    assert_eq!(
        outcome.zone_of(deep),
        Zone::Library,
        "the third library card was never exiled"
    );
}
