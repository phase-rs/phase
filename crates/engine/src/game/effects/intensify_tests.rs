//! Tests for Alchemy Intensity (`Effect::Intensify` + starting intensity).
//! Declared from `effects/mod.rs` so `intensify.rs` stays implementation-only.

use super::intensify::resolve;
use crate::game::printed_cards::apply_card_face_to_object;
use crate::game::zones::create_object;
use crate::types::ability::{Effect, IntensityScope, QuantityExpr, ResolvedAbility};
use crate::types::card::CardFace;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

fn intensify_ability(source: ObjectId, scope: IntensityScope, by: i32) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::Intensify {
            scope,
            amount: QuantityExpr::Fixed { value: by },
        },
        Vec::new(),
        source,
        PlayerId(0),
    )
}

fn card(state: &mut crate::types::game_state::GameState, name: &str, zone: Zone) -> ObjectId {
    create_object(state, CardId(1), PlayerId(0), name.to_string(), zone)
}

#[test]
fn source_scope_intensifies_only_the_source() {
    let mut state = crate::types::game_state::GameState::new_two_player(42);
    let a = card(&mut state, "Bellowsbreath Ogre", Zone::Battlefield);
    let b = card(&mut state, "Bellowsbreath Ogre", Zone::Hand);

    let mut events = Vec::new();
    resolve(
        &mut state,
        &intensify_ability(a, IntensityScope::Source, 2),
        &mut events,
    )
    .unwrap();

    assert_eq!(state.objects.get(&a).unwrap().intensity, 2);
    assert_eq!(
        state.objects.get(&b).unwrap().intensity,
        0,
        "only the source"
    );
}

#[test]
fn owned_same_name_intensifies_every_copy_across_zones() {
    // "cards you own named X intensify by 1" hits all owned copies, any zone.
    let mut state = crate::types::game_state::GameState::new_two_player(42);
    let bf = card(&mut state, "Arek", Zone::Battlefield);
    let hand = card(&mut state, "Arek", Zone::Hand);
    let lib = card(&mut state, "Arek", Zone::Library);
    let other = card(&mut state, "Someone Else", Zone::Hand);
    // A copy owned by the opponent must NOT be touched.
    let opp = create_object(
        &mut state,
        CardId(9),
        PlayerId(1),
        "Arek".to_string(),
        Zone::Hand,
    );

    let mut events = Vec::new();
    resolve(
        &mut state,
        &intensify_ability(bf, IntensityScope::OwnedSameName, 1),
        &mut events,
    )
    .unwrap();

    for id in [bf, hand, lib] {
        assert_eq!(
            state.objects.get(&id).unwrap().intensity,
            1,
            "all owned copies"
        );
    }
    assert_eq!(
        state.objects.get(&other).unwrap().intensity,
        0,
        "different name"
    );
    assert_eq!(
        state.objects.get(&opp).unwrap().intensity,
        0,
        "opponent's copy"
    );
}

#[test]
fn owned_subtype_intensifies_every_matching_card() {
    let mut state = crate::types::game_state::GameState::new_two_player(42);
    let chorus_a = card(&mut state, "Hymn to the Ages", Zone::Hand);
    let chorus_b = card(&mut state, "Ribald Shanty", Zone::Library);
    let plain = card(&mut state, "Mountain", Zone::Battlefield);
    for id in [chorus_a, chorus_b] {
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .subtypes
            .push("Chorus".to_string());
    }

    let mut events = Vec::new();
    resolve(
        &mut state,
        &intensify_ability(
            chorus_a,
            IntensityScope::OwnedSubtype {
                subtype: "Chorus".to_string(),
            },
            1,
        ),
        &mut events,
    )
    .unwrap();

    assert_eq!(state.objects.get(&chorus_a).unwrap().intensity, 1);
    assert_eq!(state.objects.get(&chorus_b).unwrap().intensity, 1);
    assert_eq!(
        state.objects.get(&plain).unwrap().intensity,
        0,
        "non-Chorus untouched"
    );
}

#[test]
fn starting_intensity_is_stamped_from_the_keyword_once() {
    // apply_card_face_to_object reads `Keyword::StartingIntensity` and sets it
    // on first application, then never resets it.
    let face = CardFace {
        name: "Great Desert Hellion".to_string(),
        keywords: vec![Keyword::StartingIntensity(1)],
        ..CardFace::default()
    };

    let mut state = crate::types::game_state::GameState::new_two_player(42);
    let id = card(&mut state, "Great Desert Hellion", Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    apply_card_face_to_object(obj, &face);
    assert_eq!(obj.intensity, 1);

    // Accumulate, then re-apply the face — intensity must NOT reset.
    obj.intensity = 5;
    apply_card_face_to_object(obj, &face);
    assert_eq!(
        obj.intensity, 5,
        "re-stamping the face must not reset intensity"
    );
}
