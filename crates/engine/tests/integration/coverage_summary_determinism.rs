//! Identical card data yields byte-identical coverage and audit output.

use engine::database::CardDatabase;
use engine::game::coverage::{
    analyze_coverage, audit_resolver_features, audit_semantic, format_semantic_audit_markdown,
};
use flate2::read::GzDecoder;
use serde::Serialize;

fn load() -> CardDatabase {
    CardDatabase::from_export_reader(GzDecoder::new(
        &include_bytes!("../fixtures/integration_cards.json.gz")[..],
    ))
    .expect("integration fixture loads")
}

fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("summary serializes")
}

#[test]
fn summaries_do_not_depend_on_hash_order() {
    let (first, second) = (load(), load());
    let keys = |db: &CardDatabase| {
        db.face_iter()
            .map(|(k, _)| k.to_owned())
            .collect::<Vec<_>>()
    };
    assert!(
        keys(&first) != keys(&second),
        "the two loads must walk their hash maps in different orders"
    );

    // `assert!` rather than `assert_eq!`: a failure would print megabytes.
    assert!(
        json(&analyze_coverage(&first)) == json(&analyze_coverage(&second)),
        "analyze_coverage"
    );
    assert!(
        json(&audit_resolver_features(&first)) == json(&audit_resolver_features(&second)),
        "audit_resolver_features"
    );
    let (semantic_first, semantic_second) = (audit_semantic(&first), audit_semantic(&second));
    assert!(
        json(&semantic_first) == json(&semantic_second),
        "audit_semantic"
    );
    assert!(
        format_semantic_audit_markdown(&semantic_first)
            == format_semantic_audit_markdown(&semantic_second),
        "format_semantic_audit_markdown"
    );
}
