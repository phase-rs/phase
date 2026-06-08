//! Tests for Hideaway (CR 702.75) synthesis, runtime, and visibility. Declared
//! from `database/mod.rs` so the implementation modules (`database/hideaway.rs`
//! and `game/effects/hideaway.rs`) stay free of inline test scaffolding.

use super::hideaway::synthesize_hideaway;
use crate::database::mtgjson::{AtomicCard, AtomicIdentifiers};
use crate::game::ability_utils::build_resolved_from_def;
use crate::game::effects::resolve_ability_chain;
use crate::game::filter_state_for_viewer;
use crate::game::zones::create_object;
use crate::types::ability::{Effect, ResolvedAbility, TargetFilter};
use crate::types::actions::GameAction;
use crate::types::card::CardFace;
use crate::types::game_state::{ExileLink, ExileLinkKind, GameState, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

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
            && l.kind == ExileLinkKind::HideawayLookable),
        "exiled card is linked to the source with the look-permission kind"
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
        kind: ExileLinkKind::HideawayLookable,
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
