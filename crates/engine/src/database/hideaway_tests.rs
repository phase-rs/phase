//! Tests for Hideaway (CR 702.75) synthesis, runtime, and visibility. Declared
//! from `database/mod.rs` so the implementation modules (`database/hideaway.rs`
//! and `game/effects/hideaway.rs`) stay free of inline test scaffolding.

use super::hideaway::synthesize_hideaway;
use crate::database::mtgjson::{AtomicCard, AtomicIdentifiers};
use crate::game::ability_utils::build_resolved_from_def;
use crate::game::effects::resolve_ability_chain;
use crate::game::filter_state_for_viewer;
use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
use crate::game::zones::create_object;
use crate::types::ability::{
    CastingPermission, Effect, PermissionGrantee, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::actions::GameAction;
use crate::types::card::CardFace;
use crate::types::events::GameEvent;
use crate::types::game_state::{ExileLink, ExileLinkKind, GameState, LookGrant, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId, TrackedSetId};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Synthesis-shape tests (building-block level)
// ---------------------------------------------------------------------------

fn face_with(keyword: Keyword) -> CardFace {
    let mut face = CardFace::default();
    face.keywords.push(keyword);
    face
}

/// CR 702.75a: synthesize_hideaway produces a self-ETB trigger whose effect is a
/// `Dig` (look at top N, keep one to Exile, rest to library bottom) chained to a
/// `HideawayConceal` continuation.
#[test]
fn synthesize_hideaway_builds_etb_dig_conceal_trigger() {
    let mut face = face_with(Keyword::Hideaway(4));
    synthesize_hideaway(&mut face);

    assert_eq!(face.triggers.len(), 1, "exactly one ETB trigger");
    let trigger = &face.triggers[0];
    assert!(matches!(trigger.mode, TriggerMode::ChangesZone));
    assert_eq!(trigger.destination, Some(Zone::Battlefield));
    assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));

    let dig = trigger.execute.as_ref().expect("execute ability");
    match dig.effect.as_ref() {
        Effect::Dig {
            count,
            keep_count,
            destination,
            rest_destination,
            reveal,
            player,
            ..
        } => {
            assert_eq!(player, &TargetFilter::Controller);
            assert!(
                matches!(
                    count,
                    crate::types::ability::QuantityExpr::Fixed { value: 4 }
                ),
                "looks at the top N cards"
            );
            assert_eq!(*keep_count, Some(1), "exile exactly one");
            assert_eq!(*destination, Some(Zone::Exile), "kept card is exiled");
            assert_eq!(
                *rest_destination,
                Some(Zone::Library),
                "rest go to the bottom of the library"
            );
            assert!(!*reveal, "cards are looked at privately, not revealed");
        }
        other => panic!("expected Dig effect, got {other:?}"),
    }

    let conceal = dig.sub_ability.as_ref().expect("conceal continuation");
    assert!(
        matches!(conceal.effect.as_ref(), Effect::HideawayConceal { .. }),
        "Dig is chained to the HideawayConceal step"
    );
}

/// Cards without the keyword are untouched.
#[test]
fn synthesize_hideaway_is_noop_without_keyword() {
    let mut face = face_with(Keyword::Flying);
    synthesize_hideaway(&mut face);
    assert!(face.triggers.is_empty());
}

/// Re-running synthesis does not stack duplicate triggers.
#[test]
fn synthesize_hideaway_is_idempotent() {
    let mut face = face_with(Keyword::Hideaway(4));
    synthesize_hideaway(&mut face);
    synthesize_hideaway(&mut face);
    assert_eq!(face.triggers.len(), 1);
}

/// CR 113.2c: multiple instances of the same ability function independently, so
/// a face printing two Hideaway instances synthesizes one ETB trigger each — and
/// re-running synthesis stays idempotent at that count (no duplicate stacking).
#[test]
fn synthesize_hideaway_handles_multiple_instances_and_stays_idempotent() {
    let mut face = face_with(Keyword::Hideaway(4));
    face.keywords.push(Keyword::Hideaway(2));

    synthesize_hideaway(&mut face);
    assert_eq!(
        face.triggers.len(),
        2,
        "one independent ETB trigger per Hideaway instance"
    );

    synthesize_hideaway(&mut face);
    assert_eq!(
        face.triggers.len(),
        2,
        "re-running synthesis must not stack duplicates"
    );
}

// ---------------------------------------------------------------------------
// Conceal-step resolver (the custom building block)
// ---------------------------------------------------------------------------

fn main_phase_state() -> GameState {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.phase = Phase::PreCombatMain;
    state
}

/// CR 702.75a + CR 406.3 + CR 607.2a: HideawayConceal turns the exiled target
/// card face down and links it to the source in `exile_links`.
#[test]
fn hideaway_conceal_marks_face_down_and_links_to_source() {
    let mut state = main_phase_state();
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Windbrisk Heights".to_string(),
        Zone::Battlefield,
    );
    let exiled = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Hidden Bomb".to_string(),
        Zone::Exile,
    );

    // The conceal step acts on the parent-inherited target (the just-exiled
    // card), carried in `ability.targets`; the source is the hideaway permanent.
    let ability = ResolvedAbility::new(
        Effect::HideawayConceal {
            target: TargetFilter::ParentTarget,
            grantee: None,
        },
        vec![crate::types::ability::TargetRef::Object(exiled)],
        source,
        PlayerId(0),
    );

    let mut events = Vec::new();
    crate::game::effects::hideaway::resolve(&mut state, &ability, &mut events).unwrap();

    assert!(state.objects[&exiled].face_down, "exiled card is face down");
    assert!(
        state.exile_links.iter().any(|l| l.exiled_id == exiled
            && l.source_id == source
            && matches!(
                &l.kind,
                ExileLinkKind::HideawayLookable {
                    grant: LookGrant::SourceController,
                    lookers,
                    ..
                } if *lookers == BTreeSet::from([PlayerId(0)])
            )),
        "exiled card is linked to the source under the source-controller rule"
    );
}

// ---------------------------------------------------------------------------
// End-to-end ETB → Dig → choose → conceal (the full interactive flow)
// ---------------------------------------------------------------------------

/// CR 702.75a: firing the synthesized ability looks at the top N, the player
/// chooses one, and it ends up exiled face down and linked to the source while
/// the rest stay in the library.
#[test]
fn hideaway_etb_exiles_chosen_card_face_down_and_links_it() {
    let mut state = main_phase_state();
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Mosswort Bridge".to_string(),
        Zone::Battlefield,
    );
    // Put four known cards on top of the controller's library.
    for i in 0..4 {
        create_object(
            &mut state,
            CardId(100 + i),
            PlayerId(0),
            format!("Lib {i}"),
            Zone::Library,
        );
    }
    let top: Vec<ObjectId> = state.players[0].library.iter().take(4).copied().collect();
    assert_eq!(top.len(), 4);

    // Build and resolve the synthesized Hideaway ability's effect chain.
    let mut face = face_with(Keyword::Hideaway(4));
    synthesize_hideaway(&mut face);
    let execute = face.triggers[0].execute.as_ref().unwrap();
    let resolved = build_resolved_from_def(execute, source, PlayerId(0));

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &resolved, &mut events, 0).unwrap();

    // CR 701.20e: the Dig step paused for the controller's selection.
    let looked_at = match &state.waiting_for {
        WaitingFor::DigChoice { cards, .. } => cards.clone(),
        other => panic!("expected DigChoice, got {other:?}"),
    };
    assert_eq!(looked_at.len(), 4, "looked at the top four");

    // Choose the second card to hide away.
    let chosen = looked_at[1];
    crate::game::engine::apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![chosen],
        },
    )
    .expect("selection resolves");

    // CR 702.75a: chosen card is exiled, face down, and linked to the source.
    assert_eq!(state.objects[&chosen].zone, Zone::Exile);
    assert!(state.objects[&chosen].face_down, "hidden card is face down");
    assert!(
        state
            .exile_links
            .iter()
            .any(|l| l.exiled_id == chosen && l.source_id == source),
        "hidden card is linked to the hideaway source"
    );
    // The other looked-at cards are not exiled (they go back to the library).
    for other in looked_at.iter().filter(|c| **c != chosen) {
        assert_ne!(
            state.objects[other].zone,
            Zone::Exile,
            "non-chosen cards are not exiled"
        );
    }
}

// ---------------------------------------------------------------------------
// Visibility (hidden-information correctness)
// ---------------------------------------------------------------------------

/// CR 702.75a: the controller of the permanent that exiled the card may look at
/// it; opponents may not.
#[test]
fn hideaway_exiled_card_visible_to_controller_hidden_from_opponent() {
    let mut state = main_phase_state();
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Shelldock Isle".to_string(),
        Zone::Battlefield,
    );
    let hidden = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Secret Plan".to_string(),
        Zone::Exile,
    );
    state.objects.get_mut(&hidden).unwrap().face_down = true;
    state.exile_links.push(ExileLink {
        exiled_id: hidden,
        source_id: source,
        kind: ExileLinkKind::HideawayLookable {
            grant: crate::types::game_state::LookGrant::SourceController,
            lookers: std::collections::BTreeSet::new(),
            source_incarnation: state.objects[&source].incarnation,
        },
    });

    // Controller (P0) may look — real identity survives the filter.
    let for_controller = filter_state_for_viewer(&state, PlayerId(0));
    assert_eq!(
        for_controller.objects[&hidden].name, "Secret Plan",
        "the controller may look at the card it hid away"
    );

    // Opponent (P1) may not — the card is redacted.
    let for_opponent = filter_state_for_viewer(&state, PlayerId(1));
    assert_eq!(
        for_opponent.objects[&hidden].name, "Hidden Card",
        "opponents cannot see the hidden card"
    );
}

/// CR 406.3 + CR 702.75a regression: the Hideaway look-permission must be keyed
/// on `ExileLinkKind::HideawayLookable` specifically, NOT on the mere presence
/// of a `TrackedBySource` link. A face-down card exiled by a permanent that only
/// tracks-by-source for later retrieval (Bomat Courier — "(You can't look at
/// it.)", whose "put all cards exiled with this creature into their owners'
/// hands" ability makes its face-down exiles source-tracked) must stay redacted
/// even for the controller of the exiling permanent.
#[test]
fn tracked_by_source_face_down_exile_stays_hidden_from_controller() {
    let mut state = main_phase_state();
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Bomat Courier".to_string(),
        Zone::Battlefield,
    );
    let hidden = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Bomat Exile".to_string(),
        Zone::Exile,
    );
    state.objects.get_mut(&hidden).unwrap().face_down = true;
    state.exile_links.push(ExileLink {
        exiled_id: hidden,
        source_id: source,
        kind: ExileLinkKind::TrackedBySource,
    });

    // Even the controller of the exiling permanent may NOT look — Bomat-style
    // tracked exiles grant no look-permission.
    let for_controller = filter_state_for_viewer(&state, PlayerId(0));
    assert_eq!(
        for_controller.objects[&hidden].name, "Hidden Card",
        "a plain TrackedBySource face-down exile must stay hidden from its source's controller"
    );

    // Opponent likewise sees nothing.
    let for_opponent = filter_state_for_viewer(&state, PlayerId(1));
    assert_eq!(
        for_opponent.objects[&hidden].name, "Hidden Card",
        "opponents cannot see a tracked-by-source face-down exile either"
    );
}

// ---------------------------------------------------------------------------
// Real-pipeline integration (MTGJSON -> parse -> synthesize)
// ---------------------------------------------------------------------------

/// Real Hideaway card (Windbrisk Heights) routed through `build_oracle_face` —
/// exercises the true production path (MTGJSON keyword parse -> `synthesize_all`
/// -> `synthesize_hideaway`).
#[test]
fn real_hideaway_card_synthesizes_etb_trigger() {
    let atomic = AtomicCard {
        name: "Windbrisk Heights".to_string(),
        mana_cost: None,
        colors: Vec::new(),
        color_identity: vec!["W".to_string()],
        text: Some(
            "Hideaway 4 (When this land enters, look at the top four cards of your library, exile \
             one face down, then put the rest on the bottom in a random order.)\n\
             This land enters tapped.\n\
             {T}: Add {W}.\n\
             {W}, {T}: You may play the exiled card without paying its mana cost if you attacked \
             with three or more creatures this turn."
                .to_string(),
        ),
        power: None,
        toughness: None,
        loyalty: None,
        defense: None,
        layout: "normal".to_string(),
        type_line: Some("Land".to_string()),
        types: vec!["Land".to_string()],
        subtypes: Vec::new(),
        supertypes: Vec::new(),
        keywords: Some(vec!["Hideaway".to_string()]),
        side: None,
        face_name: None,
        mana_value: 0.0,
        legalities: Default::default(),
        leadership_skills: None,
        printings: Vec::new(),
        rulings: Vec::new(),
        is_game_changer: false,
        identifiers: AtomicIdentifiers {
            scryfall_oracle_id: Some("windbrisk-heights-oracle".to_string()),
            scryfall_id: Some("windbrisk-heights-face".to_string()),
        },
        foreign_data: Vec::new(),
        related_cards: crate::database::mtgjson::SetRelatedCards::default(),
    };

    let face = crate::database::synthesis::build_oracle_face(&atomic, None);
    assert!(
        face.keywords
            .iter()
            .any(|k| matches!(k, Keyword::Hideaway(_))),
        "Hideaway keyword must parse from MTGJSON"
    );
    let trigger = face
        .triggers
        .iter()
        .find(|t| {
            matches!(t.mode, TriggerMode::ChangesZone)
                && t.destination == Some(Zone::Battlefield)
                && t.execute.as_ref().is_some_and(|a| {
                    matches!(a.effect.as_ref(), Effect::Dig { .. })
                        && a.sub_ability.as_ref().is_some_and(|s| {
                            matches!(s.effect.as_ref(), Effect::HideawayConceal { .. })
                        })
                })
        })
        .expect("a Hideaway ETB Dig→Conceal trigger must be synthesized");
    let _ = trigger;
    // CR 614: the card is now genuinely runnable, not a parse stub.
    assert!(
        !crate::game::coverage::card_face_has_unimplemented_parts(&face),
        "face must have no Unimplemented parts after synthesis"
    );
}

// ---------------------------------------------------------------------------
// Look authority: the live rule and the CR 406.3 latch (production paths)
// ---------------------------------------------------------------------------

const P2: PlayerId = PlayerId(2);
const HIDEAWAY_CREATURE: &str = "Hideaway 4 (When this creature enters, look at the top four cards of your library, exile one face down, then put the rest on the bottom in a random order.)";
const MOSSWORT_BRIDGE: &str = "Hideaway 4 (When this land enters, look at the top four cards of your library, exile one face down, then put the rest on the bottom in a random order.)\nThis land enters tapped.\n{T}: Add {G}.\n{G}, {T}: You may play the exiled card without paying its mana cost if creatures you control have total power 10 or greater.";
const STEAL_UNTIL_EOT: &str = "Gain control of target creature until end of turn.";
const STEAL_PERMANENT: &str = "Gain control of target permanent.";
const CONTROL_AURA: &str = "Flash\nEnchant creature\nYou control enchanted creature.";
const DESTROY_CREATURE: &str = "Destroy target creature.";
const DESTROY_ENCHANTMENT: &str = "Destroy target enchantment.";
const DESTROY_LAND: &str = "Destroy target land.";
const RETURN_LAND: &str = "Return target land card from your graveyard to the battlefield.";

fn seen_as(state: &GameState, viewer: PlayerId, card: ObjectId) -> String {
    filter_state_for_viewer(state, viewer).objects[&card]
        .name
        .clone()
}

fn look_link(state: &GameState, card: ObjectId) -> &ExileLinkKind {
    &state
        .exile_links
        .iter()
        .find(|l| l.exiled_id == card && matches!(l.kind, ExileLinkKind::HideawayLookable { .. }))
        .expect("the card has a look link")
        .kind
}

fn free_spell(scenario: &mut GameScenario, player: PlayerId, name: &str, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, name, true, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn seed_library(scenario: &mut GameScenario, player: PlayerId, count: usize) {
    for i in 0..count {
        scenario.add_card_to_library_top(player, &format!("Library Card {i}"));
    }
}

/// Pass priority on an empty stack until `player` holds it.
fn give_priority(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..8 {
        if runner.state().priority_player == player
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority");
    }
    panic!("{player:?} never received priority");
}

/// Drive a Hideaway trigger to completion, hiding the first offered card; returns
/// it and the player who chose it.
fn drive_hideaway(runner: &mut GameRunner) -> (ObjectId, PlayerId) {
    let mut chosen = None;
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DigChoice { player, cards, .. } => {
                chosen = Some((cards[0], player));
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .expect("hiding a card");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() && chosen.is_some() => {
                break
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority");
            }
            other => panic!("unexpected prompt while hiding a card: {other:?}"),
        }
    }
    chosen.expect("the Hideaway trigger offered a DigChoice")
}

/// P0 casts a Hideaway creature and hides a card; returns (runner, creature, card, extras).
fn hideaway_creature_board(
    extras: impl FnOnce(&mut GameScenario) -> Vec<ObjectId>,
) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "Hideaway Creature", 2, 2, HIDEAWAY_CREATURE)
        .with_mana_cost(ManaCost::zero())
        .id();
    seed_library(&mut scenario, P0, 6);
    seed_library(&mut scenario, P1, 4);
    let ids = extras(&mut scenario);
    let mut runner = scenario.build();
    runner.cast(creature).commit();
    let (card, _) = drive_hideaway(&mut runner);
    (runner, creature, card, ids)
}

/// A concealed card exiled face down by `source` with a fixed-player grantee.
fn conceal_for_ability_controller(runner: &mut GameRunner, source: ObjectId, card: ObjectId) {
    let ability = ResolvedAbility::new(
        Effect::HideawayConceal {
            target: TargetFilter::ParentTarget,
            grantee: Some(PermissionGrantee::AbilityController),
        },
        vec![TargetRef::Object(card)],
        source,
        P0,
    );
    let mut events = Vec::new();
    crate::game::effects::hideaway::resolve(runner.state_mut(), &ability, &mut events)
        .expect("conceal resolves");
}

/// CR 701.24a: shuffle `pile` as a face-down pile through the pile primitive.
fn shuffle_pile(runner: &mut GameRunner, pile: Vec<ObjectId>, source: ObjectId) {
    let state = runner.state_mut();
    let id = TrackedSetId(state.next_tracked_set_id);
    state.next_tracked_set_id += 1;
    state.tracked_object_sets.insert(id, pile);
    let ability = ResolvedAbility::new(
        Effect::Shuffle {
            target: TargetFilter::TrackedSet { id },
        },
        vec![],
        source,
        P0,
    );
    let mut events = Vec::new();
    crate::game::effects::shuffle::resolve(state, &ability, &mut events).expect("pile shuffle");
}

/// CR 406.3: a fixed-player look survives a control change and its source leaving.
#[test]
fn fixed_player_look_survives_control_change_and_source_departure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Exiling Creature", 2, 2).id();
    let card = scenario
        .add_creature_to_exile(P0, "Concealed Card", 1, 1)
        .id();
    let steal = free_spell(&mut scenario, P1, "Borrow", STEAL_UNTIL_EOT);
    let kill = free_spell(&mut scenario, P1, "Slay", DESTROY_CREATURE);
    let mut runner = scenario.build();
    conceal_for_ability_controller(&mut runner, source, card);
    assert!(matches!(
        look_link(runner.state(), card),
        ExileLinkKind::HideawayLookable {
            grant: LookGrant::Player { player: P0 },
            ..
        }
    ));
    assert_eq!(seen_as(runner.state(), P0, card), "Concealed Card");
    assert_eq!(seen_as(runner.state(), P1, card), "Hidden Card");

    give_priority(&mut runner, P1);
    runner.cast(steal).target_objects(&[source]).resolve();
    assert_eq!(runner.state().objects[&source].controller, P1);
    assert_eq!(seen_as(runner.state(), P0, card), "Concealed Card");
    assert_eq!(seen_as(runner.state(), P1, card), "Hidden Card");

    give_priority(&mut runner, P1);
    runner.cast(kill).target_objects(&[source]).resolve();
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
    assert_eq!(seen_as(runner.state(), P0, card), "Concealed Card");
    assert_eq!(seen_as(runner.state(), P1, card), "Hidden Card");
}

/// CR 702.75a + CR 406.3: a Hideaway card is seen by the source's new controller,
/// and by every former controller after the control effect ends.
#[test]
fn hideaway_look_follows_control_and_latches_every_controller() {
    let (mut runner, creature, card, ids) =
        hideaway_creature_board(|s| vec![free_spell(s, P1, "Borrow", STEAL_UNTIL_EOT)]);
    assert_eq!(seen_as(runner.state(), P1, card), "Hidden Card");

    give_priority(&mut runner, P1);
    runner.cast(ids[0]).target_objects(&[creature]).resolve();
    assert_eq!(runner.state().objects[&creature].controller, P1);
    assert_ne!(seen_as(runner.state(), P1, card), "Hidden Card");
    assert_ne!(seen_as(runner.state(), P0, card), "Hidden Card");

    runner.advance_to_upkeep();
    assert_eq!(runner.state().objects[&creature].controller, P0);
    assert_ne!(seen_as(runner.state(), P1, card), "Hidden Card");
}

/// CR 406.3: a player admitted by a static control effect keeps the look after
/// the effect is removed.
#[test]
fn hideaway_look_latches_a_static_control_effects_controller() {
    let (mut runner, creature, card, ids) = hideaway_creature_board(|s| {
        let aura = s
            .add_spell_to_hand(P1, "Leash", true)
            .as_enchantment()
            .with_subtypes(vec!["Aura"])
            .from_oracle_text(CONTROL_AURA)
            .with_mana_cost(ManaCost::zero())
            .id();
        vec![aura, free_spell(s, P0, "Unleash", DESTROY_ENCHANTMENT)]
    });
    give_priority(&mut runner, P1);
    let aura_on = runner.cast(ids[0]).target_objects(&[creature]).resolve();
    assert!(!aura_on
        .events()
        .iter()
        .any(|e| matches!(e, GameEvent::ControllerChanged { .. })));
    assert_eq!(runner.state().objects[&creature].controller, P1);
    assert_ne!(seen_as(runner.state(), P1, card), "Hidden Card");

    give_priority(&mut runner, P0);
    runner.cast(ids[1]).target_objects(&[ids[0]]).resolve();
    assert_eq!(runner.state().objects[&creature].controller, P0);
    assert_ne!(seen_as(runner.state(), P1, card), "Hidden Card");
}

/// CR 406.3 + CR 608.2c: the Hideaway trigger's controller keeps the look even
/// if control of the source changed while the trigger was on the stack.
#[test]
fn hideaway_trigger_controller_looks_after_source_stolen_in_response() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "Hideaway Creature", 2, 2, HIDEAWAY_CREATURE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let steal = free_spell(&mut scenario, P1, "Borrow", STEAL_UNTIL_EOT);
    seed_library(&mut scenario, P0, 6);
    let mut runner = scenario.build();
    runner.cast(creature).commit();
    for _ in 0..40 {
        if runner.state().objects[&creature].zone == Zone::Battlefield
            && runner.state().stack.len() == 1
        {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority");
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the Hideaway trigger is on the stack"
    );
    give_priority(&mut runner, P1);
    runner.cast(steal).target_objects(&[creature]).commit();
    let (card, chooser) = drive_hideaway(&mut runner);
    assert_eq!(chooser, P0);
    assert_eq!(runner.state().objects[&creature].controller, P1);
    assert_ne!(seen_as(runner.state(), P0, card), "Hidden Card");
    assert_ne!(seen_as(runner.state(), P1, card), "Hidden Card");
}

/// P0 plays Mosswort Bridge and hides X; P1 destroys it; P0 returns it and it
/// hides Y. Returns (runner, bridge, x, y, take).
fn returned_bridge_board() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let bridge = scenario
        .add_land_to_hand(P0, "Mosswort Bridge")
        .from_oracle_text(MOSSWORT_BRIDGE)
        .id();
    seed_library(&mut scenario, P0, 8);
    seed_library(&mut scenario, P1, 4);
    seed_library(&mut scenario, P2, 4);
    scenario.add_creature(P0, "Big Creature", 10, 1);
    let destroy = free_spell(&mut scenario, P1, "Quake", DESTROY_LAND);
    let recover = free_spell(&mut scenario, P0, "Recover", RETURN_LAND);
    let take = free_spell(&mut scenario, P2, "Take", STEAL_PERMANENT);
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&bridge].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: bridge,
            card_id,
        })
        .expect("playing the land");
    let (x, _) = drive_hideaway(&mut runner);
    give_priority(&mut runner, P1);
    runner.cast(destroy).target_objects(&[bridge]).resolve();
    assert_eq!(runner.state().objects[&bridge].zone, Zone::Graveyard);
    assert_ne!(seen_as(runner.state(), P0, x), "Hidden Card");
    assert_eq!(seen_as(runner.state(), P1, x), "Hidden Card");
    assert_eq!(seen_as(runner.state(), P2, x), "Hidden Card");
    let departed_incarnation = runner.state().objects[&bridge].incarnation;
    give_priority(&mut runner, P0);
    runner.cast(recover).target_objects(&[bridge]).commit();
    let (y, _) = drive_hideaway(&mut runner);
    assert_eq!(runner.state().objects[&bridge].zone, Zone::Battlefield);
    assert_ne!(
        runner.state().objects[&bridge].incarnation,
        departed_incarnation
    );
    (runner, bridge, x, y, take)
}

/// CR 406.3 + CR 400.7: the last controller of a departed Hideaway source keeps
/// the look; a player who takes the returned permanent sees only what it hid.
#[test]
fn departed_hideaway_source_keeps_its_look_and_the_returned_object_is_new() {
    let (mut runner, bridge, x, y, take) = returned_bridge_board();
    give_priority(&mut runner, P2);
    runner.cast(take).target_objects(&[bridge]).resolve();
    assert_eq!(runner.state().objects[&bridge].controller, P2);
    assert_ne!(seen_as(runner.state(), P2, y), "Hidden Card");
    assert_eq!(seen_as(runner.state(), P2, x), "Hidden Card");
    assert_ne!(seen_as(runner.state(), P0, x), "Hidden Card");
}

/// Reach guard: X is still exiled, linked by the Bridge's earlier incarnation.
fn assert_first_card_still_linked_to_the_departed_bridge(
    state: &GameState,
    bridge: ObjectId,
    x: ObjectId,
) {
    assert_eq!(state.objects[&x].zone, Zone::Exile);
    assert!(state.exile_links.iter().any(|l| l.exiled_id == x
        && matches!(
            l.kind,
            ExileLinkKind::HideawayLookable { source_incarnation, .. }
                if source_incarnation < state.objects[&bridge].incarnation
        )));
}

/// CR 400.7 + CR 607.2a: the returned Mosswort Bridge's linked cards, as read
/// by the rules and as projected to every viewer, are only the card it hid.
#[test]
fn returned_hideaway_source_links_only_its_own_exiled_card() {
    let (runner, bridge, x, y, _) = returned_bridge_board();
    let state = runner.state();
    assert_first_card_still_linked_to_the_departed_bridge(state, bridge, x);
    let linked: Vec<ObjectId> = crate::game::players::linked_exile_cards_for_source(state, bridge)
        .iter()
        .map(|snapshot| snapshot.exiled_id)
        .collect();
    assert_eq!(linked, vec![y]);
    for viewer in [P0, P1] {
        let filtered = filter_state_for_viewer(state, viewer);
        let views =
            crate::game::derived_views::derive_filtered_views(state, &filtered, Some(viewer));
        assert_eq!(views.linked_exile_ids.get(&bridge), Some(&vec![y]));
    }
}

/// CR 400.7 + CR 607.2a: the returned Mosswort Bridge's "play the exiled card"
/// offer grants only the card it hid.
#[test]
fn returned_hideaway_source_offers_only_its_own_exiled_card() {
    let (mut runner, bridge, x, y, _) = returned_bridge_board();
    assert_first_card_still_linked_to_the_departed_bridge(runner.state(), bridge, x);
    let hid_turn = runner.state().turn_number;
    for _ in 0..200 {
        let state = runner.state();
        if state.turn_number > hid_turn
            && state.active_player == P0
            && state.phase == Phase::PreCombatMain
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { player: P0 })
        {
            break;
        }
        match state.waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => runner
                .act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declaring no attackers"),
            _ => runner
                .act(GameAction::PassPriority)
                .expect("passing priority"),
        };
    }
    assert!(runner.state().turn_number > hid_turn);
    assert_eq!(runner.state().active_player, P0);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(!runner.state().objects[&bridge].tapped);
    runner.state_mut().players[0]
        .mana_pool
        .add(crate::types::mana::ManaUnit::new(
            crate::types::mana::ManaType::Green,
            ObjectId(0),
            false,
            vec![],
        ));
    runner
        .act(GameAction::ActivateAbility {
            source_id: bridge,
            ability_index: 1,
        })
        .expect("activating the play ability");
    let mut offered = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                offered = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting the offer");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority");
            }
            other => panic!("unexpected prompt while activating: {other:?}"),
        }
    }
    assert!(offered, "total power 10 surfaces the offer");
    let may_play = |card: ObjectId| {
        runner.state().objects[&card]
            .casting_permissions
            .iter()
            .any(|p| {
                matches!(
                    p,
                    CastingPermission::PlayFromExile {
                        source_id: Some(src),
                        ..
                    } if *src == bridge
                )
            })
    };
    assert!(may_play(y));
    assert!(!may_play(x));
}

/// CR 406.3 + CR 701.24a: shuffling the card into a pile ends every latched and
/// fixed-player look; the Hideaway source's controller at that time is re-admitted.
#[test]
fn pile_shuffle_resets_the_look_latch_to_the_live_rule() {
    let (mut runner, creature, card, ids) =
        hideaway_creature_board(|s| vec![free_spell(s, P1, "Borrow", STEAL_UNTIL_EOT)]);
    give_priority(&mut runner, P1);
    runner.cast(ids[0]).target_objects(&[creature]).resolve();
    assert_ne!(seen_as(runner.state(), P0, card), "Hidden Card");
    assert_ne!(seen_as(runner.state(), P1, card), "Hidden Card");

    shuffle_pile(&mut runner, vec![card], creature);
    assert_eq!(seen_as(runner.state(), P0, card), "Hidden Card");
    assert_ne!(seen_as(runner.state(), P1, card), "Hidden Card");

    runner.advance_to_upkeep();
    assert_eq!(runner.state().objects[&creature].controller, P0);
    assert_ne!(seen_as(runner.state(), P0, card), "Hidden Card");
    assert_ne!(seen_as(runner.state(), P1, card), "Hidden Card");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Exiling Creature", 2, 2).id();
    let secret = scenario
        .add_creature_to_exile(P0, "Concealed Card", 1, 1)
        .id();
    let mut runner = scenario.build();
    conceal_for_ability_controller(&mut runner, source, secret);
    assert_eq!(seen_as(runner.state(), P0, secret), "Concealed Card");
    shuffle_pile(&mut runner, vec![secret], source);
    assert_eq!(seen_as(runner.state(), P0, secret), "Hidden Card");
}
