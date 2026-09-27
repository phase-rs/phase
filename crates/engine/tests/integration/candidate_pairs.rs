use std::collections::BTreeSet;

use engine::analysis::{candidate_cycles, candidate_pairs};
use engine::types::card::CardFace;

fn fixture_face(name: &str) -> &'static CardFace {
    crate::support::shared_card_db()
        .expect("the integration card fixture must load")
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("the integration card fixture must contain {name}"))
}

fn fixture_pairs() -> [[&'static CardFace; 2]; 2] {
    [
        [
            fixture_face("Priest of Titania"),
            fixture_face("Umbral Mantle"),
        ],
        [
            fixture_face("Heliod, Sun-Crowned"),
            fixture_face("Walking Ballista"),
        ],
    ]
}

#[test]
fn candidate_pairs_returns_at_most_the_two_input_faces() {
    let faces = [
        fixture_face("Priest of Titania"),
        fixture_face("Umbral Mantle"),
        fixture_face("Heliod, Sun-Crowned"),
        fixture_face("Walking Ballista"),
    ];
    let candidates = candidate_pairs(&faces);
    assert!(
        !candidates.is_empty(),
        "the fixture must exercise at least one candidate pair"
    );
    for candidate in candidates {
        let allowed = BTreeSet::from([
            faces[candidate.left].name.as_str(),
            faces[candidate.right].name.as_str(),
        ]);
        assert!(candidate
            .cycle
            .faces
            .iter()
            .all(|name| allowed.contains(name.as_str())));
    }
}

#[test]
fn candidate_pairs_matches_candidate_cycles_on_two_faces() {
    for faces in fixture_pairs() {
        let expected = candidate_cycles(&faces);
        let actual = candidate_pairs(&faces);
        assert_eq!(
            actual
                .iter()
                .map(|candidate| &candidate.cycle)
                .collect::<Vec<_>>(),
            expected.iter().collect::<Vec<_>>()
        );
        assert!(actual
            .iter()
            .all(|candidate| candidate.left == 0 && candidate.right == 1));
    }
}

#[test]
fn thoracle_consultation_produces_no_candidate() {
    let faces = [
        fixture_face("Thassa's Oracle"),
        fixture_face("Demonic Consultation"),
    ];
    // This is a one-shot win, not a cycle. Keep the SCC cycle test strict: a
    // missing candidate here is an intentional detector blind spot.
    assert!(candidate_pairs(&faces).is_empty());
}

#[test]
fn a_face_that_pairs_with_nothing_is_absent_from_the_output() {
    let lonely = CardFace {
        name: "Pairs With Nothing".to_string(),
        ..CardFace::default()
    };
    let faces = [
        fixture_face("Priest of Titania"),
        fixture_face("Umbral Mantle"),
        &lonely,
    ];
    let candidates = candidate_pairs(&faces);
    assert!(!candidates.is_empty());
    assert!(candidates
        .iter()
        .all(|candidate| candidate.left != 2 && candidate.right != 2));
}

#[test]
fn candidate_pairs_is_deterministic() {
    let faces = [
        fixture_face("Priest of Titania"),
        fixture_face("Umbral Mantle"),
        fixture_face("Heliod, Sun-Crowned"),
        fixture_face("Walking Ballista"),
    ];
    assert_eq!(candidate_pairs(&faces), candidate_pairs(&faces));
}
