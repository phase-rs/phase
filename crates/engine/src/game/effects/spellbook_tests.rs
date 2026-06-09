//! Tests for the Alchemy spellbook draft (`Effect::DraftFromSpellbook`).
//! Declared from `effects/mod.rs` so `spellbook.rs` stays implementation-only.

use super::resolve_ability_chain;
use super::spellbook::{complete_draft, resolve};
use crate::game::zones::create_object;
use crate::parser::oracle_effect::parse_effect;
use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::CardId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

fn draft_ability(
    source: crate::types::identifiers::ObjectId,
    destination: Zone,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DraftFromSpellbook {
            destination,
            tapped: false,
        },
        Vec::new(),
        source,
        PlayerId(0),
    )
}

/// A source object carrying a spellbook list.
fn source_with_spellbook(
    state: &mut GameState,
    names: &[&str],
) -> crate::types::identifiers::ObjectId {
    let id = create_object(
        state,
        CardId(1),
        PlayerId(0),
        "Adaptive Armorer".to_string(),
        Zone::Battlefield,
    );
    state.objects.get_mut(&id).unwrap().spellbook = names.iter().map(|s| s.to_string()).collect();
    id
}

#[test]
fn resolve_raises_choice_from_the_sources_spellbook() {
    // The resolver reads the list off the source object and pauses for a choice.
    let mut state = GameState::new_two_player(42);
    let source = source_with_spellbook(&mut state, &["Fireshrieker", "Lion Sash", "Fishing Pole"]);

    let mut events = Vec::new();
    resolve(&mut state, &draft_ability(source, Zone::Hand), &mut events).expect("resolves");

    match &state.waiting_for {
        WaitingFor::SpellbookDraft {
            player,
            options,
            destination,
            ..
        } => {
            assert_eq!(*player, PlayerId(0));
            assert_eq!(options.len(), 3);
            assert!(options.iter().any(|o| o == "Lion Sash"));
            assert_eq!(*destination, Zone::Hand);
        }
        other => panic!("expected SpellbookDraft, got {other:?}"),
    }
}

#[test]
fn resolve_is_a_noop_when_the_source_has_no_spellbook() {
    // With no spellbook list, the draft resolves without pausing.
    let mut state = GameState::new_two_player(42);
    let source = source_with_spellbook(&mut state, &[]);

    let mut events = Vec::new();
    resolve(&mut state, &draft_ability(source, Zone::Hand), &mut events).expect("resolves");

    assert!(
        !matches!(state.waiting_for, WaitingFor::SpellbookDraft { .. }),
        "an empty spellbook must not pause on a choice"
    );
}

#[test]
fn resolve_chain_stashes_spellbook_continuation_until_choice_resolves() {
    let mut state = GameState::new_two_player(42);
    let source = source_with_spellbook(&mut state, &["Fireshrieker"]);
    create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Drawn Card".to_string(),
        Zone::Library,
    );

    let draw_tail = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        source,
        PlayerId(0),
    );
    let ability = draft_ability(source, Zone::Hand).sub_ability(draw_tail);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("resolves to choice");

    assert!(matches!(
        state.waiting_for,
        WaitingFor::SpellbookDraft { .. }
    ));
    assert!(
        state.pending_continuation.is_some(),
        "the Draw tail must wait until the spellbook choice is submitted"
    );
    assert_eq!(
        state.players[0].hand.len(),
        0,
        "the tail must not run before the spellbook choice resolves"
    );
}

#[test]
fn complete_draft_conjures_the_chosen_card_into_the_destination() {
    // Choosing a card from the list creates it in the destination zone (via the
    // shared conjure path).
    let mut state = GameState::new_two_player(42);
    let source = source_with_spellbook(&mut state, &["Fireshrieker", "Lion Sash"]);
    let options = vec!["Fireshrieker".to_string(), "Lion Sash".to_string()];

    let mut events = Vec::new();
    complete_draft(
        &mut state,
        PlayerId(0),
        source,
        &options,
        "Lion Sash",
        Zone::Hand,
        false,
        &mut events,
    )
    .expect("the chosen card is conjured");

    let made = state.players[0]
        .hand
        .iter()
        .filter_map(|id| state.objects.get(id))
        .any(|o| o.name == "Lion Sash");
    assert!(made, "the chosen card is created in the controller's hand");
}

#[test]
fn complete_draft_rejects_a_card_not_in_the_offered_list() {
    let mut state = GameState::new_two_player(42);
    let source = source_with_spellbook(&mut state, &["Fireshrieker"]);
    let options = vec!["Fireshrieker".to_string()];

    let mut events = Vec::new();
    let result = complete_draft(
        &mut state,
        PlayerId(0),
        source,
        &options,
        "Black Lotus",
        Zone::Hand,
        false,
        &mut events,
    );
    assert!(result.is_err(), "a card outside the spellbook is illegal");
}

#[test]
fn parser_maps_draft_clauses_to_the_right_destination() {
    // Default → hand; "put it onto the battlefield" → battlefield; "exile it" → exile.
    // Trailing periods (as cards actually print) must still match.
    assert!(matches!(
        parse_effect("draft a card from Big Spender's spellbook."),
        Effect::DraftFromSpellbook {
            destination: Zone::Hand,
            tapped: false,
        }
    ));
    assert!(matches!(
        parse_effect(
            "draft a card from Adaptive Armorer's spellbook and put it onto the battlefield."
        ),
        Effect::DraftFromSpellbook {
            destination: Zone::Battlefield,
            tapped: false,
        }
    ));
    assert!(matches!(
        parse_effect("draft a card from this creature's spellbook and exile it."),
        Effect::DraftFromSpellbook {
            destination: Zone::Exile,
            tapped: false,
        }
    ));
}

#[test]
fn parser_honours_the_tapped_rider() {
    // CR-correct battlefield state: "...onto the battlefield tapped." sets tapped.
    assert!(matches!(
        parse_effect(
            "draft a card from this creature's spellbook and put it onto the battlefield tapped."
        ),
        Effect::DraftFromSpellbook {
            destination: Zone::Battlefield,
            tapped: true,
        }
    ));
}

#[test]
fn parser_rejects_unmodeled_riders_as_unimplemented() {
    // "exile it face down", "twice, then …", and other unmodeled tails must NOT
    // collapse to a wrong effect — they fall through to a clean Unimplemented so
    // the coverage tooling flags them (and no clause is silently swallowed).
    for text in [
        "draft a card from this creature's spellbook and exile it face down.",
        "draft a card from this creature's spellbook twice, then put those cards onto the battlefield.",
        "draft a card from this creature's spellbook twice, then put one of those cards onto the battlefield tapped.",
    ] {
        assert!(
            !matches!(parse_effect(text), Effect::DraftFromSpellbook { .. }),
            "unmodeled spellbook rider must not parse to DraftFromSpellbook: {text:?}"
        );
    }
}
