//! Issue #2005 — Lead the Stampede: "When opponent resolves Lead the Stampede
//! and selects cards, I can't see the cards, but they have to be revealed."
//!
//! Determines the layer that owns the bug. Lead the Stampede parses to a
//! reveal-dig (`Effect::Dig { reveal: true, .. }`): "Look at the top five cards
//! of your library. You may reveal any number of creature cards from among them
//! and put the revealed cards into your hand. Put the rest on the bottom ...".
//!
//! CR 701.20b / CR 701.20a: revealed cards are public to ALL players, and a
//! reveal-dig must keep the looked-at cards public for the duration of the
//! `DigChoice`. This test drives the real resolution path and asserts the
//! ENGINE exposes those cards in a non-controller's filtered view — i.e. the
//! data the opponent needs is present at the engine boundary. If this passes,
//! the reported "can't see the cards" symptom is a frontend rendering gap, not
//! an engine/visibility defect.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect};
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const LEAD_ORACLE: &str = "Look at the top five cards of your library. You may reveal \
    any number of creature cards from among them and put the revealed cards into your \
    hand. Put the rest on the bottom of your library in any order.";

fn library_creature(state: &mut GameState, card_id: u64, owner: PlayerId, name: &str) -> ObjectId {
    let oid = create_object(
        state,
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Library,
    );
    let obj = state.objects.get_mut(&oid).expect("just created");
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    oid
}

#[test]
fn lead_the_stampede_reveals_dig_cards_to_opponent_during_choice() {
    let def = parse_effect_chain(LEAD_ORACLE, AbilityKind::Spell);
    assert!(
        matches!(def.effect.as_ref(), Effect::Dig { reveal: true, .. }),
        "Lead the Stampede must parse to a reveal-dig: {:?}",
        def.effect
    );

    let mut state = GameState::new_two_player(2005);

    // Six creature cards in the caster's library: the top five are looked at and
    // revealed; the sixth sits below the look window and must stay hidden.
    let lib: Vec<ObjectId> = (0..6)
        .map(|i| library_creature(&mut state, 100 + i, PlayerId(0), &format!("Beast {i}")))
        .collect();

    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Lead the Stampede".to_string(),
        Zone::Stack,
    );
    let ability = build_resolved_from_def(&def, source, PlayerId(0));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0)
        .expect("Lead the Stampede resolves");

    // The reveal-dig pauses on a player selection (DigChoice), the window during
    // which the looked-at cards are public.
    assert!(
        matches!(state.waiting_for, WaitingFor::DigChoice { .. }),
        "reveal-dig must surface a DigChoice, got {:?}",
        state.waiting_for
    );

    // Engine marks the looked-at cards publicly revealed (CR 701.20a). There must
    // be at least one revealed card and at least one unrevealed library card to
    // make the visibility contrast meaningful.
    let revealed: Vec<ObjectId> = lib
        .iter()
        .copied()
        .filter(|id| state.revealed_cards.contains(id))
        .collect();
    let unrevealed: Vec<ObjectId> = lib
        .iter()
        .copied()
        .filter(|id| !state.revealed_cards.contains(id))
        .collect();
    assert!(
        !revealed.is_empty(),
        "reveal-dig must publish looked-at cards"
    );
    assert!(
        !unrevealed.is_empty(),
        "a card below the five-card look window must remain unrevealed"
    );

    // The opponent's filtered view (PlayerId(1)) MUST expose the revealed cards'
    // real identities — this is the data a client needs to display the reveal.
    let opp_view = filter_state_for_viewer(&state, PlayerId(1));
    for id in &revealed {
        let real_name = &state.objects[id].name;
        assert_eq!(
            &opp_view.objects[id].name, real_name,
            "opponent must see the revealed dig card's identity (CR 701.20b), got redaction"
        );
        assert!(
            !opp_view.objects[id].face_down,
            "revealed dig card must not be face-down in the opponent's view"
        );
    }

    // Contrast: a library card outside the look window stays redacted for the
    // opponent, proving the exposure above is reveal-scoped, not a blanket leak.
    for id in &unrevealed {
        assert_eq!(
            opp_view.objects[id].name, "Hidden Card",
            "unrevealed library card must remain hidden from the opponent"
        );
    }
}
