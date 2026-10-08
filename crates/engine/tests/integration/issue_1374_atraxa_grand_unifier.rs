//! Regression: GitHub issue #1374 — Atraxa, Grand Unifier ETB must let the
//! controller choose from among the revealed library cards, not from the
//! graveyard (or any other stale tracked set).
//!
//! Oracle (ETB half):
//!   "When Atraxa enters, reveal the top ten cards of your library. For each
//!    card type, you may put a card of that type from among the revealed cards
//!    into your hand. Put the rest on the bottom of your library in a random
//!    order."
//!
//! Bug: `RevealTop` emits `CardsRevealed`, not `ZoneChanged`, so
//! `affected_objects_from_events` published an empty tracked set. `ChooseFromZone`
//! then fell back to `latest_tracked_set_id`, which could return a stale
//! graveyard/mill set from an earlier resolution — offering only graveyard cards.
//!
//! CR 701.20b: Revealing does not move cards; the choice pool is the revealed set.
//! CR 608.2d: "From among" restricts the resolution-time choice to that set.

use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::P0;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    CardSelectionMode, ChooseFromZoneConstraint, Chooser, Effect, ResolvedAbility, TargetFilter,
    ZoneChoiceCandidateSource, ZoneOwner,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId, TrackedSetId};
use engine::types::zones::Zone;

const ATRAXA_ETB: &str = "Flying, vigilance, deathtouch, lifelink\n\
    When Atraxa enters, reveal the top ten cards of your library. For each card type, \
    you may put a card of that type from among the revealed cards into your hand. \
    Put the rest on the bottom of your library in a random order.";

fn distinct_card_type_categories() -> Vec<CoreType> {
    vec![
        CoreType::Artifact,
        CoreType::Battle,
        CoreType::Creature,
        CoreType::Enchantment,
        CoreType::Instant,
        CoreType::Land,
        CoreType::Planeswalker,
        CoreType::Sorcery,
    ]
}

/// Parser precondition: the ETB trigger must lower to RevealTop → ChooseFromZone.
#[test]
fn atraxa_parser_wires_reveal_top_to_distinct_card_type_choose() {
    let parsed = parse_oracle_text(
        ATRAXA_ETB,
        "Atraxa, Grand Unifier",
        &[],
        &["Creature".to_string()],
        &["Phyrexian".to_string(), "Angel".to_string()],
    );
    assert_eq!(parsed.triggers.len(), 1);
    let execute = parsed.triggers[0]
        .execute
        .as_ref()
        .expect("ETB trigger must have execute");
    assert!(
        matches!(&*execute.effect, Effect::RevealTop { count: 10, .. }),
        "top effect must be RevealTop(10), got {:?}",
        execute.effect
    );
    let choose = execute
        .sub_ability
        .as_ref()
        .expect("RevealTop must chain to ChooseFromZone");
    assert!(
        matches!(
            &*choose.effect,
            Effect::ChooseFromZone {
                up_to: true,
                constraint: Some(ChooseFromZoneConstraint::DistinctCardTypes { .. }),
                ..
            }
        ),
        "sub-ability must be ChooseFromZone with DistinctCardTypes, got {:?}",
        choose.effect
    );
}

/// Build the runtime fixture shared by the offering test and the
/// decline/rejection test: a P0 Atraxa source, ten revealed library cards
/// alternating creature/instant, one bottom-of-library card, and a stale
/// graveyard tracked set. The chain is resolved so the state parks at
/// `WaitingFor::ChooseFromZoneChoice`.
///
/// Returns `(state, library_top, padding, graveyard_trap)`.
fn atraxa_choice_fixture() -> (GameState, Vec<ObjectId>, ObjectId, ObjectId) {
    let mut state = GameState::new_two_player(42);
    let source = create_object(
        &mut state,
        CardId(900),
        P0,
        "Atraxa, Grand Unifier".to_string(),
        Zone::Battlefield,
    );

    let mut library_top = Vec::new();
    for i in 0..10 {
        let id = create_object(
            &mut state,
            CardId(i + 1),
            P0,
            format!("Top Card {i}"),
            Zone::Library,
        );
        let core_type = if i % 2 == 0 {
            CoreType::Creature
        } else {
            CoreType::Instant
        };
        state.objects.get_mut(&id).unwrap().card_types.core_types = vec![core_type];
        library_top.push(id);
    }
    let padding = create_object(
        &mut state,
        CardId(50),
        P0,
        "Library Bottom".to_string(),
        Zone::Library,
    );
    let graveyard_trap = create_object(
        &mut state,
        CardId(99),
        P0,
        "Graveyard Trap".to_string(),
        Zone::Graveyard,
    );
    state
        .tracked_object_sets
        .insert(TrackedSetId(7), vec![graveyard_trap]);
    state.next_tracked_set_id = 8;

    let categories = distinct_card_type_categories();
    let change_zone = Box::new(ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Hand,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![],
        source,
        P0,
    ));
    let choose = ResolvedAbility {
        sub_ability: Some(change_zone),
        ..ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: categories.len() as u32,
                zone: Zone::Library,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Controller.into(),
                candidate_source: ZoneChoiceCandidateSource::Legacy,
                reciprocal_role: None,
                up_to: true,
                selection: CardSelectionMode::Chosen,
                constraint: Some(ChooseFromZoneConstraint::DistinctCardTypes { categories }),
            },
            vec![],
            source,
            P0,
        )
    };
    let reveal = ResolvedAbility {
        sub_ability: Some(Box::new(choose)),
        ..ResolvedAbility::new(
            Effect::RevealTop {
                player: TargetFilter::Controller,
                count: 10,
            },
            vec![],
            source,
            P0,
        )
    };

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &reveal, &mut events, 0)
        .expect("Atraxa ETB reveal/choose chain must resolve");

    (state, library_top, padding, graveyard_trap)
}

/// Runtime regression: after RevealTop resolves, the choice modal must list the
/// ten revealed library cards — never a stale graveyard tracked set.
#[test]
fn atraxa_etb_choice_offers_revealed_library_not_graveyard() {
    let (state, library_top, padding, graveyard_trap) = atraxa_choice_fixture();

    match state.waiting_for {
        WaitingFor::ChooseFromZoneChoice { cards, up_to, .. } => {
            assert!(up_to, "per-card-type picks are optional (up_to)");
            assert_eq!(
                cards.len(),
                10,
                "choice pool must be exactly the ten revealed cards; got {cards:?}"
            );
            for id in &library_top {
                assert!(
                    cards.contains(id),
                    "revealed library card {id:?} must be choosable"
                );
            }
            assert!(
                !cards.contains(&graveyard_trap),
                "graveyard cards must never be offered (issue #1374)"
            );
            assert!(
                !cards.contains(&padding),
                "unrevealed library cards must not be offered"
            );
        }
        other => panic!("expected ChooseFromZoneChoice for Atraxa ETB, got {other:?}"),
    }
}

/// CR 608.2d: choosing ZERO cards from the per-card-type `ChooseFromZone`
/// (`up_to: true`) is the legal decline — the very optionality the swallow
/// auditor reads as evidence. The production `GameAction::SelectCards` path must
/// therefore accept an empty selection, while a selection that violates the
/// `DistinctCardTypes` constraint is rejected and leaves the prompt pending.
#[test]
fn atraxa_etb_decline_choose_zero_cards_is_accepted() {
    let (mut state, library_top, _padding, _graveyard_trap) = atraxa_choice_fixture();

    // Reach guard: the production prompt is pending with the ten revealed cards —
    // the same assertion the offering test makes — so the submissions below are
    // answered by the real resolution-choice handler.
    match &state.waiting_for {
        WaitingFor::ChooseFromZoneChoice {
            player,
            cards,
            up_to,
            ..
        } => {
            assert_eq!(*player, P0);
            assert!(*up_to, "per-card-type picks are optional (up_to)");
            assert_eq!(
                cards.len(),
                10,
                "choice pool must be the ten revealed cards"
            );
        }
        other => panic!("expected the Atraxa ChooseFromZoneChoice prompt, got {other:?}"),
    }

    // Paired negative: two cards of the SAME card type admit no injective
    // assignment to distinct card types, so `choose_from_zone::selection_satisfies_constraint`
    // rejects the submission and the prompt stays pending.
    let creature_a = library_top[0];
    let creature_b = library_top[2];
    assert!(
        state.objects[&creature_a].card_types.core_types == vec![CoreType::Creature]
            && state.objects[&creature_b].card_types.core_types == vec![CoreType::Creature],
        "reach: the negative needs two same-type cards"
    );
    let rejected = engine::game::engine::apply(
        &mut state,
        P0,
        GameAction::SelectCards {
            cards: vec![creature_a, creature_b],
        },
    );
    assert!(
        rejected.is_err(),
        "two cards of the same card type must violate DistinctCardTypes, got {rejected:?}"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::ChooseFromZoneChoice { .. }),
        "a rejected selection must leave the prompt pending"
    );

    // The decline: zero cards is a legal `up_to` selection.
    engine::game::engine::apply(&mut state, P0, GameAction::SelectCards { cards: vec![] })
        .expect("declining the per-card-type choice (zero cards) must be accepted");
    assert!(
        !matches!(state.waiting_for, WaitingFor::ChooseFromZoneChoice { .. }),
        "the answered prompt must not remain pending"
    );
}
