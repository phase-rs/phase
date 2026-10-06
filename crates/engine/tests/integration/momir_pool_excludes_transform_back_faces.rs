//! CR 202.1b + CR 202.3b + CR 712.8a: a transform/flip/meld BACK face must
//! never be a Momir-pickable creature card.
//!
//! Outside the battlefield a double-faced card has only its front face's
//! characteristics, so its back face is not a separately castable creature card
//! and must not be drawable by `Effect::CreateTokenCopyFromPool`. A back face
//! has no printed mana cost, which maps to `ManaCost::NoCost` and therefore to
//! mana value 0 — without the guard it would be drawable at every `{X}` of 0.
//!
//! These assert on `face_is_eligible`, the resolver's single eligibility
//! authority, against the REAL card fixture. Asserting the predicate directly
//! (rather than sampling the random draw) makes the guarantee class-general and
//! deterministic: it covers every costless face in the corpus, not whichever
//! one an RNG seed happened to surface.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::effects::create_token_copy_from_pool::face_is_eligible;
use engine::types::ability::{Comparator, TargetFilter};
use engine::types::card_type::CoreType;
use engine::types::mana::ManaCost;

fn fixture_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        CardDatabase::from_mtgjson(&data.join("mtgjson/test_fixture.json"))
            .expect("CardDatabase::from_mtgjson should succeed")
    })
}

/// BUG A DISCRIMINATING TEST.
///
/// "Insectile Aberration" is the transform back face of Delver of Secrets. It
/// is a creature face with no printed mana cost, so before the guard it was
/// eligible at mana value 0.
#[test]
fn transform_back_face_is_never_eligible() {
    let db = fixture_db();
    let face = db
        .get_face_by_name("Insectile Aberration")
        .expect("test fixture must contain the transform back face 'Insectile Aberration'");

    assert!(
        face.card_type.core_types.contains(&CoreType::Creature),
        "precondition: the back face really is a creature face"
    );
    assert!(
        matches!(face.mana_cost, ManaCost::NoCost),
        "precondition: a transform back face carries no printed mana cost"
    );
    assert!(
        !face_is_eligible(face, Comparator::EQ, 0, &TargetFilter::Any),
        "CR 202.3b: the transform back face 'Insectile Aberration' must not be \
         drawable at mana value 0 — it is not a separately castable creature card"
    );
}

/// Generalization guard (build-for-the-class): NO creature face carrying no
/// castable mana cost may be drawable, at any bound, under any comparator.
#[test]
fn no_costless_creature_face_is_eligible_under_any_comparator() {
    let db = fixture_db();
    let comparators = [
        Comparator::EQ,
        Comparator::NE,
        Comparator::LE,
        Comparator::LT,
        Comparator::GE,
        Comparator::GT,
    ];

    let mut offenders: Vec<String> = Vec::new();
    for face in db.faces_in_scan_order() {
        if !matches!(face.mana_cost, ManaCost::NoCost) {
            continue;
        }
        // A costless face must be ineligible for every comparator and every
        // bound it could plausibly be compared against.
        for comparator in comparators {
            for bound in -1..=16 {
                if face_is_eligible(face, comparator, bound, &TargetFilter::Any) {
                    offenders.push(format!("{} ({comparator:?} {bound})", face.name));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "CR 202.1b: creature faces with no castable mana cost (transform/flip/meld \
         back faces) must never be Momir-pickable. Offenders: {offenders:?}"
    );
}

/// Positive control: a normal creature face with a real mana cost IS eligible
/// at its own mana value. Without this, the test above would still pass if
/// `face_is_eligible` were changed to reject everything.
#[test]
fn ordinary_creature_face_is_eligible_at_its_mana_value() {
    let db = fixture_db();
    let face = db
        .faces_in_scan_order()
        .find(|face| {
            face.card_type.core_types.contains(&CoreType::Creature)
                && !matches!(face.mana_cost, ManaCost::NoCost)
        })
        .expect("fixture must contain at least one ordinary creature card");

    let mana_value = face.mana_cost.mana_value() as i32;
    assert!(
        face_is_eligible(face, Comparator::EQ, mana_value, &TargetFilter::Any),
        "an ordinary creature card must be drawable at its own mana value ({})",
        face.name
    );
}
