//! Composite-name resolution for multi-face cards, exercised across the crate
//! boundary through `CardDatabase`'s public API.
//!
//! `CardDatabase::lookup_key` is `pub(crate)` by design — its doc comment is
//! explicit that callers must not re-implement `//` splitting and that the
//! function must not be widened. No `pub` seam was added for this file:
//! `get_face_by_name` is ALREADY public and already routes every name through
//! `lookup_key`, so composite resolution is observable from outside the crate
//! as it stands. Asserting through it is also the stronger choice — it pins
//! the behavior at the surface external code actually depends on, so renaming,
//! inlining, or re-scoping `lookup_key` cannot silently break decklist loading
//! while this file still compiles.
//!
//! Note that `get_by_name` is NOT usable here: `from_export_entries` builds
//! `cards: HashMap::new()`, so the rules map is empty for any export-built
//! database and that accessor returns `None` for every name, composite or not.
//! Only the MTGJSON loader populates it.
//!
//! None of the three cards named here is newly unlocked — all three already
//! resolve, because `data/card-data.json` keys every face individually (of its
//! 35,798 keys exactly one contains `//`) and `lookup_key`'s `//`-splitting
//! predates this file. These eight tests exist as a regression barrier around
//! `lookup_key` becoming the sole name-resolution authority, not as evidence of
//! newly supported cards. Their layouts and face names were verified against
//! the Scryfall API rather than recalled: `Response // Resurgence` and
//! `Rough // Tumble` are `split`, `Dowsing Dagger // Lost Vale` is `transform`.
//! Both layouts are one physical card and must collapse to the front face
//! identically.
//!
//! CR 709.2: although split cards have two castable halves, each split card is
//! only one card — so a deck entry for `"Fire // Ice"` is one copy of that
//! card, not two.
//! CR 712.1: a double-faced card has a Magic card face on one side and either a
//! Magic card face or half of an oversized card on the other; it, too, is one
//! card.

use std::collections::HashMap;

use engine::database::card_db::CardDatabase;
use engine::types::card::CardFace;

/// Build a database whose keys are individual faces, mirroring
/// `data/card-data.json`, which "stores each face under its own key and
/// contains no composite `\"A // B\"` keys". That absence is the whole point:
/// composite support rests entirely on name resolution, with no data-level
/// backstop to mask a regression.
fn db_with_faces(face_names: &[&str]) -> CardDatabase {
    let mut map: HashMap<String, CardFace> = HashMap::new();
    for name in face_names {
        let face = CardFace {
            name: (*name).to_string(),
            ..CardFace::default()
        };
        map.insert(name.to_lowercase(), face);
    }
    let json = serde_json::to_string(&map).expect("faces must serialize");
    CardDatabase::from_json_str(&json).expect("face-keyed export must load")
}

fn resolved_name<'a>(db: &'a CardDatabase, queried: &str) -> Option<&'a str> {
    db.get_face_by_name(queried).map(|face| face.name.as_str())
}

/// CR 709.2 / CR 712.1: every spelling a decklist can plausibly use for a
/// multi-face card resolves to the SAME single front face — the canonical
/// spaced form, the hand-typed glued form, the front name alone, and any
/// casing of each.
#[test]
fn every_composite_spelling_resolves_to_the_one_front_face() {
    // Two split cards (CR 709.2) and one transforming DFC (CR 712.1); the
    // layouts differ but the identity rule does not, so both must collapse
    // identically.
    let cases: [(&str, &str, &str); 3] = [
        ("Response", "Resurgence", "Response"),
        ("Rough", "Tumble", "Rough"),
        ("Dowsing Dagger", "Lost Vale", "Dowsing Dagger"),
    ];

    for (front, back, expected) in cases {
        let db = db_with_faces(&[front, back]);

        for spelling in [
            format!("{front} // {back}"),
            format!("{front}//{back}"),
            format!("{front} //{back}"),
            format!("{front}// {back}"),
            front.to_string(),
            format!("{front} // {back}").to_lowercase(),
            format!("{front} // {back}").to_uppercase(),
        ] {
            assert_eq!(
                resolved_name(&db, &spelling),
                Some(expected),
                "{spelling:?} must resolve to the front face {expected:?}"
            );
        }

        // PREMISE: the back face really is present under its own key, so the
        // assertions above demonstrate a deliberate front-face collapse rather
        // than the back face merely being absent from the database.
        assert_eq!(
            resolved_name(&db, back),
            Some(back),
            "{back:?} must still be reachable by its own name"
        );
    }
}

/// CR 709.4a: each split card has two names, and an effect instructing a player
/// to choose a card name must choose one, not both. Composite resolution is
/// deliberately lossy in that direction — the BACK half is not reachable
/// through a composite name — so this pins the documented asymmetry rather than
/// leaving it to be "fixed" by a future refactor that would break name-choice
/// effects.
#[test]
fn composite_resolution_never_yields_the_back_face() {
    let db = db_with_faces(&["Response", "Resurgence"]);

    assert_eq!(
        resolved_name(&db, "Response // Resurgence"),
        Some("Response")
    );
    assert_ne!(
        resolved_name(&db, "Response // Resurgence"),
        Some("Resurgence"),
        "a composite name must collapse to the FRONT face; routing name-choice \
         effects through this collapse would violate CR 709.4a"
    );
    // Reversing the halves does not make the back face the answer: the name is
    // resolved by position, not by membership.
    assert_eq!(
        resolved_name(&db, "Resurgence // Response"),
        Some("Resurgence"),
        "resolution takes whichever half is written first"
    );
}

/// Issue #4790: `"SP//dr, Piloted by Peni"` is a SINGLE-faced card whose printed
/// name literally contains `//`. Exact-match must precede the `//` split, or a
/// legitimate name is mistaken for a composite one and resolves to a
/// nonexistent `"SP"` face. This is the false-positive guard, asserted here at
/// the public boundary.
#[test]
fn a_single_face_name_containing_a_double_slash_is_not_split() {
    let db = db_with_faces(&["SP//dr, Piloted by Peni"]);

    assert_eq!(
        resolved_name(&db, "SP//dr, Piloted by Peni"),
        Some("SP//dr, Piloted by Peni")
    );
    // PREMISE: no "sp" face exists, so a wrongly-split lookup would miss
    // entirely rather than silently returning something plausible.
    assert_eq!(resolved_name(&db, "SP"), None);
}

/// The exact-match-first ordering is not merely a nicety: when a card is
/// printed with a `//` in its own name AND a same-named front face exists,
/// the whole name must still win. Without the ordering this resolves to the
/// wrong card rather than merely failing.
#[test]
fn an_exact_whole_name_match_beats_the_split_interpretation() {
    let db = db_with_faces(&["SP//dr, Piloted by Peni", "SP"]);

    assert_eq!(
        resolved_name(&db, "SP//dr, Piloted by Peni"),
        Some("SP//dr, Piloted by Peni"),
        "the whole printed name must win over splitting it into \"SP\" // \"dr, …\""
    );
}

/// Decklists are typed by hand and routinely drop diacritics. The alias fold
/// must survive composite resolution, since both steps run inside the same
/// lookup and a naive ordering would apply only one of them.
#[test]
fn unaccented_spellings_resolve_through_composite_names_too() {
    let db = db_with_faces(&["Nazgûl", "Séance Board"]);

    assert_eq!(resolved_name(&db, "Nazgul"), Some("Nazgûl"));
    assert_eq!(
        resolved_name(&db, "Nazgul // Seance Board"),
        Some("Nazgûl"),
        "the unaccented fold must apply to the front segment of a composite name"
    );
}

/// A composite name naming a card that is not in the database must resolve to
/// nothing, not to a partial or fabricated match — otherwise an unknown-card
/// report would silently swallow typos.
#[test]
fn an_unknown_composite_name_resolves_to_nothing() {
    let db = db_with_faces(&["Response", "Resurgence"]);

    assert_eq!(resolved_name(&db, "Nonexistent // Card"), None);
    assert_eq!(resolved_name(&db, "Nonexistent//Card"), None);
}

/// Composite resolution must not depend on the two halves being adjacent, or
/// on either half being the only entry — a real database holds ~36k faces, and
/// the front segment is looked up by key rather than by scanning neighbours.
#[test]
fn composite_resolution_is_unaffected_by_unrelated_cards_in_the_database() {
    let db = db_with_faces(&[
        "Lightning Bolt",
        "Resurgence",
        "Grizzly Bears",
        "Response",
        "Island",
    ]);

    assert_eq!(
        resolved_name(&db, "Response // Resurgence"),
        Some("Response")
    );
    // A composite name built from two REAL but unrelated faces still resolves
    // to whichever is written first: resolution is by name, and the database
    // has no notion of which faces belong to the same physical card.
    assert_eq!(
        resolved_name(&db, "Lightning Bolt // Island"),
        Some("Lightning Bolt")
    );
}

/// Whitespace around the separator is normalized, so a decklist that pads it
/// (or that came through a formatter) still resolves. Guards the `trim()` on
/// the front segment.
#[test]
fn surrounding_whitespace_does_not_defeat_composite_resolution() {
    let db = db_with_faces(&["Rough", "Tumble"]);

    for spelling in [
        "Rough   //   Tumble",
        "Rough\t//\tTumble",
        "  Rough // Tumble",
    ] {
        assert_eq!(
            resolved_name(&db, spelling),
            Some("Rough"),
            "{spelling:?} must resolve to the front face"
        );
    }
}
