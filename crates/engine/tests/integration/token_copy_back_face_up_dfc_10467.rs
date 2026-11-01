//! #10467 — a token copy of a double-faced permanent is a double-faced token, and
//! enters with its back face up when the copied permanent had its back face up.
//!
//! CR 707.8a: "If an effect creates a token that is a copy of a double-faced
//! permanent or a double-faced card not on the battlefield, the resulting token is
//! a double-faced token that has both a front face and a back face. The
//! characteristics of each face are determined by the copiable values of the same
//! face of the permanent or card it is a copy of, as modified by any other copy
//! effects that apply to that object. If the token is a copy of a double-faced
//! permanent with its back face up, the token enters the battlefield with its back
//! face up. ..."
//!
//! Before the fix the token carried only the face that was up (CR 707.8) as a
//! single-faced object: `transformed == false`, `back_face == None`. A copy of a
//! transformed Thraben Gargoyle therefore looked right (Stonewing Antagonizer) but
//! was flagged front-face and could never transform back.
//!
//! Witness: Cackling Counterpart ("Create a token that's a copy of target creature
//! you control.") copying Thraben Gargoyle, driven through the real cast pipeline
//! and the real copy-token entry (`token_copy.rs` -> `materialize_token_copy_body`).
//! Every test carries a positive reach-guard (the source really is in the asserted
//! orientation, exactly one real token exists), so no assertion is vacuous, and the
//! front-face-up case is the paired positive for the back-face-up case.

use engine::game::game_object::BackFaceData;
use engine::game::printed_cards::swap_object_faces;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::transform::transform_permanent;
use engine::game::zones::move_to_zone;
use engine::types::card::LayoutKind;
use engine::types::card_type::{CardType, CoreType};
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CACKLING_COUNTERPART: &str = "Create a token that's a copy of target creature you control.";

/// Which face of the copied permanent is up when the copy is made.
#[derive(Clone, Copy, PartialEq, Debug)]
enum SourceFace {
    Front,
    Back,
    /// Transformed, bounced to hand and replayed through the production zone
    /// moves: front face up again, but the stored face is the layout-less swap
    /// snapshot `transform_permanent` made.
    RoundTripped,
}

/// The stored other face of Thraben Gargoyle // Stonewing Antagonizer, with the
/// printed layout `layout` (Transform for the real card; Modal exercises the MDFC
/// shape through the same copy path).
fn stonewing_antagonizer(layout: LayoutKind) -> BackFaceData {
    BackFaceData {
        is_swap_snapshot: false,
        trigger_printed_origins: Vec::new(),
        name: "Stonewing Antagonizer".to_string(),
        power: Some(4),
        toughness: Some(2),
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Gargoyle".to_string(), "Horror".to_string()],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![Keyword::Flying],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: Some(layout),
        parse_warnings: vec![],
    }
}

struct Copied {
    runner: GameRunner,
    source: ObjectId,
    token: ObjectId,
    events: Vec<GameEvent>,
}

/// Put a Thraben Gargoyle with the given other-face layout on the battlefield in the
/// given orientation, then cast Cackling Counterpart targeting it and resolve.
fn copy_gargoyle(layout: LayoutKind, face: SourceFace) -> Copied {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gargoyle = scenario.add_creature(P0, "Thraben Gargoyle", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Cackling Counterpart", true, CACKLING_COUNTERPART)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&gargoyle)
        .unwrap()
        .back_face = Some(stonewing_antagonizer(layout));
    if face == SourceFace::Back {
        match layout {
            // CR 701.27a: a transforming double-faced permanent turned over.
            LayoutKind::Transform => {
                let mut events = Vec::new();
                transform_permanent(runner.state_mut(), gargoyle, &mut events)
                    .expect("the Gargoyle has a back face to transform into");
            }
            // CR 712.8f: a modal double-faced permanent cast back face up shows its
            // stashed face through `modal_back_face` (the real cast path's shape).
            LayoutKind::Modal => {
                let obj = runner.state_mut().objects.get_mut(&gargoyle).unwrap();
                swap_object_faces(obj);
                obj.modal_back_face = true;
            }
            other => panic!("back-face-up fixture only builds Transform and Modal, got {other:?}"),
        }
    }
    if face == SourceFace::RoundTripped {
        assert_eq!(
            layout,
            LayoutKind::Transform,
            "round trip is the transform shape"
        );
        let mut events = Vec::new();
        transform_permanent(runner.state_mut(), gargoyle, &mut events)
            .expect("the Gargoyle has a back face to transform into");
        // Production zone exit (swap back to the front face) and re-entry.
        move_to_zone(runner.state_mut(), gargoyle, Zone::Hand, &mut events);
        move_to_zone(runner.state_mut(), gargoyle, Zone::Battlefield, &mut events);
    }

    let outcome = runner.cast(spell).target_object(gargoyle).resolve();
    let events = outcome.events().to_vec();
    drop(outcome);

    let tokens: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield)
        .map(|o| o.id)
        .collect();
    // REACH GUARD: the spell resolved through the real copy-token pipeline and made
    // exactly one token that is not the source. Without it the face assertions
    // below could be reading a fixture where no copy happened.
    assert_eq!(
        tokens.len(),
        1,
        "Cackling Counterpart must have created exactly one token"
    );
    assert_ne!(
        tokens[0], gargoyle,
        "the token is a new object, not the source"
    );
    Copied {
        token: tokens[0],
        source: gargoyle,
        runner,
        events,
    }
}

fn transformed_events(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::Transformed { .. }))
        .count()
}

/// CR 707.8a: the token is a copy of a transformed Thraben Gargoyle, so it enters
/// with its back face up (Stonewing Antagonizer), is double-faced, and keeps the
/// front face so a later transform turns it front face up (CR 701.27a).
#[test]
fn copy_of_back_face_up_transform_dfc_enters_back_face_up() {
    let mut c = copy_gargoyle(LayoutKind::Transform, SourceFace::Back);

    // REACH GUARD: the copied permanent really had its back face up.
    let source = &c.runner.state().objects[&c.source];
    assert!(
        source.transformed,
        "the copied permanent has its back face up"
    );
    assert_eq!(source.name, "Stonewing Antagonizer");

    let token = &c.runner.state().objects[&c.token];
    assert!(
        token.transformed,
        "CR 707.8a: a copy of a back-face-up double-faced permanent enters back face up"
    );
    assert_eq!(token.name, "Stonewing Antagonizer");
    assert_eq!(token.power, Some(4));
    assert_eq!(token.toughness, Some(2));
    assert_eq!(
        token.back_face.as_ref().map(|f| f.name.as_str()),
        Some("Thraben Gargoyle"),
        "CR 707.8a: the token is double-faced and keeps the front face"
    );
    // CR 712.14a: entering back face up is not a transform.
    assert_eq!(transformed_events(&c.events), 0, "no Transformed event");
    assert_eq!(token.transformation_count, 0);

    // CR 701.27a: it can transform, and does so to its front face.
    let mut events = Vec::new();
    transform_permanent(c.runner.state_mut(), c.token, &mut events).unwrap();
    let token = &c.runner.state().objects[&c.token];
    assert!(!token.transformed, "transforming turns it front face up");
    assert_eq!(token.name, "Thraben Gargoyle");
    assert_eq!(token.power, Some(2));
    assert_eq!(
        token.back_face.as_ref().map(|f| f.name.as_str()),
        Some("Stonewing Antagonizer")
    );
    assert_eq!(transformed_events(&events), 1);
}

/// Paired positive: a copy of a FRONT-face-up Thraben Gargoyle enters front face
/// up, is still a double-faced token with both faces (CR 707.8a), and transforms
/// to Stonewing Antagonizer.
#[test]
fn copy_of_front_face_up_transform_dfc_enters_front_face_up_and_is_double_faced() {
    let mut c = copy_gargoyle(LayoutKind::Transform, SourceFace::Front);

    let source = &c.runner.state().objects[&c.source];
    assert!(
        !source.transformed,
        "REACH GUARD: the copied permanent is front face up"
    );
    assert_eq!(source.name, "Thraben Gargoyle");

    let token = &c.runner.state().objects[&c.token];
    assert!(
        !token.transformed,
        "a copy of a front-face-up permanent enters front face up"
    );
    assert_eq!(token.name, "Thraben Gargoyle");
    assert_eq!(token.power, Some(2));
    assert_eq!(
        token.back_face.as_ref().map(|f| f.name.as_str()),
        Some("Stonewing Antagonizer"),
        "CR 707.8a: the token has both a front face and a back face"
    );
    assert_eq!(transformed_events(&c.events), 0);

    let mut events = Vec::new();
    transform_permanent(c.runner.state_mut(), c.token, &mut events).unwrap();
    let token = &c.runner.state().objects[&c.token];
    assert!(token.transformed);
    assert_eq!(token.name, "Stonewing Antagonizer");
    assert_eq!(token.power, Some(4));
    assert_eq!(
        token.back_face.as_ref().map(|f| f.name.as_str()),
        Some("Thraben Gargoyle")
    );
}

/// CR 707.8a covers every double-faced permanent, including a modal double-faced
/// permanent (CR 712.1) cast back face up (CR 712.8f).
#[test]
fn copy_of_back_face_up_modal_dfc_enters_back_face_up() {
    let c = copy_gargoyle(LayoutKind::Modal, SourceFace::Back);

    let source = &c.runner.state().objects[&c.source];
    assert!(
        source.modal_back_face,
        "REACH GUARD: the MDFC is cast back face up"
    );
    assert_eq!(source.name, "Stonewing Antagonizer");

    let token = &c.runner.state().objects[&c.token];
    assert!(
        token.modal_back_face && !token.transformed,
        "CR 707.8a: the token enters back face up, shown the way a modal DFC shows it"
    );
    assert_eq!(token.name, "Stonewing Antagonizer");
    assert_eq!(
        token.back_face.as_ref().map(|f| f.name.as_str()),
        Some("Thraben Gargoyle")
    );
    assert_eq!(transformed_events(&c.events), 0);
}

/// Paired positive for the modal shape.
#[test]
fn copy_of_front_face_up_modal_dfc_enters_front_face_up_and_is_double_faced() {
    let c = copy_gargoyle(LayoutKind::Modal, SourceFace::Front);

    let source = &c.runner.state().objects[&c.source];
    assert!(
        !source.modal_back_face && !source.transformed,
        "REACH GUARD: front face up"
    );

    let token = &c.runner.state().objects[&c.token];
    assert!(!token.transformed);
    assert!(
        !token.modal_back_face,
        "a copy of a front-face-up modal permanent is not shown back face up"
    );
    assert_eq!(token.name, "Thraben Gargoyle");
    assert_eq!(
        token.back_face.as_ref().map(|f| f.name.as_str()),
        Some("Stonewing Antagonizer")
    );
}

/// X1: a transforming permanent that was transformed, bounced and replayed is
/// front face up with `transformed == false` AND an erased stored-face layout
/// (`transform_permanent` snapshots the front face with `layout_kind: None`, zone
/// exit swaps it back preserving that). It is still a double-faced permanent, so
/// its token copy must be double-faced (CR 707.8a). Built entirely through the
/// production transform and zone-move path, not hand-stamped fields.
#[test]
fn copy_of_transformed_bounced_and_replayed_dfc_is_double_faced() {
    let mut c = copy_gargoyle(LayoutKind::Transform, SourceFace::RoundTripped);

    let source = &c.runner.state().objects[&c.source];
    // REACH GUARD: the exact shape that defeated a layout-only gate.
    assert_eq!(source.zone, Zone::Battlefield);
    assert!(!source.transformed, "front face up again");
    assert_eq!(source.name, "Thraben Gargoyle");
    let stored = source.back_face.as_ref().expect("the other face is stored");
    assert_eq!(stored.name, "Stonewing Antagonizer");
    assert!(stored.is_swap_snapshot, "stored by the production swap");
    assert_eq!(
        stored.layout_kind, None,
        "the transform round trip erased the layout"
    );

    let token = &c.runner.state().objects[&c.token];
    assert!(!token.transformed, "the source was front face up");
    assert_eq!(token.name, "Thraben Gargoyle");
    assert_eq!(
        token.back_face.as_ref().map(|f| f.name.as_str()),
        Some("Stonewing Antagonizer"),
        "CR 707.8a: the token is double-faced"
    );
    let mut events = Vec::new();
    transform_permanent(c.runner.state_mut(), c.token, &mut events)
        .expect("the token can transform");
    assert_eq!(
        c.runner.state().objects[&c.token].name,
        "Stonewing Antagonizer"
    );
}

/// Boundary (printed shapes: Alrund, God of the Cosmos // Hakka, Whispering Raven
/// for the modal fixtures, Bruna, the Fading Light // Brisela for meld): a meld card yields a single-faced token.
/// Meld cards cannot transform (CR 712.4c, CR 712.9), so 707.8a's double-faced
/// token shape is not given to a copy of one. The reach guard is that the copy
/// itself happened (`copy_gargoyle` asserts the token), and for the meld case that
/// the source really carries a Meld other face.
#[test]
fn copy_of_meld_card_is_not_a_double_faced_token() {
    let c = copy_gargoyle(LayoutKind::Meld, SourceFace::Front);
    let source = &c.runner.state().objects[&c.source];
    assert_eq!(
        source.back_face.as_ref().and_then(|f| f.layout_kind),
        Some(LayoutKind::Meld),
        "REACH GUARD: the source carries a meld other face"
    );
    let token = &c.runner.state().objects[&c.token];
    assert!(!token.transformed);
    assert!(
        token.back_face.is_none(),
        "a meld card's token copy is single-faced"
    );

    // PAIRED POSITIVE in the same scenario: the identical source with a
    // Transform other face DOES yield a double-faced token, so the assertions
    // above discriminate the feature, not just a loosened layout gate.
    let positive = copy_gargoyle(LayoutKind::Transform, SourceFace::Front);
    assert!(
        positive.runner.state().objects[&positive.token]
            .back_face
            .is_some(),
        "paired positive: a Transform DFC copy is double-faced"
    );
}

/// CR 707.8a + CR 712.8a: a double-faced CARD drawn from the Momir pool makes a
/// double-faced token that enters front face up. Deterministic without sampling the
/// RNG: against the real MTGJSON fixture, mana value 1 plus the Wizard subtype
/// admits exactly one face (Delver of Secrets), asserted by the reach guard below.
#[test]
fn momir_pool_pick_of_double_faced_card_makes_double_faced_token() {
    use engine::database::card_db::CardDatabase;
    use engine::game::effects::create_token_copy_from_pool::{face_is_eligible, resolve};
    use engine::types::ability::{
        CardSelectionMode, Comparator, Effect, QuantityExpr, ResolvedAbility, TargetFilter,
        TypeFilter, TypedFilter,
    };
    use engine::types::format::FormatConfig;
    use engine::types::game_state::GameState;
    use std::path::Path;
    use std::sync::Arc;

    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let db = Arc::new(
        CardDatabase::from_mtgjson(&data.join("mtgjson/test_fixture.json"))
            .expect("fixture database loads"),
    );
    let wizard = TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype("Wizard".to_string())));

    // REACH GUARD: exactly one pickable face, and it is the Delver front face.
    let eligible: Vec<&str> = db
        .faces_in_scan_order()
        .filter(|f| face_is_eligible(f, Comparator::EQ, 1, &wizard))
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(eligible, vec!["Delver of Secrets"], "the draw is forced");

    let mut state = GameState::new(FormatConfig::momir(), 2, 42);
    engine::game::install_card_db(&mut state, Arc::clone(&db));
    let effect = Effect::CreateTokenCopyFromPool {
        owner: TargetFilter::Controller,
        type_filter: wizard,
        mv: Comparator::EQ,
        mv_bound: QuantityExpr::Fixed { value: 1 },
        selection: CardSelectionMode::Random,
        count: QuantityExpr::Fixed { value: 1 },
        tapped: false,
        enters_attacking: false,
    };
    let ability = ResolvedAbility::new(effect, vec![], ObjectId(500), P0);
    let mut events = Vec::new();
    resolve(&mut state, &ability, &mut events).expect("pool copy resolves");

    let tokens: Vec<_> = state
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield)
        .collect();
    assert_eq!(tokens.len(), 1, "exactly one token was made");
    let token = tokens[0];
    assert_eq!(token.name, "Delver of Secrets");
    assert!(
        !token.transformed,
        "a card outside the battlefield is front face up"
    );
    assert_eq!(
        token.back_face.as_ref().map(|f| f.name.as_str()),
        Some("Insectile Aberration"),
        "CR 707.8a: the pool token is double-faced"
    );
}

/// A populated `DoubleFacedCopy` survives a serde round trip in every orientation
/// (it rides in serialized in-flight copy-token state and the CR 733 journal).
#[test]
fn double_faced_copy_serde_round_trips_every_orientation() {
    use engine::types::proposed_event::{DoubleFacedCopy, DoubleFacedOrientation};
    for orientation in [
        DoubleFacedOrientation::FrontUp,
        DoubleFacedOrientation::TransformedBack,
        DoubleFacedOrientation::ModalBack,
    ] {
        let original = DoubleFacedCopy {
            other_face: Box::new(stonewing_antagonizer(LayoutKind::Modal)),
            orientation,
        };
        let json = serde_json::to_string(&original).expect("serializes");
        let back: DoubleFacedCopy = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, original, "{orientation:?} round trips");
    }
}
