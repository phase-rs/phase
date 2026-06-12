//! Tests for Splice onto Arcane (CR 702.47). Declared from `game/mod.rs` so
//! `splice.rs` stays implementation-only.

use super::splice::{eligible_splice_cards, resolve_offer};
use crate::database::mtgjson::parse_mtgjson_mana_cost;
use crate::game::zones::create_object;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ResolvedAbility, TargetFilter,
};
use crate::types::game_state::{GameState, PendingCast, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// A simple controller-targeted spell effect, so a built `ResolvedAbility`
/// resolves without an interactive target slot.
fn gain_two_life() -> Effect {
    Effect::GainLife {
        amount: crate::types::ability::QuantityExpr::Fixed { value: 2 },
        player: TargetFilter::Controller,
    }
}

/// Create an Arcane instant being cast (host spell) on the stack, owned by p0.
fn arcane_host(state: &mut GameState, subtype: &str) -> ObjectId {
    let id = create_object(
        state,
        CardId(1),
        PlayerId(0),
        "Host Spell".to_string(),
        Zone::Stack,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.subtypes.push(subtype.to_string());
    id
}

/// Put a "Splice onto [subtype] [cost]" card with a spell ability into p0's hand.
fn splice_card_in_hand(state: &mut GameState, card: u64, subtype: &str, cost: &str) -> ObjectId {
    let id = create_object(
        state,
        CardId(card),
        PlayerId(0),
        format!("Splicer {card}"),
        Zone::Hand,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.keywords.push(Keyword::Splice {
        subtype: subtype.to_string(),
        cost: parse_mtgjson_mana_cost(cost),
    });
    obj.abilities = std::sync::Arc::new(vec![AbilityDefinition::new(
        AbilityKind::Spell,
        gain_two_life(),
    )]);
    id
}

fn host_pending(state: &GameState, host: ObjectId, cost: &str) -> PendingCast {
    let ability = ResolvedAbility::new(gain_two_life(), Vec::new(), host, PlayerId(0));
    let mut pending = PendingCast::new(host, CardId(1), ability, parse_mtgjson_mana_cost(cost));
    pending.base_cost = Some(parse_mtgjson_mana_cost(cost));
    let _ = state;
    pending
}

#[test]
fn eligible_detects_matching_arcane_splice_card() {
    // CR 702.47a: a "Splice onto Arcane" card in hand is offered when casting
    // an Arcane spell; an unrelated hand card is not.
    let mut state = GameState::new_two_player(42);
    let host = arcane_host(&mut state, "Arcane");
    let splicer = splice_card_in_hand(&mut state, 2, "Arcane", "{1}{R}");
    let _vanilla = create_object(
        &mut state,
        CardId(9),
        PlayerId(0),
        "Bear".to_string(),
        Zone::Hand,
    );

    let eligible = eligible_splice_cards(&state, PlayerId(0), host);
    assert_eq!(eligible, vec![splicer]);
}

#[test]
fn eligible_rejects_non_arcane_spell() {
    // CR 702.47a: the host spell's subtype must match the splice subtype.
    let mut state = GameState::new_two_player(42);
    let host = arcane_host(&mut state, "Trap");
    splice_card_in_hand(&mut state, 2, "Arcane", "{1}{R}");

    assert!(eligible_splice_cards(&state, PlayerId(0), host).is_empty());
}

#[test]
fn eligible_rejects_subtype_mismatch() {
    // CR 702.47a: a "Splice onto Trap" card is not offered for an Arcane spell.
    let mut state = GameState::new_two_player(42);
    let host = arcane_host(&mut state, "Arcane");
    splice_card_in_hand(&mut state, 2, "Trap", "{1}{R}");

    assert!(eligible_splice_cards(&state, PlayerId(0), host).is_empty());
}

#[test]
fn splicing_folds_cost_merges_ability_and_reoffers() {
    // CR 702.47b/c/e: splicing one of two eligible cards folds its cost into the
    // host cost, appends its text box to the host ability chain, reveals it (it
    // stays in hand), and re-presents the offer for the remaining card.
    let mut state = GameState::new_two_player(42);
    let host = arcane_host(&mut state, "Arcane");
    let first = splice_card_in_hand(&mut state, 2, "Arcane", "{2}");
    let second = splice_card_in_hand(&mut state, 3, "Arcane", "{1}");
    let pending = host_pending(&state, host, "{1}");
    let eligible = eligible_splice_cards(&state, PlayerId(0), host);
    assert_eq!(eligible.len(), 2);

    let mut events = Vec::new();
    let next = resolve_offer(
        &mut state,
        PlayerId(0),
        pending,
        eligible,
        Some(first),
        &mut events,
    )
    .expect("splice should resolve");

    let WaitingFor::SpliceOffer {
        pending_cast,
        eligible,
        ..
    } = next
    else {
        panic!("expected the offer to be re-presented for the remaining card");
    };

    // CR 702.47b: {1} host + {2} splice = {3}.
    assert_eq!(pending_cast.cost, parse_mtgjson_mana_cost("{3}"));
    // CR 702.47c: the spliced text box is appended to the host ability chain.
    assert!(pending_cast.ability.sub_ability.is_some());
    // CR 702.47e: only the not-yet-spliced card remains eligible.
    assert_eq!(eligible, vec![second]);
    // CR 702.47a: the spliced card was revealed and stays in hand.
    assert!(events
        .iter()
        .any(|e| matches!(e, crate::types::events::GameEvent::CardsRevealed { .. })));
    assert!(state.players[0].hand.contains(&first));
}

#[test]
fn declining_proceeds_past_the_offer() {
    // CR 601.2c: declining the offer (card: None) leaves the splice step and
    // advances the cast — it does not loop back to another SpliceOffer.
    let mut state = GameState::new_two_player(42);
    let host = arcane_host(&mut state, "Arcane");
    splice_card_in_hand(&mut state, 2, "Arcane", "{2}");
    let pending = host_pending(&state, host, "{1}");
    let eligible = eligible_splice_cards(&state, PlayerId(0), host);

    let mut events = Vec::new();
    let result = resolve_offer(
        &mut state,
        PlayerId(0),
        pending,
        eligible,
        None,
        &mut events,
    );

    // Declining leaves the splice step and advances the cast (here into cost
    // payment); it must never loop back to another SpliceOffer.
    assert!(
        !matches!(result, Ok(WaitingFor::SpliceOffer { .. })),
        "declining must not re-present the splice offer"
    );
}

#[test]
fn unknown_card_is_rejected() {
    // A card not in the eligible set cannot be spliced.
    let mut state = GameState::new_two_player(42);
    let host = arcane_host(&mut state, "Arcane");
    splice_card_in_hand(&mut state, 2, "Arcane", "{2}");
    let pending = host_pending(&state, host, "{1}");
    let eligible = eligible_splice_cards(&state, PlayerId(0), host);

    let mut events = Vec::new();
    let result = resolve_offer(
        &mut state,
        PlayerId(0),
        pending,
        eligible,
        Some(ObjectId(9999)),
        &mut events,
    );
    assert!(result.is_err());
}
